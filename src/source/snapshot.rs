use std::path::{Path, PathBuf};

use super::{Commit, MirrorKey, NormalizedUrl, SourceError, SourceName};

use super::model::{SourceEntryKind, SourceEntryMeta, SourceInventory, SourcePath};
use super::{Result, hash_framed_entry};

pub(super) fn frame_tag(kind: SourceEntryKind) -> &'static [u8] {
    match kind {
        SourceEntryKind::File => b"\x00file\x00",
        SourceEntryKind::Executable => b"\x00exec\x00",
        SourceEntryKind::Symlink => b"\x00link\x00",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotId {
    Git {
        mirror: MirrorKey,
        commit: Commit,
    },
    Worktree {
        root: CanonicalSourceRoot,
        head: Option<Commit>,
        mirror: MirrorKey,
        capture_digest: Commit,
    },
}

impl SnapshotId {
    #[must_use]
    pub fn mirror(&self) -> &MirrorKey {
        match self {
            Self::Git { mirror, .. } | Self::Worktree { mirror, .. } => mirror,
        }
    }

    #[must_use]
    pub fn commit(&self) -> &Commit {
        match self {
            Self::Git { commit, .. } => commit,
            Self::Worktree { capture_digest, .. } => capture_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSourceRoot(PathBuf);

impl CanonicalSourceRoot {
    /// # Errors
    /// Returns an I/O error when the source root cannot be canonicalized.
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self(root.canonicalize()?))
    }

    pub(super) fn from_canonical(root: PathBuf) -> Self {
        Self(root)
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLocation {
    Git { url: String },
    Url { url: String },
    Worktree { root: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionSpec {
    Branch(String),
    Tag(String),
    Commit(Commit),
    Default,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvePolicy {
    Refresh,
    CachedOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveRequest {
    pub name: SourceName,
    pub location: SourceLocation,
    pub revision: RevisionSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRevision {
    Commit(Commit),
    WorktreeHead(Option<Commit>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceIdentity {
    Git(NormalizedUrl),
    Url(NormalizedUrl),
    Worktree(CanonicalSourceRoot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTimestamp(u64);

impl SourceTimestamp {
    #[must_use]
    pub fn from_unix_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    #[must_use]
    pub fn unix_seconds(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub name: SourceName,
    pub snapshot: SnapshotId,
    pub revision: ResolvedRevision,
    pub authored_at: SourceTimestamp,
    pub normalized_location: SourceIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub meta: SourceEntryMeta,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDirectoryEntryKind {
    File,
    Executable,
    Symlink,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDirectoryEntry {
    pub path: SourcePath,
    pub kind: SourceDirectoryEntryKind,
}

pub trait SourceStore: Send + Sync {
    fn resolve(&self, request: &ResolveRequest, policy: ResolvePolicy) -> Result<ResolvedSource>;

    fn inventory(
        &self,
        snapshot: &SnapshotId,
        root: Option<&SourcePath>,
    ) -> Result<SourceInventory>;

    fn read(&self, snapshot: &SnapshotId, path: &SourcePath) -> Result<SourceEntry>;

    fn list_directory(
        &self,
        snapshot: &SnapshotId,
        path: Option<&SourcePath>,
    ) -> Result<Vec<SourceDirectoryEntry>>;
}

/// # Errors
/// Returns the first source-read error, including directory and missing-leaf errors.
pub fn digest_snapshot(
    store: &dyn SourceStore,
    snapshot: &SnapshotId,
    leaves: &[SourcePath],
) -> Result<String> {
    let mut sorted: Vec<&SourcePath> = leaves.iter().collect();
    sorted.sort_unstable();
    sorted.dedup();

    let mut hasher = blake3::Hasher::new();
    for path in sorted {
        let entry = store.read(snapshot, path)?;
        hash_framed_entry(
            &mut hasher,
            path.as_str().as_bytes(),
            frame_tag(entry.meta.kind),
            &entry.bytes,
        );
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub(super) fn commit_from_hex(value: &str) -> Result<Commit> {
    value
        .parse()
        .map_err(|error| SourceError::Source(format!("{error}")))
}
