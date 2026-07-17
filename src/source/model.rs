use crate::kernel::{KernelError, safe_relpath};

/// A source-relative path validated by the lexical `safe_relpath` rule, preserved
/// verbatim (no case fold, no NFC): distinct case and distinct Unicode forms are
/// distinct paths, ordered by UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePath(String);

impl SourcePath {
    /// # Errors
    /// Returns [`KernelError`] when `path` is not a safe forward-slashed relative path.
    pub fn new(path: &str) -> std::result::Result<Self, KernelError> {
        safe_relpath(path)?;
        Ok(Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for SourcePath {
    type Err = KernelError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl std::fmt::Display for SourcePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The kind of a discovered source entry. PR5 populates `Executable`/`Symlink` from
/// real discovery; the pure `SourceInventory::from_paths` seed uses `File`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceEntryKind {
    File,
    Executable,
    Symlink,
}

/// One discovered source entry: its source-relative path and kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceEntryMeta {
    pub path: SourcePath,
    pub kind: SourceEntryKind,
}

/// The pure discovered leaf set of a source snapshot: entries in a deterministic
/// ascending path order, independent of discovery order. PR5 adds the store I/O that
/// populates this from a real snapshot; the projection consumes it as a pure value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceInventory {
    pub entries: Vec<SourceEntryMeta>,
}

impl SourceInventory {
    /// Seeds an inventory from source-relative paths as plain `File` entries, ordered
    /// ascending by path.
    ///
    /// # Errors
    /// Returns [`KernelError`] when any path is not a safe forward-slashed relative path.
    pub fn from_paths<I, S>(paths: I) -> std::result::Result<Self, KernelError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut entries: Vec<SourceEntryMeta> = paths
            .into_iter()
            .map(|p| {
                SourcePath::new(p.as_ref()).map(|path| SourceEntryMeta {
                    path,
                    kind: SourceEntryKind::File,
                })
            })
            .collect::<std::result::Result<_, _>>()?;
        entries.sort();
        Ok(Self { entries })
    }
}
