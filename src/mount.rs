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
    pub fn lexical(path: &Path) -> Option<Self> {
        path.is_absolute().then(|| Self(normalize_lexical(path)))
    }

    #[must_use]
    pub fn join(&self, name: impl AsRef<Path>) -> Self {
        Self(normalize_lexical(&self.0.join(name)))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Statx `mnt_id` on Linux, `st_dev` on macOS; valid within one boot, never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountId(u64);

impl MountId {
    #[cfg(target_os = "linux")]
    pub fn of(path: &Path) -> std::io::Result<Self> {
        use rustix::fs::{AtFlags, CWD, StatxFlags, statx};

        let stx = statx(CWD, path, AtFlags::empty(), StatxFlags::MNT_ID)?;
        if stx.stx_mask & StatxFlags::MNT_ID.bits() != 0 {
            Ok(Self(stx.stx_mnt_id))
        } else {
            Ok(Self(
                (u64::from(stx.stx_dev_major) << 32) | u64::from(stx.stx_dev_minor),
            ))
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn of(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;

        Ok(Self(std::fs::metadata(path)?.dev()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    OnAnchorMount,
    Elsewhere,
}

impl Placement {
    /// Where `path` sits relative to `anchor`'s mount, both read now.
    pub fn of(path: &Path, anchor: &Path) -> std::io::Result<Self> {
        Ok(if MountId::of(path)? == MountId::of(anchor)? {
            Self::OnAnchorMount
        } else {
            Self::Elsewhere
        })
    }
}

/// The anchor directory's path, as recorded or as seen now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorDir(NormalPath);

impl AnchorDir {
    #[must_use]
    pub fn new(path: NormalPath) -> Self {
        Self(path)
    }

    #[must_use]
    pub fn path(&self) -> &NormalPath {
        &self.0
    }
}

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
