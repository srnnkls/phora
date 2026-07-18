//! Deployment: drift detection, copy/scan, atomic directory swap.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::{Registry, RegistryRecord};
use crate::sync::scan::scan_dir_strict;

pub use crate::sync::inspect::{ArtifactState, check_artifact_state};

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
    mode: JournalMode,
}

enum JournalMode {
    Writable,
    ReadOnly { root: PathBuf },
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
            mode: JournalMode::Writable,
        })
    }

    /// A frozen sync's read-only journal: never creates `locks_dir`, replays existing
    /// entries, and refuses any write, naming the state root instead of raising EACCES.
    #[must_use]
    pub fn open_readonly(locks_dir: &Path) -> Self {
        let root = locks_dir
            .parent()
            .map_or_else(|| locks_dir.to_path_buf(), Path::to_path_buf);
        Self {
            path: locks_dir.join("journal.toml"),
            mode: JournalMode::ReadOnly { root },
        }
    }

    #[must_use]
    pub fn refuses_writes(&self) -> bool {
        matches!(self.mode, JournalMode::ReadOnly { .. })
    }

    /// The read-only pending-work error naming the state root; meaningful only when
    /// [`refuses_writes`](Self::refuses_writes) holds.
    #[must_use]
    pub fn readonly_error(&self) -> Error {
        match &self.mode {
            JournalMode::ReadOnly { root } => {
                Error::StoreCtx(crate::store::readonly_root_error(root))
            }
            JournalMode::Writable => {
                unreachable!("readonly_error called on a writable journal")
            }
        }
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
        if self.refuses_writes() {
            return Err(self.readonly_error());
        }
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
    let entries = journal.entries()?;
    if journal.refuses_writes() && !entries.is_empty() {
        return Err(journal.readonly_error());
    }
    for entry in entries {
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
    use crate::store::{
        ArtifactKey, EjectedEntry, FileRegistry, HookState, ManifestFile, StoreError,
    };

    type StoreResult<T> = std::result::Result<T, StoreError>;
    use std::collections::BTreeSet;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn read_mtime_secs(path: &Path) -> u64 {
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
            read_mtime_secs(&dst),
            known,
            "copy_file must explicitly carry the src mtime onto dst (reflink does not copy mtime)"
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

    // deploy_artifact / recovery_sweep

    /// `<target_parent>/.phora-stage/`: deploy stages here and cleans it up afterward.
    fn staging_base(target_parent: &Path) -> PathBuf {
        target_parent.join(".phora-stage")
    }

    /// Create the already-staged dir with the given files, mirroring what
    /// `stage_artifact` leaves before the swap.
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
                mtime: read_mtime_secs(&staging.join(rel)),
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
            deploy_root: None,
            layout_separator: None,
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

    #[test]
    fn frozen_recovery_sweep_with_a_pending_entry_refuses_before_discarding_staging() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");
        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);

        journal_for(&reg)
            .append(&entry(&base, &staging, &dst, &record, false))
            .expect("seed a crashed-swap intent through a writable journal");

        let readonly = Journal::open_readonly(&reg.state_root().join("locks"));
        let err = recovery_sweep(parent.path(), &readonly, &reg).expect_err(
            "a frozen recovery sweep with a pending journal entry must refuse before mutating",
        );

        let msg = err.to_string();
        assert!(
            msg.contains(&reg.state_root().display().to_string())
                && msg.to_lowercase().contains("read-only"),
            "the refusal must name the read-only state root, got: {msg}"
        );
        assert!(
            staging.exists(),
            "the sweep must fail BEFORE discarding the pending swap's staging dir — a read-only \
             frozen sync performs zero mutation when there is pending recovery work"
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
}
