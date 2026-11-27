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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The anchor directory as read in this run, with the lookups `locate` makes beyond `symlink_metadata`.
#[derive(Debug)]
pub struct CurrentAnchor {
    dir: AnchorDir,
    mount: MountId,
    encrypted: fn(&Path) -> std::io::Result<bool>,
    canonicalize: fn(&Path) -> std::io::Result<PathBuf>,
}

impl CurrentAnchor {
    #[must_use]
    pub fn new(dir: AnchorDir, mount: MountId) -> Self {
        Self {
            dir,
            mount,
            #[cfg(target_os = "linux")]
            encrypted: statx_encrypted,
            // APFS encrypts whole volumes, which the mount comparison covers.
            #[cfg(not(target_os = "linux"))]
            encrypted: |_| Ok(false),
            canonicalize: Path::canonicalize,
        }
    }

    /// Replaces the encryption and canonicalize lookups, standing in for another volume.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_lookups(
        self,
        encrypted: fn(&Path) -> std::io::Result<bool>,
        canonicalize: fn(&Path) -> std::io::Result<PathBuf>,
    ) -> Self {
        Self {
            encrypted,
            canonicalize,
            ..self
        }
    }
}

/// Statx `STATX_ATTR_ENCRYPTED`; a mask without that bit reads as unencrypted.
#[cfg(target_os = "linux")]
fn statx_encrypted(path: &Path) -> std::io::Result<bool> {
    use rustix::fs::{AtFlags, CWD, StatxAttributes, StatxFlags, statx};

    let stx = statx(CWD, path, AtFlags::empty(), StatxFlags::empty())?;
    Ok((stx.stx_attributes & stx.stx_attributes_mask).contains(StatxAttributes::ENCRYPTED))
}

/// `NotFound`, or `NotADirectory` on an ancestor component.
pub fn is_absent(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::NotFound | ErrorKind::NotADirectory)
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

/// Walks `path` with `symlink_metadata`; an absent path is `Deleted` only when every deletion-proof clause holds.
pub fn locate(
    path: &NormalPath,
    placement: Placement,
    recorded: &AnchorDir,
    current: &CurrentAnchor,
) -> Location {
    let mut existing = PathBuf::new();
    for component in path.as_path().components() {
        let next = existing.join(component);
        match std::fs::symlink_metadata(&next) {
            Ok(stat) if stat.file_type().is_symlink() => {
                return Location::Unproven(Unproven::SymlinkedComponent(NormalPath(next)));
            }
            Ok(_) => existing = next,
            Err(error) if is_absent(error.kind()) => {
                return absent(path, placement, recorded, current, &existing);
            }
            Err(error) => return Location::Unproven(Unproven::Fault(error.kind())),
        }
    }
    match (current.canonicalize)(path.as_path()) {
        Ok(now) if now == path.as_path() => Location::Present,
        Ok(now) => Location::Unproven(Unproven::CanonicalDiffers {
            now: NormalPath(normalize_lexical(&now)),
        }),
        Err(error) => Location::Unproven(Unproven::Fault(error.kind())),
    }
}

fn absent(
    path: &NormalPath,
    placement: Placement,
    recorded: &AnchorDir,
    current: &CurrentAnchor,
    ancestor: &Path,
) -> Location {
    if placement != Placement::OnAnchorMount {
        return Location::Unproven(Unproven::RecordedElsewhere);
    }
    if *recorded != current.dir {
        return Location::Unproven(Unproven::AnchorMoved {
            now: current.dir.path().clone(),
        });
    }
    match MountId::of(ancestor) {
        Ok(mount) if mount != current.mount => return Location::Unproven(Unproven::OffMount),
        Ok(_) => {}
        Err(error) => return Location::Unproven(Unproven::Fault(error.kind())),
    }
    match (current.encrypted)(ancestor) {
        Ok(false) => Location::Deleted(DeletionProof { path: path.clone() }),
        Ok(true) => Location::Unproven(Unproven::Encrypted),
        Err(error) => Location::Unproven(Unproven::Fault(error.kind())),
    }
}
