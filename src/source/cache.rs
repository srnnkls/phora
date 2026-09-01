use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::SourceName;

use super::{MIRROR_REFSPECS, MirrorKey, NormalizedUrl, Result, SourceError, WorktreeAdminId};

/// `<MirrorKey>.git` under `git_dir`; the single source of mirror-directory layout.
pub(crate) fn mirror_path(git_dir: &Path, url: &str) -> PathBuf {
    let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
    mirror_path_for_key(git_dir, &key)
}

pub(super) fn mirror_path_for_key(git_dir: &Path, key: &MirrorKey) -> PathBuf {
    git_dir.join(format!("{}.git", key.as_str()))
}

pub(super) fn mirror_lock_path(git_dir: &Path, url: &str) -> PathBuf {
    let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
    mirror_lock_path_for_key(git_dir, &key)
}

pub(super) fn mirror_lock_path_for_key(git_dir: &Path, key: &MirrorKey) -> PathBuf {
    let mut path = mirror_path_for_key(git_dir, key).into_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

pub(super) fn lock_mirror_for_key(
    git_dir: &Path,
    source: &SourceName,
    key: &MirrorKey,
) -> Result<std::fs::File> {
    lock_mirror_at(git_dir, source, mirror_lock_path_for_key(git_dir, key))
}

pub(super) fn lock_mirror(git_dir: &Path, source: &SourceName, url: &str) -> Result<std::fs::File> {
    lock_mirror_at(git_dir, source, mirror_lock_path(git_dir, url))
}

fn lock_mirror_at(git_dir: &Path, source: &SourceName, path: PathBuf) -> Result<std::fs::File> {
    std::fs::create_dir_all(git_dir)
        .map_err(|e| SourceError::Source(format!("create git dir for lock {source}: {e}")))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
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
    detach_managed_worktree_heads(source, repo)?;
    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| SourceError::Source(format!("find origin in {source}: {e}")))?;
    remote
        .replace_refspecs(
            MIRROR_REFSPECS.iter().copied(),
            gix::remote::Direction::Fetch,
        )
        .map_err(|e| SourceError::Source(format!("set mirror refspec in {source}: {e}")))?;
    let outcome = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| SourceError::Source(format!("connect origin in {source}: {e}")))?
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|e| SourceError::Source(format!("prepare fetch in {source}: {e}")))?
        .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| SourceError::Source(format!("receive pack in {source}: {e}")))?;
    let updates = match outcome.status {
        gix::remote::fetch::Status::NoPackReceived { update_refs, .. }
        | gix::remote::fetch::Status::Change { update_refs, .. } => update_refs,
    };
    if let Some(rejected) = updates.updates.iter().find(|update| {
        matches!(
            update.mode,
            gix::remote::fetch::refs::update::Mode::RejectedSourceObjectNotFound { .. }
                | gix::remote::fetch::refs::update::Mode::RejectedTagUpdate
                | gix::remote::fetch::refs::update::Mode::RejectedNonFastForward
                | gix::remote::fetch::refs::update::Mode::RejectedToReplaceWithUnborn
                | gix::remote::fetch::refs::update::Mode::RejectedCurrentlyCheckedOut { .. }
        )
    }) {
        return Err(SourceError::Source(format!(
            "fetch rejected ref update in {source}: {}",
            rejected.mode
        )));
    }
    Ok(())
}

fn detach_managed_worktree_heads(source: &SourceName, repo: &gix::Repository) -> Result<()> {
    let worktrees = repo.path().join("worktrees");
    let entries = match std::fs::read_dir(&worktrees) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SourceError::Source(format!(
                "read worktree administration for {source}: {error}"
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            SourceError::Source(format!(
                "read worktree administration for {source}: {error}"
            ))
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = name.strip_prefix("ph-") else {
            continue;
        };
        if id.parse::<WorktreeAdminId>().is_err() || !entry.path().is_dir() {
            continue;
        }
        let head = entry.path().join("HEAD");
        let contents = match std::fs::read_to_string(&head) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "read managed worktree HEAD for {source}: {error}"
                )));
            }
        };
        let Some(reference) = contents.trim().strip_prefix("ref: ") else {
            continue;
        };
        let oid = repo
            .find_reference(reference)
            .map_err(|error| {
                SourceError::Source(format!(
                    "resolve managed worktree HEAD for {source}: {error}"
                ))
            })?
            .peel_to_id()
            .map_err(|error| {
                SourceError::Source(format!("peel managed worktree HEAD for {source}: {error}"))
            })?;
        std::fs::write(&head, format!("{}\n", oid.detach())).map_err(|error| {
            SourceError::Source(format!("detach managed worktree HEAD: {error}"))
        })?;
    }
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
    let (repo, _) = gix::prepare_clone_bare(url, &staging.path)
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
    carry_managed_worktrees(mirror, &staging.path, &repo, source)?;
    if mirror.exists() {
        std::fs::remove_dir_all(mirror)
            .map_err(|e| SourceError::Source(format!("remove corrupt mirror {source}: {e}")))?;
    }
    staging.commit_to(mirror, source.as_str())
}

fn carry_managed_worktrees(
    mirror: &Path,
    staging: &Path,
    repo: &gix::Repository,
    source: &SourceName,
) -> Result<()> {
    let Some(previous) = open_mirror(source, mirror)? else {
        return Ok(());
    };
    let references = previous.references().map_err(|error| {
        SourceError::Source(format!("read carried worktree pins for {source}: {error}"))
    })?;
    let pins = references
        .prefixed("refs/phora/worktrees/")
        .map_err(|error| {
            SourceError::Source(format!("read carried worktree pins for {source}: {error}"))
        })?
        .peeled()
        .map_err(|error| {
            SourceError::Source(format!("read carried worktree pins for {source}: {error}"))
        })?;
    for pin in pins {
        let mut pin = pin.map_err(|error| {
            SourceError::Source(format!("read carried worktree pin for {source}: {error}"))
        })?;
        let name = pin.name().to_string();
        let Some(id) = name.strip_prefix("refs/phora/worktrees/") else {
            continue;
        };
        if id.parse::<WorktreeAdminId>().is_err() {
            continue;
        }
        let oid = pin.peel_to_id().map_err(|error| {
            SourceError::Source(format!("peel carried worktree pin for {source}: {error}"))
        })?;
        let oid = oid.detach();
        if repo.find_commit(oid).is_err() {
            continue;
        }
        let admin = mirror.join("worktrees").join(format!("ph-{id}"));
        if !admin_metadata_is_dir(&admin, source)? {
            continue;
        }
        copy_overlay_tree(
            &admin,
            &staging.join("worktrees").join(format!("ph-{id}")),
            source,
        )?;
        let pin_path = staging.join("refs/phora/worktrees").join(id);
        let parent = pin_path.parent().ok_or_else(|| {
            SourceError::Source(format!(
                "carried worktree pin has no parent: {}",
                pin_path.display()
            ))
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            SourceError::Source(format!(
                "create carried worktree pins for {source}: {error}"
            ))
        })?;
        std::fs::write(pin_path, format!("{oid}\n")).map_err(|error| {
            SourceError::Source(format!("write carried worktree pin for {source}: {error}"))
        })?;
    }
    Ok(())
}

fn admin_metadata_is_dir(path: &Path, source: &SourceName) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SourceError::Source(format!(
            "stat carried worktree administration for {source}: {error}"
        ))),
    }
}

fn copy_overlay_tree(source: &Path, destination: &Path, label: &SourceName) -> Result<()> {
    std::fs::create_dir_all(destination).map_err(|error| {
        SourceError::Source(format!("create carried mirror state for {label}: {error}"))
    })?;
    for entry in std::fs::read_dir(source).map_err(|error| {
        SourceError::Source(format!("read carried mirror state for {label}: {error}"))
    })? {
        let entry = entry.map_err(|error| {
            SourceError::Source(format!(
                "read carried mirror state entry for {label}: {error}"
            ))
        })?;
        let destination = destination.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|error| {
                SourceError::Source(format!("stat carried mirror state for {label}: {error}"))
            })?
            .is_dir()
        {
            copy_overlay_tree(&entry.path(), &destination, label)?;
        } else {
            std::fs::copy(entry.path(), destination).map_err(|error| {
                SourceError::Source(format!("copy carried mirror state for {label}: {error}"))
            })?;
        }
    }
    Ok(())
}

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
