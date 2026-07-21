use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub mod file;
pub mod locking;

use file::{ArtifactKey, EjectedEntry, HookState, RegistryRecord, StoreError};
use locking::StateLockGuard;

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

pub trait StateStore {
    fn artifact(&self, key: &ArtifactKey) -> Result<Option<RegistryRecord>, StoreError>;
    fn put_artifact(&self, record: &RegistryRecord) -> Result<(), StoreError>;
    fn remove_artifact(&self, key: &ArtifactKey) -> Result<(), StoreError>;
    fn target_artifacts(&self, target: &str) -> Result<Vec<RegistryRecord>, StoreError>;
    fn all_artifacts(&self) -> Result<Vec<RegistryRecord>, StoreError>;
    fn ejections(&self, target: &str) -> Result<Vec<EjectedEntry>, StoreError>;
    fn save_ejections(&self, target: &str, entries: &[EjectedEntry]) -> Result<(), StoreError>;
    fn hook_state(&self, target: &str) -> Result<Vec<HookState>, StoreError>;
    fn record_hook_success(
        &self,
        target: &str,
        hook_id: &str,
        digest_set: &BTreeSet<String>,
    ) -> Result<(), StoreError>;
    fn acquire_lock(&self) -> Result<StateLockGuard, StoreError>;
    fn journal_root(&self) -> PathBuf;
}
