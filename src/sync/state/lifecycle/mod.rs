//! State-dir lifecycle: record each project's root, classify every state dir, drop the dead ones.

mod model;
mod wire;

pub use model::*;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use crate::config::Paths;
use crate::mount::{
    AnchorDir, CurrentAnchor, DeletionProof, Location, MountId, NormalPath, Placement, is_absent,
    locate,
};
use crate::source::{
    GitBackend, SourceError, SourceName, SourceStore as _, WorktreeDeployRequest, mirror_exists,
};
use crate::sync::confine::{ProtectedPathSet, confine_removal_destination};
use crate::sync::journal::Journal;
use crate::sync::nonce;
use crate::sync::observe::{history_address, normalize_history_deploy_root_at};
use crate::sync::prune::orphan_artifact_path;
use crate::sync::state::file::atomic_write;
use crate::sync::state::locking::lock_existing;
use crate::sync::state::{
    ArtifactRecord, FileStateStore, ProjectId, StateError, StateLock, StateStore as _,
    ejected_index,
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
    let _lock = lock_index(default_dir)?;

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
            write_index(&index_path, &index)?;
        }
    }
    Ok(Some(index))
}

/// Takes the index lock next to the index; released when the file closes.
fn lock_index(default_dir: &Path) -> Result<std::fs::File, StateError> {
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
    Ok(lock)
}

fn write_index(index_path: &Path, index: &StateRootIndex) -> Result<(), StateError> {
    let text = wire::render_index(index)
        .map_err(|error| StateError::StateStore(format!("serialize {INDEX_FILE}: {error}")))?;
    atomic_write(index_path, &text)
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
    let lock = match lock_existing(dir.as_path()) {
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
        return remove_stateless(copy, &lock);
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

/// The `state.lock` holder removes a dir that holds no records, target meta or pending
/// journal before releasing the lock.
pub fn remove_stateless(dir: &StateDirPath, _held: &StateLock) -> Result<(), StateError> {
    let path = dir.path();
    let path = path.as_path();
    if holds_state(path)? {
        return Ok(());
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("remove", path, &error)),
    }
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

/// Gathers the filesystem facts `classify` reads; records only for a root proven deleted.
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
    let (ejected, unejected) =
        unejected_records(dir, root.path().as_path()).map_err(|_| ErrorKind::Other)?;
    let fates = std::iter::repeat_with(|| RecordFate::Ejected)
        .take(ejected)
        .chain(
            unejected
                .into_iter()
                .map(|(record, destination)| record_fate(root, record, destination)),
        )
        .collect();
    Ok(Remains::new(journal, fates))
}

/// A record with its destination rebased onto a root, `None` when it does not resolve.
type RebasedRecord = (ArtifactRecord, Option<NormalPath>);

/// The count of ejected records, and every other record rebased onto `root`.
fn unejected_records(dir: &Path, root: &Path) -> Result<(usize, Vec<RebasedRecord>), StateError> {
    let store = FileStateStore::open(dir.to_path_buf())?;
    let records = store.all_artifacts()?;
    let ejected = ejected_index(&store, &records)?;
    let total = records.len();
    let unejected: Vec<_> = records
        .into_iter()
        .filter(|record| {
            let key = &record.key;
            !ejected.contains(&(key.target.clone(), key.source.clone(), key.artifact.clone()))
        })
        .map(|record| {
            let destination = orphan_artifact_path(&record).and_then(|destination| {
                NormalPath::lexical(&normalize_history_deploy_root_at(root, &destination))
            });
            (record, destination)
        })
        .collect();
    Ok((total - unejected.len(), unejected))
}

/// The record's rebased destination, tested with `symlink_metadata`.
fn record_fate(
    root: &ProjectRoot,
    record: ArtifactRecord,
    destination: Option<NormalPath>,
) -> RecordFate {
    let Some(destination) = destination else {
        return RecordFate::Unresolvable { key: record.key };
    };
    match std::fs::symlink_metadata(destination.as_path()) {
        Ok(stat) => releasable_link(root, &record, &destination, &stat).map_or(
            RecordFate::PresentOutside(destination),
            RecordFate::Releasable,
        ),
        Err(error) if is_absent(error.kind()) => RecordFate::Absent,
        Err(error) => RecordFate::Fault {
            key: record.key,
            kind: error.kind(),
        },
    }
}

/// A link still reading its recorded target, which lies, absent, under the deleted root.
fn releasable_link(
    root: &ProjectRoot,
    record: &ArtifactRecord,
    path: &NormalPath,
    stat: &Metadata,
) -> Option<ReleasableLink> {
    let recorded = Path::new(record.link_target.as_deref()?);
    if !stat.is_symlink() || std::fs::read_link(path.as_path()).ok()? != recorded {
        return None;
    }
    let link_target = NormalPath::lexical(recorded)?;
    let target_absent = std::fs::symlink_metadata(link_target.as_path())
        .is_err_and(|error| is_absent(error.kind()));
    if !target_absent || !link_target.as_path().starts_with(root.path().as_path()) {
        return None;
    }
    let anchor = NormalPath::lexical(&normalize_history_deploy_root_at(
        root.path().as_path(),
        Path::new(record.deploy_root.as_deref()?),
    ))?;
    Some(ReleasableLink {
        path: path.clone(),
        link_target,
        anchor,
    })
}

/// The sweep input of a run: the default state root, the index as read now, and its own dir.
pub fn sweep_input(default: StateRoot, own: Option<StateDirPath>) -> SweepInput {
    let index = read_index(&default.path().as_path().join(INDEX_FILE));
    SweepInput {
        default,
        index,
        own,
    }
}

/// Classifies every state dir of the default and every registered state root, drops the dead
/// ones, empties each trash and prunes the index; every failure lands in the report.
pub fn sweep(input: &SweepInput) -> SweepReport {
    let mut report = SweepReport::default();
    for root in &swept_roots(input, &mut report) {
        empty_trash(root, &mut report);
        sweep_root(root, input.own.as_ref(), &mut report);
    }
    if matches!(input.index, IndexRead::Read(_))
        && let Err(error) = prune_index(&input.default)
    {
        report.fault(None, error);
    }
    report
}

/// A corrupt or unreadable index leaves the default root only, and a fault.
fn swept_roots(input: &SweepInput, report: &mut SweepReport) -> Vec<StateRoot> {
    let mut roots = vec![input.default.clone()];
    let index_path = input.default.path().as_path().join(INDEX_FILE);
    match &input.index {
        IndexRead::Read(index) => roots.extend(
            index
                .roots()
                .keys()
                .filter(|root| **root != input.default)
                .cloned(),
        ),
        IndexRead::Missing => {}
        IndexRead::Corrupt(defect) => report.fault(
            None,
            StateError::StateStore(format!(
                "state-root index {} is corrupt ({defect:?}); registered state roots are skipped",
                index_path.display()
            )),
        ),
        IndexRead::Unreadable(kind) => report.fault(
            None,
            StateError::StateStore(format!(
                "read state-root index {}: {kind}; registered state roots are skipped",
                index_path.display()
            )),
        ),
    }
    roots
}

fn sweep_root(root: &StateRoot, own: Option<&StateDirPath>, report: &mut SweepReport) {
    let projects = root.path().join("projects");
    let dirs = match state_dirs(root) {
        Ok(dirs) => dirs,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => return report.fault(None, io_error("read", projects.as_path(), &error)),
    };
    for dir in dirs {
        let dir = match dir {
            Ok(dir) => dir,
            Err(error) => {
                report.fault(None, io_error("read entry in", projects.as_path(), &error));
                continue;
            }
        };
        if own != Some(&dir) {
            sweep_dir(dir, report);
        }
    }
}

fn sweep_dir(dir: StateDirPath, report: &mut SweepReport) {
    let class = classify(&probe_now(&dir));
    let reason = class.reason();
    let action = match class {
        StateDirClass::Keep(_) => return,
        StateDirClass::Drop(reason) => PlannedAction::Drop { dir, reason },
        StateDirClass::Release(ReleaseReason::Stranded(stranded)) if !stranded.links.is_empty() => {
            PlannedAction::ReleaseLinks { dir, stranded }
        }
        StateDirClass::Release(_) | StateDirClass::Forget(_) => return report.attend(reason),
    };
    apply_sweep(action, reason, report);
}

/// Lock, re-probe, release links, then trash and delete a dir that is still droppable.
fn apply_sweep(action: PlannedAction, planned: Reason, report: &mut SweepReport) {
    let dir = action.dir().clone();
    match settle(action, planned, false, report) {
        DropOutcome::Deleted => report.dropped.push(dir),
        DropOutcome::Leftover(leftover) => {
            report.dropped.push(dir);
            report.leftovers.push(leftover.entry);
        }
        DropOutcome::Vanished => {}
        DropOutcome::Busy => report.busy.push(dir),
        DropOutcome::Released(_) => report.attend(Reason::Stranded),
        DropOutcome::Reclassified(reason) => report.attend(reason),
        DropOutcome::Fault(error) => report.fault(Some(dir), error),
    }
}

/// A `gone` dir whose links did not all come out `Unlinked` stays as `stranded`, unless
/// `trash_kept`, as under a granted `gc --eject`.
fn settle(
    action: PlannedAction,
    planned: Reason,
    trash_kept: bool,
    report: &mut SweepReport,
) -> DropOutcome {
    let release_only = matches!(action, PlannedAction::ReleaseLinks { .. });
    let held = match lock(action) {
        Ok(held) => held,
        Err(NotHeld::Vanished) => return DropOutcome::Vanished,
        Err(NotHeld::Busy) => return DropOutcome::Busy,
        Err(NotHeld::Fault(kind)) => {
            return DropOutcome::Fault(StateError::StateStore(format!("lock: {kind}")));
        }
    };
    let dir = held.dir.clone();
    let confirmed = match confirm(held, planned) {
        Ok(confirmed) => confirmed,
        Err(reason) => return DropOutcome::Reclassified(reason),
    };
    let links = release_links(&confirmed.class, dir.root(), report);
    let all_unlinked = links
        .iter()
        .all(|outcome| matches!(outcome, LinkOutcome::Unlinked));
    match confirmed.class.reason() {
        Reason::Gone if !all_unlinked && !trash_kept => {
            return DropOutcome::Reclassified(Reason::Stranded);
        }
        Reason::Stranded if release_only => return DropOutcome::Released(links),
        _ => {}
    }
    if let Err(error) = eject_worktrees(&confirmed.class, &dir) {
        return DropOutcome::Fault(error);
    }
    match trash(confirmed) {
        Ok(trashed) => match delete(trashed) {
            Ok(()) => DropOutcome::Deleted,
            Err(leftover) => DropOutcome::Leftover(leftover),
        },
        Err(error) => DropOutcome::Fault(error),
    }
}

/// Releases the dangling links into the deleted root that the confirmed class carries.
fn release_links(
    class: &StateDirClass,
    swept: &StateRoot,
    report: &mut SweepReport,
) -> Vec<LinkOutcome> {
    let (proof, links) = match class {
        StateDirClass::Drop(DropReason::Gone { proof, links }) => (proof, links),
        StateDirClass::Release(ReleaseReason::Stranded(stranded)) => {
            (&stranded.proof, &stranded.links)
        }
        _ => return Vec::new(),
    };
    if links.is_empty() {
        return Vec::new();
    }
    let protected = protected_paths(proof, swept);
    links
        .iter()
        .map(|link| {
            let outcome = protected
                .as_ref()
                .map_or(LinkOutcome::Kept(LinkKept::Confined), |protected| {
                    release_link(link, protected)
                });
            match outcome {
                LinkOutcome::Unlinked => report.released.push(link.path.clone()),
                LinkOutcome::Kept(_) => {
                    report.links_kept.push((link.path.clone(), outcome.clone()));
                }
            }
            outcome
        })
        .collect()
}

/// Resolved from the recorded root with default `Paths`, plus the swept state root.
fn protected_paths(proof: &DeletionProof, swept: &StateRoot) -> Option<ProtectedPathSet> {
    ProtectedPathSet::resolve(&Paths::default(), proof.path().as_path())
        .ok()
        .map(|protected| protected.protecting(swept.path().as_path()))
}

fn release_link(link: &ReleasableLink, protected: &ProtectedPathSet) -> LinkOutcome {
    let Ok(path) = confine_removal_destination(
        Some(link.anchor.as_path()),
        None,
        link.path.as_path(),
        false,
        protected,
    ) else {
        return LinkOutcome::Kept(LinkKept::Confined);
    };
    match std::fs::remove_file(path) {
        Ok(()) => LinkOutcome::Unlinked,
        Err(error) if error.kind() == ErrorKind::NotFound => LinkOutcome::Unlinked,
        Err(error) => LinkOutcome::Kept(LinkKept::Fault(error.kind())),
    }
}

/// Deletes every entry under `trash/` that parses as a `TrashEntry`, never another one.
fn empty_trash(root: &StateRoot, report: &mut SweepReport) {
    let names = match trash_names(root) {
        Ok(names) => names,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => {
            let trash_dir = root.path().join(TRASH_DIR);
            return report.fault(None, io_error("read", trash_dir.as_path(), &error));
        }
    };
    for name in names {
        let TrashName::Entry(entry) = name else {
            continue;
        };
        match std::fs::remove_dir_all(trash_path(&entry)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => report.leftovers.push(entry),
        }
    }
}

fn trash_entry(root: &StateRoot, name: &OsStr) -> Option<TrashEntry> {
    let (id, suffix) = name.to_str()?.split_once('.')?;
    if suffix.is_empty() {
        return None;
    }
    Some(TrashEntry {
        root: root.clone(),
        id: ProjectId::parse(OsStr::new(id))?,
        suffix: TrashSuffix::new(suffix.to_owned()),
    })
}

/// Drops each entry whose root holds no state dir or trash entry, or is proven deleted.
fn prune_index(default: &StateRoot) -> Result<(), StateError> {
    unregister_where(default, |root, fate| match fate {
        RegisteredRoot::Present => root.path().as_path().is_dir() && !holds_state_or_trash(root),
        RegisteredRoot::Deleted => true,
        RegisteredRoot::Unproven => false,
    })
}

/// A registered root, symlinks followed, against the index's recorded default-root anchor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RegisteredRoot {
    /// Present, or its stat failed other than as absent.
    Present,
    Deleted,
    /// Absent and not provably deleted.
    Unproven,
}

fn registered_root(
    root: &StateRoot,
    placement: Placement,
    index: &StateRootIndex,
    current: Option<&CurrentAnchor>,
) -> RegisteredRoot {
    match std::fs::metadata(root.path().as_path()) {
        Err(error) if is_absent(error.kind()) => {
            let proven = current.is_some_and(|current| {
                matches!(
                    locate(root.path(), placement, index.anchor(), current),
                    Location::Deleted(_)
                )
            });
            if proven {
                RegisteredRoot::Deleted
            } else {
                RegisteredRoot::Unproven
            }
        }
        _ => RegisteredRoot::Present,
    }
}

fn current_anchor(default: &StateRoot) -> std::io::Result<CurrentAnchor> {
    let default = default.path();
    MountId::of(default.as_path())
        .map(|mount| CurrentAnchor::new(AnchorDir::new(default.clone()), mount))
}

/// Under the index lock, unregisters each root `dead` picks from the index as read now.
fn unregister_where(
    default: &StateRoot,
    dead: impl Fn(&StateRoot, RegisteredRoot) -> bool,
) -> Result<(), StateError> {
    let default_dir = default.path().as_path();
    let _lock = lock_index(default_dir)?;
    let index_path = default_dir.join(INDEX_FILE);
    let IndexRead::Read(mut index) = read_index(&index_path) else {
        return Ok(());
    };
    let current =
        current_anchor(default).map_err(|error| io_error("read mount of", default_dir, &error))?;
    let dead: Vec<StateRoot> = index
        .roots()
        .iter()
        .filter(|(root, placement)| {
            dead(
                root,
                registered_root(root, **placement, &index, Some(&current)),
            )
        })
        .map(|(root, _)| root.clone())
        .collect();
    if dead.is_empty() {
        return Ok(());
    }
    for root in &dead {
        index.unregister(root);
    }
    write_index(&index_path, &index)
}

/// The state dirs under `projects/`.
fn state_dirs(
    root: &StateRoot,
) -> std::io::Result<impl Iterator<Item = std::io::Result<StateDirPath>> + use<'_>> {
    Ok(projects_entries(root)?.filter_map(|entry| match entry {
        Ok(ProjectsEntry::State(dir)) => Some(Ok(dir)),
        Ok(ProjectsEntry::Foreign(_)) => None,
        Err(error) => Some(Err(error)),
    }))
}

/// Whether the root holds a state dir or a trash entry; an unreadable dir counts as holding.
fn holds_state_or_trash(root: &StateRoot) -> bool {
    let holds = |read: std::io::Result<bool>| {
        read.unwrap_or_else(|error| error.kind() != ErrorKind::NotFound)
    };
    let trash = std::fs::read_dir(root.path().join(TRASH_DIR).as_path()).map(|mut entries| {
        entries
            .any(|entry| entry.is_ok_and(|entry| trash_entry(root, &entry.file_name()).is_some()))
    });
    holds(state_dirs(root).map(|mut dirs| dirs.any(|dir| dir.is_ok()))) || holds(trash)
}

/// One record per non-live dir of every swept root, plus what gc lists but never touches.
pub fn plan_gc(input: &GcInput) -> GcPlan {
    let sweep = &input.sweep;
    let mut report = SweepReport::default();
    let roots = swept_roots(sweep, &mut report);
    let mut plan = GcPlan {
        records: Vec::new(),
        index_removals: unproven_absent_roots(sweep),
        foreign: Vec::new(),
        foreign_trash: Vec::new(),
        default: sweep.default.clone(),
        roots: Vec::new(),
        index_read: matches!(sweep.index, IndexRead::Read(_)),
        dry_run: input.dry_run,
        faults: Vec::new(),
    };
    for root in &roots {
        plan_root(root, input, &mut plan, &mut report);
    }
    plan.roots = roots;
    plan.faults = report.into_faults();
    plan
}

fn plan_root(root: &StateRoot, input: &GcInput, plan: &mut GcPlan, report: &mut SweepReport) {
    let projects = root.path().join("projects");
    match projects_entries(root).and_then(Iterator::collect::<std::io::Result<Vec<_>>>) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    ProjectsEntry::State(dir) if input.sweep.own.as_ref() == Some(&dir) => {}
                    ProjectsEntry::State(dir) => {
                        plan.records.extend(gc_record(dir, input.eject, report));
                    }
                    ProjectsEntry::Foreign(path) => plan.foreign.push(path),
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => report.fault(None, io_error("read", projects.as_path(), &error)),
    }
    let trash_dir = root.path().join(TRASH_DIR);
    match trash_names(root) {
        Ok(names) => plan
            .foreign_trash
            .extend(names.into_iter().filter_map(|name| match name {
                TrashName::Foreign(name) => Some(trash_dir.as_path().join(name).into_os_string()),
                TrashName::Entry(_) => None,
            })),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => report.fault(None, io_error("read", trash_dir.as_path(), &error)),
    }
}

/// Each entry under `projects/`: a dir whose name parses as a `ProjectId` is a state dir.
fn projects_entries(
    root: &StateRoot,
) -> std::io::Result<impl Iterator<Item = std::io::Result<ProjectsEntry>> + use<'_>> {
    let projects = root.path().join("projects");
    let entries = std::fs::read_dir(projects.as_path())?;
    Ok(entries.map(move |entry| {
        let entry = entry?;
        let name = entry.file_name();
        let id =
            ProjectId::parse(&name).filter(|_| entry.file_type().is_ok_and(|kind| kind.is_dir()));
        Ok(match id {
            Some(id) => ProjectsEntry::State(StateDirPath::new(root.clone(), id)),
            None => ProjectsEntry::Foreign(projects.join(&name)),
        })
    }))
}

fn trash_names(root: &StateRoot) -> std::io::Result<Vec<TrashName>> {
    std::fs::read_dir(root.path().join(TRASH_DIR).as_path())?
        .map(|entry| {
            let name = entry?.file_name();
            Ok(trash_entry(root, &name).map_or(TrashName::Foreign(name), TrashName::Entry))
        })
        .collect()
}

/// `None` for a live dir; only `--eject` plans ejects and Forget-class drops.
fn gc_record(dir: StateDirPath, eject: bool, report: &mut SweepReport) -> Option<GcRecord> {
    let action = match classify(&probe_now(&dir)) {
        StateDirClass::Keep(_) => return None,
        StateDirClass::Drop(reason) => PlannedAction::Drop { dir, reason },
        StateDirClass::Release(reason) if eject => {
            let worktrees =
                ejectable_worktrees(&dir, release_root(&reason)).unwrap_or_else(|error| {
                    report.fault(Some(dir.clone()), error);
                    Vec::new()
                });
            PlannedAction::Eject {
                dir,
                reason,
                worktrees,
            }
        }
        StateDirClass::Release(ReleaseReason::Stranded(stranded)) if !stranded.links.is_empty() => {
            PlannedAction::ReleaseLinks { dir, stranded }
        }
        StateDirClass::Release(ReleaseReason::Stranded(stranded)) => PlannedAction::Report {
            dir,
            attention: Attention::Stranded(stranded),
        },
        StateDirClass::Release(ReleaseReason::Retired { root }) => PlannedAction::Report {
            dir,
            attention: Attention::Retired { root },
        },
        StateDirClass::Forget(reason) if eject => PlannedAction::Forget { dir, reason },
        StateDirClass::Forget(reason) => PlannedAction::Report {
            dir,
            attention: Attention::Forget(reason),
        },
    };
    if action.reason() == Reason::Unrooted {
        let legacy = legacy_evidence(action.dir()).unwrap_or_else(|error| {
            report.fault(Some(action.dir().clone()), error);
            LegacyEvidence {
                records: 0,
                recorded_paths: Vec::new(),
                newest_record: None,
            }
        });
        return Some(GcRecord::Unrooted { action, legacy });
    }
    let leftovers = if let Some(present) = action.present_outside() {
        present.to_vec()
    } else if let Some(root) = action.live_root() {
        let path = action.dir().path();
        remains(path.as_path(), root, JournalState::Clear).map_or_else(
            |kind| {
                let error = StateError::StateStore(format!("read records: {kind}"));
                report.fault(Some(action.dir().clone()), error);
                Vec::new()
            },
            |remains| remains.present(),
        )
    } else {
        Vec::new()
    };
    Some(GcRecord::Rooted { action, leftovers })
}

fn legacy_evidence(dir: &StateDirPath) -> Result<LegacyEvidence, StateError> {
    let store = FileStateStore::open(dir.path().as_path().to_path_buf())?;
    let records = store.all_artifacts()?;
    let recorded_paths = records
        .iter()
        .filter_map(orphan_artifact_path)
        .filter(|path| path.is_absolute())
        .collect();
    Ok(LegacyEvidence {
        records: records.len(),
        recorded_paths,
        newest_record: store.newest_record_modified()?,
    })
}

fn release_root(reason: &ReleaseReason) -> &NormalPath {
    match reason {
        ReleaseReason::Retired { root } => root.path(),
        ReleaseReason::Stranded(stranded) => stranded.proof.path(),
    }
}

/// Each history record not ejected whose deploy root resolves against `root`.
fn ejectable_worktrees(
    dir: &StateDirPath,
    root: &NormalPath,
) -> Result<Vec<EjectableWorktree>, StateError> {
    let (_, records) = unejected_records(dir.path().as_path(), root.as_path())?;
    let mut worktrees = Vec::new();
    for (record, deploy_root) in records {
        if record.linked {
            continue;
        }
        let Ok(Some((address, admin_id))) = history_address(&record) else {
            continue;
        };
        let Some(deploy_root) = deploy_root else {
            continue;
        };
        let Ok(commit) = record.commit.parse() else {
            continue;
        };
        let mirror_present =
            mirror_exists(&address).map_err(|error| StateError::StateStore(error.to_string()))?;
        worktrees.push(EjectableWorktree {
            record: record.key,
            deploy_root,
            admin_id,
            address,
            commit,
            mirror_present,
        });
    }
    Ok(worktrees)
}

/// Recomputed under the dir's lock; a worktree whose mirror is gone keeps its gitlink.
fn eject_worktrees(class: &StateDirClass, dir: &StateDirPath) -> Result<(), StateError> {
    let StateDirClass::Release(reason) = class else {
        return Ok(());
    };
    for worktree in ejectable_worktrees(dir, release_root(reason))? {
        if worktree.mirror_present {
            eject(&worktree)?;
        }
    }
    Ok(())
}

fn eject(worktree: &EjectableWorktree) -> Result<(), StateError> {
    let fail = |error: SourceError| {
        StateError::StateStore(format!(
            "eject history worktree {}: {error}",
            worktree.deploy_root.as_path().display()
        ))
    };
    let source = SourceName::trusted(worktree.record.source.clone());
    let guard = GitBackend::new(worktree.address.cache_git_root.clone())
        .lock_worktree_mirror_at(&source, &worktree.address)
        .map_err(fail)?;
    guard
        .eject_worktree(&WorktreeDeployRequest {
            admin_id: worktree.admin_id.clone(),
            deploy_root: worktree.deploy_root.as_path().to_path_buf(),
            commit: worktree.commit.clone(),
        })
        .map_err(fail)?;
    Ok(())
}

/// Registered roots that are absent and not provably deleted against the index's anchor.
fn unproven_absent_roots(sweep: &SweepInput) -> Vec<StateRoot> {
    let IndexRead::Read(index) = &sweep.index else {
        return Vec::new();
    };
    let current = current_anchor(&sweep.default).ok();
    index
        .roots()
        .iter()
        .filter(|(root, placement)| {
            *root != &sweep.default
                && registered_root(root, **placement, index, current.as_ref())
                    == RegisteredRoot::Unproven
        })
        .map(|(root, _)| root.clone())
        .collect()
}

/// Without `Granted`, the ejects and Forget-class drops stay pending; a dry-run plan applies nothing.
pub fn apply_gc(plan: GcPlan, confirmation: Confirmation) -> GcOutcome {
    let mut outcome = GcOutcome::default();
    if plan.dry_run {
        outcome.pending = plan
            .records
            .iter()
            .map(GcRecord::action)
            .filter(|action| !matches!(action, PlannedAction::Report { .. }))
            .map(|action| action.dir().clone())
            .collect();
        return outcome;
    }
    let mut report = SweepReport::default();
    for root in &plan.roots {
        empty_trash(root, &mut report);
    }
    let granted = confirmation == Confirmation::Granted;
    for record in plan.records {
        let action = record.into_action();
        let planned = action.reason();
        let dir = action.dir().clone();
        let action = match action {
            PlannedAction::Report { .. } => continue,
            PlannedAction::Eject {
                dir,
                reason: ReleaseReason::Stranded(stranded),
                ..
            } if !granted => {
                outcome.pending.push(dir.clone());
                if stranded.links.is_empty() {
                    continue;
                }
                PlannedAction::ReleaseLinks { dir, stranded }
            }
            PlannedAction::Eject { .. } | PlannedAction::Forget { .. } if !granted => {
                outcome.pending.push(dir);
                continue;
            }
            action => action,
        };
        let kept_from = report.links_kept.len();
        let settled = settle(action, planned, granted, &mut report);
        outcome
            .links_kept
            .extend(
                report
                    .links_kept
                    .drain(kept_from..)
                    .filter_map(|(path, link)| match link {
                        LinkOutcome::Kept(kept) => Some((dir.clone(), path, kept)),
                        LinkOutcome::Unlinked => None,
                    }),
            );
        outcome.applied.push((dir, settled));
    }
    if plan.index_read
        && let Err(error) = prune_index(&plan.default)
    {
        report.fault(None, error);
    }
    outcome.faults = report.into_faults();
    outcome
}

/// Unregisters, under the index lock, each of `roots` that is still absent.
pub fn unregister_roots(default: &StateRoot, roots: &[StateRoot]) -> Result<(), StateError> {
    if roots.is_empty() {
        return Ok(());
    }
    unregister_where(default, |root, fate| {
        fate != RegisteredRoot::Present && roots.contains(root)
    })
}

/// The content store's GC roots: `projects/` of the default and every registered state root.
pub fn projects_dirs(default: &StateRoot) -> Vec<PathBuf> {
    let projects = |root: &StateRoot| root.path().join("projects").as_path().to_path_buf();
    let mut dirs = vec![projects(default)];
    if let IndexRead::Read(index) = read_index(&default.path().as_path().join(INDEX_FILE)) {
        dirs.extend(
            index
                .roots()
                .keys()
                .filter(|root| *root != default)
                .map(projects),
        );
    }
    dirs
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
    let class = classify(&probe_now(&held.dir));
    match class.reason() {
        reason if reason == planned => Ok(Confirmed { held, class }),
        reason => Err(reason),
    }
}

/// Probes against the state dir as seen now.
fn probe_now(dir: &StateDirPath) -> StateDirProbe {
    let path = dir.path();
    match MountId::of(path.as_path()) {
        Ok(mount) => probe(dir, &CurrentAnchor::new(AnchorDir::new(path), mount)),
        Err(error) => StateDirProbe::Unreadable(error.kind()),
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
            suffix: TrashSuffix::new(format!("{}-{}", std::process::id(), nonce())),
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
    let Trashed { entry, lock } = trashed;
    let deleted = match std::fs::remove_dir_all(trash_path(&entry)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Leftover { entry }),
    };
    drop(lock);
    deleted
}

fn trash_path(entry: &TrashEntry) -> PathBuf {
    entry.root.path().as_path().join(TRASH_DIR).join(format!(
        "{}.{}",
        entry.id.as_str(),
        entry.suffix.as_str()
    ))
}
