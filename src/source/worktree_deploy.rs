use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::{Commit, MirrorKey, Result, SourceError, SourceName};

/// Stable, path-safe identity for a managed linked worktree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorktreeAdminId(String);

impl WorktreeAdminId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for WorktreeAdminId {
    type Err = SourceError;

    fn from_str(value: &str) -> Result<Self> {
        let valid = value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(SourceError::Source(format!(
                "invalid worktree administration id `{value}`: expected 16 lowercase hex chars"
            )))
        }
    }
}

impl fmt::Display for WorktreeAdminId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMirrorAddress {
    pub cache_git_root: PathBuf,
    pub key: MirrorKey,
}

/// Derive a persisted worktree name without placing untrusted identity text in a path or ref.
pub fn worktree_admin_id(
    project_root: &Path,
    deploy_root: &Path,
    target_name: &str,
    identity: &str,
) -> Result<WorktreeAdminId> {
    let project_root = project_root.canonicalize().map_err(|error| {
        SourceError::Source(format!(
            "canonicalize project root {}: {error}",
            project_root.display()
        ))
    })?;
    if !deploy_root.is_absolute() {
        return Err(SourceError::Source(format!(
            "deployment root must be an expanded absolute path: {}",
            deploy_root.display()
        )));
    }

    let mut hasher = blake3::Hasher::new();
    for value in [
        project_root.as_os_str().as_encoded_bytes(),
        deploy_root.as_os_str().as_encoded_bytes(),
        target_name.as_bytes(),
        identity.as_bytes(),
    ] {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    WorktreeAdminId::from_str(&hasher.finalize().to_hex()[..16])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDeployRequest {
    pub admin_id: WorktreeAdminId,
    pub deploy_root: PathBuf,
    pub commit: Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDeployment {
    pub admin_id: WorktreeAdminId,
    pub deploy_root: PathBuf,
    pub admin_dir: PathBuf,
    pub commit: Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemoveRequest {
    pub admin_id: WorktreeAdminId,
    pub deploy_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeObservationLock {
    Try,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeObservationLevel {
    Cheap,
    Semantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeObservationResult {
    Conformant,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeObservationRequest {
    pub source: SourceName,
    pub address: WorktreeMirrorAddress,
    pub admin_id: WorktreeAdminId,
    pub deploy_root: PathBuf,
    pub lock: WorktreeObservationLock,
    pub level: WorktreeObservationLevel,
}

/// Holds the per-mirror exclusive lock while mutating linked-worktree administration.
pub struct WorktreeMirrorGuard {
    pub(crate) address: WorktreeMirrorAddress,
    pub(crate) mirror: gix::Repository,
    pub(crate) _lock: std::fs::File,
}

impl WorktreeMirrorGuard {
    pub(crate) fn new(
        address: WorktreeMirrorAddress,
        mirror: gix::Repository,
        lock: std::fs::File,
    ) -> Self {
        Self {
            address,
            mirror,
            _lock: lock,
        }
    }

    pub fn publish_worktree(&self, request: &WorktreeDeployRequest) -> Result<WorktreeDeployment> {
        Self::sweep_gitlink_staging(&request.deploy_root)?;
        self.publish_worktree_after_sweep(request).map_err(
            |error| match Self::sweep_gitlink_staging(&request.deploy_root) {
                Ok(()) => error,
                Err(cleanup) => SourceError::Source(format!(
                    "{error}; clean staged worktree gitlinks: {cleanup}"
                )),
            },
        )
    }

    fn publish_worktree_after_sweep(
        &self,
        request: &WorktreeDeployRequest,
    ) -> Result<WorktreeDeployment> {
        let mirror =
            super::cache::mirror_path_for_key(&self.address.cache_git_root, &self.address.key);
        let worktrees = mirror.join("worktrees");
        std::fs::create_dir_all(&worktrees).map_err(|error| {
            SourceError::Source(format!("create worktree administration root: {error}"))
        })?;
        std::fs::create_dir_all(&request.deploy_root).map_err(|error| {
            SourceError::Source(format!("create worktree deployment root: {error}"))
        })?;

        let admin_name = format!("ph-{}", request.admin_id.as_str());
        let admin_dir = worktrees.join(&admin_name);
        let staging = create_transient_dir(&mirror, &admin_name, "staging")?;
        self.create_gitlink_directories(&request.deploy_root, &request.commit)?;
        self.write_admin(&staging, &admin_dir, &request.deploy_root, &request.commit)?;
        let gitlink = create_gitlink_staging(&request.deploy_root, &admin_dir)?;

        let backup = if path_exists(&admin_dir, "inspect worktree administration")? {
            let backup = create_transient_dir(&mirror, &admin_name, "backup")?;
            std::fs::rename(&admin_dir, &backup).map_err(|error| {
                SourceError::Source(format!("back up prior worktree administration: {error}"))
            })?;
            Some(backup)
        } else {
            None
        };
        if let Err(error) = std::fs::rename(&staging, &admin_dir) {
            return Err(restore_admin(
                &admin_dir,
                backup.as_deref(),
                "publish worktree administration",
                &error,
            ));
        }
        Self::write_pin(&mirror, request)?;
        if let Err(error) = std::fs::rename(&gitlink, request.deploy_root.join(".git")) {
            return Err(SourceError::Source(format!(
                "publish worktree gitlink: {error}"
            )));
        }
        if let Some(backup) = backup {
            std::fs::remove_dir_all(backup).map_err(|error| {
                SourceError::Source(format!("remove prior worktree administration: {error}"))
            })?;
        }

        Ok(WorktreeDeployment {
            admin_id: request.admin_id.clone(),
            deploy_root: request.deploy_root.clone(),
            admin_dir,
            commit: request.commit.clone(),
        })
    }

    fn write_admin(
        &self,
        staging: &Path,
        admin_dir: &Path,
        deploy_root: &Path,
        commit: &Commit,
    ) -> Result<()> {
        self.write_index(staging, admin_dir, deploy_root, commit)?;
        let gitlink = deploy_root.join(".git");
        for (path, contents) in [
            (staging.join("HEAD"), format!("{}\n", commit.as_str())),
            (staging.join("commondir"), "../..\n".to_owned()),
            (staging.join("gitdir"), format!("{}\n", gitlink.display())),
        ] {
            std::fs::write(&path, contents).map_err(|error| {
                SourceError::Source(format!(
                    "write worktree administration {}: {error}",
                    path.display()
                ))
            })?;
        }
        Ok(())
    }

    fn write_pin(mirror: &Path, request: &WorktreeDeployRequest) -> Result<()> {
        let pin = mirror
            .join("refs/phora/worktrees")
            .join(request.admin_id.as_str());
        let parent = pin.parent().ok_or_else(|| {
            SourceError::Source(format!("worktree pin ref has no parent: {}", pin.display()))
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| SourceError::Source(format!("create worktree pin refs: {error}")))?;
        std::fs::write(&pin, format!("{}\n", request.commit.as_str()))
            .map_err(|error| SourceError::Source(format!("write worktree pin ref: {error}")))
    }

    fn write_index(
        &self,
        staging: &Path,
        admin_dir: &Path,
        deploy_root: &Path,
        commit: &Commit,
    ) -> Result<()> {
        let oid = gix::ObjectId::from_hex(commit.as_str().as_bytes()).map_err(|error| {
            SourceError::Source(format!("parse worktree commit {commit}: {error}"))
        })?;
        let commit = self.mirror.find_commit(oid).map_err(|error| {
            SourceError::Source(format!("find worktree commit {commit}: {error}"))
        })?;
        let tree = commit
            .tree_id()
            .map_err(|error| SourceError::Source(format!("read worktree commit tree: {error}")))?;
        let mut index = self.mirror.index_from_tree(&tree).map_err(|error| {
            SourceError::Source(format!("build worktree index from tree: {error}"))
        })?;
        for (entry, path) in index.entries_mut_with_paths() {
            let path = std::str::from_utf8(path.as_ref()).map_err(|error| {
                SourceError::Source(format!("read worktree index path: {error}"))
            })?;
            let metadata = gix::index::fs::Metadata::from_path_no_follow(&deploy_root.join(path))
                .map_err(|error| {
                SourceError::Source(format!("stat worktree deployment path {path}: {error}"))
            })?;
            entry.stat = gix::index::entry::Stat::from_fs(&metadata).map_err(|error| {
                SourceError::Source(format!("record worktree index stat {path}: {error}"))
            })?;
        }
        index.set_path(admin_dir.join("index"));
        let file = std::fs::File::create(staging.join("index"))
            .map_err(|error| SourceError::Source(format!("create worktree index: {error}")))?;
        index
            .write_to(file, gix::index::write::Options::default())
            .map_err(|error| SourceError::Source(format!("write worktree index: {error}")))?;
        Ok(())
    }

    fn create_gitlink_directories(&self, deploy_root: &Path, commit: &Commit) -> Result<()> {
        let oid = gix::ObjectId::from_hex(commit.as_str().as_bytes()).map_err(|error| {
            SourceError::Source(format!("parse worktree commit {commit}: {error}"))
        })?;
        let tree = self
            .mirror
            .find_commit(oid)
            .map_err(|error| {
                SourceError::Source(format!("find worktree commit {commit}: {error}"))
            })?
            .tree()
            .map_err(|error| SourceError::Source(format!("read worktree commit tree: {error}")))?;
        self.create_gitlink_directories_in_tree(deploy_root, &tree)
    }

    fn create_gitlink_directories_in_tree(
        &self,
        deploy_root: &Path,
        tree: &gix::Tree<'_>,
    ) -> Result<()> {
        for entry in tree.iter() {
            let entry = entry.map_err(|error| {
                SourceError::Source(format!("read worktree tree entry: {error}"))
            })?;
            let path = deploy_root.join(entry.filename().to_string());
            match entry.kind() {
                gix::object::tree::EntryKind::Commit => {
                    std::fs::create_dir_all(path).map_err(|error| {
                        SourceError::Source(format!("create gitlink deployment directory: {error}"))
                    })?;
                }
                gix::object::tree::EntryKind::Tree => {
                    let child = self.mirror.find_tree(entry.object_id()).map_err(|error| {
                        SourceError::Source(format!("read worktree subtree: {error}"))
                    })?;
                    self.create_gitlink_directories_in_tree(&path, &child)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn remove_worktree(&self, request: &WorktreeRemoveRequest) -> Result<()> {
        let mirror =
            super::cache::mirror_path_for_key(&self.address.cache_git_root, &self.address.key);
        let admin_dir = mirror
            .join("worktrees")
            .join(format!("ph-{}", request.admin_id.as_str()));
        let gitlink = request.deploy_root.join(".git");
        if gitlink_targets(&gitlink, &admin_dir)? {
            remove_file_if_exists(&gitlink, "remove worktree gitlink")?;
        }
        remove_dir_if_exists(&admin_dir, "remove worktree administration")?;
        let pin = mirror
            .join("refs/phora/worktrees")
            .join(request.admin_id.as_str());
        remove_file_if_exists(&pin, "remove worktree pin ref")?;
        Ok(())
    }

    pub fn sweep_worktrees(&self) -> Result<()> {
        let mirror =
            super::cache::mirror_path_for_key(&self.address.cache_git_root, &self.address.key);
        let entries = match std::fs::read_dir(&mirror) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "read transient worktree administration: {error}"
                )));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                SourceError::Source(format!(
                    "read transient worktree administration entry: {error}"
                ))
            })?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some((admin_name, kind)) = transient_name(&name) else {
                continue;
            };
            let final_admin = mirror.join("worktrees").join(admin_name);
            if kind == "backup" && !path_exists(&final_admin, "inspect worktree administration")? {
                std::fs::rename(entry.path(), final_admin).map_err(|error| {
                    SourceError::Source(format!("restore worktree administration backup: {error}"))
                })?;
            } else {
                std::fs::remove_dir_all(entry.path()).map_err(|error| {
                    SourceError::Source(format!("sweep transient worktree administration: {error}"))
                })?;
            }
        }
        Ok(())
    }

    fn sweep_gitlink_staging(deploy_root: &Path) -> Result<()> {
        let entries = match std::fs::read_dir(deploy_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "read staged worktree gitlinks: {error}"
                )));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                SourceError::Source(format!("read staged worktree gitlink: {error}"))
            })?;
            if staged_gitlink_name(&entry.file_name().to_string_lossy()) {
                remove_file_if_exists(&entry.path(), "remove staged worktree gitlink")?;
            }
        }
        Ok(())
    }
}

fn create_transient_dir(mirror: &Path, admin_name: &str, kind: &str) -> Result<PathBuf> {
    for _ in 0..256 {
        let path = transient_path(mirror, admin_name, kind);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "create transient worktree administration: {error}"
                )));
            }
        }
    }
    Err(SourceError::Source(
        "allocate transient worktree administration name".to_owned(),
    ))
}

fn transient_path(mirror: &Path, admin_name: &str, kind: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    mirror.join(format!(
        ".{admin_name}.{kind}-{}-{nonce}",
        std::process::id()
    ))
}

fn create_gitlink_staging(deploy_root: &Path, admin_dir: &Path) -> Result<PathBuf> {
    for _ in 0..256 {
        let path = deploy_root.join(format!(
            ".git.phora-staging-{}",
            std::sync::atomic::AtomicU64::fetch_add(
                &GITLINK_COUNTER,
                1,
                std::sync::atomic::Ordering::Relaxed
            )
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(format!("gitdir: {}\n", admin_dir.display()).as_bytes())
                    .map_err(|error| {
                        SourceError::Source(format!("stage worktree gitlink: {error}"))
                    })?;
                return Ok(path);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "stage worktree gitlink: {error}"
                )));
            }
        }
    }
    Err(SourceError::Source(
        "allocate staged worktree gitlink name".to_owned(),
    ))
}

static GITLINK_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn path_exists(path: &Path, action: &str) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SourceError::Source(format!("{action}: {error}"))),
    }
}

fn remove_file_if_exists(path: &Path, action: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SourceError::Source(format!("{action}: {error}"))),
    }
}

fn remove_dir_if_exists(path: &Path, action: &str) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SourceError::Source(format!("{action}: {error}"))),
    }
}

fn staged_gitlink_name(name: &str) -> bool {
    name.strip_prefix(".git.phora-staging-")
        .is_some_and(|counter| counter.parse::<u64>().is_ok())
}

fn restore_admin(
    admin: &Path,
    backup: Option<&Path>,
    action: &str,
    error: &std::io::Error,
) -> SourceError {
    let restore = backup.map(|backup| std::fs::rename(backup, admin));
    match restore {
        Some(Err(restore_error)) => SourceError::Source(format!(
            "{action}: {error}; restore prior worktree administration: {restore_error}"
        )),
        _ => SourceError::Source(format!("{action}: {error}")),
    }
}

fn gitlink_targets(gitlink: &Path, admin: &Path) -> Result<bool> {
    let contents = match std::fs::read_to_string(gitlink) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(SourceError::Source(format!(
                "read worktree gitlink {}: {error}",
                gitlink.display()
            )));
        }
    };
    let Some(target) = contents.trim().strip_prefix("gitdir: ") else {
        return Ok(false);
    };
    let target = match std::fs::canonicalize(target) {
        Ok(target) => target,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(SourceError::Source(format!(
                "normalize worktree gitlink {}: {error}",
                gitlink.display()
            )));
        }
    };
    let admin = match std::fs::canonicalize(admin) {
        Ok(admin) => admin,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(SourceError::Source(format!(
                "normalize worktree administration {}: {error}",
                admin.display()
            )));
        }
    };
    Ok(target == admin)
}

fn transient_name(name: &str) -> Option<(&str, &str)> {
    let name = name.strip_prefix('.')?;
    let (admin, suffix) = name.split_once('.')?;
    let id = admin.strip_prefix("ph-")?;
    id.parse::<WorktreeAdminId>().ok()?;
    for kind in ["staging", "backup"] {
        if suffix.starts_with(&format!("{kind}-")) {
            return Some((admin, kind));
        }
    }
    None
}
