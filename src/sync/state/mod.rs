use std::collections::BTreeSet;
use std::path::PathBuf;

pub mod file;
pub mod locking;

use file::{ArtifactKey, EjectedEntry, HookState, RegistryRecord, StoreError};
use locking::StateLockGuard;

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
