use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::error::{Error as CrateError, Result};

/// Path-traversal guard failure.
#[derive(Debug, Error)]
pub enum KernelError {
    #[error("unsafe path component: {0:?}")]
    UnsafeComponent(String),
}

/// Rejects any string that is not a single inert path component, so a malicious git
/// tree or archive can never escape the staging dir when joined onto a path. Also
/// rejects cross-platform foot-guns that are inert on Unix but escape on Windows:
/// an NTFS alternate-data-stream `:` and the reserved DOS device names.
pub(crate) fn safe_component(name: &str) -> std::result::Result<&str, KernelError> {
    let unsafe_component = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || is_reserved_device_name(name);
    if unsafe_component {
        return Err(KernelError::UnsafeComponent(name.to_owned()));
    }
    Ok(name)
}

pub(crate) fn safe_relpath(path: &str) -> std::result::Result<&str, KernelError> {
    if path.contains('\0') {
        return Err(KernelError::UnsafeComponent(path.to_owned()));
    }
    for component in path.split('/') {
        if safe_component(component).is_err() {
            return Err(KernelError::UnsafeComponent(path.to_owned()));
        }
    }
    Ok(path)
}

fn is_reserved_device_name(name: &str) -> bool {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    if let Some(digit) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    {
        return matches!(digit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9");
    }
    false
}

/// A resolved git commit id: 40-hex (sha1) or 64-hex (sha256), canonicalized to lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Commit(String);

impl Commit {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Commit {
    type Err = CrateError;

    fn from_str(s: &str) -> Result<Self> {
        let len_ok = s.len() == 40 || s.len() == 64;
        let hex_ok = s.bytes().all(|b| b.is_ascii_hexdigit());
        if len_ok && hex_ok {
            Ok(Self(s.to_ascii_lowercase()))
        } else {
            Err(CrateError::Source(format!(
                "invalid commit id `{s}`: expected 40 or 64 hex chars"
            )))
        }
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A configured source identifier: the `[sources.<name>]` table key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceName(String);

impl SourceName {
    pub(crate) fn trusted(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SourceName {
    type Err = CrateError;

    fn from_str(s: &str) -> Result<Self> {
        safe_component(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for SourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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
