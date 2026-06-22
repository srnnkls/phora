use std::path::Path;

use crate::error::{Error, Result};

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
