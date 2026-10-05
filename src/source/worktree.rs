use std::path::{Path, PathBuf};

use super::SourceName;

use super::archive::{EntryKind, ExtractedEntry};
use super::cache::{lock_mirror, mirror_path};
use super::import::import_captured;
use super::snapshot::{CanonicalSourceRoot, SnapshotId, SourceTimestamp, commit_from_hex};
use super::{Commit, MirrorKey, NormalizedUrl, Result, SourceError};
use capture_index::{CaptureIndex, FileStamp};

pub fn capture_worktree(
    git_dir: &Path,
    source: &SourceName,
    root: &Path,
    follow_symlinks: bool,
) -> Result<SnapshotId> {
    capture_worktree_at(
        git_dir,
        source,
        root,
        follow_symlinks,
        std::time::SystemTime::now(),
    )
}

fn capture_worktree_at(
    git_dir: &Path,
    source: &SourceName,
    root: &Path,
    follow_symlinks: bool,
    captured_at: std::time::SystemTime,
) -> Result<SnapshotId> {
    let root = root.canonicalize().map_err(|error| {
        SourceError::Source(format!("canonicalize worktree for {source}: {error}"))
    })?;
    std::fs::create_dir_all(git_dir)
        .map_err(|e| SourceError::Source(format!("create cache dir for {source}: {e}")))?;
    let cache_rel = contained_cache_rel(&root, git_dir);
    let head = read_local_head(&root.to_string_lossy())
        .map_err(|e| SourceError::Source(format!("read head of {source}: {e}")))?;
    let url = root.to_string_lossy().into_owned();
    let _lock = lock_mirror(git_dir, source, &url)?;
    let mirror = mirror_path(git_dir, &url);
    let index = CaptureIndex::load(&mirror);
    let entries = capture_stamped(&root, cache_rel.as_deref(), follow_symlinks, &index)?;
    let (capture_digest, oids) = import_captured(git_dir, &url, &entries)?;
    let stamped = entries
        .iter()
        .zip(oids)
        .map(|(entry, oid)| (entry.path.as_path(), entry.stamp, oid));
    CaptureIndex::store(&mirror, captured_at, stamped)
        .map_err(|e| SourceError::Source(format!("store capture index for {source}: {e}")))?;
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

pub(super) struct CapturedEntry {
    pub(super) path: PathBuf,
    pub(super) absolute: PathBuf,
    pub(super) kind: EntryKind,
    pub(super) stamp: FileStamp,
    pub(super) cached: Option<gix::ObjectId>,
}

fn capture_stamped(
    root: &Path,
    cache_rel: Option<&Path>,
    follow_symlinks: bool,
    index: &CaptureIndex,
) -> Result<Vec<CapturedEntry>> {
    let mut entries = Vec::new();
    let skip = |rel: &Path| rel.file_name() == Some(".git".as_ref()) || Some(rel) == cache_rel;
    walk_worktree(root, follow_symlinks, &skip, &mut |rel, path, meta| {
        if !meta.is_file() {
            return Ok(());
        }
        let stamp = FileStamp::of(meta);
        let cached = index.lookup(&rel, stamp);
        entries.push(CapturedEntry {
            path: rel,
            absolute: path.to_path_buf(),
            kind: entry_kind(meta),
            stamp,
            cached,
        });
        Ok(())
    })?;
    Ok(entries)
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
        if !self.follow_symlinks {
            return self.dir(dir, rel);
        }
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

pub(super) fn capture_entries(
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
        entries.push(ExtractedEntry {
            path: rel,
            kind: entry_kind(meta),
            data,
        });
        Ok(())
    })?;
    Ok(entries)
}

fn entry_kind(meta: &std::fs::Metadata) -> EntryKind {
    if is_executable(meta) {
        EntryKind::BlobExecutable
    } else {
        EntryKind::Blob
    }
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

mod capture_index {
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    const HEADER: &str = "phora-capture-index 1";
    const FILE_NAME: &str = "phora-capture-index";
    const RACY_MARGIN: Duration = Duration::from_secs(2);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(in crate::source) struct FileStamp {
        size: u64,
        mtime: (i64, i64),
        ctime: (i64, i64),
        ino: u64,
        mode: u32,
    }

    impl FileStamp {
        #[cfg(unix)]
        pub(in crate::source) fn of(meta: &std::fs::Metadata) -> Self {
            use std::os::unix::fs::MetadataExt as _;
            Self {
                size: meta.size(),
                mtime: (meta.mtime(), meta.mtime_nsec()),
                ctime: (meta.ctime(), meta.ctime_nsec()),
                ino: meta.ino(),
                mode: meta.mode(),
            }
        }

        #[cfg(not(unix))]
        pub(in crate::source) fn of(meta: &std::fs::Metadata) -> Self {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or((0, 0), |since| {
                    (
                        i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
                        i64::from(since.subsec_nanos()),
                    )
                });
            Self {
                size: meta.len(),
                mtime: modified,
                ctime: modified,
                ino: 0,
                mode: u32::from(meta.permissions().readonly()),
            }
        }

        fn settled_before(&self, cutoff: SystemTime) -> bool {
            let Ok(cutoff) = cutoff.duration_since(SystemTime::UNIX_EPOCH) else {
                return false;
            };
            let cutoff = i64::try_from(cutoff.as_secs()).unwrap_or(i64::MAX);
            self.mtime.0 < cutoff && self.ctime.0 < cutoff
        }

        fn encode(&self) -> String {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                self.size,
                self.mtime.0,
                self.mtime.1,
                self.ctime.0,
                self.ctime.1,
                self.ino,
                self.mode
            )
        }

        fn decode(fields: &[&str]) -> Option<Self> {
            let [
                size,
                modified,
                modified_nsec,
                changed,
                changed_nsec,
                ino,
                mode,
            ] = fields
            else {
                return None;
            };
            Some(Self {
                size: size.parse().ok()?,
                mtime: (modified.parse().ok()?, modified_nsec.parse().ok()?),
                ctime: (changed.parse().ok()?, changed_nsec.parse().ok()?),
                ino: ino.parse().ok()?,
                mode: mode.parse().ok()?,
            })
        }
    }

    #[derive(Debug, Default)]
    pub(in crate::source) struct CaptureIndex {
        entries: HashMap<PathBuf, (FileStamp, gix::ObjectId)>,
    }

    impl CaptureIndex {
        pub(in crate::source) fn load(mirror: &Path) -> Self {
            let Ok(text) = std::fs::read_to_string(mirror.join(FILE_NAME)) else {
                return Self::default();
            };
            let mut lines = text.lines();
            if lines.next() != Some(HEADER) {
                return Self::default();
            }
            let entries = lines
                .filter_map(|line| {
                    let fields: Vec<&str> = line.split('\t').collect();
                    let (oid, rest) = fields.split_first()?;
                    let (path, stamp) = rest.split_last()?;
                    let oid = gix::ObjectId::from_hex(oid.as_bytes()).ok()?;
                    Some((PathBuf::from(path), (FileStamp::decode(stamp)?, oid)))
                })
                .collect();
            Self { entries }
        }

        pub(in crate::source) fn lookup(
            &self,
            path: &Path,
            stamp: FileStamp,
        ) -> Option<gix::ObjectId> {
            self.entries
                .get(path)
                .and_then(|(recorded, oid)| (*recorded == stamp).then_some(*oid))
        }

        pub(in crate::source) fn store<'a>(
            mirror: &Path,
            captured_at: SystemTime,
            entries: impl Iterator<Item = (&'a Path, FileStamp, gix::ObjectId)>,
        ) -> std::io::Result<()> {
            let cutoff = captured_at.checked_sub(RACY_MARGIN).unwrap_or(captured_at);
            let mut text = format!("{HEADER}\n");
            for (path, stamp, oid) in entries {
                let Some(path) = path.to_str() else {
                    continue;
                };
                if path.contains(['\t', '\n', '\r']) || !stamp.settled_before(cutoff) {
                    continue;
                }
                let _ = writeln!(text, "{oid}\t{}\t{path}", stamp.encode());
            }
            let temp = mirror.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(text.as_bytes())?;
            drop(file);
            std::fs::rename(&temp, mirror.join(FILE_NAME))
        }
    }
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

    fn backdate(path: &Path) -> std::time::SystemTime {
        let mtime = std::time::SystemTime::now() - std::time::Duration::from_mins(1);
        set_mtime(path, mtime);
        mtime
    }

    fn set_mtime(path: &Path, mtime: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(mtime)
            .expect("set mtime");
    }

    fn later() -> std::time::SystemTime {
        std::time::SystemTime::now() + std::time::Duration::from_mins(1)
    }

    fn capture_digest(cache: &Path, root: &Path, captured_at: std::time::SystemTime) -> Commit {
        match capture_worktree_at(cache, &SourceName::trusted("wt"), root, false, captured_at)
            .expect("capture")
        {
            SnapshotId::Worktree { capture_digest, .. } => capture_digest,
            other @ SnapshotId::Git { .. } => panic!("worktree snapshot expected, got {other:?}"),
        }
    }

    fn mirror_of(cache: &Path, root: &Path) -> PathBuf {
        let url = root.canonicalize().expect("canonical root");
        mirror_path(cache, &url.to_string_lossy())
    }

    fn indexed_paths(cache: &Path, root: &Path) -> Vec<String> {
        let index = mirror_of(cache, root).join("phora-capture-index");
        let mut paths: Vec<String> = std::fs::read_to_string(index)
            .expect("capture index written")
            .lines()
            .skip(1)
            .filter_map(|line| line.rsplit('\t').next().map(str::to_owned))
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn recapture_of_an_unchanged_tree_reuses_indexed_blobs_and_keeps_its_digest() {
        let src = tempfile::tempdir().expect("src");
        let cache = tempfile::tempdir().expect("cache");
        std::fs::create_dir_all(src.path().join("skills")).expect("mkdir");
        std::fs::write(src.path().join("skills/a.md"), "alpha\n").expect("write");
        std::fs::write(src.path().join("b.md"), "beta\n").expect("write");

        let first = capture_digest(cache.path(), src.path(), later());
        assert_eq!(
            indexed_paths(cache.path(), src.path()),
            vec!["b.md", "skills/a.md"]
        );
        assert_eq!(capture_digest(cache.path(), src.path(), later()), first);
    }

    #[test]
    fn files_touched_within_the_racy_margin_are_not_indexed() {
        let src = tempfile::tempdir().expect("src");
        let cache = tempfile::tempdir().expect("cache");
        std::fs::write(src.path().join("a.md"), "alpha\n").expect("write");

        capture_digest(cache.path(), src.path(), std::time::SystemTime::now());
        assert!(indexed_paths(cache.path(), src.path()).is_empty());
    }

    #[test]
    fn an_edit_that_keeps_size_and_restores_mtime_is_still_recaptured() {
        let src = tempfile::tempdir().expect("src");
        let cache = tempfile::tempdir().expect("cache");
        let file = src.path().join("a.md");
        std::fs::write(&file, "alpha\n").expect("write");
        let mtime = backdate(&file);
        let first = capture_digest(cache.path(), src.path(), later());
        assert_eq!(indexed_paths(cache.path(), src.path()), vec!["a.md"]);

        std::fs::write(&file, "omega\n").expect("rewrite same size");
        set_mtime(&file, mtime);
        let second = capture_digest(cache.path(), src.path(), later());
        assert_ne!(
            second, first,
            "a content edit must change the capture digest"
        );
    }

    #[test]
    fn an_indexed_blob_missing_from_the_mirror_is_read_again() {
        let src = tempfile::tempdir().expect("src");
        let cache = tempfile::tempdir().expect("cache");
        std::fs::write(src.path().join("a.md"), "alpha\n").expect("write");
        let first = capture_digest(cache.path(), src.path(), later());

        let objects = mirror_of(cache.path(), src.path()).join("objects");
        std::fs::remove_dir_all(&objects).expect("drop objects");
        std::fs::create_dir_all(objects.join("pack")).expect("objects/pack");
        std::fs::create_dir_all(objects.join("info")).expect("objects/info");
        assert_eq!(indexed_paths(cache.path(), src.path()), vec!["a.md"]);

        assert_eq!(capture_digest(cache.path(), src.path(), later()), first);
        let repo = gix::open(mirror_of(cache.path(), src.path())).expect("mirror");
        let blob = gix::ObjectId::from_hex(b"4a58007052a65fbc2fc3f910f2855f45a4058e74")
            .expect("alpha blob id");
        assert!(repo.has_object(blob), "the missing blob is written again");
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
