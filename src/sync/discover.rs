use std::path::Path;

use crate::error::{Error, Result};

pub(crate) fn discover_working_tree_leaves(
    git: &Path,
    root: Option<&Path>,
    follow_symlinks: bool,
) -> Result<Vec<String>> {
    let base = root.map_or_else(|| git.to_path_buf(), |r| git.join(r));
    let mut leaves = Vec::new();
    crate::source::walk_worktree(&base, follow_symlinks, &|_| false, &mut |rel, _, _| {
        leaves.push(rel.to_string_lossy().replace('\\', "/"));
        Ok(())
    })
    .map_err(|e| Error::Sync(e.to_string()))?;
    leaves.sort_unstable();
    Ok(leaves)
}
