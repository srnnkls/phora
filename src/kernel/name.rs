use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::error::{Error, Result};

/// Path-traversal guard failure.
#[derive(Debug, Error)]
pub enum KernelError {
    #[error("unsafe path component: {0:?}")]
    UnsafeComponent(String),
}

/// Rejects any string that is not a single inert path component, so a malicious git
/// tree or archive can never escape the staging dir when joined onto a path.
pub(crate) fn safe_component(name: &str) -> std::result::Result<&str, KernelError> {
    let unsafe_component =
        name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\');
    if unsafe_component {
        return Err(KernelError::UnsafeComponent(name.to_owned()));
    }
    Ok(name)
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
    type Err = Error;

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

/// A single artifact path component discovered in a source tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactName(String);

impl ArtifactName {
    pub(crate) fn trusted(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ArtifactName {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        safe_component(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for ArtifactName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
