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
    pub expected_commit: Commit,
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
        self.publish_worktree_unchecked(request)
            .map_err(
                |error| match Self::sweep_gitlink_staging(&request.deploy_root) {
                    Ok(()) => error,
                    Err(cleanup) => SourceError::Source(format!(
                        "{error}; clean staged worktree gitlinks: {cleanup}"
                    )),
                },
            )
    }

    fn publish_worktree_unchecked(
        &self,
        request: &WorktreeDeployRequest,
    ) -> Result<WorktreeDeployment> {
        let mirror = verified_mirror_root(&self.address)?;
        let worktrees = mirror.join("worktrees");
        std::fs::create_dir_all(&worktrees).map_err(|error| {
            SourceError::Source(format!("create worktree administration root: {error}"))
        })?;
        std::fs::create_dir_all(&request.deploy_root).map_err(|error| {
            SourceError::Source(format!("create worktree deployment root: {error}"))
        })?;

        let admin_name = format!("ph-{}", request.admin_id.as_str());
        let admin_dir = worktrees.join(&admin_name);
        let physical_gitlink = canonical_gitlink_path(&request.deploy_root)?;
        let staging = create_transient_dir(&mirror, &admin_name, "staging")?;
        self.create_gitlink_directories(&request.deploy_root, &request.commit)?;
        self.write_admin(
            &staging,
            &admin_dir,
            &request.deploy_root,
            &physical_gitlink,
            &request.commit,
        )?;
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
        gitlink: &Path,
        commit: &Commit,
    ) -> Result<()> {
        self.write_index(staging, admin_dir, deploy_root, commit)?;
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
        let mirror = verified_mirror_root(&self.address)?;
        remove_worktree_gitlink(&self.address, &request.admin_id, &request.deploy_root)?;
        let admin_dir = mirror
            .join("worktrees")
            .join(format!("ph-{}", request.admin_id.as_str()));
        remove_dir_if_exists(&admin_dir, "remove worktree administration")?;
        let pin = mirror
            .join("refs/phora/worktrees")
            .join(request.admin_id.as_str());
        remove_file_if_exists(&pin, "remove worktree pin ref")?;
        Ok(())
    }

    pub fn sweep_worktrees(&self) -> Result<()> {
        let mirror = verified_mirror_root(&self.address)?;
        let worktrees = mirror.join("worktrees");
        sweep_stale_worktrees(&mirror, &worktrees)?;
        sweep_transient_worktrees(&mirror)
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

pub(crate) fn remove_missing_mirror_worktree_gitlink(
    address: &WorktreeMirrorAddress,
    admin_id: &WorktreeAdminId,
    deploy_root: &Path,
) -> Result<bool> {
    let Some(cache_root) = physical_cache_git_root(&address.cache_git_root)? else {
        remove_worktree_gitlink(address, admin_id, deploy_root)?;
        return Ok(true);
    };
    let mirror = super::cache::mirror_path_for_key(&cache_root, &address.key);
    match std::fs::symlink_metadata(&mirror) {
        Ok(_) => Ok(false),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            remove_worktree_gitlink(address, admin_id, deploy_root)?;
            Ok(true)
        }
        Err(error) => Err(SourceError::Source(format!(
            "inspect worktree mirror {}: {error}",
            mirror.display()
        ))),
    }
}

pub(crate) fn verified_cache_git_root(cache_root: &Path) -> Result<PathBuf> {
    physical_cache_git_root(cache_root)?.ok_or_else(|| {
        SourceError::Source(format!(
            "worktree cache root {} is absent",
            cache_root.display()
        ))
    })
}

pub(crate) fn physical_cache_git_root(cache_root: &Path) -> Result<Option<PathBuf>> {
    if !cache_root.is_absolute() {
        return Err(SourceError::Source(format!(
            "worktree cache root must be absolute: {}",
            cache_root.display()
        )));
    }
    if cache_root.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    }) {
        return Err(SourceError::Source(format!(
            "worktree cache root {} has a `.` or `..` component",
            cache_root.display()
        )));
    }
    let metadata = match std::fs::symlink_metadata(cache_root) {
        Ok(metadata) => metadata,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(error) => {
            return Err(SourceError::Source(format!(
                "inspect worktree cache root {}: {error}",
                cache_root.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SourceError::Source(format!(
            "worktree cache root {} is not a directory without symlinks",
            cache_root.display()
        )));
    }
    let physical = cache_root.canonicalize().map_err(|error| {
        SourceError::Source(format!(
            "canonicalize worktree cache root {}: {error}",
            cache_root.display()
        ))
    })?;
    Ok(Some(physical))
}

fn verified_mirror_root(address: &WorktreeMirrorAddress) -> Result<PathBuf> {
    let cache_root = verified_cache_git_root(&address.cache_git_root)?;
    let mirror = super::cache::mirror_path_for_key(&cache_root, &address.key);
    if !real_directory(&mirror, "inspect worktree mirror")? {
        return Err(SourceError::Source(format!(
            "inspect worktree mirror {}: path is not a directory without symlinks",
            mirror.display()
        )));
    }
    Ok(mirror)
}

pub(crate) fn physical_gitlink_path(deploy_root: &Path) -> Result<Option<PathBuf>> {
    match deploy_root.canonicalize() {
        Ok(root) => Ok(Some(root.join(".git"))),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(SourceError::Source(format!(
            "canonicalize worktree deployment root {}: {error}",
            deploy_root.display()
        ))),
    }
}

fn canonical_gitlink_path(deploy_root: &Path) -> Result<PathBuf> {
    physical_gitlink_path(deploy_root)?.ok_or_else(|| {
        SourceError::Source(format!(
            "worktree deployment root {} is absent",
            deploy_root.display()
        ))
    })
}

fn remove_worktree_gitlink(
    address: &WorktreeMirrorAddress,
    admin_id: &WorktreeAdminId,
    deploy_root: &Path,
) -> Result<()> {
    let _ = physical_cache_git_root(&address.cache_git_root)?;
    let admin_dir = super::cache::mirror_path_for_key(&address.cache_git_root, &address.key)
        .join("worktrees")
        .join(format!("ph-{}", admin_id.as_str()));
    let Some(gitlink) = physical_gitlink_path(deploy_root)? else {
        return Ok(());
    };
    if gitlink_targets(&gitlink, &admin_dir)? {
        remove_file_if_exists(&gitlink, "remove worktree gitlink")?;
    }
    Ok(())
}

pub(super) fn sweep_stale_worktrees(mirror: &Path, worktrees: &Path) -> Result<()> {
    let Some(entries) = read_directory_no_follow(worktrees, "read worktree administration root")?
    else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            SourceError::Source(format!("read worktree administration entry: {error}"))
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = admin_id_from_name(name) else {
            continue;
        };
        let admin = worktrees.join(name);
        if !administration_is_live(&admin)? {
            remove_swept_worktree(mirror, &admin, &id)?;
        }
    }
    Ok(())
}

pub(super) fn sweep_transient_worktrees(mirror: &Path) -> Result<()> {
    let Some(entries) = read_directory_no_follow(mirror, "read transient worktree administration")?
    else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            SourceError::Source(format!(
                "read transient worktree administration entry: {error}"
            ))
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((_admin_name, _kind)) = transient_name(name) else {
            continue;
        };
        let transient = entry.path();
        remove_path_if_exists(&transient, "sweep transient worktree administration")?;
    }
    Ok(())
}

fn administration_is_live(admin: &Path) -> Result<bool> {
    if !real_directory(admin, "inspect worktree administration")? {
        return Ok(false);
    }
    let Some(gitlink) = read_admin_gitdir(admin)? else {
        return Ok(false);
    };
    if !physical_regular_file(&gitlink, "inspect worktree gitlink")? {
        return Ok(false);
    }
    let Some(back_pointer) = read_gitlink_back_pointer(&gitlink)? else {
        return Ok(false);
    };
    Ok(back_pointer == admin)
}

fn read_admin_gitdir(admin: &Path) -> Result<Option<PathBuf>> {
    let Some(value) =
        read_regular_text_no_follow(&admin.join("gitdir"), "read worktree administration gitdir")?
    else {
        return Ok(None);
    };
    let path = PathBuf::from(value.trim());
    if path.is_absolute()
        && path.file_name().is_some_and(|name| name == ".git")
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

fn read_gitlink_back_pointer(gitlink: &Path) -> Result<Option<PathBuf>> {
    let Some(value) = read_regular_text_no_follow(gitlink, "read worktree gitlink")? else {
        return Ok(None);
    };
    let Some(path) = value.trim().strip_prefix("gitdir: ") else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    if path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

fn remove_swept_worktree(mirror: &Path, admin: &Path, id: &WorktreeAdminId) -> Result<()> {
    remove_path_if_exists(admin, "remove stale worktree administration")?;
    let pin = mirror.join("refs/phora/worktrees").join(id.as_str());
    if physical_path_within(mirror, &pin)? {
        remove_path_if_exists(&pin, "remove stale worktree pin ref")?;
    }
    Ok(())
}

fn read_directory_no_follow(path: &Path, action: &str) -> Result<Option<std::fs::ReadDir>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::read_dir(path).map(Some).map_err(|error| {
                SourceError::Source(format!("{action} {}: {error}", path.display()))
            })
        }
        Ok(_) => Err(SourceError::Source(format!(
            "{action} {}: path is not a directory without symlinks",
            path.display()
        ))),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(SourceError::Source(format!(
            "{action} {}: {error}",
            path.display()
        ))),
    }
}

fn real_directory(path: &Path, action: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(false)
        }
        Err(error) => Err(SourceError::Source(format!(
            "{action} {}: {error}",
            path.display()
        ))),
    }
}

fn physical_regular_file(path: &Path, action: &str) -> Result<bool> {
    if !path.is_absolute() {
        return Ok(false);
    }
    let mut physical = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir | std::path::Component::ParentDir => return Ok(false),
            _ => physical.push(component.as_os_str()),
        }
        let metadata = match std::fs::symlink_metadata(&physical) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(false);
            }
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "{action} {}: {error}",
                    physical.display()
                )));
            }
        };
        if metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if physical != path && !metadata.is_dir() {
            return Ok(false);
        }
        if physical == path && !metadata.is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn physical_path_within(root: &Path, path: &Path) -> Result<bool> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(false);
    };
    if !real_directory(root, "inspect mirror root")? {
        return Ok(false);
    }
    let mut physical = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Ok(false);
        };
        physical.push(component);
        match std::fs::symlink_metadata(&physical) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(false),
            Ok(metadata) if physical != path && !metadata.is_dir() => return Ok(false),
            Ok(_) => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(false);
            }
            Err(error) => {
                return Err(SourceError::Source(format!(
                    "inspect mirror path {}: {error}",
                    physical.display()
                )));
            }
        }
    }
    Ok(true)
}

fn read_regular_text_no_follow(path: &Path, action: &str) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            match std::fs::read_to_string(path) {
                Ok(value) => Ok(Some(value)),
                Err(error) if error.kind() == ErrorKind::InvalidData => Ok(None),
                Err(error)
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
                {
                    Ok(None)
                }
                Err(error) => Err(SourceError::Source(format!(
                    "{action} {}: {error}",
                    path.display()
                ))),
            }
        }
        Ok(_) => Ok(None),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(SourceError::Source(format!(
            "{action} {}: {error}",
            path.display()
        ))),
    }
}

fn remove_path_if_exists(path: &Path, action: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
                .map_err(|error| SourceError::Source(format!("{action}: {error}")))
        }
        Ok(_) => std::fs::remove_file(path)
            .map_err(|error| SourceError::Source(format!("{action}: {error}"))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SourceError::Source(format!("{action}: {error}"))),
    }
}

fn admin_id_from_name(name: &str) -> Option<WorktreeAdminId> {
    name.strip_prefix("ph-")?.parse().ok()
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
    let target_path = Path::new(target);
    if target_path == admin {
        return Ok(true);
    }
    let target = match std::fs::canonicalize(target_path) {
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
        let Some(nonce) = suffix.strip_prefix(&format!("{kind}-")) else {
            continue;
        };
        let (process, counter) = nonce.split_once('-')?;
        process.parse::<u32>().ok()?;
        counter.parse::<u64>().ok()?;
        return Some((admin, kind));
    }
    None
}
