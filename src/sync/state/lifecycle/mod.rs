//! State-dir lifecycle: record each project's root, classify every state dir, drop the dead ones.
#![allow(dead_code, reason = "stubs wired by T004 to T008")]

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
use crate::sync::confine::{ProtectedPathSet, confine_removal_destination};
use crate::sync::journal::Journal;
use crate::sync::nonce;
use crate::sync::observe::normalize_history_deploy_root_at;
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
    for root in &roots {
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

fn sweep_root(root: &StateRoot, own: Option<&StateDirPath>, report: &mut SweepReport) {
    let projects = root.path().join("projects");
    let ids = match state_dir_ids(root) {
        Ok(ids) => ids,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => return report.fault(None, io_error("read", projects.as_path(), &error)),
    };
    for id in ids {
        let id = match id {
            Ok(id) => id,
            Err(error) => {
                report.fault(None, io_error("read entry in", projects.as_path(), &error));
                continue;
            }
        };
        let dir = StateDirPath::new(root.clone(), id);
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
    let held = match lock(action) {
        Ok(held) => held,
        Err(NotHeld::Vanished) => return,
        Err(NotHeld::Busy) => return report.busy.push(dir),
        Err(NotHeld::Fault(kind)) => {
            let error = StateError::StateStore(format!("lock: {kind}"));
            return report.fault(Some(dir), error);
        }
    };
    let confirmed = match confirm(held, planned) {
        Ok(confirmed) => confirmed,
        Err(reason) => return report.attend(reason),
    };
    let all_unlinked = release_links(&confirmed.class, dir.root(), report);
    if !all_unlinked || confirmed.class.reason() == Reason::Stranded {
        return report.attend(Reason::Stranded);
    }
    match trash(confirmed) {
        Ok(trashed) => {
            report.dropped.push(dir);
            if let Err(leftover) = delete(trashed) {
                report.leftovers.push(leftover.entry);
            }
        }
        Err(error) => report.fault(Some(dir), error),
    }
}

/// Releases the dangling links into the deleted root that the confirmed class carries; whether every one came out `Unlinked`.
fn release_links(class: &StateDirClass, swept: &StateRoot, report: &mut SweepReport) -> bool {
    let (proof, links) = match class {
        StateDirClass::Drop(DropReason::Gone { proof, links }) => (proof, links),
        StateDirClass::Release(ReleaseReason::Stranded(stranded)) => {
            (&stranded.proof, &stranded.links)
        }
        _ => return true,
    };
    if links.is_empty() {
        return true;
    }
    let protected = protected_paths(proof, swept);
    let mut all_unlinked = true;
    for link in links {
        let outcome = protected
            .as_ref()
            .map_or(LinkOutcome::Kept(LinkKept::Confined), |protected| {
                release_link(link, protected)
            });
        match outcome {
            LinkOutcome::Unlinked => report.released.push(link.path.clone()),
            kept @ LinkOutcome::Kept(_) => {
                all_unlinked = false;
                report.links_kept.push((link.path.clone(), kept));
            }
        }
    }
    all_unlinked
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
    let trash_dir = root.path().join(TRASH_DIR);
    let entries = match std::fs::read_dir(trash_dir.as_path()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => return report.fault(None, io_error("read", trash_dir.as_path(), &error)),
    };
    for entry in entries.flatten() {
        let Some(entry) = trash_entry(root, &entry.file_name()) else {
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

/// Under the index lock, drops each entry whose root holds no state dir or trash entry, or is
/// proven deleted against the index's recorded default-root anchor.
fn prune_index(default: &StateRoot) -> Result<(), StateError> {
    let default_dir = default.path().as_path();
    let _lock = lock_index(default_dir)?;
    let index_path = default_dir.join(INDEX_FILE);
    let IndexRead::Read(mut index) = read_index(&index_path) else {
        return Ok(());
    };
    let mount =
        MountId::of(default_dir).map_err(|error| io_error("read mount of", default_dir, &error))?;
    let current = CurrentAnchor::new(AnchorDir::new(default.path().clone()), mount);
    let dead: Vec<StateRoot> = index
        .roots()
        .iter()
        .filter(
            |(root, placement)| match std::fs::metadata(root.path().as_path()) {
                Ok(stat) => stat.is_dir() && !holds_state_or_trash(root),
                Err(_) => matches!(
                    locate(root.path(), **placement, index.anchor(), &current),
                    Location::Deleted(_)
                ),
            },
        )
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

/// The ids of the dirs under `projects/` whose names parse as a `ProjectId`.
fn state_dir_ids(
    root: &StateRoot,
) -> std::io::Result<impl Iterator<Item = std::io::Result<ProjectId>>> {
    let entries = std::fs::read_dir(root.path().join("projects").as_path())?;
    Ok(entries.filter_map(|entry| {
        entry
            .map(|entry| {
                ProjectId::parse(&entry.file_name())
                    .filter(|_| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            })
            .transpose()
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
    holds(state_dir_ids(root).map(|mut ids| ids.any(|id| id.is_ok()))) || holds(trash)
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
        entry.suffix.as_str()
    ))
}
