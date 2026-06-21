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

pub fn safe_relpath(path: &str) -> std::result::Result<&str, KernelError> {
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
            safe_relpath("file.txt").expect("a single-component path is still a valid relpath"),
            "file.txt",
            "safe_relpath must remain a superset of safe_component"
        );
    }

    #[test]
    fn safe_relpath_rejects_empty() {
        let err = safe_relpath("").expect_err("an empty path must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s.is_empty()),
            "an empty path must reject as UnsafeComponent naming the whole input \"\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_leading_slash() {
        let err = safe_relpath("/a")
            .expect_err("a leading `/` makes the path absolute and must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "/a"),
            "a leading `/` must reject as UnsafeComponent naming the whole input \"/a\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_dotdot_component() {
        let err =
            safe_relpath("..").expect_err("a bare `..` escapes the root and must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == ".."),
            "a bare `..` must reject as UnsafeComponent naming the whole input \"..\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_interior_dotdot() {
        let err = safe_relpath("a/../b")
            .expect_err("an interior `..` component can escape the root and must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a/../b"),
            "an interior `..` must reject as UnsafeComponent naming the whole input \"a/../b\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_empty_interior_component() {
        let err = safe_relpath("a//b")
            .expect_err("an empty interior component (`a//b`) must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a//b"),
            "an empty interior component must reject as UnsafeComponent naming the whole input \"a//b\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_trailing_slash() {
        let err = safe_relpath("a/b/")
            .expect_err("a trailing `/` yields an empty final component and must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a/b/"),
            "a trailing `/` must reject as UnsafeComponent naming the whole input \"a/b/\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_embedded_nul() {
        let err = safe_relpath("a\0b").expect_err("an embedded NUL byte must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a\0b"),
            "a single component carrying a NUL must reject naming that component verbatim, got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_backslash() {
        let err =
            safe_relpath("a\\b").expect_err("a backslash escapes on Windows and must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a\\b"),
            "a single component carrying a `\\` must reject naming that component verbatim, got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_windows_drive_prefix() {
        let err = safe_relpath("C:/tmp/x")
            .expect_err("a `C:` drive-prefixed component carries a colon and must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "C:/tmp/x"),
            "a Windows drive prefix must reject as UnsafeComponent naming the whole input \"C:/tmp/x\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_bare_drive_component() {
        let err = safe_relpath("a:b").expect_err(
            "a component carrying a colon (NTFS ADS / drive separator) must be rejected",
        );
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a:b"),
            "a colon-bearing component must reject as UnsafeComponent naming the whole input \"a:b\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_reserved_device_name() {
        let err =
            safe_relpath("NUL").expect_err("the reserved DOS device name `NUL` must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "NUL"),
            "a reserved device name must reject as UnsafeComponent naming the whole input \"NUL\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_reserved_device_in_subpath() {
        let err = safe_relpath("dir/NUL/file")
            .expect_err("a reserved DOS device name in an interior component must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "dir/NUL/file"),
            "an interior reserved device name must reject as UnsafeComponent naming the whole input \"dir/NUL/file\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_com_port_device() {
        let err =
            safe_relpath("COM1").expect_err("the reserved DOS device name `COM1` must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "COM1"),
            "a reserved COM-port device name must reject as UnsafeComponent naming the whole input \"COM1\", got {err:?}"
        );
    }

    #[test]
    fn safe_relpath_rejects_colon_in_interior_component() {
        let err =
            safe_relpath("a/b:c/d").expect_err("a colon in an interior component must be rejected");
        assert!(
            matches!(&err, KernelError::UnsafeComponent(s) if s == "a/b:c/d"),
            "an interior colon must reject as UnsafeComponent naming the whole input \"a/b:c/d\", got {err:?}"
        );
    }
}
