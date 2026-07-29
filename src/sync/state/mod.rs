use std::path::Path;

pub mod file;
pub mod locking;

pub use file::{
    ArtifactKey, ArtifactRecord, Ejection, FileStateStore, HookState, ManifestFile,
    NewArtifactRecord, RecordKind, ScannedFile, StateError, ejected_index, readonly_root_error,
};
pub use locking::StateLock;

pub trait StateStore {
    fn artifact(&self, key: &ArtifactKey) -> Result<Option<ArtifactRecord>, StateError>;
    fn put_artifact(&self, record: &ArtifactRecord) -> Result<(), StateError>;
    fn remove_artifact(&self, key: &ArtifactKey) -> Result<(), StateError>;
    fn target_artifacts(&self, target: &str) -> Result<Vec<ArtifactRecord>, StateError>;
    fn all_artifacts(&self) -> Result<Vec<ArtifactRecord>, StateError>;
    fn ejections(&self, target: &str) -> Result<Vec<Ejection>, StateError>;
    fn save_ejections(&self, target: &str, entries: &[Ejection]) -> Result<(), StateError>;
    fn hook_state(&self, target: &str) -> Result<Vec<HookState>, StateError>;
    fn record_hook_success(
        &self,
        target: &str,
        hook_id: &str,
        digest_set: &std::collections::BTreeSet<String>,
    ) -> Result<(), StateError>;
    fn acquire_lock(&self) -> Result<StateLock, StateError>;
    fn journal_root(&self) -> std::path::PathBuf;
}

/// Per-project registry identity: BLAKE3 of the canonical project root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectId(String);

impl ProjectId {
    /// Path-hash identity: BLAKE3 of the canonical project root, first 16 hex chars.
    pub fn for_path(root: &Path) -> crate::error::Result<Self> {
        let canonical = root.canonicalize()?;
        let hash = blake3::hash(canonical.to_string_lossy().as_bytes());
        Ok(Self(hash.to_hex()[..16].to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
