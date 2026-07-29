//! Target-side artifact copy, link, and atomic swap operations.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::sync::scan::scan_dir_strict;
use crate::sync::state::{ArtifactRecord, StateStore};

use super::SyncWarning;
use super::journal::{Journal, JournalEntry};
use super::recovery::{backup_path, remove_path, rollback_swap};
use super::request::SyncEvents;

#[cfg(test)]
use super::recovery::recovery_sweep;

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

pub fn apply_artifact(
    staging_base: &Path,
    staging: &Path,
    dst: &Path,
    record: ArtifactRecord,
    journal: &Journal,
    registry: &dyn StateStore,
) -> Result<()> {
    let mut events = SyncEvents::default();
    apply_artifact_report(
        staging_base,
        staging,
        dst,
        record,
        journal,
        registry,
        &mut events,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "caller hands off ownership of the record being deployed"
)]
pub(super) fn apply_artifact_report(
    staging_base: &Path,
    staging: &Path,
    dst: &Path,
    record: ArtifactRecord,
    journal: &Journal,
    registry: &dyn StateStore,
    events: &mut SyncEvents,
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

    swap_into(staging, dst, record.allow_symlinks, events)?;
    journal.mark_swap_completed(dst)?;

    if let Err(put_err) = registry.put_artifact(&record) {
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
    record: ArtifactRecord,
    journal: &Journal,
    registry: &dyn StateStore,
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

    let is_file = matches!(record.kind, crate::sync::state::RecordKind::File);
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

    if let Err(put_err) = registry.put_artifact(&record) {
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
fn swap_into(
    staging: &Path,
    dst: &Path,
    allow_symlinks: bool,
    events: &mut SyncEvents,
) -> Result<()> {
    match std::fs::rename(staging, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            events.warnings.push(SyncWarning::CrossDeviceFallback {
                destination: dst.to_path_buf(),
            });
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

fn is_cross_device(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::CrossesDevices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::state::{
        ArtifactKey, Ejection, FileStateStore, HookState, ManifestFile, StateError,
    };

    type StoreResult<T> = std::result::Result<T, StateError>;
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

    fn registry() -> (TempDir, FileStateStore) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileStateStore::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    // apply_artifact / recovery_sweep

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

    /// Record describing what `apply_artifact` should persist; file metadata is filled
    /// from the staging dir so a post-deploy `check_artifact_state` reads as Clean.
    fn record_for(staging: &Path, files: &[(&str, &[u8])]) -> ArtifactRecord {
        let mut manifest = Vec::new();
        for (rel, contents) in files {
            manifest.push(ManifestFile {
                path: PathBuf::from(rel),
                size: contents.len() as u64,
                mtime: read_mtime_secs(&staging.join(rel)),
                blake3: blake3::hash(contents).to_hex().to_string(),
            });
        }
        ArtifactRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::sync::state::RecordKind::Dir,
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

    fn journal_for(reg: &FileStateStore) -> Journal {
        Journal::open(&reg.state_root().join("locks")).expect("open journal")
    }

    fn entry(
        base: &Path,
        staging: &Path,
        dst: &Path,
        record: &ArtifactRecord,
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

        apply_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

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

        apply_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

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

        apply_artifact(&base, &staging, &dst, record.clone(), &jrnl, &reg)
            .expect("deploy must succeed");

        let got = reg
            .artifact(&key())
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

        apply_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

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

    /// `StateStore` double that, at `put` time, records whether the write-ahead invariants already
    /// hold: the journal carries the intent and `dst` already holds the staged content. Reads
    /// delegate to an inner `FileStateStore`; `put` succeeds.
    struct OrderingProbeRegistry {
        inner: FileStateStore,
        journal_dir: PathBuf,
        dst: PathBuf,
        staged_content: Vec<u8>,
        journal_nonempty_at_put: std::cell::Cell<bool>,
        dst_held_staged_content_at_put: std::cell::Cell<bool>,
    }

    impl StateStore for OrderingProbeRegistry {
        fn artifact(&self, key: &ArtifactKey) -> StoreResult<Option<ArtifactRecord>> {
            self.inner.artifact(key)
        }
        fn put_artifact(&self, record: &ArtifactRecord) -> StoreResult<()> {
            let journal = Journal::open(&self.journal_dir).expect("open journal at put time");
            let entries = journal.entries().expect("read journal at put time");
            self.journal_nonempty_at_put.set(!entries.is_empty());
            let on_disk = std::fs::read(self.dst.join("a.json")).unwrap_or_default();
            self.dst_held_staged_content_at_put
                .set(on_disk == self.staged_content);
            self.inner.put_artifact(record)
        }
        fn remove_artifact(&self, key: &ArtifactKey) -> StoreResult<()> {
            self.inner.remove_artifact(key)
        }
        fn target_artifacts(&self, target: &str) -> StoreResult<Vec<ArtifactRecord>> {
            self.inner.target_artifacts(target)
        }
        fn all_artifacts(&self) -> StoreResult<Vec<ArtifactRecord>> {
            self.inner.all_artifacts()
        }
        fn ejections(&self, target: &str) -> StoreResult<Vec<Ejection>> {
            self.inner.ejections(target)
        }
        fn save_ejections(&self, target: &str, ejected: &[Ejection]) -> StoreResult<()> {
            self.inner.save_ejections(target, ejected)
        }
        fn hook_state(&self, target: &str) -> StoreResult<Vec<HookState>> {
            self.inner.hook_state(target)
        }
        fn record_hook_success(
            &self,
            target: &str,
            hook_id: &str,
            digest_set: &BTreeSet<String>,
        ) -> StoreResult<()> {
            self.inner.record_hook_success(target, hook_id, digest_set)
        }
        fn acquire_lock(&self) -> StoreResult<crate::sync::state::StateLock> {
            self.inner.acquire_lock()
        }
        fn journal_root(&self) -> PathBuf {
            self.inner.journal_root()
        }
    }

    #[test]
    fn deploy_journals_intent_before_put_and_swaps_before_put() {
        let dir = TempDir::new().expect("temp state root");
        let inner = FileStateStore::open(dir.path().to_path_buf()).expect("open inner registry");
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

        apply_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy must succeed");

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

    /// `StateStore` double whose `put` always fails; all reads delegate to an inner `FileStateStore`.
    struct FailingPutRegistry {
        inner: FileStateStore,
    }

    impl StateStore for FailingPutRegistry {
        fn artifact(&self, key: &ArtifactKey) -> StoreResult<Option<ArtifactRecord>> {
            self.inner.artifact(key)
        }
        fn put_artifact(&self, _record: &ArtifactRecord) -> StoreResult<()> {
            Err(StateError::StateStore("injected put failure".to_owned()))
        }
        fn remove_artifact(&self, key: &ArtifactKey) -> StoreResult<()> {
            self.inner.remove_artifact(key)
        }
        fn target_artifacts(&self, target: &str) -> StoreResult<Vec<ArtifactRecord>> {
            self.inner.target_artifacts(target)
        }
        fn all_artifacts(&self) -> StoreResult<Vec<ArtifactRecord>> {
            self.inner.all_artifacts()
        }
        fn ejections(&self, target: &str) -> StoreResult<Vec<Ejection>> {
            self.inner.ejections(target)
        }
        fn save_ejections(&self, target: &str, ejected: &[Ejection]) -> StoreResult<()> {
            self.inner.save_ejections(target, ejected)
        }
        fn hook_state(&self, target: &str) -> StoreResult<Vec<HookState>> {
            self.inner.hook_state(target)
        }
        fn record_hook_success(
            &self,
            target: &str,
            hook_id: &str,
            digest_set: &BTreeSet<String>,
        ) -> StoreResult<()> {
            self.inner.record_hook_success(target, hook_id, digest_set)
        }
        fn acquire_lock(&self) -> StoreResult<crate::sync::state::StateLock> {
            self.inner.acquire_lock()
        }
        fn journal_root(&self) -> PathBuf {
            self.inner.journal_root()
        }
    }

    #[test]
    fn deploy_rolls_back_to_original_content_when_put_fails() {
        let dir = TempDir::new().expect("temp state root");
        let reg = FailingPutRegistry {
            inner: FileStateStore::open(dir.path().to_path_buf()).expect("open inner registry"),
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

        let result = apply_artifact(&base, &staging, &dst, record, &jrnl, &reg);

        assert!(
            result.is_err(),
            "a failing registry put must make apply_artifact return Err"
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
            inner: FileStateStore::open(dir.path().to_path_buf()).expect("open inner registry"),
        };
        let parent = TempDir::new().expect("target parent");
        let dst = parent.path().join("vscode");

        let files: &[(&str, &[u8])] = &[("a.json", b"NEW")];
        let base = staging_base(parent.path());
        let staging = make_staging(&base, files);
        let record = record_for(&staging, files);
        let jrnl = journal_for(&reg.inner);

        let result = apply_artifact(&base, &staging, &dst, record, &jrnl, &reg);

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
            reg.artifact(&key()).expect("pre-sweep get").is_none(),
            "premise: registry has no record yet (the crash happened before put)"
        );

        recovery_sweep(parent.path(), &jrnl, &reg).expect("recovery sweep must succeed");

        let persisted = reg
            .artifact(&key())
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
            reg.artifact(&key()).expect("post-sweep get").is_none(),
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
            reg.artifact(&key()).expect("post-sweep get").is_none(),
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

        apply_artifact(&base, &staging, &dst, record, &jrnl, &reg).expect("deploy A must succeed");

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
