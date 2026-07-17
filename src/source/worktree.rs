use std::path::{Path, PathBuf};

use crate::kernel::SourceName;

use super::archive::{EntryKind, ExtractedEntry};
use super::cache::lock_mirror;
use super::import::import_tree;
use super::snapshot::SnapshotId;
use super::{Result, SourceError};

pub fn capture_worktree(git_dir: &Path, source: &SourceName, root: &Path) -> Result<SnapshotId> {
    std::fs::create_dir_all(git_dir)
        .map_err(|e| SourceError::Source(format!("create cache dir for {source}: {e}")))?;
    let cache_rel = contained_cache_rel(root, git_dir);
    let mut entries = Vec::new();
    collect_capture_entries(root, Path::new(""), cache_rel.as_deref(), &mut entries)?;
    let head = read_local_head(&root.to_string_lossy())
        .map_err(|e| SourceError::Source(format!("read head of {source}: {e}")))?;
    let url = root.to_string_lossy().into_owned();
    let _lock = lock_mirror(git_dir, source, &url)?;
    let capture_digest = import_tree(git_dir, &url, &entries)?;
    Ok(SnapshotId::Worktree {
        root: root.to_path_buf(),
        head,
        capture_digest,
    })
}

fn contained_cache_rel(root: &Path, git_dir: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let cache = git_dir.canonicalize().ok()?;
    cache.strip_prefix(&root).ok().map(Path::to_path_buf)
}

fn collect_capture_entries(
    base: &Path,
    rel: &Path,
    cache_rel: Option<&Path>,
    entries: &mut Vec<ExtractedEntry>,
) -> Result<()> {
    let dir = base.join(rel);
    let read = std::fs::read_dir(&dir)
        .map_err(|e| SourceError::Source(format!("scan worktree {}: {e}", dir.display())))?;
    for entry in read {
        let entry = entry
            .map_err(|e| SourceError::Source(format!("read entry in {}: {e}", dir.display())))?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let Some(name) = name.to_str() else {
            return Err(SourceError::Source(format!(
                "non-utf8 path in worktree: {}",
                entry.path().display()
            )));
        };
        let entry_rel = rel.join(name);
        if cache_rel == Some(entry_rel.as_path()) {
            continue;
        }
        let ft = entry
            .file_type()
            .map_err(|e| SourceError::Source(format!("stat {}: {e}", entry.path().display())))?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            collect_capture_entries(base, &entry_rel, cache_rel, entries)?;
        } else if ft.is_file() {
            let meta = entry.metadata().map_err(|e| {
                SourceError::Source(format!("stat {}: {e}", entry.path().display()))
            })?;
            let data = std::fs::read(entry.path()).map_err(|e| {
                SourceError::Source(format!("read {}: {e}", entry.path().display()))
            })?;
            let kind = if is_executable(&meta) {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            };
            entries.push(ExtractedEntry {
                path: entry_rel,
                kind,
                data,
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// HEAD of a local working-tree repo; a non-repo or unborn HEAD yields the
/// `"link"` sentinel, since link mode must tolerate a plain directory.
pub fn read_local_head(path: &str) -> crate::error::Result<String> {
    let Ok(repo) = gix::open(path) else {
        return Ok("link".to_owned());
    };
    match repo.head_id() {
        Ok(id) => Ok(id.to_hex().to_string()),
        Err(_) => Ok("link".to_owned()),
    }
}

/// True when `git` is a local filesystem path (absolute or existing), not a scheme/scp-style URL.
#[must_use]
pub fn is_local_path(git: &str) -> bool {
    if git.contains("://") {
        return false;
    }
    if matches!(git.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic()) {
        return true;
    }
    let first_slash = git.find('/');
    if let Some(colon) = git.find(':')
        && first_slash.is_none_or(|slash| colon < slash)
    {
        return false;
    }
    let path = Path::new(git);
    path.is_absolute() || path.exists()
}
