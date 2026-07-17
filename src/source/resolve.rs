use std::path::Path;

use crate::kernel::SourceName;

use super::Result;
use super::snapshot::ResolvedSource;
use super::worktree::capture_worktree;

pub fn resolve_worktree(git_dir: &Path, name: &SourceName, root: &Path) -> Result<ResolvedSource> {
    let snapshot = capture_worktree(git_dir, name, root)?;
    Ok(ResolvedSource {
        name: name.clone(),
        url: root.to_string_lossy().into_owned(),
        snapshot,
    })
}
