use std::path::{Path, PathBuf};

use crate::kernel::SourceName;

use super::{MIRROR_REFSPECS, MirrorKey, NormalizedUrl, Result, SourceError};

/// `<MirrorKey>.git` under `git_dir`; the single source of mirror-directory layout.
pub(crate) fn mirror_path(git_dir: &Path, url: &str) -> PathBuf {
    let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
    git_dir.join(format!("{}.git", key.as_str()))
}

fn mirror_lock_path(git_dir: &Path, url: &str) -> PathBuf {
    let mut s = mirror_path(git_dir, url).into_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

pub(super) fn lock_mirror(git_dir: &Path, source: &SourceName, url: &str) -> Result<std::fs::File> {
    std::fs::create_dir_all(git_dir)
        .map_err(|e| SourceError::Source(format!("create git dir for lock {source}: {e}")))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(mirror_lock_path(git_dir, url))
        .map_err(|e| SourceError::Source(format!("open mirror lock {source}: {e}")))?;
    lock.lock()
        .map_err(|e| SourceError::Source(format!("lock mirror {source}: {e}")))?;
    Ok(lock)
}

/// Idle window above any real clone; a staging dir older than this lost its
/// owning process before `Drop` could clean it up.
const STAGING_ORPHAN_GRACE: std::time::Duration = std::time::Duration::from_hours(1);

/// Removes staging dirs abandoned by a killed clone of `url`'s mirror.
///
/// Caller must hold the per-mirror lock: it excludes a concurrent full fetch of
/// this key, and [`STAGING_ORPHAN_GRACE`] excludes the lock-free shallow clone.
/// Errors are ignored; a sweep must never fail the fetch it precedes.
pub(super) fn sweep_orphan_staging(git_dir: &Path, url: &str) {
    let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
    let prefix = format!(".{}.staging-", key.as_str());
    let Ok(entries) = std::fs::read_dir(git_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(&prefix) || staging_is_recent(&entry.path()) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// A clone writes its pack into `objects/pack/` subdirs, so the staging root's own
/// mtime freezes early; liveness must consider the newest mtime in the whole tree.
fn staging_is_recent(path: &Path) -> bool {
    let mut saw_mtime = false;
    for entry in walkdir::WalkDir::new(path) {
        let Ok(entry) = entry else { return true };
        let Ok(meta) = entry.metadata() else {
            return true;
        };
        let Ok(modified) = meta.modified() else {
            return true;
        };
        saw_mtime = true;
        if modified
            .elapsed()
            .map_or(true, |age| age < STAGING_ORPHAN_GRACE)
        {
            return true;
        }
    }
    !saw_mtime
}

/// Opens the canonical mirror, distinguishing "re-clone" from "leave untouched".
///
/// `Ok(None)` means absent or genuinely not a repository — safe to re-clone. Any
/// other open failure (I/O, permissions, unreadable config) may be transient, so it
/// propagates rather than destroying a possibly-healthy cache.
pub(super) fn open_mirror(source: &SourceName, mirror: &Path) -> Result<Option<gix::Repository>> {
    if !mirror.exists() {
        return Ok(None);
    }
    match gix::open(mirror) {
        Ok(repo) => Ok(Some(repo)),
        Err(gix::open::Error::NotARepository { .. }) => Ok(None),
        Err(e) => Err(SourceError::Source(format!("open mirror {source}: {e}"))),
    }
}

pub(super) fn fetch_into_mirror(source: &SourceName, repo: &gix::Repository) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| SourceError::Source(format!("find origin in {source}: {e}")))?;
    remote
        .replace_refspecs(
            MIRROR_REFSPECS.iter().copied(),
            gix::remote::Direction::Fetch,
        )
        .map_err(|e| SourceError::Source(format!("set mirror refspec in {source}: {e}")))?;
    remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| SourceError::Source(format!("connect origin in {source}: {e}")))?
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|e| SourceError::Source(format!("prepare fetch in {source}: {e}")))?
        .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| SourceError::Source(format!("receive pack in {source}: {e}")))?;
    Ok(())
}

/// Clones a fresh mirror into staging, then replaces `mirror`. The canonical mirror
/// is removed only after the clone succeeds, so a failed clone (unreachable remote)
/// leaves an existing mirror intact rather than trading it for nothing.
pub(super) fn reclone_mirror(
    git_dir: &Path,
    source: &SourceName,
    url: &str,
    mirror: &Path,
) -> Result<()> {
    let staging = MirrorStaging::create(git_dir, url);
    gix::prepare_clone_bare(url, &staging.path)
        .map_err(|e| SourceError::Source(format!("prepare clone {source}: {e}")))?
        .configure_remote(|mut remote| {
            remote.replace_refspecs(
                MIRROR_REFSPECS.iter().copied(),
                gix::remote::Direction::Fetch,
            )?;
            Ok(remote)
        })
        .fetch_only(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| SourceError::Source(format!("clone bare {source}: {e}")))?;
    if mirror.exists() {
        std::fs::remove_dir_all(mirror)
            .map_err(|e| SourceError::Source(format!("remove corrupt mirror {source}: {e}")))?;
    }
    staging.commit_to(mirror, source.as_str())
}

/// A scratch mirror renamed into the canonical path on success, removed on drop otherwise.
pub(super) struct MirrorStaging {
    pub(super) path: PathBuf,
    armed: bool,
}

impl MirrorStaging {
    pub(super) fn create(git_dir: &Path, url: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
        let name = format!(".{}.staging-{}-{nonce}", key.as_str(), std::process::id());
        Self {
            path: git_dir.join(name),
            armed: true,
        }
    }

    pub(super) fn commit_to(mut self, mirror: &Path, label: &str) -> Result<()> {
        std::fs::rename(&self.path, mirror)
            .map_err(|e| SourceError::Source(format!("publish mirror {label}: {e}")))?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for MirrorStaging {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
