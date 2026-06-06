//! Registry port (`Registry`) and its file adapter (`FileRegistry`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactKey {
    pub target: String,
    pub source: String,
    pub artifact: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegistryRecord {
    pub version: u32,
    pub key: ArtifactKey,
    pub commit: String,
    pub digest: String,
    pub projected_at: String,
    pub layout: String,
    pub allow_symlinks: bool,
    pub preserve_executable: bool,
    pub files: Vec<ManifestFile>,
}

/// Registry record file entry (carries the content hash used by `phora verify`).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ManifestFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub blake3: String,
}

/// Filesystem scan entry: stat metadata only, no content hash.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EjectedEntry {
    pub source: String,
    pub artifact: String,
    pub ejected_at: String,
}

/// Content digest with a `blake3:` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest(String);

impl Digest {
    pub fn parse(s: &str) -> Result<Self> {
        if s.strip_prefix("blake3:").is_some_and(|hex| !hex.is_empty()) {
            Ok(Self(s.to_string()))
        } else {
            Err(Error::Registry(format!("invalid digest: {s}")))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub trait Registry {
    fn get(&self, key: &ArtifactKey) -> Result<Option<RegistryRecord>>;
    fn put(&self, record: &RegistryRecord) -> Result<()>;
    fn remove(&self, key: &ArtifactKey) -> Result<()>;
    fn list_target(&self, target: &str) -> Result<Vec<RegistryRecord>>;
    fn list_all(&self) -> Result<Vec<RegistryRecord>>;

    fn load_ejected(&self, target: &str) -> Result<Vec<EjectedEntry>>;
    fn save_ejected(&self, target: &str, ejected: &[EjectedEntry]) -> Result<()>;
}

pub struct FileRegistry {
    state_root: PathBuf,
}

impl FileRegistry {
    pub fn open(state_root: PathBuf) -> Result<Self> {
        Ok(Self { state_root })
    }

    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }
}

impl Registry for FileRegistry {
    fn get(&self, _key: &ArtifactKey) -> Result<Option<RegistryRecord>> {
        Err(Error::NotImplemented("FileRegistry::get"))
    }

    fn put(&self, _record: &RegistryRecord) -> Result<()> {
        Err(Error::NotImplemented("FileRegistry::put"))
    }

    fn remove(&self, _key: &ArtifactKey) -> Result<()> {
        Err(Error::NotImplemented("FileRegistry::remove"))
    }

    fn list_target(&self, _target: &str) -> Result<Vec<RegistryRecord>> {
        Err(Error::NotImplemented("FileRegistry::list_target"))
    }

    fn list_all(&self) -> Result<Vec<RegistryRecord>> {
        Err(Error::NotImplemented("FileRegistry::list_all"))
    }

    fn load_ejected(&self, _target: &str) -> Result<Vec<EjectedEntry>> {
        Err(Error::NotImplemented("FileRegistry::load_ejected"))
    }

    fn save_ejected(&self, _target: &str, _ejected: &[EjectedEntry]) -> Result<()> {
        Err(Error::NotImplemented("FileRegistry::save_ejected"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_requires_blake3_prefix() {
        assert!(Digest::parse("blake3:abc").is_ok());
        assert!(Digest::parse("sha256:abc").is_err());
        assert!(Digest::parse("blake3:").is_err());
    }
}
