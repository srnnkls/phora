use std::path::{Path, PathBuf};

use super::SourceName;

use super::archive::{EntryKind, ExtractedEntry};
use super::cache::lock_mirror;
use super::import::import_tree;
use super::snapshot::{CanonicalSourceRoot, SnapshotId, SourceTimestamp, commit_from_hex};
use super::{Commit, MirrorKey, NormalizedUrl, Result, SourceError};

pub fn capture_worktree(
    git_dir: &Path,
    source: &SourceName,
    root: &Path,
    follow_symlinks: bool,
) -> Result<SnapshotId> {
    let root = root.canonicalize().map_err(|error| {
        SourceError::Source(format!("canonicalize worktree for {source}: {error}"))
    })?;
    std::fs::create_dir_all(git_dir)
        .map_err(|e| SourceError::Source(format!("create cache dir for {source}: {e}")))?;
    let cache_rel = contained_cache_rel(&root, git_dir);
    let entries = capture_entries(&root, cache_rel.as_deref(), follow_symlinks)?;
    let head = read_local_head(&root.to_string_lossy())
        .map_err(|e| SourceError::Source(format!("read head of {source}: {e}")))?;
    let url = root.to_string_lossy().into_owned();
    let _lock = lock_mirror(git_dir, source, &url)?;
    let capture_digest = import_tree(git_dir, &url, &entries)?;
    let head = (head != "link")
        .then(|| commit_from_hex(&head))
        .transpose()?;
    let mirror = MirrorKey::from_url(&NormalizedUrl::parse(&url));
    let capture_digest = commit_from_hex(&capture_digest)?;
    Ok(SnapshotId::Worktree {
        root: CanonicalSourceRoot::from_canonical(root),
        head,
        mirror,
        capture_digest,
    })
}

pub(super) fn read_worktree_manifest_bytes(root: &Path) -> Result<Vec<u8>> {
    std::fs::read(root.join("phora.toml")).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SourceError::DependencyManifestMissing {
                remote: root.display().to_string(),
                source: Box::new(error),
            }
        } else {
            SourceError::Io(error)
        }
    })
}

pub(super) fn worktree_authored_at(root: &Path, head: &Commit) -> Result<SourceTimestamp> {
    let repo = gix::open(root).map_err(|error| {
        SourceError::Source(format!("open worktree {}: {error}", root.display()))
    })?;
    let commit = repo
        .find_commit(
            gix::ObjectId::from_hex(head.as_str().as_bytes()).map_err(|error| {
                SourceError::Source(format!("parse worktree head {head}: {error}"))
            })?,
        )
        .map_err(|error| SourceError::Source(format!("find worktree head {head}: {error}")))?;
    let seconds = commit
        .author()
        .map_err(|error| SourceError::Source(format!("author of {head}: {error}")))?
        .time()
        .map_err(|error| SourceError::Source(format!("author time of {head}: {error}")))?
        .seconds;
    Ok(SourceTimestamp::from_unix_seconds(
        u64::try_from(seconds)
            .map_err(|error| SourceError::Source(format!("author time of {head}: {error}")))?,
    ))
}

fn contained_cache_rel(root: &Path, git_dir: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let cache = git_dir.canonicalize().ok()?;
    cache.strip_prefix(&root).ok().map(Path::to_path_buf)
}

/// Walks the non-directory entries under `base` as `(relative, absolute, metadata)`.
/// Symlinks are skipped unless `follow_symlinks`; a followed link reports its target's
/// metadata at its own logical path, a dangling link is skipped, and a link back into a
/// directory on the current descent is a cycle and is skipped. `skip` prunes by relative path.
pub(crate) fn walk_worktree(
    base: &Path,
    follow_symlinks: bool,
    skip: &dyn Fn(&Path) -> bool,
    visit: &mut dyn FnMut(PathBuf, &Path, &std::fs::Metadata) -> Result<()>,
) -> Result<()> {
    let mut walk = Walk {
        follow_symlinks,
        skip,
        visit,
        ancestors: base.canonicalize().into_iter().collect(),
    };
    walk.dir(base, Path::new(""))
}

struct Walk<'a> {
    follow_symlinks: bool,
    skip: &'a dyn Fn(&Path) -> bool,
    visit: &'a mut dyn FnMut(PathBuf, &Path, &std::fs::Metadata) -> Result<()>,
    ancestors: Vec<PathBuf>,
}

impl Walk<'_> {
    fn dir(&mut self, dir: &Path, rel: &Path) -> Result<()> {
        let read = std::fs::read_dir(dir)
            .map_err(|e| SourceError::Source(format!("scan worktree {}: {e}", dir.display())))?;
        for entry in read {
            let entry = entry.map_err(|e| {
                SourceError::Source(format!("read entry in {}: {e}", dir.display()))
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(SourceError::Source(format!(
                    "non-utf8 path in worktree: {}",
                    entry.path().display()
                )));
            };
            let entry_rel = rel.join(name);
            if (self.skip)(&entry_rel) {
                continue;
            }
            let path = entry.path();
            let ft = entry
                .file_type()
                .map_err(|e| SourceError::Source(format!("stat {}: {e}", path.display())))?;
            let meta = if ft.is_symlink() {
                if !self.follow_symlinks {
                    continue;
                }
                match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        return Err(SourceError::Source(format!(
                            "follow {}: {e}",
                            path.display()
                        )));
                    }
                }
            } else {
                entry
                    .metadata()
                    .map_err(|e| SourceError::Source(format!("stat {}: {e}", path.display())))?
            };
            if meta.is_dir() {
                self.descend(&path, &entry_rel)?;
            } else {
                (self.visit)(entry_rel, &path, &meta)?;
            }
        }
        Ok(())
    }

    fn descend(&mut self, dir: &Path, rel: &Path) -> Result<()> {
        let canonical = dir
            .canonicalize()
            .map_err(|e| SourceError::Source(format!("canonicalize {}: {e}", dir.display())))?;
        if self.ancestors.contains(&canonical) {
            return Ok(());
        }
        self.ancestors.push(canonical);
        let walked = self.dir(dir, rel);
        self.ancestors.pop();
        walked
    }
}

fn capture_entries(
    root: &Path,
    cache_rel: Option<&Path>,
    follow_symlinks: bool,
) -> Result<Vec<ExtractedEntry>> {
    let mut entries = Vec::new();
    let skip = |rel: &Path| rel.file_name() == Some(".git".as_ref()) || Some(rel) == cache_rel;
    walk_worktree(root, follow_symlinks, &skip, &mut |rel, path, meta| {
        if !meta.is_file() {
            return Ok(());
        }
        let data = std::fs::read(path)
            .map_err(|e| SourceError::Source(format!("read {}: {e}", path.display())))?;
        let kind = if is_executable(meta) {
            EntryKind::BlobExecutable
        } else {
            EntryKind::Blob
        };
        entries.push(ExtractedEntry {
            path: rel,
            kind,
            data,
        });
        Ok(())
    })?;
    Ok(entries)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn captured(root: &Path, follow_symlinks: bool) -> Vec<(PathBuf, EntryKind, Vec<u8>)> {
        let mut entries: Vec<_> = capture_entries(root, None, follow_symlinks)
            .expect("capture")
            .into_iter()
            .map(|entry| (entry.path, entry.kind, entry.data))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    fn linked_tree() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tmp");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("guidance")).expect("mkdir");
        std::fs::write(target.join("guidance/hints.cue"), "hints\n").expect("write");
        std::fs::write(target.join("run.sh"), "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(
            target.join("run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("rules")).expect("mkdir");
        symlink(target.join("guidance"), root.join("rules/guidance")).expect("dir link");
        symlink(target.join("run.sh"), root.join("rules/run.sh")).expect("file link");
        symlink(tmp.path().join("missing"), root.join("rules/dangling")).expect("dangling");
        symlink(&root, root.join("rules/loop")).expect("cycle");
        (tmp, root)
    }

    #[test]
    fn followed_links_enter_at_their_logical_paths() {
        let (_tmp, root) = linked_tree();
        assert_eq!(
            captured(&root, true),
            vec![
                (
                    PathBuf::from("rules/guidance/hints.cue"),
                    EntryKind::Blob,
                    b"hints\n".to_vec()
                ),
                (
                    PathBuf::from("rules/run.sh"),
                    EntryKind::BlobExecutable,
                    b"#!/bin/sh\n".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn links_are_skipped_unless_followed() {
        let (_tmp, root) = linked_tree();
        std::fs::write(root.join("rules/own.md"), "own\n").expect("write");
        assert_eq!(
            captured(&root, false),
            vec![(
                PathBuf::from("rules/own.md"),
                EntryKind::Blob,
                b"own\n".to_vec()
            )]
        );
    }

    #[test]
    fn capture_digest_tracks_link_target_content() {
        let (tmp, root) = linked_tree();
        let git_dir = tmp.path().join("cache");
        let name = SourceName::trusted("fas".to_owned());
        let before = capture_worktree(&git_dir, &name, &root, true).expect("capture");
        std::fs::write(tmp.path().join("target/guidance/hints.cue"), "edited\n").expect("edit");
        let after = capture_worktree(&git_dir, &name, &root, true).expect("recapture");
        assert_ne!(before.commit(), after.commit());
    }
}
