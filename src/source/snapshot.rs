use std::path::PathBuf;

use crate::kernel::SourceName;

use super::Result;
use super::model::{SourceEntryMeta, SourceInventory, SourcePath};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotId {
    Git {
        commit: String,
    },
    /// Link-mode artifacts are the exception to snapshot immutability: they
    /// deploy as symlinks that track the live worktree, while this snapshot
    /// freezes the inventory and copy-mode reads against the captured tree.
    Worktree {
        root: PathBuf,
        head: String,
        capture_digest: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub name: SourceName,
    pub url: String,
    pub snapshot: SnapshotId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub meta: SourceEntryMeta,
    pub bytes: Vec<u8>,
}

pub trait SourceStore {
    fn inventory(&self, source: &ResolvedSource) -> Result<SourceInventory>;

    fn read(&self, source: &ResolvedSource, path: &SourcePath) -> Result<SourceEntry>;
}
