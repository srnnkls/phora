use std::path::Path;

use crate::config::{DeployMode, ParsedSource};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Selection, SourceName};
use crate::source::SourceBackend;

/// Discover artifact directories by scanning the live working tree at
/// `<git>/<root>` (Link mode). Mirrors the ODB `discover_artifacts`: only
/// directory entries become artifacts, `Selection` gates inclusion (the dotfile
/// opt-in lives there), and the result is sorted. A missing path/root is an error.
pub(super) fn discover_working_tree(
    git: &Path,
    root: Option<&Path>,
    selection: &Selection,
) -> Result<Vec<ArtifactName>> {
    let base = root.map_or_else(|| git.to_path_buf(), |r| git.join(r));
    let entries = std::fs::read_dir(&base)
        .map_err(|e| Error::Sync(format!("scan working tree {}: {e}", base.display())))?;

    let mut artifacts = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read entry in {}: {e}", base.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !selection.selects_artifact(&name) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            artifacts.push(ArtifactName::new(name));
        }
    }

    artifacts.sort();
    Ok(artifacts)
}

pub(super) fn discover_artifacts_for_source(
    source: &ParsedSource,
    git: &str,
    source_name: &SourceName,
    commit: &str,
    backend: &dyn SourceBackend,
    selection: &Selection,
) -> Result<Vec<ArtifactName>> {
    match source.deploy_mode() {
        DeployMode::Link => {
            discover_working_tree(Path::new(git), source.root.as_deref(), selection)
        }
        DeployMode::Copy => {
            backend.discover_artifacts(source_name, git, commit, source.root.as_deref(), selection)
        }
    }
}
