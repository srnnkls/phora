//! State-dir lifecycle: record each project's root, classify every state dir, drop the dead ones.
#![allow(dead_code, reason = "stubs wired by T004 to T008")]

mod model;
mod wire;

pub use model::*;

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use crate::mount::{
    AnchorDir, CurrentAnchor, Location, MountId, NormalPath, Placement, is_absent, locate,
};
use crate::sync::journal::Journal;
use crate::sync::nonce;
use crate::sync::observe::normalize_history_deploy_root_at;
use crate::sync::prune::orphan_artifact_path;
use crate::sync::state::file::atomic_write;
use crate::sync::state::locking::lock_existing;
use crate::sync::state::{
    ArtifactRecord, FileStateStore, StateError, StateLock, StateStore as _, ejected_index,
};

const META_FILE: &str = "project.toml";
const INDEX_FILE: &str = "state-roots.toml";
const INDEX_LOCK_FILE: &str = "state-roots.lock";
const TRASH_DIR: &str = "trash";
/// Fresh suffixes tried when a crashed run left an entry under the same name.
const TRASH_ATTEMPTS: u32 = 8;

/// The project's own locked run: refresh its `project.toml`, register a non-default state root,
/// and mark every other copy of its id superseded. Each failure comes back as a warning.
pub fn record_own_run(
    own: &StateDirPath,
    default: &StateRoot,
    root: &ProjectRoot,
    _held: &StateLock,
) -> Vec<StateError> {
    let mut warnings = Vec::new();
    if let Err(error) =
        meta_for(own, root).and_then(|meta| write_meta(own, &meta, &Succession::Current))
    {
        warnings.push(error);
    }
    let index = register_root(own.root(), default).unwrap_or_else(|error| {
        warnings.push(error);
        None
    });
    let copies: BTreeSet<&StateRoot> = std::iter::once(default)
        .chain(index.iter().flat_map(|index| index.roots().keys()))
        .filter(|copy_root| *copy_root != own.root())
        .collect();
    for copy_root in copies {
        let copy = StateDirPath::new(copy_root.clone(), own.id().clone());
        if let Err(error) = supersede(&copy, own.root(), root) {
            warnings.push(error);
        }
    }
    warnings
}

fn meta_for(dir: &StateDirPath, root: &ProjectRoot) -> Result<ProjectMeta, StateError> {
    let path = dir.path();
    let placement = Placement::of(root.path().as_path(), path.as_path())
        .map_err(|error| io_error("read mount of", path.as_path(), &error))?;
    Ok(ProjectMeta::new(
        root.clone(),
        placement,
        AnchorDir::new(path),
    ))
}

fn write_meta(
    dir: &StateDirPath,
    meta: &ProjectMeta,
    succession: &Succession,
) -> Result<(), StateError> {
    let text = wire::render_meta(meta, succession)
        .map_err(|error| StateError::StateStore(format!("serialize {META_FILE}: {error}")))?;
    atomic_write(&dir.path().as_path().join(META_FILE), &text)
}

/// Registers `own` when it is not the default root, under the index lock; the index as left
/// lists the roots whose copies to mark, and a corrupt or unreadable one comes back as an error.
fn register_root(
    own: &StateRoot,
    default: &StateRoot,
) -> Result<Option<StateRootIndex>, StateError> {
    let default_dir = default.path().as_path();
    std::fs::create_dir_all(default_dir)
        .map_err(|error| io_error("create state root", default_dir, &error))?;
    let lock_path = default_dir.join(INDEX_LOCK_FILE);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| io_error("open", &lock_path, &error))?;
    lock.lock()
        .map_err(|error| io_error("lock", &lock_path, &error))?;

    let index_path = default_dir.join(INDEX_FILE);
    let mut index = match read_index(&index_path) {
        IndexRead::Read(index) => index,
        IndexRead::Missing if own == default => return Ok(None),
        IndexRead::Missing => {
            StateRootIndex::new(AnchorDir::new(default.path().clone()), BTreeMap::new())
        }
        IndexRead::Corrupt(defect) => {
            return Err(StateError::StateStore(format!(
                "state-root index {} is corrupt ({defect:?}); registered state roots are skipped",
                index_path.display()
            )));
        }
        IndexRead::Unreadable(kind) => {
            return Err(StateError::StateStore(format!(
                "read state-root index {}: {kind}; registered state roots are skipped",
                index_path.display()
            )));
        }
    };
    if own != default {
        let placement = Placement::of(own.path().as_path(), default_dir)
            .map_err(|error| io_error("read mount of", own.path().as_path(), &error))?;
        if index.register(own.clone(), placement) {
            let text = wire::render_index(&index).map_err(|error| {
                StateError::StateStore(format!("serialize {INDEX_FILE}: {error}"))
            })?;
            atomic_write(&index_path, &text)?;
        }
    }
    Ok(Some(index))
}

fn read_index(path: &Path) -> IndexRead {
    match std::fs::read_to_string(path) {
        Ok(text) => wire::parse_index(&text).map_or_else(IndexRead::Corrupt, IndexRead::Read),
        Err(error) if error.kind() == ErrorKind::NotFound => IndexRead::Missing,
        Err(error) => IndexRead::Unreadable(error.kind()),
    }
}

/// Marks another copy of the project's id with its successor, backfilling a meta it lacks.
/// A vanished copy, one whose `state.lock` is held and one holding no state are left unmarked.
fn supersede(
    copy: &StateDirPath,
    successor: &StateRoot,
    root: &ProjectRoot,
) -> Result<(), StateError> {
    let dir = copy.path();
    match std::fs::symlink_metadata(dir.as_path()) {
        Ok(stat) if stat.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("stat", dir.as_path(), &error)),
    }
    let _lock = match lock_existing(dir.as_path()) {
        Ok(lock) => lock,
        Err(NotHeld::Vanished | NotHeld::Busy) => return Ok(()),
        Err(NotHeld::Fault(kind)) => {
            return Err(StateError::StateStore(format!(
                "lock {}: {kind}",
                dir.as_path().display()
            )));
        }
    };
    if !holds_state(dir.as_path())? {
        return Ok(());
    }
    let meta_path = dir.as_path().join(META_FILE);
    let meta = match std::fs::read_to_string(&meta_path) {
        Ok(text) => match wire::parse_meta(&text, copy.id()) {
            MetaRead::Valid {
                succession: Succession::SupersededBy(marked),
                ..
            } if marked == *successor => return Ok(()),
            MetaRead::Valid { meta, .. } => meta,
            MetaRead::Rootless(_) => meta_for(copy, root)?,
        },
        Err(error) if error.kind() == ErrorKind::NotFound => meta_for(copy, root)?,
        Err(error) => return Err(io_error("read", &meta_path, &error)),
    };
    write_meta(copy, &meta, &Succession::SupersededBy(successor.clone()))
}

/// Whether the dir holds records, target meta or a pending journal.
fn holds_state(dir: &Path) -> Result<bool, StateError> {
    if FileStateStore::open(dir.to_path_buf())?.holds_records_or_meta()? {
        return Ok(true);
    }
    let pending = Journal::open_readonly(&dir.join("locks"))
        .entries()
        .map_err(|error| StateError::StateStore(error.to_string()))?;
    Ok(!pending.is_empty())
}

fn io_error(what: &str, path: &Path, error: &std::io::Error) -> StateError {
    StateError::StateStore(format!("{what} {}: {error}", path.display()))
}

/// Gathers the filesystem facts `classify` reads; records only for a root AD-2 proves deleted.
pub fn probe(dir: &StateDirPath, anchor: &CurrentAnchor) -> StateDirProbe {
    probe_dir(dir, anchor).unwrap_or_else(StateDirProbe::Unreadable)
}

fn probe_dir(dir: &StateDirPath, anchor: &CurrentAnchor) -> Result<StateDirProbe, ErrorKind> {
    let path = dir.path();
    let path = path.as_path();
    let holds_state = FileStateStore::open(path.to_path_buf())
        .and_then(|store| store.holds_records_or_meta())
        .map_err(|_| ErrorKind::Other)?;
    let journal = if holds_state {
        None
    } else {
        match journal_state(path)? {
            JournalState::Clear => return Ok(StateDirProbe::Empty),
            pending @ JournalState::Pending { .. } => Some(pending),
        }
    };
    let (meta, succession) = match std::fs::read_to_string(path.join(META_FILE)) {
        Ok(text) => match wire::parse_meta(&text, dir.id()) {
            MetaRead::Valid { meta, succession } => (meta, succession),
            MetaRead::Rootless(meta) => return Ok(StateDirProbe::Unrooted { meta }),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(StateDirProbe::Unrooted {
                meta: RootlessMeta::Absent,
            });
        }
        Err(error) => return Err(error.kind()),
    };
    let root = match locate(
        meta.root().path(),
        *meta.placement(),
        meta.state_dir(),
        anchor,
    ) {
        Location::Present => RootObservation::Present {
            manifest: probe_manifest(meta.root()),
        },
        Location::Unproven(cause) => RootObservation::Unproven(cause),
        Location::Deleted(proof) => {
            let journal = match journal {
                Some(journal) => journal,
                None => journal_state(path)?,
            };
            RootObservation::Deleted {
                proof,
                remains: remains(path, meta.root(), journal)?,
            }
        }
    };
    Ok(StateDirProbe::Rooted {
        meta,
        succession,
        root,
    })
}

fn journal_state(dir: &Path) -> Result<JournalState, ErrorKind> {
    let entries = Journal::open_readonly(&dir.join("locks"))
        .entries()
        .map_err(|_| ErrorKind::Other)?;
    Ok(
        NonZeroUsize::new(entries.len()).map_or(JournalState::Clear, |entries| {
            JournalState::Pending { entries }
        }),
    )
}

fn probe_manifest(root: &ProjectRoot) -> Probed {
    match std::fs::metadata(root.path().as_path().join("phora.toml")) {
        Ok(_) => Probed::Present,
        Err(error) if error.kind() == ErrorKind::NotFound => Probed::Absent,
        Err(error) => Probed::Fault(error.kind()),
    }
}

fn remains(dir: &Path, root: &ProjectRoot, journal: JournalState) -> Result<Remains, ErrorKind> {
    let store = FileStateStore::open(dir.to_path_buf()).map_err(|_| ErrorKind::Other)?;
    let records = store.all_artifacts().map_err(|_| ErrorKind::Other)?;
    let ejected = ejected_index(&store, &records).map_err(|_| ErrorKind::Other)?;
    let fates = records
        .into_iter()
        .map(|record| {
            let key = &record.key;
            if ejected.contains(&(key.target.clone(), key.source.clone(), key.artifact.clone())) {
                RecordFate::Ejected
            } else {
                record_fate(root, record)
            }
        })
        .collect();
    Ok(Remains::new(journal, fates))
}

/// The record's destination, rebased onto the recorded root, tested with `symlink_metadata`.
fn record_fate(root: &ProjectRoot, record: ArtifactRecord) -> RecordFate {
    let Some(destination) = orphan_artifact_path(&record).and_then(|destination| {
        NormalPath::lexical(&normalize_history_deploy_root_at(
            root.path().as_path(),
            &destination,
        ))
    }) else {
        return RecordFate::Unresolvable { key: record.key };
    };
    match std::fs::symlink_metadata(destination.as_path()) {
        Ok(_) => RecordFate::PresentOutside(destination),
        Err(error) if is_absent(error.kind()) => RecordFate::Absent,
        Err(error) => RecordFate::Fault {
            key: record.key,
            kind: error.kind(),
        },
    }
}

pub fn sweep(_input: &SweepInput) -> SweepReport {
    todo!()
}

pub fn plan_gc(_input: &GcInput) -> GcPlan {
    todo!()
}

pub fn apply_gc(_plan: GcPlan, _confirmation: Confirmation) -> GcOutcome {
    todo!()
}

/// Takes the dir's `state.lock` with zero timeout, creating nothing outside an existing dir.
pub fn lock(action: PlannedAction) -> Result<Held, NotHeld> {
    let (PlannedAction::Drop { dir, .. }
    | PlannedAction::ReleaseLinks { dir, .. }
    | PlannedAction::Eject { dir, .. }
    | PlannedAction::Forget { dir, .. }
    | PlannedAction::Report { dir, .. }) = action;
    let lock = lock_existing(dir.path().as_path())?;
    Ok(Held { dir, lock })
}

/// Re-probes under the lock; proceeds only when the reason still equals `planned`.
pub fn confirm(held: Held, planned: Reason) -> Result<Confirmed, Reason> {
    let path = held.dir.path();
    let probed = match MountId::of(path.as_path()) {
        Ok(mount) => probe(&held.dir, &CurrentAnchor::new(AnchorDir::new(path), mount)),
        Err(error) => StateDirProbe::Unreadable(error.kind()),
    };
    let class = classify(&probed);
    match class.reason() {
        reason if reason == planned => Ok(Confirmed { held, class }),
        reason => Err(reason),
    }
}

/// Renames the dir to `<root>/trash/<id>.<suffix>`, keeping its lock.
pub fn trash(confirmed: Confirmed) -> Result<Trashed, StateError> {
    let Held { dir, lock } = confirmed.held;
    let from = dir.path();
    let mut attempts = TRASH_ATTEMPTS;
    loop {
        let entry = TrashEntry {
            root: dir.root().clone(),
            id: dir.id().clone(),
            suffix: TrashSuffix(format!("{}-{}", std::process::id(), nonce())),
        };
        let to = trash_path(&entry);
        if let Some(trash_dir) = to.parent() {
            std::fs::create_dir_all(trash_dir)
                .map_err(|error| io_error("create trash", trash_dir, &error))?;
        }
        match std::fs::rename(from.as_path(), &to) {
            Ok(()) => return Ok(Trashed { entry, lock }),
            Err(error)
                if attempts > 1
                    && matches!(
                        error.kind(),
                        ErrorKind::AlreadyExists | ErrorKind::DirectoryNotEmpty
                    ) =>
            {
                attempts -= 1;
            }
            Err(error) => {
                return Err(StateError::StateStore(format!(
                    "rename {} -> {}: {error}",
                    from.as_path().display(),
                    to.display()
                )));
            }
        }
    }
}

/// Deletes the trash entry, `NotFound` counting as deleted, then releases the lock.
pub fn delete(trashed: Trashed) -> Result<(), Leftover> {
    match std::fs::remove_dir_all(trash_path(&trashed.entry)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Leftover {
            entry: trashed.entry,
        }),
    }
}

fn trash_path(entry: &TrashEntry) -> PathBuf {
    entry.root.path().as_path().join(TRASH_DIR).join(format!(
        "{}.{}",
        entry.id.as_str(),
        entry.suffix.0
    ))
}
