//! Placement of a path relative to an anchor directory's mount, and proof of its deletion.
#![allow(dead_code, reason = "stubs wired by T004 to T008")]

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// Collapses `.`/`..` without touching the filesystem, so confinement holds for paths that do not yet exist.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// An absolute, lexically normalized path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NormalPath(PathBuf);

impl NormalPath {
    pub fn lexical(_path: &Path) -> Option<Self> {
        todo!()
    }
}

/// Statx `mnt_id` on Linux, `st_dev` on macOS; valid within one boot, never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountId(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    OnAnchorMount,
    Elsewhere,
}

/// The anchor directory's path, as recorded or as seen now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorDir(NormalPath);

/// The anchor directory as read in this run.
#[derive(Debug)]
pub struct CurrentAnchor {
    dir: AnchorDir,
    mount: MountId,
}

/// Constructed only by `locate`.
#[derive(Debug, Clone)]
pub struct DeletionProof {
    path: NormalPath,
}

impl DeletionProof {
    pub fn path(&self) -> &NormalPath {
        &self.path
    }
}

#[derive(Debug, Clone)]
pub enum Unproven {
    RecordedElsewhere,
    AnchorMoved {
        now: NormalPath,
    },
    OffMount,
    Encrypted,
    SymlinkedComponent(NormalPath),
    /// Present, but resolves elsewhere.
    CanonicalDiffers {
        now: NormalPath,
    },
    Fault(ErrorKind),
}

#[derive(Debug)]
pub enum Location {
    Present,
    Deleted(DeletionProof),
    Unproven(Unproven),
}

pub fn locate(
    _path: &NormalPath,
    _placement: Placement,
    _recorded: &AnchorDir,
    _current: &CurrentAnchor,
) -> Location {
    todo!()
}
