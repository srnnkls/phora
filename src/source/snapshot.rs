use std::path::PathBuf;

use crate::kernel::SourceName;

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

    fn digest_snapshot(&self, source: &ResolvedSource, leaves: &[SourcePath]) -> Result<String> {
        let mut sorted: Vec<&SourcePath> = leaves.iter().collect();
        sorted.sort_unstable();
        sorted.dedup();

        let mut hasher = blake3::Hasher::new();
        for path in sorted {
            let entry = self.read(source, path)?;
            hash_framed_entry(
                &mut hasher,
                path.as_str().as_bytes(),
                frame_tag(entry.meta.kind),
                &entry.bytes,
            );
        }
        Ok(format!("blake3:{}", hasher.finalize().to_hex()))
    }
}
