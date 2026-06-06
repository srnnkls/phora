//! Deployment: drift detection, copy/scan, atomic directory swap.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::registry::{ArtifactKey, EjectedEntry, Registry, RegistryRecord, ScannedFile};

pub enum ArtifactState {
    Clean,
    Modified { changed: Vec<PathBuf> },
    Foreign,
    Missing,
    Ejected,
}

#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    /// Relative paths of symlinks encountered (excluded from `files`).
    pub symlinks: Vec<PathBuf>,
}

pub fn check_artifact_state(
    _target_path: &Path,
    _expected_source: &str,
    _expected_commit: &str,
    _ejected: &[EjectedEntry],
    _artifact_name: &str,
    _registry: &dyn Registry,
    _key: &ArtifactKey,
) -> Result<ArtifactState> {
    Err(Error::NotImplemented("check_artifact_state"))
}

/// Soft scan: never errors on symlinks, reports them for "treat as Modified".
pub fn scan_dir_soft(_dir: &Path) -> Result<ScanResult> {
    Err(Error::NotImplemented("scan_dir_soft"))
}

/// Copy a file from staging to target, preferring reflink, preserving mtime.
pub fn copy_file(_src: &Path, _dst: &Path) -> Result<()> {
    Err(Error::NotImplemented("copy_file"))
}

pub fn copy_tree(_src: &Path, _dst: &Path, _allow_symlinks: bool) -> Result<()> {
    Err(Error::NotImplemented("copy_tree"))
}

/// Atomic swap of staging into the destination, then persist the registry record.
pub fn deploy_artifact(
    _staging_base: &Path,
    _staging: &Path,
    _dst: &Path,
    _record: RegistryRecord,
    _registry: &dyn Registry,
) -> Result<()> {
    Err(Error::NotImplemented("deploy_artifact"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_artifact_state_is_constructible() {
        let _ = ArtifactState::Missing;
    }
}
