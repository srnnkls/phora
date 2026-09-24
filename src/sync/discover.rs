use std::path::Path;

use crate::error::{Error, Result};

pub(crate) fn discover_working_tree_leaves(
    git: &Path,
    root: Option<&Path>,
    follow_symlinks: bool,
) -> Result<Vec<String>> {
    let base = root.map_or_else(|| git.to_path_buf(), |r| git.join(r));
    let mut leaves = Vec::new();
    let skip = |rel: &Path| rel.file_name() == Some(".git".as_ref());
    crate::source::walk_worktree(&base, follow_symlinks, &skip, &mut |rel, _, _| {
        leaves.push(rel.to_string_lossy().replace('\\', "/"));
        Ok(())
    })
    .map_err(|e| Error::Sync(e.to_string()))?;
    leaves.sort_unstable();
    Ok(leaves)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_tree_leaves_exclude_git_metadata() {
        let root = tempfile::TempDir::new().expect("working tree");
        std::fs::create_dir_all(root.path().join(".git/objects")).expect("create .git");
        std::fs::write(root.path().join(".git/HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        std::fs::create_dir_all(root.path().join("editor")).expect("create editor");
        std::fs::write(root.path().join("editor/init.lua"), b"-- init\n").expect("init");

        let leaves =
            discover_working_tree_leaves(root.path(), None, false).expect("discover leaves");

        assert_eq!(leaves, ["editor/init.lua"]);
    }
}
