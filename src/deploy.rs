//! Deployment: drift detection, copy/scan, atomic directory swap.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::{
    ArtifactKey, EjectedEntry, ManifestFile, Registry, RegistryRecord, ScannedFile,
};

#[derive(Debug)]
pub enum ArtifactState {
    Clean,
    /// Managed artifact whose lock advanced past the deployed commit; redeploys without `--force`.
    Outdated,
    Modified {
        changed: Vec<PathBuf>,
    },
    Foreign,
    Missing,
    Ejected,
    Linked,
    /// Clean-like state carrying refreshed per-file metadata.
    Revalidated {
        fresh: Vec<ScannedFile>,
    },
}

#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    /// Relative paths of symlinks encountered (excluded from `files`).
    pub symlinks: Vec<PathBuf>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "drift inputs are independent scalars; bundling them into a struct would only relocate the arity"
)]
pub fn check_artifact_state(
    target_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    ejected: &[EjectedEntry],
    artifact_name: &str,
    registry: &dyn Registry,
    key: &ArtifactKey,
    expected_vars_digest: Option<&str>,
) -> Result<ArtifactState> {
    let is_ejected = ejected.iter().any(|e| {
        e.source == expected_source
            && (e.artifact == artifact_name
                || artifact_name.starts_with(&format!("{}/", e.artifact))
                || e.artifact.starts_with(&format!("{artifact_name}/")))
    });
    if is_ejected {
        return Ok(ArtifactState::Ejected);
    }

    match target_path.try_exists() {
        Ok(false) => return Ok(ArtifactState::Missing),
        Ok(true) => {}
        Err(e) => {
            return Err(Error::Projection(format!(
                "stat {}: {e}",
                target_path.display()
            )));
        }
    }

    if std::fs::symlink_metadata(target_path).is_ok_and(|m| m.is_file()) {
        return check_file_artifact_state(
            target_path,
            expected_source,
            expected_commit,
            registry,
            key,
            expected_vars_digest,
        );
    }

    let record = match artifact_record(registry, key, expected_source)? {
        Ok(record) => record,
        Err(state) => return Ok(state),
    };

    let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
    let mut fresh: Vec<ScannedFile> = Vec::new();

    for mf in &record.files {
        let file_path = target_path.join(&mf.path);
        // No-follow stat: a recorded regular file swapped for a symlink is drift.
        let meta = match std::fs::symlink_metadata(&file_path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                changed.insert(mf.path.clone());
                continue;
            }
            Err(e) => {
                return Err(Error::Projection(format!(
                    "stat {}: {e}",
                    file_path.display()
                )));
            }
        };
        if !meta.is_file() {
            changed.insert(mf.path.clone());
            continue;
        }
        if meta.len() != mf.size || mtime_secs(&meta, &file_path)? != mf.mtime {
            match revalidate_file(&file_path, &meta, mf)? {
                Some(scanned) => fresh.push(scanned),
                None => {
                    changed.insert(mf.path.clone());
                }
            }
        }
    }

    let scan = scan_dir_soft(target_path)?;
    let known: HashSet<&PathBuf> = record.files.iter().map(|f| &f.path).collect();
    for cf in &scan.files {
        if !known.contains(&cf.path) {
            changed.insert(cf.path.clone());
        }
    }
    if !record.allow_symlinks {
        changed.extend(scan.symlinks);
    }

    Ok(classify_drift(
        &record,
        changed.into_iter().collect(),
        fresh,
        expected_commit,
        expected_vars_digest,
    ))
}

/// Drift check when the target IS a single renamed FILE, not a directory of recorded files.
fn check_file_artifact_state(
    file_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    registry: &dyn Registry,
    key: &ArtifactKey,
    expected_vars_digest: Option<&str>,
) -> Result<ArtifactState> {
    let record = match artifact_record(registry, key, expected_source)? {
        Ok(record) => record,
        Err(state) => return Ok(state),
    };

    let meta = std::fs::symlink_metadata(file_path)
        .map_err(|e| Error::Projection(format!("stat {}: {e}", file_path.display())))?;
    let (changed, fresh) = match record.files.first() {
        Some(_) if !meta.is_file() => (vec![file_path.to_path_buf()], vec![]),
        Some(mf) if meta.len() != mf.size || mtime_secs(&meta, file_path)? != mf.mtime => {
            match revalidate_file(file_path, &meta, mf)? {
                Some(scanned) => (vec![], vec![scanned]),
                None => (vec![file_path.to_path_buf()], vec![]),
            }
        }
        Some(_) => (vec![], vec![]),
        None => (vec![file_path.to_path_buf()], vec![]),
    };

    Ok(classify_drift(
        &record,
        changed,
        fresh,
        expected_commit,
        expected_vars_digest,
    ))
}

/// `Err(state)` is not a failure: it carries the early Linked/Outdated/Foreign `ArtifactState`.
fn artifact_record(
    registry: &dyn Registry,
    key: &ArtifactKey,
    expected_source: &str,
) -> Result<std::result::Result<RegistryRecord, ArtifactState>> {
    let Some(record) = registry.get(key)? else {
        if managed_under_sibling_shape(registry, key, expected_source)? {
            return Ok(Err(ArtifactState::Outdated));
        }
        return Ok(Err(ArtifactState::Foreign));
    };
    if record.linked {
        return Ok(Err(ArtifactState::Linked));
    }
    if record.key.source != expected_source {
        return Ok(Err(ArtifactState::Foreign));
    }
    Ok(Ok(record))
}

/// True when `expected_source` holds a record under the collapsed-dir/per-leaf counterpart of `key`.
fn managed_under_sibling_shape(
    registry: &dyn Registry,
    key: &ArtifactKey,
    expected_source: &str,
) -> Result<bool> {
    let under = |child: &str, parent: &str| {
        child
            .strip_prefix(parent)
            .is_some_and(|r| r.starts_with('/'))
    };
    Ok(registry.list_target(&key.target)?.iter().any(|record| {
        record.key.source == expected_source
            && (under(&record.key.artifact, &key.artifact)
                || under(&key.artifact, &record.key.artifact))
    }))
}

fn classify_drift(
    record: &RegistryRecord,
    changed: Vec<PathBuf>,
    fresh: Vec<ScannedFile>,
    expected_commit: &str,
    expected_vars_digest: Option<&str>,
) -> ArtifactState {
    if !changed.is_empty() {
        return ArtifactState::Modified { changed };
    }
    let commit_advanced = record.commit != expected_commit;
    let vars_changed =
        record.vars_digest.is_some() && record.vars_digest.as_deref() != expected_vars_digest;
    if commit_advanced || vars_changed {
        return ArtifactState::Outdated;
    }
    if !fresh.is_empty() {
        return ArtifactState::Revalidated { fresh };
    }
    ArtifactState::Clean
}

/// `None` declines the refresh on any uncertainty (read error, re-stat error, mid-flight
/// change, hash mismatch); `Some` is a revalidated stat whose bytes matched `mf.blake3`.
fn revalidate_file(
    file_path: &Path,
    meta: &std::fs::Metadata,
    mf: &ManifestFile,
) -> Result<Option<ScannedFile>> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let Ok(mut file) = std::fs::File::open(file_path) else {
        return Ok(None);
    };
    let Ok(pre) = file.metadata() else {
        return Ok(None);
    };
    if !pre.is_file() {
        return Ok(None);
    }
    // Closes the path-resolution TOCTOU: the opened fd must be the same inode the caller's
    // no-follow pre-stat saw, else a mid-validation path swap could mask content drift.
    if pre.ino() != meta.ino() || pre.dev() != meta.dev() {
        return Ok(None);
    }

    let mut content = Vec::new();
    if file.read_to_end(&mut content).is_err() {
        return Ok(None);
    }

    let Ok(post) = file.metadata() else {
        return Ok(None);
    };
    let size = post.len();
    let mtime = mtime_secs(&post, file_path)?;
    // The held inode's size/mtime moved across the read: an in-place mid-validation change.
    if size != pre.len() || mtime != mtime_secs(&pre, file_path)? {
        return Ok(None);
    }

    if blake3::hash(&content).to_hex().to_string() != mf.blake3 {
        return Ok(None);
    }

    Ok(Some(ScannedFile {
        path: mf.path.clone(),
        size,
        mtime,
    }))
}

#[derive(Debug, Clone, Copy)]
enum ScanMode {
    Strict,
    Soft,
}

/// Soft scan: never errors on symlinks, reports them for "treat as Modified".
pub fn scan_dir_soft(dir: &Path) -> Result<ScanResult> {
    scan_dir(dir, true, ScanMode::Soft)
}

/// Strict scan (write path): errors on a disallowed symlink.
pub fn scan_dir_strict(dir: &Path, allow_symlinks: bool) -> Result<ScanResult> {
    scan_dir(dir, allow_symlinks, ScanMode::Strict)
}

fn scan_dir(dir: &Path, allow_symlinks: bool, mode: ScanMode) -> Result<ScanResult> {
    let mut files = Vec::new();
    let mut symlinks = Vec::new();

    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::Projection(format!("walk {}: {e}", dir.display())))?;
        let ft = entry.file_type();
        let rel = relative(entry.path(), dir)?;

        if ft.is_symlink() {
            match mode {
                ScanMode::Strict if !allow_symlinks => {
                    return Err(Error::SymlinkNotAllowed { path: rel });
                }
                ScanMode::Strict | ScanMode::Soft => symlinks.push(rel),
            }
            continue;
        }

        if !ft.is_file() {
            continue;
        }

        let meta = entry
            .metadata()
            .map_err(|e| Error::Projection(format!("stat {}: {e}", entry.path().display())))?;
        files.push(ScannedFile {
            path: rel,
            size: meta.len(),
            mtime: mtime_secs(&meta, entry.path())?,
        });
    }

    Ok(ScanResult { files, symlinks })
}

fn relative(path: &Path, base: &Path) -> Result<PathBuf> {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .map_err(|e| Error::Projection(format!("strip prefix {}: {e}", path.display())))
}

fn mtime_secs(meta: &std::fs::Metadata, path: &Path) -> Result<u64> {
    meta.modified()
        .map_err(|e| Error::Projection(format!("mtime {}: {e}", path.display())))?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| Error::Projection(format!("mtime before epoch {}: {e}", path.display())))
}

/// Copy a file from staging to target, preferring reflink, preserving mtime.
pub fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if reflink_copy::reflink(src, dst).is_err() {
        std::fs::copy(src, dst).map_err(|e| {
            Error::Projection(format!("copy {} -> {}: {e}", src.display(), dst.display()))
        })?;
    }
    copy_mtime(src, dst)
}

fn copy_mtime(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::metadata(src)
        .map_err(|e| Error::Projection(format!("stat {}: {e}", src.display())))?;
    let mtime = filetime::FileTime::from_last_modification_time(&meta);
    filetime::set_file_mtime(dst, mtime)
        .map_err(|e| Error::Projection(format!("set mtime {}: {e}", dst.display())))
}

/// Cross-device fallback for the atomic-rename swap: recursively copy `src` into `dst`.
pub fn copy_tree(src: &Path, dst: &Path, allow_symlinks: bool) -> Result<()> {
    let scan = scan_dir_strict(src, allow_symlinks)?;
    std::fs::create_dir_all(dst)
        .map_err(|e| Error::Projection(format!("create dir {}: {e}", dst.display())))?;
    for file in &scan.files {
        let from = src.join(&file.path);
        let to = dst.join(&file.path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Projection(format!("create dir {}: {e}", parent.display())))?;
        }
        copy_file(&from, &to)?;
    }
    for link in &scan.symlinks {
        let to = dst.join(link);
        let target = std::fs::read_link(src.join(link))
            .map_err(|e| Error::Projection(format!("read link {}: {e}", link.display())))?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Projection(format!("create dir {}: {e}", parent.display())))?;
        }
        std::os::unix::fs::symlink(&target, &to)
            .map_err(|e| Error::Projection(format!("symlink {}: {e}", to.display())))?;
    }
    Ok(())
}

/// Write-ahead journal of in-flight swaps, persisted under a `locks/` dir.
pub struct Journal {
    path: PathBuf,
}

/// One intent record: enough to replay or roll back a single artifact swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub staging_base: PathBuf,
    pub staging: PathBuf,
    pub dst: PathBuf,
    pub record: RegistryRecord,
    /// True once the stage→dst rename completed (registry put still pending).
    pub swap_completed: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct JournalFile {
    #[serde(default, rename = "entry")]
    entries: Vec<JournalEntry>,
}

impl Journal {
    /// Open (creating if needed) the journal living in `locks_dir`.
    pub fn open(locks_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(locks_dir).map_err(|e| {
            Error::Projection(format!("create locks dir {}: {e}", locks_dir.display()))
        })?;
        Ok(Self {
            path: locks_dir.join("journal.toml"),
        })
    }

    fn load(&self) -> Result<JournalFile> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                Error::Projection(format!("parse journal {}: {e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(JournalFile::default()),
            Err(e) => Err(Error::Projection(format!(
                "read journal {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn persist(&self, file: &JournalFile) -> Result<()> {
        let serialized = toml::to_string(file)
            .map_err(|e| Error::Projection(format!("serialize journal: {e}")))?;
        let tmp = self.path.with_extension("toml.tmp");
        {
            use std::io::Write as _;
            let mut handle = std::fs::File::create(&tmp)
                .map_err(|e| Error::Projection(format!("create temp {}: {e}", tmp.display())))?;
            handle
                .write_all(serialized.as_bytes())
                .map_err(|e| Error::Projection(format!("write temp {}: {e}", tmp.display())))?;
            handle
                .sync_all()
                .map_err(|e| Error::Projection(format!("fsync temp {}: {e}", tmp.display())))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::Projection(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                self.path.display()
            ))
        })
    }

    /// Append an intent before the swap.
    pub fn append(&self, entry: &JournalEntry) -> Result<()> {
        let mut file = self.load()?;
        file.entries.push(entry.clone());
        self.persist(&file)
    }

    /// Mark the most recent matching intent as swap-completed.
    pub fn mark_swap_completed(&self, dst: &Path) -> Result<()> {
        let mut file = self.load()?;
        if let Some(entry) = file.entries.iter_mut().rev().find(|e| e.dst == dst) {
            entry.swap_completed = true;
        }
        self.persist(&file)
    }

    pub fn entries(&self) -> Result<Vec<JournalEntry>> {
        Ok(self.load()?.entries)
    }

    /// Remove the most recent intent targeting `dst`.
    pub fn remove(&self, dst: &Path) -> Result<()> {
        let mut file = self.load()?;
        if let Some(pos) = file.entries.iter().rposition(|e| e.dst == dst) {
            file.entries.remove(pos);
        }
        self.persist(&file)
    }

    pub fn clear(&self) -> Result<()> {
        self.persist(&JournalFile::default())
    }
}

/// Atomic swap of `staging` into `dst`, then persist `record`.
///
/// `staging_base` is `<target_parent>/.phora-stage/`; `staging` is the exported dir
/// inside it. The intent is journaled before the swap and cleared after the put.
/// Removes its tracked paths on drop; `base` is the shared `staging_base`, pruned only
/// when empty so a sibling artifact's pending staging is never wiped.
struct CleanupGuard {
    paths: Vec<PathBuf>,
    base: Option<PathBuf>,
}

impl CleanupGuard {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            base: None,
        }
    }

    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn prune_base_if_empty(&mut self, base: PathBuf) {
        self.base = Some(base);
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = remove_path(path);
        }
        if let Some(base) = &self.base {
            let _ = std::fs::remove_dir(base);
        }
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "caller hands off ownership of the record being deployed"
)]
pub fn deploy_artifact(
    staging_base: &Path,
    staging: &Path,
    dst: &Path,
    record: RegistryRecord,
    journal: &Journal,
    registry: &dyn Registry,
) -> Result<()> {
    let mut cleanup = CleanupGuard::new();
    cleanup.track(staging.to_path_buf());
    cleanup.track(backup_path(staging_base, dst));
    cleanup.prune_base_if_empty(staging_base.to_path_buf());

    journal.append(&JournalEntry {
        staging_base: staging_base.to_path_buf(),
        staging: staging.to_path_buf(),
        dst: dst.to_path_buf(),
        record: record.clone(),
        swap_completed: false,
    })?;

    let backup = match dst.try_exists() {
        Ok(true) => {
            let backup = backup_path(staging_base, dst);
            std::fs::rename(dst, &backup).map_err(|e| {
                Error::Projection(format!(
                    "rename {} -> {}: {e}",
                    dst.display(),
                    backup.display()
                ))
            })?;
            Some(backup)
        }
        Ok(false) => None,
        Err(e) => return Err(Error::Projection(format!("stat {}: {e}", dst.display()))),
    };

    swap_into(staging, dst, record.allow_symlinks)?;
    journal.mark_swap_completed(dst)?;

    if let Err(put_err) = registry.put(&record) {
        rollback_swap(dst, backup.as_deref())?;
        journal.remove(dst)?;
        return Err(put_err.into());
    }

    journal.remove(dst)?;
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::symlink as symlink_unix;
#[cfg(windows)]
use std::os::windows::fs::{symlink_dir, symlink_file};

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path, _is_file: bool) -> std::io::Result<()> {
    symlink_unix(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path, is_file: bool) -> std::io::Result<()> {
    if is_file {
        symlink_file(target, link)
    } else {
        symlink_dir(target, link)
    }
}

/// Crash-safe symlink deploy: stage a fresh symlink beside `dst`, journal the
/// intent, then atomically `rename` it over `dst`. No copy/swap is involved —
/// the link points at the absolute working-tree `target`.
#[expect(
    clippy::needless_pass_by_value,
    reason = "caller hands off ownership of the record being deployed"
)]
pub fn link_artifact(
    staging_base: &Path,
    dst: &Path,
    target: &Path,
    record: RegistryRecord,
    journal: &Journal,
    registry: &dyn Registry,
) -> Result<()> {
    let parent = dst
        .parent()
        .ok_or_else(|| Error::Projection(format!("link dst {} has no parent", dst.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| Error::Projection(format!("create link dir {}: {e}", parent.display())))?;
    std::fs::create_dir_all(staging_base).map_err(|e| {
        Error::Projection(format!(
            "create staging dir {}: {e}",
            staging_base.display()
        ))
    })?;

    let leaf = dst.file_name().map_or_else(
        || "artifact".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let staging = staging_base.join(format!("link-{leaf}-{}", link_nonce()));
    let mut cleanup = CleanupGuard::new();
    cleanup.track(staging.clone());
    cleanup.track(backup_path(staging_base, dst));
    cleanup.prune_base_if_empty(staging_base.to_path_buf());

    let is_file = matches!(record.kind, crate::store::RecordKind::File);
    create_symlink(target, &staging, is_file).map_err(|e| {
        Error::Projection(format!(
            "symlink {} -> {}: {e}",
            staging.display(),
            target.display()
        ))
    })?;

    journal.append(&JournalEntry {
        staging_base: staging_base.to_path_buf(),
        staging: staging.clone(),
        dst: dst.to_path_buf(),
        record: record.clone(),
        swap_completed: false,
    })?;

    let backup = match dst.try_exists() {
        Ok(true) => {
            let backup = backup_path(staging_base, dst);
            std::fs::rename(dst, &backup).map_err(|e| {
                Error::Projection(format!(
                    "rename {} -> {}: {e}",
                    dst.display(),
                    backup.display()
                ))
            })?;
            Some(backup)
        }
        Ok(false) => None,
        Err(e) => return Err(Error::Projection(format!("stat {}: {e}", dst.display()))),
    };

    std::fs::rename(&staging, dst).map_err(|e| {
        Error::Projection(format!(
            "rename {} -> {}: {e}",
            staging.display(),
            dst.display()
        ))
    })?;
    journal.mark_swap_completed(dst)?;

    if let Err(put_err) = registry.put(&record) {
        rollback_swap(dst, backup.as_deref())?;
        journal.remove(dst)?;
        return Err(put_err.into());
    }

    journal.remove(dst)?;
    Ok(())
}

fn link_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Same-mount `rename` is atomic; a cross-device error falls back to copy+fsync.
fn swap_into(staging: &Path, dst: &Path, allow_symlinks: bool) -> Result<()> {
    match std::fs::rename(staging, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            eprintln!(
                "phora: staging on a different mount than {}; falling back to recursive copy",
                dst.display()
            );
            if staging.is_file() {
                copy_file(staging, dst)
            } else {
                copy_tree(staging, dst, allow_symlinks)
            }
        }
        Err(e) => Err(Error::Projection(format!(
            "rename {} -> {}: {e}",
            staging.display(),
            dst.display()
        ))),
    }
}

fn rollback_swap(dst: &Path, backup: Option<&Path>) -> Result<()> {
    remove_path(dst)
        .map_err(|e| Error::Projection(format!("rollback remove {}: {e}", dst.display())))?;
    if let Some(backup) = backup {
        std::fs::rename(backup, dst).map_err(|e| {
            Error::Projection(format!(
                "rollback restore {} -> {}: {e}",
                backup.display(),
                dst.display()
            ))
        })?;
    }
    Ok(())
}

fn backup_path(staging_base: &Path, dst: &Path) -> PathBuf {
    let leaf = dst.file_name().map_or_else(
        || "artifact".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    staging_base.join(format!(".phora-backup-{leaf}"))
}

fn is_cross_device(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::CrossesDevices
}

/// Startup reconciliation: replay `journal`, then remove any orphaned
/// `<target_parent>/.phora-stage*` left by a crash.
pub fn recovery_sweep(
    target_parent: &Path,
    journal: &Journal,
    registry: &dyn Registry,
) -> Result<()> {
    for entry in journal.entries()? {
        if entry.swap_completed {
            registry.put(&entry.record)?;
        } else {
            let backup = backup_path(&entry.staging_base, &entry.dst);
            if backup
                .try_exists()
                .map_err(|e| Error::Projection(format!("stat backup {}: {e}", backup.display())))?
            {
                std::fs::rename(&backup, &entry.dst).map_err(|e| {
                    Error::Projection(format!(
                        "restore backup {} -> {}: {e}",
                        backup.display(),
                        entry.dst.display()
                    ))
                })?;
            }
            remove_path(&entry.staging).map_err(|e| {
                Error::Projection(format!("discard staging {}: {e}", entry.staging.display()))
            })?;
        }
        journal.remove(&entry.dst)?;
    }
    remove_orphaned_staging(target_parent)
}

fn remove_orphaned_staging(target_parent: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(target_parent) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(Error::Projection(format!(
                "read dir {}: {e}",
                target_parent.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::Projection(format!("read entry in {}: {e}", target_parent.display()))
        })?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".phora-stage")
        {
            remove_path(&entry.path()).map_err(|e| {
                Error::Projection(format!("remove orphan {}: {e}", entry.path().display()))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{FileRegistry, HookState, ManifestFile, StoreError};

    type StoreResult<T> = std::result::Result<T, StoreError>;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn mtime_secs(path: &Path) -> u64 {
        std::fs::metadata(path)
            .expect("metadata")
            .modified()
            .expect("modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after epoch")
            .as_secs()
    }

    fn set_mtime(path: &Path, secs: u64) {
        filetime::set_file_mtime(
            path,
            filetime::FileTime::from_unix_time(secs.cast_signed(), 0),
        )
        .expect("set mtime");
    }

    // copy_file

    #[test]
    fn copy_file_reproduces_source_content() {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"reflink-or-copy payload").expect("write src");

        copy_file(&src, &dst).expect("copy_file");

        assert_eq!(
            std::fs::read(&dst).expect("read dst"),
            b"reflink-or-copy payload",
            "dst content must be byte-identical to src regardless of reflink vs plain copy"
        );
    }

    #[test]
    fn copy_file_preserves_source_mtime() {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"payload").expect("write src");
        let known = 1_700_000_000u64;
        set_mtime(&src, known);

        copy_file(&src, &dst).expect("copy_file");

        assert_eq!(
            mtime_secs(&dst),
            known,
            "copy_file must explicitly carry the src mtime onto dst (reflink does not copy mtime)"
        );
    }

    // scan_dir_soft / scan_dir_strict

    #[test]
    fn soft_scan_reports_regular_files_with_relative_path() {
        let dir = TempDir::new().expect("tempdir");
        let file = dir.path().join("settings.json");
        std::fs::write(&file, b"{}").expect("write file");
        let known_mtime = 1_700_000_123u64;
        set_mtime(&file, known_mtime);

        let scan = scan_dir_soft(dir.path()).expect("soft scan must not error");

        let scanned = scan
            .files
            .iter()
            .find(|f| f.path == *Path::new("settings.json"))
            .unwrap_or_else(|| {
                panic!(
                    "soft scan must list the regular file by its path relative to the scanned dir, \
                     got {:?}",
                    scan.files
                )
            });
        assert_eq!(
            scanned.size, 2,
            "scanned file size must be the on-disk byte length"
        );
        assert_eq!(
            scanned.mtime, known_mtime,
            "scanned file mtime must be the on-disk mtime in whole seconds since epoch"
        );
    }

    #[test]
    fn soft_scan_reports_symlink_without_error_and_excludes_it_from_files() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"hi").expect("write real");
        symlink("real.txt", dir.path().join("link.txt")).expect("create symlink");

        let scan = scan_dir_soft(dir.path()).expect("soft scan must never error on symlinks");

        assert!(
            scan.symlinks.contains(&PathBuf::from("link.txt")),
            "soft scan must report the symlink in `symlinks`, got {:?}",
            scan.symlinks
        );
        assert!(
            !scan.files.iter().any(|f| f.path == *Path::new("link.txt")),
            "symlink must be excluded from `files`, got {:?}",
            scan.files
        );
        assert!(
            scan.files.iter().any(|f| f.path == *Path::new("real.txt")),
            "the regular file must still appear in `files`, got {:?}",
            scan.files
        );
    }

    #[test]
    fn strict_scan_errors_on_disallowed_symlink() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"hi").expect("write real");
        symlink("real.txt", dir.path().join("link.txt")).expect("create symlink");

        let err = scan_dir_strict(dir.path(), false)
            .expect_err("strict scan with symlinks disallowed must return Err on a symlink");

        let Error::SymlinkNotAllowed { path } = err else {
            panic!("strict scan must reject the disallowed symlink specifically, got {err:?}");
        };
        assert!(
            path.ends_with("link.txt"),
            "the error must name the offending symlink (link.txt), got {}",
            path.display()
        );
    }

    #[test]
    fn strict_scan_records_an_allowed_symlink_so_it_survives_the_copy_fallback() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"hi").expect("write real");
        symlink("real.txt", dir.path().join("link.txt")).expect("create symlink");

        let scan = scan_dir_strict(dir.path(), true).expect("strict scan with symlinks allowed");

        assert!(
            scan.symlinks.contains(&PathBuf::from("link.txt")),
            "an ALLOWED symlink must be recorded, not silently dropped, or copy_tree loses it on \
             the cross-device fallback; got {:?}",
            scan.symlinks
        );
        assert!(
            !scan.files.iter().any(|f| f.path == *Path::new("link.txt")),
            "the symlink must still be excluded from `files`, got {:?}",
            scan.files
        );
    }

    // copy_tree

    #[test]
    fn copy_tree_recreates_symlinks_not_just_regular_files() {
        let root = TempDir::new().expect("tempdir");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        std::fs::create_dir(&src).expect("mkdir src");
        std::fs::write(src.join("real.txt"), b"payload").expect("write real");
        symlink("real.txt", src.join("link.txt")).expect("create symlink");

        copy_tree(&src, &dst, true).expect("copy_tree with symlinks allowed");

        assert_eq!(
            std::fs::read(dst.join("real.txt")).expect("read copied file"),
            b"payload",
            "the regular file must be copied"
        );
        let link = dst.join("link.txt");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat copied link")
                .file_type()
                .is_symlink(),
            "copy_tree must recreate the symlink AS a symlink, not drop it or dereference it"
        );
        assert_eq!(
            std::fs::read_link(&link).expect("read copied link"),
            PathBuf::from("real.txt"),
            "the recreated symlink must preserve its original target"
        );
    }

    // check_artifact_state

    const SOURCE: &str = "company-configs";
    const COMMIT: &str = "abc123def456";
    const ARTIFACT: &str = "snippets";
    const TARGET: &str = "vscode";

    fn key() -> ArtifactKey {
        ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: ARTIFACT.to_owned(),
        }
    }

    fn registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    /// Record's `ManifestFile` entries carry on-disk size+mtime, so the result reads as Clean.
    fn deploy_and_record(
        target: &Path,
        files: &[(&str, &[u8])],
        allow_symlinks: bool,
    ) -> RegistryRecord {
        let mut manifest = Vec::new();
        for (rel, contents) in files {
            let path = target.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::write(&path, contents).expect("write artifact file");
            manifest.push(ManifestFile {
                path: PathBuf::from(rel),
                size: contents.len() as u64,
                mtime: mtime_secs(&path),
                blake3: blake3::hash(contents).to_hex().to_string(),
            });
        }
        RegistryRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::store::RecordKind::Dir,
            allow_symlinks,
            preserve_executable: true,
            files: manifest,
            linked: false,
            vars_digest: None,
        }
    }

    fn deploy_and_record_file(file_path: &Path, contents: &[u8]) -> RegistryRecord {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(file_path, contents).expect("write file artifact");
        let leaf = file_path.file_name().expect("file leaf");
        let mf = ManifestFile {
            path: PathBuf::from(leaf),
            size: contents.len() as u64,
            mtime: mtime_secs(file_path),
            blake3: blake3::hash(contents).to_hex().to_string(),
        };
        RegistryRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::store::RecordKind::File,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![mf],
            linked: false,
            vars_digest: None,
        }
    }

    fn ejected(source: &str, artifact: &str) -> EjectedEntry {
        EjectedEntry {
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        }
    }

    fn state(target: &Path, ejected: &[EjectedEntry], reg: &FileRegistry) -> ArtifactState {
        check_artifact_state(target, SOURCE, COMMIT, ejected, ARTIFACT, reg, &key(), None)
            .expect("check_artifact_state")
    }

    #[test]
    fn ejected_beats_missing_target() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let missing = target.path().join("never-deployed");

        let st = state(&missing, &[ejected(SOURCE, ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Ejected),
            "an ejected artifact stays Ejected even when its target dir is absent, got {st:?}"
        );
    }

    #[test]
    fn ejected_beats_existing_clean_deployment() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");

        let st = state(target.path(), &[ejected(SOURCE, ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Ejected),
            "ejected beats everything, even a present matching record, got {st:?}"
        );
    }

    #[test]
    fn not_ejected_when_artifact_matches_but_source_differs() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");

        let st = state(target.path(), &[ejected("other-source", ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "an ejected entry whose artifact matches but whose source differs from expected_source \
             must not eject this artifact: ejection keys on (artifact, source), got {st:?}"
        );
    }

    #[test]
    fn a_dir_eject_blocks_redeploy_of_a_leaf_under_it() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let leaf = target.path().join("editor").join("a.md");
        std::fs::create_dir_all(leaf.parent().expect("leaf parent")).expect("mkdir leaf parent");
        std::fs::write(&leaf, b"alpha\n").expect("write leaf");

        let leaf_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "editor/a.md".to_owned(),
        };
        let st = check_artifact_state(
            &leaf,
            SOURCE,
            COMMIT,
            &[ejected(SOURCE, "editor")],
            "editor/a.md",
            &reg,
            &leaf_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Ejected),
            "a dir-ejected `editor` must block redeploy of the leaf `editor/a.md` that falls under \
             it — the user took over the whole `editor` path; got {st:?}"
        );
    }

    #[test]
    fn a_leaf_eject_blocks_redeploy_of_a_collapsed_dir_over_it() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let dir = target.path().join("editor");
        std::fs::create_dir_all(&dir).expect("mkdir dir");

        let dir_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "editor".to_owned(),
        };
        let st = check_artifact_state(
            &dir,
            SOURCE,
            COMMIT,
            &[ejected(SOURCE, "editor/a.md")],
            "editor",
            &reg,
            &dir_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Ejected),
            "a leaf-ejected `editor/a.md` must block redeploy of a collapsed `editor` dir that \
             would overwrite it; got {st:?}"
        );
    }

    #[test]
    fn missing_when_target_absent_and_not_ejected() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let missing = target.path().join("not-here");

        let st = state(&missing, &[], &reg);

        assert!(
            matches!(st, ArtifactState::Missing),
            "absent target + not ejected => Missing, got {st:?}"
        );
    }

    #[test]
    fn foreign_when_target_exists_but_no_record() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        std::fs::write(target.path().join("a.json"), b"{}").expect("write file");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Foreign),
            "existing target with no registry record => Foreign, got {st:?}"
        );
    }

    fn sibling_record(
        artifact: &str,
        kind: crate::store::RecordKind,
        linked: bool,
    ) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: TARGET.to_owned(),
                source: SOURCE.to_owned(),
                artifact: artifact.to_owned(),
            },
            source: SOURCE.to_owned(),
            commit: if linked {
                "link".to_owned()
            } else {
                COMMIT.to_owned()
            },
            digest: "link:".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked,
            vars_digest: None,
        }
    }

    #[test]
    fn collapsed_dir_key_over_recorded_per_leaf_reads_outdated_not_foreign() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let dir = target.path().join("snippets");
        std::fs::create_dir_all(&dir).expect("mkdir dir");

        reg.put(&sibling_record(
            "snippets/a.json",
            crate::store::RecordKind::File,
            true,
        ))
        .expect("put per-leaf record");

        let dir_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "snippets".to_owned(),
        };
        let st = check_artifact_state(&dir, SOURCE, COMMIT, &[], "snippets", &reg, &dir_key, None)
            .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "a plan that collapsed `snippets` this run while the prior run recorded it per-leaf \
             (`snippets/a.json`) must redeploy under the new key, not read it as Foreign; got {st:?}"
        );
    }

    #[test]
    fn per_leaf_key_under_recorded_collapsed_dir_reads_outdated_not_foreign() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let leaf = target.path().join("snippets").join("a.json");
        std::fs::create_dir_all(leaf.parent().expect("leaf parent")).expect("mkdir");
        std::fs::write(&leaf, b"{}").expect("write leaf");

        reg.put(&sibling_record(
            "snippets",
            crate::store::RecordKind::Dir,
            true,
        ))
        .expect("put collapsed record");

        let leaf_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "snippets/a.json".to_owned(),
        };
        let st = check_artifact_state(
            &leaf,
            SOURCE,
            COMMIT,
            &[],
            "snippets/a.json",
            &reg,
            &leaf_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "a plan that split `snippets` into per-leaf this run while the prior run recorded the \
             collapsed dir must redeploy under the new key, not read it as Foreign; got {st:?}"
        );
    }

    #[test]
    fn unrelated_source_sibling_stays_foreign() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let dir = target.path().join("snippets");
        std::fs::create_dir_all(&dir).expect("mkdir dir");

        reg.put(&sibling_record(
            "snippets/a.json",
            crate::store::RecordKind::File,
            true,
        ))
        .expect("put per-leaf record");

        let dir_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: "other-source".to_owned(),
            artifact: "snippets".to_owned(),
        };
        let st = check_artifact_state(
            &dir,
            "other-source",
            COMMIT,
            &[],
            "snippets",
            &reg,
            &dir_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Foreign),
            "a sibling-shaped record from a DIFFERENT source is not this artifact under another \
             shape; the dir stays Foreign, got {st:?}"
        );
    }

    #[test]
    fn foreign_when_record_source_differs() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            "other-source",
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Foreign),
            "record is findable under key (source={SOURCE}) yet expected_source is other-source: \
             record.key.source != expected_source => Foreign, got {st:?}"
        );
    }

    #[test]
    fn outdated_when_commit_advanced_but_files_clean() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            "other-commit",
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "same source, lock advanced past the deployed commit, on-disk files still match the \
             record => Outdated (a managed artifact to redeploy), not Foreign, got {st:?}"
        );
    }

    #[test]
    fn modified_when_commit_advanced_and_files_diverge() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        std::fs::write(target.path().join("a.json"), b"locally edited").expect("tamper file");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            "other-commit",
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Modified { .. }),
            "a user edit must read as Modified even when the lock also advanced, so the redeploy \
             warns instead of silently clobbering local changes, got {st:?}"
        );
    }

    // ── linked artifacts (DLD-005) ─────────────────────────────────

    /// A linked record: no manifest files, sentinel commit/digest, `linked = true`.
    fn linked_record() -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::store::RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: true,
            vars_digest: None,
        }
    }

    /// A symlink deployed at the key, pointing at a live directory, with a linked record
    /// whose sentinel commit deliberately mismatches `expected_commit`. The linked
    /// short-circuit must fire BEFORE the commit/source Foreign check, yielding Linked.
    #[test]
    fn linked_record_reads_linked_not_foreign_despite_commit_mismatch() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let live = parent.path().join("worktree-artifact");
        std::fs::create_dir_all(&live).expect("mkdir live target");
        std::fs::write(live.join("snippets.json"), b"{}").expect("write live file");
        let dst = parent.path().join("deployed-link");
        symlink(&live, &dst).expect("deploy symlink");

        reg.put(&linked_record()).expect("put linked record");

        let st = check_artifact_state(&dst, SOURCE, COMMIT, &[], ARTIFACT, &reg, &key(), None)
            .expect("check_artifact_state on a linked symlink");

        assert!(
            matches!(st, ArtifactState::Linked),
            "a linked record must short-circuit to Linked BEFORE the commit-mismatch Foreign \
             check (sentinel commit `link` != expected {COMMIT}), got {st:?}"
        );
    }

    /// Even when the symlink target's content diverges from anything recorded, a linked
    /// artifact must never be reported Modified — it is quarantined from per-file drift.
    #[test]
    fn linked_record_never_reads_modified_when_target_content_differs() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let live = parent.path().join("worktree-artifact");
        std::fs::create_dir_all(&live).expect("mkdir live target");
        std::fs::write(live.join("anything.json"), b"locally edited content")
            .expect("write divergent file");
        let dst = parent.path().join("deployed-link");
        symlink(&live, &dst).expect("deploy symlink");

        reg.put(&linked_record()).expect("put linked record");

        let st = state(&dst, &[], &reg);

        assert!(
            matches!(st, ArtifactState::Linked),
            "a linked record is quarantined from per-file drift; it must read Linked, never \
             Modified, even when the live target content changes, got {st:?}"
        );
    }

    /// A dangling linked symlink (its target deleted): `try_exists` follows the link and
    /// returns Ok(false), so the state is Missing — and the call must not crash.
    #[test]
    fn dangling_linked_symlink_reads_missing_without_crashing() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let gone = parent.path().join("deleted-worktree-artifact");
        let dst = parent.path().join("deployed-link");
        symlink(&gone, &dst).expect("deploy dangling symlink");
        assert!(
            !gone.exists(),
            "premise: the symlink target must be absent so the link dangles"
        );

        reg.put(&linked_record()).expect("put linked record");

        let st = state(&dst, &[], &reg);

        assert!(
            matches!(st, ArtifactState::Missing),
            "a dangling linked symlink follows to a non-existent target => Missing (redeploy), \
             and must not error, got {st:?}"
        );
    }

    #[test]
    fn clean_when_disk_matches_record() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        reg.put(&record).expect("put record");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "every recorded file present with matching size+mtime, no extras => Clean, got {st:?}"
        );
    }

    #[test]
    fn modified_when_recorded_file_size_changed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        let edited = target.path().join("a.json");
        std::fs::write(&edited, b"{\"changed\": true}").expect("rewrite file");
        set_mtime(&edited, record.files[0].mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("size change must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the size-changed file must appear in `changed`, got {changed:?}"
        );
    }

    /// Reframes the former `modified_when_recorded_file_mtime_changed` bug pin.
    #[test]
    fn revalidated_when_only_mtime_changed_but_bytes_identical() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        let new_mtime = record.files[0].mtime + 999;
        set_mtime(&target.path().join("a.json"), new_mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "a touched-but-byte-identical file must escalate to its recorded blake3, match, \
                 and reclassify from the false-positive Modified to Revalidated, got {st:?}"
            );
        };
        let entry = fresh
            .iter()
            .find(|f| f.path == *Path::new("a.json"))
            .unwrap_or_else(|| {
                panic!("Revalidated must carry fresh stat for the revalidated file, got {fresh:?}")
            });
        assert_eq!(
            entry.mtime, new_mtime,
            "fresh stat must carry the NEW on-disk mtime so the refresh returns it to the fast path"
        );
        assert_eq!(
            entry.size, record.files[0].size,
            "the byte-identical file's size is unchanged and must be carried through as recorded"
        );
    }

    #[test]
    fn modified_when_bytes_differ_at_same_size_with_bumped_mtime() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        let edited = target.path().join("a.json");
        std::fs::write(&edited, b"[]").expect("rewrite to same-length different content");
        set_mtime(&edited, record.files[0].mtime + 5);
        assert_eq!(
            std::fs::metadata(&edited).expect("edited meta").len(),
            record.files[0].size,
            "premise: the edit preserves byte length so only mtime + content diverge"
        );

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!(
                "a same-size content change must read Modified once the mtime drift forces a \
                 blake3 escalation that mismatches the recorded hash, got {st:?}"
            );
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the genuinely edited file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn revalidated_when_all_stat_divergent_files_are_byte_identical() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        reg.put(&record).expect("put record");
        let a_mtime = record.files[0].mtime + 100;
        let b_mtime = record.files[1].mtime + 200;
        set_mtime(&target.path().join("a.json"), a_mtime);
        set_mtime(&target.path().join("b.txt"), b_mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "when every stat-divergent file hash-matches its record the artifact must be \
                 Revalidated, not Modified, got {st:?}"
            );
        };
        let a = fresh
            .iter()
            .find(|f| f.path == *Path::new("a.json"))
            .unwrap_or_else(|| panic!("fresh must include a.json, got {fresh:?}"));
        let b = fresh
            .iter()
            .find(|f| f.path == *Path::new("b.txt"))
            .unwrap_or_else(|| panic!("fresh must include b.txt, got {fresh:?}"));
        assert_eq!(
            a.mtime, a_mtime,
            "a.json fresh stat must carry its new mtime"
        );
        assert_eq!(
            b.mtime, b_mtime,
            "b.txt fresh stat must carry its new mtime"
        );
    }

    /// Partial-drift rule (DGI-D4): one real edit collapses the artifact to Modified and the
    /// touched sibling's revalidation is discarded, never half-persisted.
    #[test]
    fn modified_no_refresh_when_one_file_edited_and_sibling_only_touched() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        reg.put(&record).expect("put record");
        let a = target.path().join("a.json");
        std::fs::write(&a, b"[]").expect("same-length edit of a.json");
        set_mtime(&a, record.files[0].mtime + 13);
        assert_eq!(
            std::fs::metadata(&a).expect("a meta").len(),
            record.files[0].size,
            "premise: a.json keeps its recorded byte length so the size gate passes and only the \
             hash-miss can catch the edit"
        );
        set_mtime(&target.path().join("b.txt"), record.files[1].mtime + 777);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!(
                "any genuinely changed file must collapse the artifact to Modified — the touched \
                 sibling's revalidation must be discarded, never surfaced as Revalidated, got {st:?}"
            );
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the same-size, hash-mismatched file must appear in `changed`, got {changed:?}"
        );
        let after = reg
            .get(&key())
            .expect("get after classify")
            .expect("record still present");
        assert_eq!(
            after, record,
            "a Modified artifact must never half-persist a refreshed stat for its touched-but-\
             identical sibling: the stored record stays byte-for-byte unchanged (DGI-D4)"
        );
    }

    /// Hot path (gestalt step 1): a size+mtime stat-match is classified without reading or
    /// hashing the file. A poisoned recorded blake3 over an untouched file stays invisible.
    #[test]
    fn hot_path_never_hashes_a_file_whose_size_and_mtime_match() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let mut record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        record.files[0].blake3 = "0".repeat(64);
        reg.put(&record).expect("put record with poisoned blake3");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "a size+mtime stat-match must classify Clean without hashing; the poisoned blake3 must \
             stay invisible on the hot path, got {st:?}"
        );
    }

    /// Hot path holds per-file: a poisoned-but-stat-matching A is never hashed while a
    /// touched-identical sibling B escalates, so the artifact reads `Revalidated { fresh: [B] }`.
    #[test]
    fn hot_path_skips_stat_matching_file_while_sibling_escalates() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let mut record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        record.files[0].blake3 = "0".repeat(64);
        reg.put(&record).expect("put record with poisoned A blake3");
        let b_mtime = record.files[1].mtime + 555;
        set_mtime(&target.path().join("b.txt"), b_mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "A is a stat-match and must never be hashed, so its poisoned blake3 stays invisible \
                 while B escalates and revalidates => Revalidated, got {st:?}"
            );
        };
        assert_eq!(
            fresh.len(),
            1,
            "only the stat-divergent sibling B may be refreshed; the stat-matching A must not \
             appear in `fresh`, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].path,
            PathBuf::from("b.txt"),
            "the single fresh entry must be B, never the stat-matching A, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].mtime, b_mtime,
            "B's fresh stat must carry its new mtime, got {fresh:?}"
        );
    }

    #[test]
    fn classification_of_revalidation_does_not_persist_or_mutate_the_record() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        let recorded_blake3 = record.files[0].blake3.clone();
        reg.put(&record).expect("put record");
        set_mtime(&target.path().join("a.json"), record.files[0].mtime + 42);

        let st = state(target.path(), &[], &reg);
        assert!(
            matches!(st, ArtifactState::Revalidated { .. }),
            "premise: the touched-identical file revalidates, got {st:?}"
        );

        let after = reg
            .get(&key())
            .expect("get after classify")
            .expect("record still present");
        assert_eq!(
            after.files[0].blake3, recorded_blake3,
            "classify must never rewrite mf.blake3 across a revalidation"
        );
        assert_eq!(
            after, record,
            "the read-only classify path must persist NOTHING: the stored record (including the \
             stale size/mtime) must be byte-for-byte unchanged"
        );
    }

    #[test]
    fn file_kind_revalidates_when_touched_but_bytes_identical() {
        let (_state_dir, reg) = registry();
        let dir = TempDir::new().expect("target dir");
        let file = dir.path().join("config.json");
        let record = deploy_and_record_file(&file, b"{}");
        reg.put(&record).expect("put record");
        let new_mtime = record.files[0].mtime + 314;
        set_mtime(&file, new_mtime);

        let st = state(&file, &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "the single-renamed-FILE path must escalate a touched-but-identical file to \
                 blake3 and read Revalidated, got {st:?}"
            );
        };
        assert_eq!(
            fresh.len(),
            1,
            "a single-file artifact must produce exactly one fresh entry, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].path, record.files[0].path,
            "the fresh entry must carry the recorded file's path, not the absolute target path or a \
             wrong leaf, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].size, record.files[0].size,
            "the byte-identical file's size is unchanged and must be carried through as recorded, \
             got {fresh:?}"
        );
        assert_eq!(
            fresh[0].mtime, new_mtime,
            "file-kind revalidation must carry the fresh on-disk mtime of the revalidated file, \
             got {fresh:?}"
        );
    }

    /// Fail-closed (DGI-D5): an unreadable stat-divergent file reads Modified, never Clean.
    #[cfg(unix)]
    #[test]
    fn modified_when_stat_divergent_file_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        let f = target.path().join("a.json");
        set_mtime(&f, record.files[0].mtime + 11);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

        // chmod 0o000 does not deny root: if the file still reads, perms are bypassed (running as
        // root, e.g. a CI container) and the fail-closed precondition cannot hold — skip.
        if std::fs::read(&f).is_ok() {
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644))
                .expect("restore perms");
            return;
        }

        let st = state(target.path(), &[], &reg);

        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms for tempdir cleanup");

        assert!(
            matches!(st, ArtifactState::Modified { .. }),
            "a permission/IO error reading a stat-divergent file during escalation must fail \
             closed to Modified, never silently Clean/Revalidated, got {st:?}"
        );
    }

    /// Collapsed/linked fallback (DGI-D6): empty `files[]` takes the stat-only short path.
    #[test]
    fn empty_files_record_reads_clean_without_escalation() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[], false);
        reg.put(&record).expect("put record");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "a record with empty files[] must take the existing stat-only short path and read \
             Clean, never hash or panic, got {st:?}"
        );
    }

    /// Collapsed/linked fallback (DGI-D6): a `linked = true` record short-circuits to Linked
    /// before any per-file escalation, even with on-disk content that would otherwise be hashed.
    #[test]
    fn linked_record_short_circuits_before_escalation() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        std::fs::write(
            target.path().join("a.json"),
            b"content that would be hashed",
        )
        .expect("write file under linked target");
        reg.put(&linked_record()).expect("put linked record");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Linked),
            "a linked record must take the Linked short-circuit before per-file revalidation, \
             never hashing or panicking, got {st:?}"
        );
    }

    #[test]
    fn modified_when_recorded_file_deleted() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}"), ("b.txt", b"x")], false);
        reg.put(&record).expect("put record");
        std::fs::remove_file(target.path().join("b.txt")).expect("delete file");

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("deleted recorded file must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("b.txt")),
            "the deleted recorded file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn modified_when_extra_untracked_file_present() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        std::fs::write(target.path().join("extra.tmp"), b"stray").expect("write extra");

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("extra on-disk file must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("extra.tmp")),
            "the extra untracked file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn modified_when_symlink_present_and_disallowed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        symlink("a.json", target.path().join("link.json")).expect("create symlink");

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("disallowed on-disk symlink must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("link.json")),
            "with allow_symlinks=false, the symlink must appear in `changed`, got {changed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn modified_when_recorded_file_replaced_by_symlink() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], true);
        reg.put(&record).expect("put record");

        let recorded_path = target.path().join("a.json");
        let recorded_size = record.files[0].size;
        let recorded_mtime = record.files[0].mtime;

        let decoy_dir = TempDir::new().expect("decoy dir");
        let decoy = decoy_dir.path().join("decoy.json");
        let decoy_bytes = usize::try_from(recorded_size).expect("recorded size fits usize");
        std::fs::write(&decoy, vec![0u8; decoy_bytes]).expect("write decoy");
        set_mtime(&decoy, recorded_mtime);
        assert_eq!(
            std::fs::metadata(&decoy).expect("decoy meta").len(),
            recorded_size,
            "decoy must match the recorded file's size so a metadata-follows-symlink impl is fooled"
        );

        std::fs::remove_file(&recorded_path).expect("remove recorded regular file");
        symlink(&decoy, &recorded_path).expect("replace recorded file with symlink");

        let followed = std::fs::metadata(&recorded_path).expect("followed meta");
        assert_eq!(
            followed.len(),
            recorded_size,
            "following the symlink must yield the decoy's matching size (the trap)"
        );

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!(
                "a recorded REGULAR file replaced on disk by a symlink whose target matches \
                 size+mtime must yield Modified; a metadata-follows-symlink stat is fooled into \
                 Clean. got {st:?}"
            );
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the recorded path is now a symlink, not the original regular file, so it must appear \
             in `changed`, got {changed:?}"
        );
    }

    // deploy_artifact / recovery_sweep

    /// `<target_parent>/.phora-stage/`: deploy stages here and cleans it up afterward.
    fn staging_base(target_parent: &Path) -> PathBuf {
        target_parent.join(".phora-stage")
    }

    /// Create the already-exported staging dir with the given files, mirroring what
    /// `backend.export_artifact` leaves before the swap.
    fn make_staging(staging_base: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let staging = staging_base.join("snippets-deadbeef");
        for (rel, contents) in files {
            let path = staging.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir staging parent");
            }
            std::fs::write(&path, contents).expect("write staged file");
        }
        staging
    }

    /// Record describing what `deploy_artifact` should persist; file metadata is filled
    /// from the staging dir so a post-deploy `check_artifact_state` reads as Clean.
    fn record_for(staging: &Path, files: &[(&str, &[u8])]) -> RegistryRecord {
        let mut manifest = Vec::new();
        for (rel, contents) in files {
            manifest.push(ManifestFile {
                path: PathBuf::from(rel),
                size: contents.len() as u64,
                mtime: mtime_secs(&staging.join(rel)),
                blake3: blake3::hash(contents).to_hex().to_string(),
            });
        }
        RegistryRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::store::RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: manifest,
            linked: false,
            vars_digest: None,
        }
    }

    /// True when `target_parent` holds any `.phora-stage*` entry (staging or backup leftover).
    fn has_phora_stage_leftover(target_parent: &Path) -> bool {
        std::fs::read_dir(target_parent)
            .expect("read target parent")
            .any(|e| {
                e.expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".phora-stage")
            })
    }

    fn read_dir_names(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn journal_for(reg: &FileRegistry) -> Journal {
        Journal::open(&reg.state_root().join("locks")).expect("open journal")
    }

    fn entry(
        base: &Path,
        staging: &Path,
        dst: &Path,
        record: &RegistryRecord,
        swap_completed: bool,
    ) -> JournalEntry {
        JournalEntry {
            staging_base: base.to_path_buf(),
            staging: staging.to_path_buf(),
            dst: dst.to_path_buf(),
            record: record.clone(),
            swap_completed,
        }
    }

    #[test]
    fn deploy_makes_dst_contain_exactly_the_staged_files() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        let files: &[(&str, &[u8])] = &[("a.json", b"{}"), ("nested/b.txt", b"hello")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg);

        deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

        assert_eq!(
            std::fs::read(dst.join("a.json")).expect("read a.json"),
            b"{}",
            "dst must contain the staged a.json byte-for-byte"
        );
        assert_eq!(
            std::fs::read(dst.join("nested/b.txt")).expect("read nested/b.txt"),
            b"hello",
            "dst must contain the staged nested/b.txt byte-for-byte"
        );
    }

    #[test]
    fn deploy_leaves_no_staging_or_backup_in_target_parent() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        let files: &[(&str, &[u8])] = &[("a.json", b"{}")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg);

        deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

        assert!(
            !has_phora_stage_leftover(parent.path()),
            "after a successful deploy the target parent must hold no .phora-stage* leftover, found {:?}",
            read_dir_names(parent.path())
        );
    }

    #[test]
    fn deploy_persists_the_registry_record() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        let files: &[(&str, &[u8])] = &[("a.json", b"{}")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg);

        deploy_artifact(&base, &staging, &dst, record.clone(), &jrnl, &reg)
            .expect("deploy must succeed");

        let got = reg
            .get(&key())
            .expect("get after deploy")
            .expect("record persisted after deploy");
        assert_eq!(
            got, record,
            "deploy must persist the record so a later get returns it field-for-field"
        );
        assert!(
            journal_for(&reg)
                .entries()
                .expect("read journal after deploy")
                .is_empty(),
            "a successful deploy must clear its journal intent so recovery never replays it"
        );
    }

    #[test]
    fn deploy_swaps_out_old_dst_content_for_new_staged_content() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        std::fs::create_dir_all(&dst).expect("mkdir old dst");
        std::fs::write(dst.join("old.json"), b"OLD").expect("write old content");

        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg);

        deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

        assert_eq!(
            std::fs::read(dst.join("a.json")).expect("read new a.json"),
            b"NEW",
            "dst must hold the new staged content after the swap"
        );
        assert!(
            !dst.join("old.json").exists(),
            "the old dst content must be swapped out, old.json must be gone"
        );
        assert!(
            !has_phora_stage_leftover(parent.path()),
            "the swapped-out old version (backup) must be cleaned up, found {:?}",
            read_dir_names(parent.path())
        );
    }

    /// Registry double that, at `put` time, records whether the write-ahead invariants already
    /// hold: the journal carries the intent and `dst` already holds the staged content. Reads
    /// delegate to an inner `FileRegistry`; `put` succeeds.
    struct OrderingProbeRegistry {
        inner: FileRegistry,
        journal_dir: PathBuf,
        dst: PathBuf,
        staged_content: Vec<u8>,
        journal_nonempty_at_put: std::cell::Cell<bool>,
        dst_held_staged_content_at_put: std::cell::Cell<bool>,
    }

    impl Registry for OrderingProbeRegistry {
        fn get(&self, key: &ArtifactKey) -> StoreResult<Option<RegistryRecord>> {
            self.inner.get(key)
        }
        fn put(&self, record: &RegistryRecord) -> StoreResult<()> {
            let journal = Journal::open(&self.journal_dir).expect("open journal at put time");
            let entries = journal.entries().expect("read journal at put time");
            self.journal_nonempty_at_put.set(!entries.is_empty());
            let on_disk = std::fs::read(self.dst.join("a.json")).unwrap_or_default();
            self.dst_held_staged_content_at_put
                .set(on_disk == self.staged_content);
            self.inner.put(record)
        }
        fn remove(&self, key: &ArtifactKey) -> StoreResult<()> {
            self.inner.remove(key)
        }
        fn list_target(&self, target: &str) -> StoreResult<Vec<RegistryRecord>> {
            self.inner.list_target(target)
        }
        fn list_all(&self) -> StoreResult<Vec<RegistryRecord>> {
            self.inner.list_all()
        }
        fn load_ejected(&self, target: &str) -> StoreResult<Vec<EjectedEntry>> {
            self.inner.load_ejected(target)
        }
        fn save_ejected(&self, target: &str, ejected: &[EjectedEntry]) -> StoreResult<()> {
            self.inner.save_ejected(target, ejected)
        }
        fn load_hook_state(&self, target: &str) -> StoreResult<Vec<HookState>> {
            self.inner.load_hook_state(target)
        }
        fn record_hook_success(
            &self,
            target: &str,
            hook_id: &str,
            digest_set: &BTreeSet<String>,
        ) -> StoreResult<()> {
            self.inner.record_hook_success(target, hook_id, digest_set)
        }
        fn locks_dir(&self) -> PathBuf {
            self.inner.locks_dir()
        }
    }

    #[test]
    fn deploy_journals_intent_before_put_and_swaps_before_put() {
        let dir = TempDir::new().expect("temp state root");
        let inner = FileRegistry::open(dir.path().to_path_buf()).expect("open inner registry");
        let journal_dir = inner.state_root().join("locks");
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let reg = OrderingProbeRegistry {
            inner,
            journal_dir: journal_dir.clone(),
            dst: dst.clone(),
            staged_content: b"NEW".to_vec(),
            journal_nonempty_at_put: std::cell::Cell::new(false),
            dst_held_staged_content_at_put: std::cell::Cell::new(false),
        };
        let jrnl = Journal::open(&journal_dir).expect("open journal");

        deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

        assert!(
            reg.journal_nonempty_at_put.get(),
            "deploy must append the intent to the journal BEFORE calling registry put (write-ahead): \
             the journal was empty at put time"
        );
        assert!(
            reg.dst_held_staged_content_at_put.get(),
            "deploy must complete the stage->dst swap BEFORE calling registry put: \
             dst did not hold the staged content at put time"
        );
    }

    /// Registry double whose `put` always fails; all reads delegate to an inner `FileRegistry`.
    struct FailingPutRegistry {
        inner: FileRegistry,
    }

    impl Registry for FailingPutRegistry {
        fn get(&self, key: &ArtifactKey) -> StoreResult<Option<RegistryRecord>> {
            self.inner.get(key)
        }
        fn put(&self, _record: &RegistryRecord) -> StoreResult<()> {
            Err(StoreError::Registry("injected put failure".to_owned()))
        }
        fn remove(&self, key: &ArtifactKey) -> StoreResult<()> {
            self.inner.remove(key)
        }
        fn list_target(&self, target: &str) -> StoreResult<Vec<RegistryRecord>> {
            self.inner.list_target(target)
        }
        fn list_all(&self) -> StoreResult<Vec<RegistryRecord>> {
            self.inner.list_all()
        }
        fn load_ejected(&self, target: &str) -> StoreResult<Vec<EjectedEntry>> {
            self.inner.load_ejected(target)
        }
        fn save_ejected(&self, target: &str, ejected: &[EjectedEntry]) -> StoreResult<()> {
            self.inner.save_ejected(target, ejected)
        }
        fn load_hook_state(&self, target: &str) -> StoreResult<Vec<HookState>> {
            self.inner.load_hook_state(target)
        }
        fn record_hook_success(
            &self,
            target: &str,
            hook_id: &str,
            digest_set: &BTreeSet<String>,
        ) -> StoreResult<()> {
            self.inner.record_hook_success(target, hook_id, digest_set)
        }
        fn locks_dir(&self) -> PathBuf {
            self.inner.locks_dir()
        }
    }

    #[test]
    fn deploy_rolls_back_to_original_content_when_put_fails() {
        let dir = TempDir::new().expect("temp state root");
        let reg = FailingPutRegistry {
            inner: FileRegistry::open(dir.path().to_path_buf()).expect("open inner registry"),
        };
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        std::fs::create_dir_all(&dst).expect("mkdir old dst");
        std::fs::write(dst.join("old.json"), b"ORIGINAL").expect("write original content");

        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg.inner);

        let result = deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg);

        assert!(
            result.is_err(),
            "a failing registry put must make deploy_artifact return Err"
        );
        assert_eq!(
            std::fs::read(dst.join("old.json")).expect("read rolled-back content"),
            b"ORIGINAL",
            "on put failure the destination must be rolled back to its original pre-deploy content"
        );
        assert!(
            !dst.join("a.json").exists(),
            "rollback must remove the new-but-untracked install; a.json must not survive"
        );
        assert!(
            journal_for(&reg.inner)
                .entries()
                .expect("read journal after rollback")
                .is_empty(),
            "after rolling back a failed deploy the journal intent must be cleared so the next \
             recovery sweep does not replay the rolled-back swap"
        );
    }

    #[test]
    fn deploy_rolls_back_to_absent_when_dst_did_not_exist_and_put_fails() {
        let dir = TempDir::new().expect("temp state root");
        let reg = FailingPutRegistry {
            inner: FileRegistry::open(dir.path().to_path_buf()).expect("open inner registry"),
        };
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");

        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg.inner);

        let result = deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg);

        assert!(result.is_err(), "failing put must yield Err");
        assert!(
            !dst.exists(),
            "dst absent before deploy => after a failed put it must be absent again, never a partial install"
        );
        assert!(
            journal_for(&reg.inner)
                .entries()
                .expect("read journal after rollback")
                .is_empty(),
            "after rolling back a failed deploy the journal intent must be cleared so the next \
             recovery sweep does not replay the rolled-back swap"
        );
    }

    #[test]
    fn recovery_finishes_registry_write_when_swap_completed_but_put_did_not() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg);

        // State after a real stage->dst rename: dst holds the new content, the staging dir is
        // gone (renamed away), the journal has swap_completed=true, the registry has no record.
        jrnl.append(&entry(&base, &staging, &dst, &record, true))
            .expect("append swap-completed intent");
        std::fs::rename(&staging, &dst).expect("simulate completed stage->dst rename");
        std::fs::remove_dir_all(&base).expect("staging base removed after the move");
        assert_eq!(
            std::fs::read(dst.join("a.json")).expect("read dst after simulated swap"),
            b"NEW",
            "premise: dst already holds the new staged content (swap completed)"
        );
        assert!(
            !staging.exists(),
            "premise: the staging dir was renamed away, not left behind"
        );
        assert!(
            reg.get(&key()).expect("pre-sweep get").is_none(),
            "premise: registry has no record yet (the crash happened before put)"
        );

        recovery_sweep(parent.path(), &jrnl, &reg).expect("recovery sweep must succeed");

        let persisted = reg
            .get(&key())
            .expect("post-sweep get")
            .expect("swap-completed-but-put-missing must be reconciled by finishing the put");
        assert_eq!(
            persisted, record,
            "recovery must persist exactly the journal entry's record, field-for-field"
        );
        assert!(
            journal_for(&reg)
                .entries()
                .expect("read journal after sweep")
                .is_empty(),
            "the journal must be cleared once the entry is reconciled"
        );
    }

    #[test]
    fn recovery_discards_staging_and_leaves_dst_unchanged_when_swap_did_not_complete() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        std::fs::create_dir_all(&dst).expect("mkdir dst");
        std::fs::write(dst.join("untouched.json"), b"BEFORE").expect("write dst content");

        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg);
        jrnl.append(&entry(&base, &staging, &dst, &record, false))
            .expect("append swap-pending intent");

        recovery_sweep(parent.path(), &jrnl, &reg).expect("recovery sweep must succeed");

        assert_eq!(
            std::fs::read(dst.join("untouched.json")).expect("read dst"),
            b"BEFORE",
            "an incomplete swap must leave the destination exactly as it was"
        );
        assert!(
            reg.get(&key()).expect("post-sweep get").is_none(),
            "an incomplete swap must not produce a registry record"
        );
        assert!(
            !has_phora_stage_leftover(parent.path()),
            "recovery must discard the staging dir of the incomplete swap, found {:?}",
            read_dir_names(parent.path())
        );
        assert!(
            journal_for(&reg)
                .entries()
                .expect("read journal after sweep")
                .is_empty(),
            "the journal entry for the discarded incomplete swap must be cleared after the sweep"
        );
    }

    /// Mirrors the impl's backup naming: `<staging_base>/.phora-backup-<dst-leaf>`.
    fn backup_for(base: &Path, dst: &Path) -> PathBuf {
        let leaf = dst.file_name().map_or_else(
            || "artifact".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        base.join(format!(".phora-backup-{leaf}"))
    }

    #[test]
    fn recovery_restores_backup_when_swap_was_incomplete() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");

        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);

        let backup = backup_for(&base, &dst);
        std::fs::create_dir_all(&backup).expect("mkdir backup");
        std::fs::write(backup.join("old.json"), b"OLD").expect("write old backup content");

        let jrnl = journal_for(&reg);
        jrnl.append(&entry(&base, &staging, &dst, &record, false))
            .expect("append swap-incomplete intent");

        assert!(
            !dst.exists(),
            "premise: dst is missing (it was renamed to backup, swap not yet done)"
        );

        recovery_sweep(parent.path(), &jrnl, &reg).expect("recovery sweep must succeed");

        assert!(
            dst.exists(),
            "an incomplete swap with dst already renamed to backup must restore dst, not leave it missing"
        );
        assert_eq!(
            std::fs::read(dst.join("old.json")).expect("read restored dst"),
            b"OLD",
            "recovery must restore the ORIGINAL/backup content to dst, not the new staged content"
        );
        assert!(
            !dst.join("a.json").exists(),
            "the new staged content must NOT be installed for an incomplete swap"
        );
        assert!(
            reg.get(&key()).expect("post-sweep get").is_none(),
            "an incomplete swap must not produce a registry record"
        );
        assert!(
            journal_for(&reg)
                .entries()
                .expect("read journal after sweep")
                .is_empty(),
            "the journal entry for the reverted incomplete swap must be cleared after the sweep"
        );
    }

    #[test]
    fn deploy_preserves_unrelated_staging_in_shared_base() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");

        let files: &[(&str, &[u8])] = &[("a.json", b"{}")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);

        let sibling = base.join("b-nonce");
        std::fs::create_dir_all(&sibling).expect("mkdir sibling staging");
        let marker = sibling.join("b-marker.txt");
        std::fs::write(&marker, b"B is still pending").expect("write sibling marker");

        let jrnl = journal_for(&reg);

        deploy_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy A must succeed");

        assert_eq!(
            std::fs::read(dst.join("a.json")).expect("read deployed A"),
            b"{}",
            "artifact A must be deployed to its dst"
        );
        assert!(
            sibling.exists(),
            "deploying A must not delete sibling artifact B's staging dir in the shared base"
        );
        assert_eq!(
            std::fs::read(&marker).expect("read sibling marker"),
            b"B is still pending",
            "B's pending staging content must survive A's cleanup"
        );
        assert!(
            !staging.exists(),
            "A's own staging dir must be cleaned up after deploy"
        );
        assert!(
            !backup_for(&base, &dst).exists(),
            "A's own backup must be cleaned up after deploy"
        );
    }

    #[test]
    fn recovery_removes_orphaned_phora_stage_left_by_previous_crash() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let base = staging_base(parent.path());
        std::fs::create_dir_all(base.join("orphan-cafef00d")).expect("mkdir orphan staging");
        std::fs::write(base.join("orphan-cafef00d").join("leftover.txt"), b"x")
            .expect("write orphan file");
        assert!(
            has_phora_stage_leftover(parent.path()),
            "premise: an orphaned .phora-stage exists before the sweep"
        );
        let jrnl = journal_for(&reg);

        recovery_sweep(parent.path(), &jrnl, &reg).expect("recovery sweep must succeed");

        assert!(
            !has_phora_stage_leftover(parent.path()),
            "the startup recovery sweep must remove orphaned .phora-stage* dirs, found {:?}",
            read_dir_names(parent.path())
        );
    }

    // ── per-artifact vars digest (TPH-010) ─────────────────────────

    /// A clean deployment whose record carries `vars_digest`; on-disk files match
    /// the record so only the vars-digest comparison can move the state.
    fn deploy_and_record_with_vars(
        target: &Path,
        files: &[(&str, &[u8])],
        vars_digest: Option<&str>,
    ) -> RegistryRecord {
        let mut record = deploy_and_record(target, files, false);
        record.vars_digest = vars_digest.map(str::to_owned);
        record
    }

    #[test]
    fn outdated_when_vars_digest_differs_and_files_clean() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record =
            deploy_and_record_with_vars(target.path(), &[("a.json", b"{}")], Some("blake3:old"));
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            Some("blake3:new"),
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "same source, same commit, on-disk files clean, but the templating vars changed since \
             deploy (record vars_digest blake3:old != expected blake3:new) => Outdated so the \
             artifact re-renders and redeploys without --force, got {st:?}"
        );
    }

    #[test]
    fn clean_when_vars_digest_matches_and_nothing_else_changed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record =
            deploy_and_record_with_vars(target.path(), &[("a.json", b"{}")], Some("blake3:same"));
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            Some("blake3:same"),
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Clean),
            "record vars_digest equals the expected current vars_digest and nothing else drifted \
             => Clean, no needless re-render, got {st:?}"
        );
    }

    #[test]
    fn clean_when_feature_free_record_has_no_vars_digest() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record_with_vars(target.path(), &[("a.json", b"{}")], None);
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            Some("blake3:current-vars"),
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Clean),
            "a feature-free artifact (rendered no template, so vars_digest is None) must be \
             unaffected by a vars change: it stays Clean even when an expected vars_digest is \
             supplied (INV-8), got {st:?}"
        );
    }
}
