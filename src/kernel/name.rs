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

/// Like [`safe_component`] but admits interior `/`: a nested path that still
/// cannot escape its root when joined.
pub(crate) fn safe_relpath(path: &str) -> std::result::Result<&str, KernelError> {
    let unsafe_path = path.is_empty()
        || path.starts_with('/')
        || path.contains('\0')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..");
    if unsafe_path {
        return Err(KernelError::UnsafeComponent(path.to_owned()));
    }
    Ok(path)
}

/// Last `/`-separated segment of a tree locator (the artifact name it yields).
#[must_use]
pub(crate) fn locator_basename(locator: &str) -> &str {
    locator.rsplit_once('/').map_or(locator, |(_, base)| base)
}

/// Canonical glob predicate; Selection partitioning and collision-skip must share it.
#[must_use]
pub(crate) fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', ']', '{', '}'])
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

/// A configured target identifier: the `[targets.<name>]` table key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetName(String);

impl TargetName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TargetName {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        safe_component(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for TargetName {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_name_parses_safe_component() {
        let name: SourceName = "dotfiles".parse().expect("a safe component must parse");
        assert_eq!(name.as_str(), "dotfiles");
    }

    #[test]
    fn source_name_rejects_unsafe_component() {
        "a/b"
            .parse::<SourceName>()
            .expect_err("a component containing `/` must be rejected as path-unsafe");
    }

    #[test]
    fn target_name_parses_safe_component() {
        let name: TargetName = "staging".parse().expect("a safe component must parse");
        assert_eq!(name.as_str(), "staging");
    }

    #[test]
    fn target_name_rejects_unsafe_component() {
        "a/b"
            .parse::<TargetName>()
            .expect_err("a target name with `/` must be rejected as path-unsafe");
    }

    #[test]
    fn safe_relpath_accepts_interior_slashes() {
        assert_eq!(
            safe_relpath("a/b/c").expect("a nested relative path with interior `/` must be valid"),
            "a/b/c",
            "safe_relpath must accept a multi-segment nested path verbatim"
        );
    }

    #[test]
    fn safe_relpath_accepts_single_component() {
        assert_eq!(
            safe_relpath("x").expect("a single-component path is still a valid relpath"),
            "x",
            "safe_relpath must remain a superset of safe_component"
        );
    }

    #[test]
    fn safe_relpath_rejects_leading_slash() {
        safe_relpath("/a").expect_err("a leading `/` makes the path absolute and must be rejected");
    }

    #[test]
    fn safe_relpath_rejects_dotdot_component() {
        safe_relpath("..").expect_err("a bare `..` escapes the root and must be rejected");
    }

    #[test]
    fn safe_relpath_rejects_interior_dotdot() {
        safe_relpath("a/../b")
            .expect_err("an interior `..` component escapes the root and must be rejected");
    }

    #[test]
    fn safe_relpath_rejects_empty_interior_component() {
        safe_relpath("a//b")
            .expect_err("a doubled `/` yields an empty component and must be rejected");
    }

    #[test]
    fn safe_relpath_rejects_trailing_slash() {
        safe_relpath("a/b/")
            .expect_err("a trailing `/` yields an empty final component and must be rejected");
    }

    #[test]
    fn safe_relpath_rejects_embedded_nul() {
        safe_relpath("a\0b").expect_err("an embedded NUL byte must be rejected");
    }
}
