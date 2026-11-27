//! State-dir lifecycle: record each project's root, classify every state dir, drop the dead ones.
#![allow(dead_code, reason = "stubs wired by T004 to T008")]

mod model;
mod wire;

pub use model::*;

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::Path;

use crate::mount::{AnchorDir, CurrentAnchor, Placement};
use crate::sync::journal::Journal;
use crate::sync::state::file::atomic_write;
use crate::sync::state::locking::lock_existing;
use crate::sync::state::{FileStateStore, StateError, StateLock, StateStore as _};

const META_FILE: &str = "project.toml";
const INDEX_FILE: &str = "state-roots.toml";
const INDEX_LOCK_FILE: &str = "state-roots.lock";

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
    if !FileStateStore::open(dir.to_path_buf())?
        .all_artifacts()?
        .is_empty()
    {
        return Ok(true);
    }
    let targets = dir.join("targets");
    match std::fs::read_dir(&targets) {
        Ok(entries) => {
            for entry in entries {
                let target = entry.map_err(|error| io_error("read entry in", &targets, &error))?;
                if !target.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let meta_path = target.path().join("meta.toml");
                if meta_path
                    .try_exists()
                    .map_err(|error| io_error("stat", &meta_path, &error))?
                {
                    return Ok(true);
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("read dir", &targets, &error)),
    }
    let pending = Journal::open_readonly(&dir.join("locks"))
        .entries()
        .map_err(|error| StateError::StateStore(error.to_string()))?;
    Ok(!pending.is_empty())
}

fn io_error(what: &str, path: &Path, error: &std::io::Error) -> StateError {
    StateError::StateStore(format!("{what} {}: {error}", path.display()))
}

pub fn probe(_dir: &StateDirPath, _anchor: &CurrentAnchor) -> StateDirProbe {
    todo!()
}

pub fn classify(_probe: &StateDirProbe) -> StateDirClass {
    todo!()
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
pub fn lock(_action: PlannedAction) -> Result<Held, NotHeld> {
    todo!()
}

/// Re-probes under the lock; proceeds only when the reason still equals `planned`.
pub fn confirm(_held: Held, _planned: Reason) -> Result<Confirmed, Reason> {
    todo!()
}

pub fn trash(_confirmed: Confirmed) -> Result<Trashed, StateError> {
    todo!()
}

pub fn delete(_trashed: Trashed) -> Result<(), Leftover> {
    todo!()
}
