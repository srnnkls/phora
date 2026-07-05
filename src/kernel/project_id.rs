use std::io::Write as _;
use std::path::Path;

use crate::error::{Error, Result};

pub const IDENTITY_FILE: &str = ".phora-id";

/// Per-project registry identity: per-clone [`IDENTITY_FILE`] (UUID v4, survives
/// relocation) or a BLAKE3 path-hash fallback for projects without the file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectId(String);

impl ProjectId {
    /// Path-hash identity: BLAKE3 of the canonical project root, first 16 hex chars.
    pub fn for_path(root: &Path) -> Result<Self> {
        let canonical = root.canonicalize()?;
        let hash = blake3::hash(canonical.to_string_lossy().as_bytes());
        Ok(Self(hash.to_hex()[..16].to_string()))
    }

    /// Read-only resolution: the per-clone [`IDENTITY_FILE`] when present,
    /// otherwise the path-hash fallback. Never writes.
    pub fn resolve(root: &Path) -> Result<Self> {
        match read_identity(root)? {
            Some(id) => Ok(Self(id)),
            None => Self::for_path(root),
        }
    }

    /// A fresh per-clone identity (UUID v4).
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| {
            Error::Config(format!("system RNG unavailable for project identity: {e}"))
        })?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let h = |b: u8| format!("{b:02x}");
        let hex: String = bytes.iter().map(|b| h(*b)).collect();
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32],
        );
        Ok(Self(uuid))
    }

    /// The per-clone identity from [`IDENTITY_FILE`], or `None` when absent.
    pub fn read_identity_file(root: &Path) -> Result<Option<Self>> {
        Ok(read_identity(root)?.map(Self))
    }

    /// Wrap an identity string recovered internally (e.g. an adoption marker).
    #[must_use]
    pub fn from_raw(id: String) -> Self {
        Self(id)
    }

    /// Persist this identity to `root/.phora-id` atomically (temp + rename).
    pub fn write_identity_file(&self, root: &Path) -> Result<()> {
        let path = root.join(IDENTITY_FILE);
        let tmp = root.join(format!("{IDENTITY_FILE}.tmp"));
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(self.0.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::Io(e)
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exclude [`IDENTITY_FILE`] per-clone via `.git/info/exclude`, never the shared
/// `.gitignore`. Idempotent; a no-op for non-git projects (no `.git` directory).
pub fn exclude_identity_from_git(root: &Path) -> Result<()> {
    let git_dir = root.join(".git");
    if !git_dir.is_dir() {
        return Ok(());
    }
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info)?;
    let exclude = info.join("exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == IDENTITY_FILE) {
        return Ok(());
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(IDENTITY_FILE);
    body.push('\n');
    std::fs::write(&exclude, body)?;
    Ok(())
}

fn read_identity(root: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(root.join(IDENTITY_FILE)) {
        Ok(text) => {
            let trimmed = text.trim();
            Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
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
    fn generate_yields_distinct_uuid_v4_ids() {
        let a = ProjectId::generate().unwrap();
        let b = ProjectId::generate().unwrap();
        assert_ne!(a, b, "each generated identity must be distinct");
        let s = a.as_str();
        assert_eq!(s.len(), 36, "UUID v4 canonical form is 36 chars");
        assert_eq!(s.as_bytes()[14], b'4', "the version nibble must be 4");
    }

    #[test]
    fn resolve_prefers_identity_file_over_path_hash() {
        let dir = tempfile::TempDir::new().unwrap();
        let path_hash = ProjectId::for_path(dir.path()).unwrap();
        let generated = ProjectId::generate().unwrap();
        generated.write_identity_file(dir.path()).unwrap();

        let resolved = ProjectId::resolve(dir.path()).unwrap();
        assert_eq!(resolved, generated, "resolve must read the identity file");
        assert_ne!(resolved, path_hash, "identity file overrides the path hash");
    }

    #[test]
    fn exclude_is_idempotent_and_leaves_gitignore_untouched() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();

        exclude_identity_from_git(dir.path()).unwrap();
        exclude_identity_from_git(dir.path()).unwrap();

        let body = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            body.lines().filter(|l| l.trim() == IDENTITY_FILE).count(),
            1,
            "a repeated exclude append must not duplicate the entry"
        );
        assert!(
            !dir.path().join(".gitignore").exists(),
            "excluding must never create or touch the shared .gitignore"
        );
    }

    #[test]
    fn exclude_tolerates_non_git_project() {
        let dir = tempfile::TempDir::new().unwrap();
        exclude_identity_from_git(dir.path()).expect("no .git dir must be a clean no-op");
    }
}
