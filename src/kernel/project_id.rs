use std::path::Path;

use crate::error::Result;

/// Per-project registry identity: BLAKE3 of the canonical project root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectId(String);

impl ProjectId {
    /// Path-hash identity: BLAKE3 of the canonical project root, first 16 hex chars.
    pub fn for_path(root: &Path) -> Result<Self> {
        let canonical = root.canonicalize()?;
        let hash = blake3::hash(canonical.to_string_lossy().as_bytes());
        Ok(Self(hash.to_hex()[..16].to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_sixteen_hex() {
        let dir = std::env::temp_dir();
        let id = ProjectId::for_path(&dir).unwrap();
        assert_eq!(id.as_str().len(), 16);
    }

    #[test]
    fn project_id_is_deterministic_for_same_canonical_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = ProjectId::for_path(dir.path()).unwrap();
        let second = ProjectId::for_path(dir.path()).unwrap();
        assert_eq!(first, second, "the path hash must be stable for one path");
    }
}
