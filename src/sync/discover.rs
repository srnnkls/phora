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
            artifacts.push(ArtifactName::trusted(name));
        }
    }

    artifacts.sort();
    Ok(artifacts)
}

pub(super) fn discover_working_tree_leaves(git: &Path, root: Option<&Path>) -> Result<Vec<String>> {
    let base = root.map_or_else(|| git.to_path_buf(), |r| git.join(r));
    let mut leaves = Vec::new();
    collect_working_tree_leaves(&base, Path::new(""), &mut leaves)?;
    leaves.sort_unstable();
    Ok(leaves)
}

fn collect_working_tree_leaves(base: &Path, rel: &Path, leaves: &mut Vec<String>) -> Result<()> {
    let dir = base.join(rel);
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| Error::Sync(format!("scan working tree {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read entry in {}: {e}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let entry_rel = rel.join(&name);
        let ft = entry
            .file_type()
            .map_err(|e| Error::Sync(format!("stat {}: {e}", entry.path().display())))?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            collect_working_tree_leaves(base, &entry_rel, leaves)?;
        } else {
            leaves.push(entry_rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

pub(super) fn discover_artifacts_for_source(
    source: &ParsedSource,
    git: &str,
    source_name: &SourceName,
    commit: &str,
    backend: &dyn SourceBackend,
    selection: &Selection,
    root: Option<&Path>,
) -> Result<Vec<ArtifactName>> {
    match source.deploy_mode() {
        DeployMode::Link => discover_working_tree(Path::new(git), root, selection),
        DeployMode::Copy => {
            Ok(backend.discover_artifacts(source_name, git, commit, root, selection)?)
        }
    }
}
