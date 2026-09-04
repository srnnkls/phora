//! Write-ahead journal for target-side artifact swaps.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::sync::state::{ArtifactRecord, fsync_barrier};

/// Write-ahead journal of in-flight swaps, persisted under a `locks/` dir.
pub struct Journal {
    path: PathBuf,
    mode: JournalMode,
}

enum JournalMode {
    Writable,
    ReadOnly { root: PathBuf },
}

/// One intent record: enough to replay or roll back a single artifact swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub staging_base: PathBuf,
    pub staging: PathBuf,
    pub dst: PathBuf,
    pub record: ArtifactRecord,
    /// True once the stage→dst rename completed (registry put still pending).
    pub swap_completed: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct JournalFile {
    #[serde(default, rename = "entry")]
    entries: Vec<JournalEntry>,
}

impl Journal {
    /// Open (creating if needed) the journal living in `locks_dir`.
    pub fn open(locks_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(locks_dir).map_err(|e| {
            Error::Projection(format!("create locks dir {}: {e}", locks_dir.display()))
        })?;
        Ok(Self {
            path: locks_dir.join("journal.toml"),
            mode: JournalMode::Writable,
        })
    }

    /// A frozen sync's read-only journal: never creates `locks_dir`, replays existing
    /// entries, and refuses any write, naming the state root instead of raising EACCES.
    #[must_use]
    pub fn open_readonly(locks_dir: &Path) -> Self {
        let root = locks_dir
            .parent()
            .map_or_else(|| locks_dir.to_path_buf(), Path::to_path_buf);
        Self {
            path: locks_dir.join("journal.toml"),
            mode: JournalMode::ReadOnly { root },
        }
    }

    #[must_use]
    pub fn refuses_writes(&self) -> bool {
        matches!(self.mode, JournalMode::ReadOnly { .. })
    }

    /// The read-only pending-work error naming the state root; meaningful only when
    /// [`refuses_writes`](Self::refuses_writes) holds.
    #[must_use]
    pub fn readonly_error(&self) -> Error {
        match &self.mode {
            JournalMode::ReadOnly { root } => {
                Error::StateCtx(crate::sync::state::readonly_root_error(root))
            }
            JournalMode::Writable => {
                unreachable!("readonly_error called on a writable journal")
            }
        }
    }

    fn load(&self) -> Result<JournalFile> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                Error::Projection(format!("parse journal {}: {e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(JournalFile::default()),
            Err(e) => Err(Error::Projection(format!(
                "read journal {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn persist(&self, file: &JournalFile) -> Result<()> {
        if self.refuses_writes() {
            return Err(self.readonly_error());
        }
        let serialized = toml::to_string(file)
            .map_err(|e| Error::Projection(format!("serialize journal: {e}")))?;
        let tmp = self.path.with_extension("toml.tmp");
        {
            use std::io::Write as _;
            let mut handle = std::fs::File::create(&tmp)
                .map_err(|e| Error::Projection(format!("create temp {}: {e}", tmp.display())))?;
            handle
                .write_all(serialized.as_bytes())
                .map_err(|e| Error::Projection(format!("write temp {}: {e}", tmp.display())))?;
            fsync_barrier(&handle)
                .map_err(|e| Error::Projection(format!("fsync temp {}: {e}", tmp.display())))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::Projection(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                self.path.display()
            ))
        })
    }

    /// Append an intent before the swap.
    pub fn append(&self, entry: &JournalEntry) -> Result<()> {
        let mut file = self.load()?;
        file.entries.push(entry.clone());
        self.persist(&file)
    }

    /// Mark the most recent matching intent as swap-completed.
    pub fn mark_swap_completed(&self, dst: &Path) -> Result<()> {
        let mut file = self.load()?;
        if let Some(entry) = file.entries.iter_mut().rev().find(|e| e.dst == dst) {
            entry.swap_completed = true;
        }
        self.persist(&file)
    }

    pub fn entries(&self) -> Result<Vec<JournalEntry>> {
        Ok(self.load()?.entries)
    }

    /// Remove the most recent intent targeting `dst`.
    pub fn remove(&self, dst: &Path) -> Result<()> {
        let mut file = self.load()?;
        if let Some(pos) = file.entries.iter().rposition(|e| e.dst == dst) {
            file.entries.remove(pos);
        }
        self.persist(&file)
    }

    pub fn clear(&self) -> Result<()> {
        self.persist(&JournalFile::default())
    }
}
