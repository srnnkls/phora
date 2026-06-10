use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// Rejects any string that is not a single inert path component, so a malicious git
/// tree or archive can never escape the staging dir when joined onto a path.
pub(crate) fn safe_component(name: &str) -> Result<&str> {
    let unsafe_component =
        name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\');
    if unsafe_component {
        return Err(Error::Source(format!("unsafe path component: {name:?}")));
    }
    Ok(name)
}

/// A configured source identifier: the `[sources.<name>]` table key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceName(String);

impl SourceName {
    pub(crate) fn new(s: impl Into<String>) -> Self {
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
    pub(crate) fn new(s: impl Into<String>) -> Self {
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
