use std::path::Path;

use super::SourceName;

use super::Result;
use super::snapshot::{
    ResolvedRevision, ResolvedSource, SnapshotId, SourceIdentity, SourceTimestamp,
};
use super::worktree::{capture_worktree, worktree_authored_at};

pub(super) fn resolve_worktree(
    git_dir: &Path,
    name: &SourceName,
    root: &Path,
) -> Result<ResolvedSource> {
    let snapshot = capture_worktree(git_dir, name, root)?;
    let SnapshotId::Worktree { root, head, .. } = &snapshot else {
        unreachable!("capture_worktree always returns a worktree snapshot");
    };
    let authored_at = head.as_ref().map_or_else(
        || Ok(SourceTimestamp::from_unix_seconds(0)),
        |head| worktree_authored_at(root.as_path(), head),
    )?;
    Ok(ResolvedSource {
        name: name.clone(),
        revision: ResolvedRevision::WorktreeHead(head.clone()),
        authored_at,
        normalized_location: SourceIdentity::Worktree(root.clone()),
        snapshot,
    })
}
