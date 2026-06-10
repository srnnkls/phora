use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use crate::error::{Error, Result};

/// A normalized relative path that cannot escape its root.
///
/// Construction strips `.` segments and rejects absolute paths and leading `..`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath(PathBuf);

impl RelPath {
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl FromStr for RelPath {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let reject = |why: &str| Err(Error::Source(format!("invalid relative path `{s}`: {why}")));
        let mut normalized = PathBuf::new();
        for component in Path::new(s).components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => normalized.push(part),
                Component::ParentDir => return reject("escapes its root via `..`"),
                Component::RootDir | Component::Prefix(_) => return reject("must be relative"),
            }
        }
        Ok(Self(normalized))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_string_lossy())
    }
}
