//! T004 journal/recovery baselines (INV-9): byte-exact golden fixtures pinning the
//! crash-safety surface of `sync::{Journal, JournalEntry, recovery_sweep,
//! apply_artifact, link_artifact, copy_tree}` on unmoved `main`, so every journaled
//! intent, recovery outcome, and rollback path stays identical through the source →
//! projection → sync refactor. These are the INV-9 oracle the PR9/PR11 diffs measure
//! against (C1).
//!
//! Every pinned value is either content-addressed (per-file blake3 digests) or
//! caller-fixed (record fields, mtimes, and paths are chosen constants). Only the one
//! genuinely per-run-varying token — the tempdir root the fixture lives under — is
//! normalized, by its own literal value, so no unrelated digest is ever scrubbed.
//!
//! Serialized under `tests/compat/recovery/` — a directory distinct from the T002
//! `tests/compat/serialized/`, the T003 `tests/compat/staging/`, and the T004
//! `tests/compat/cli/` matrices — so the baselines never race on a shared fixture write.

#![cfg(unix)]

use std::fmt::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use phora::sync::apply::{apply_artifact, copy_tree, link_artifact};
use phora::sync::journal::{Journal, JournalEntry};
use phora::sync::recovery::recovery_sweep;
use phora::sync::state::{
    ArtifactKey, ArtifactRecord, Ejection, HookState, ManifestFile, RecordKind, StateError,
    StateStore,
};
use tempfile::TempDir;

/// The unmoved production baseline this matrix is pinned against (INV-9). HEAD has moved
/// past it with test-only commits, so regeneration guards on production parity rather
/// than `HEAD == this`.
const BASELINE_COMMIT: &str = "92c784e3b14496be25dcecc8d4500e32b52b1c50";

// ─── golden-fixture harness (reused verbatim from the T003 staging matrix) ──────

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat/recovery")
}

fn line_diff(old: &str, new: &str) -> String {
    let mut diff = String::new();
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    for i in 0..old_lines.len().max(new_lines.len()) {
        match (old_lines.get(i), new_lines.get(i)) {
            (a, b) if a == b => {}
            (a, b) => {
                if let Some(a) = a {
                    let _ = writeln!(diff, "- {a}");
                }
                if let Some(b) = b {
                    let _ = writeln!(diff, "+ {b}");
                }
            }
        }
    }
    let terminator = |s: &str| {
        if s.ends_with('\n') {
            "present"
        } else {
            "absent"
        }
    };
    if old.ends_with('\n') != new.ends_with('\n') {
        let _ = writeln!(
            diff,
            "~ final newline: old={} new={}",
            terminator(old),
            terminator(new),
        );
    }
    diff
}

/// `true` iff production (`src/`, `Cargo.toml`, `Cargo.lock`) is byte-identical to the
/// pinned baseline. `None` when git cannot answer, so regeneration fails closed.
fn production_matches_baseline() -> Option<bool> {
    let out = Command::new("git")
        .args([
            "diff",
            "--quiet",
            BASELINE_COMMIT,
            "HEAD",
            "--",
            "src/",
            "Cargo.toml",
            "Cargo.lock",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// `Some(reason)` when the production pathspec carries any uncommitted change or git cannot
/// answer, so regeneration fails closed. `None` only when git confirms production is clean.
fn tracked_worktree_is_dirty() -> Option<String> {
    let out = match Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            "src",
            "Cargo.toml",
            "Cargo.lock",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
    {
        Ok(out) if out.status.success() => out,
        Ok(out) => {
            return Some(format!(
                "git status failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            ));
        }
        Err(e) => return Some(format!("git status did not run: {e}")),
    };
    let dirty = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!dirty.is_empty()).then_some(dirty)
}

fn regenerate_golden(path: &Path, actual: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "PHORA_UPDATE_GOLDEN must never be set while CI is set: a golden fixture may not \
         self-bless in CI. Unset PHORA_UPDATE_GOLDEN and commit regenerated fixtures explicitly."
    );
    match production_matches_baseline() {
        Some(true) => {}
        Some(false) => panic!(
            "refusing to regenerate T004 recovery baselines: production (src/, Cargo.toml, \
             Cargo.lock) has diverged from the pinned baseline {BASELINE_COMMIT}, so a capture \
             would freeze post-refactor behavior instead of the baseline it must diff against."
        ),
        None => panic!(
            "refusing to regenerate T004 recovery baselines: `git diff` against {BASELINE_COMMIT} \
             did not run (git unavailable or not a repo); baselines must reflect unmoved production."
        ),
    }
    if let Some(reason) = tracked_worktree_is_dirty() {
        panic!(
            "refusing to regenerate T004 recovery baselines: the production pathspec (src/, \
             Cargo.toml, Cargo.lock) is not clean or git could not verify it, so the capture \
             would not reflect pinned production. Commit or stash production changes first \
             (untracked fixtures and edited test harnesses elsewhere are fine). Details: {reason}"
        );
    }
    if let Ok(old) = std::fs::read_to_string(path)
        && old != actual
    {
        eprintln!(
            "REGENERATING {} — bytes changed (old ↓ / new ↓):\n{}",
            path.display(),
            line_diff(&old, actual)
        );
    }
    std::fs::create_dir_all(path.parent().expect("golden parent")).expect("create golden dir");
    std::fs::write(path, actual).expect("write golden fixture");
}

fn assert_golden(name: &str, actual: &str) {
    let path = golden_dir().join(name);
    if let Some(val) = std::env::var_os("PHORA_UPDATE_GOLDEN") {
        assert_eq!(
            val,
            std::ffi::OsStr::new("1"),
            "PHORA_UPDATE_GOLDEN must be exactly \"1\" to regenerate golden fixtures, got {}; \
             refusing an ambiguous regeneration that could silently bless a regression",
            val.display()
        );
        regenerate_golden(&path, actual);
        return;
    }
    assert!(
        path.exists(),
        "golden fixture missing: {} (regenerate the T004 recovery baseline with PHORA_UPDATE_GOLDEN=1)",
        path.display()
    );
    let expected = std::fs::read_to_string(&path).expect("read golden fixture");
    assert_eq!(
        actual,
        expected,
        "journal/recovery bytes drifted from the pinned baseline {}",
        path.display()
    );
}

// ─── harness self-test (invariant, not a recovery golden) ───────────────────────

#[test]
fn line_diff_announces_final_newline_only_drift() {
    let dropped = line_diff("baseline\n", "baseline");
    assert!(
        !dropped.is_empty(),
        "line_diff must not be silent when the only byte change is a dropped final newline"
    );
    assert!(
        dropped.contains("newline"),
        "line_diff must name the final-newline drift, got: {dropped:?}"
    );
    let added = line_diff("baseline", "baseline\n");
    assert!(
        added.contains("newline"),
        "line_diff must name the final-newline drift, got: {added:?}"
    );
    assert_ne!(
        dropped, added,
        "dropped vs added final newline must render as distinct diffs"
    );
    assert!(
        !line_diff("same\n", "same\n").contains("newline"),
        "line_diff must not announce newline drift when both terminators match"
    );
}

// ─── deterministic fixtures ─────────────────────────────────────────────────────

const SOURCE: &str = "company-configs";
const TARGET: &str = "vscode";
const ARTIFACT: &str = "app.conf";
const COMMIT: &str = "abc123def456";
const PROJECTED_AT: &str = "2026-01-31T12:34:56Z";
/// Fixed mtime so the journaled manifest's mtime is a pinned constant, not a wall clock.
const FIXED_MTIME: u64 = 1_700_000_000;

fn key() -> ArtifactKey {
    ArtifactKey {
        target: TARGET.to_owned(),
        source: SOURCE.to_owned(),
        artifact: ARTIFACT.to_owned(),
    }
}

/// A managed dir record over one file with content-addressed blake3 and a fixed mtime.
fn record(contents: &[u8], kind: RecordKind, linked: bool) -> ArtifactRecord {
    ArtifactRecord {
        version: 1,
        key: key(),
        source: SOURCE.to_owned(),
        commit: if linked {
            "link".to_owned()
        } else {
            COMMIT.to_owned()
        },
        digest: "blake3:d4e5f6".to_owned(),
        projected_at: PROJECTED_AT.to_owned(),
        layout: "flat".to_owned(),
        kind,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from(ARTIFACT),
            size: contents.len() as u64,
            mtime: FIXED_MTIME,
            blake3: blake3::hash(contents).to_hex().to_string(),
        }],
        directories: None,
        linked,
        vars_digest: None,
        deploy_root: None,
        layout_separator: None,
    }
}

/// Replace the one per-run-varying token (the tempdir root) by its literal value, leaving
/// every content-addressed blake3 and fixed field byte-identical.
fn normalize(root: &Path, raw: &str) -> String {
    raw.replace(&root.to_string_lossy().into_owned(), "<ROOT>")
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

fn read_journal_bytes(locks_dir: &Path) -> String {
    std::fs::read_to_string(locks_dir.join("journal.toml")).expect("read journal.toml")
}

fn open_registry(root: &Path) -> phora::sync::state::FileStateStore {
    phora::sync::state::FileStateStore::open(root.join("state")).expect("open registry")
}

// ─── 1. journal serialization: intent appended before the swap ──────────────────

#[test]
fn journal_append_intent_is_byte_identical() {
    let root = TempDir::new().expect("tempdir");
    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");

    journal
        .append(&JournalEntry {
            staging_base: root.path().join("stage"),
            staging: root.path().join("stage/app.conf.new"),
            dst: root.path().join("deploy/app.conf"),
            record: record(b"new body\n", RecordKind::Dir, false),
            swap_completed: false,
        })
        .expect("append intent");

    let bytes = read_journal_bytes(&locks);
    assert!(
        bytes.contains("swap_completed = false"),
        "an appended-but-unsworn intent must serialize swap_completed = false; got:\n{bytes}"
    );
    assert_golden("journal_append.golden", &normalize(root.path(), &bytes));
}

// ─── 2. journal serialization: intent after the swap completed (before put) ─────

#[test]
fn journal_mark_swap_completed_is_byte_identical() {
    let root = TempDir::new().expect("tempdir");
    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    let dst = root.path().join("deploy/app.conf");

    journal
        .append(&JournalEntry {
            staging_base: root.path().join("stage"),
            staging: root.path().join("stage/app.conf.new"),
            dst: dst.clone(),
            record: record(b"new body\n", RecordKind::Dir, false),
            swap_completed: false,
        })
        .expect("append intent");
    journal
        .mark_swap_completed(&dst)
        .expect("mark swap completed");

    let bytes = read_journal_bytes(&locks);
    assert!(
        bytes.contains("swap_completed = true"),
        "after mark_swap_completed the intent must serialize swap_completed = true; got:\n{bytes}"
    );
    assert_golden(
        "journal_mark_completed.golden",
        &normalize(root.path(), &bytes),
    );
}

// ─── 3. journal serialization: a crash-safe symlink (link) intent ───────────────

#[test]
fn journal_link_intent_is_byte_identical() {
    let root = TempDir::new().expect("tempdir");
    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");

    journal
        .append(&JournalEntry {
            staging_base: root.path().join("stage"),
            staging: root.path().join("stage/link-app.conf-0"),
            dst: root.path().join("deploy/app.conf"),
            record: record(b"linked body\n", RecordKind::File, true),
            swap_completed: false,
        })
        .expect("append link intent");

    let bytes = read_journal_bytes(&locks);
    assert!(
        bytes.contains("linked = true") && bytes.contains("kind = \"file\""),
        "a link intent must journal a linked file record; got:\n{bytes}"
    );
    assert_golden(
        "journal_link_intent.golden",
        &normalize(root.path(), &bytes),
    );
}

struct SwapInterruption {
    golden: &'static str,
    dst_before: Option<&'static [u8]>,
    backup_before: Option<&'static [u8]>,
    staging_before: Option<&'static [u8]>,
}

fn assert_swap_interruption_recovers(case: &SwapInterruption) {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let dst = deploy.join("app.conf");
    let staging_base = deploy.join(".phora-stage");
    let staging = staging_base.join("app.conf.new");
    let backup = staging_base.join(".phora-backup-app.conf");

    if let Some(bytes) = case.dst_before {
        write(&dst, bytes);
    }
    if let Some(bytes) = case.backup_before {
        write(&backup, bytes);
    }
    if let Some(bytes) = case.staging_before {
        write(&staging, bytes);
    }

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    journal
        .append(&JournalEntry {
            staging_base: staging_base.clone(),
            staging: staging.clone(),
            dst: dst.clone(),
            record: record(b"new body\n", RecordKind::Dir, false),
            swap_completed: false,
        })
        .expect("append incomplete intent");

    let registry = open_registry(root.path());
    recovery_sweep(&deploy, &journal, &registry).expect("recovery sweep");

    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "dst_after_recovery = {:?}",
        std::fs::read_to_string(&dst).ok()
    );
    let _ = writeln!(doc, "staging_exists = {}", staging.exists());
    let _ = writeln!(doc, "backup_exists = {}", backup.exists());
    let _ = writeln!(
        doc,
        "record_committed = {}",
        registry.artifact(&key()).expect("registry get").is_some()
    );
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    assert_golden(case.golden, &normalize(root.path(), &doc));
}

// ─── 4a. interrupted after journal-append, before the backup rename ──────────────

#[test]
fn recovery_sweep_post_journal_append_pre_backup_keeps_original() {
    assert_swap_interruption_recovers(&SwapInterruption {
        golden: "recovery_post_append_pre_backup.golden",
        dst_before: Some(b"original body\n"),
        backup_before: None,
        staging_before: Some(b"new body\n"),
    });
}

// ─── 4b. interrupted after the backup rename, before the swap ────────────────────

#[test]
fn recovery_sweep_post_backup_rename_pre_swap_restores_backup() {
    assert_swap_interruption_recovers(&SwapInterruption {
        golden: "recovery_post_backup_pre_swap.golden",
        dst_before: None,
        backup_before: Some(b"original body\n"),
        staging_before: Some(b"new body\n"),
    });
}

// ─── 4c. interrupted after the swap, before mark-completed / registry-put ────────

#[test]
fn recovery_sweep_post_swap_pre_mark_rolls_back_to_backup() {
    assert_swap_interruption_recovers(&SwapInterruption {
        golden: "recovery_post_swap_pre_mark.golden",
        dst_before: Some(b"new body\n"),
        backup_before: Some(b"original body\n"),
        staging_before: None,
    });
}

// ─── 5. recovery of a completed swap: commit the pending registry put ───────────

#[test]
fn recovery_sweep_commits_record_on_completed_swap() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let dst = deploy.join("app.conf");
    write(&dst, b"new body\n");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    journal
        .append(&JournalEntry {
            staging_base: deploy.join(".phora-stage"),
            staging: deploy.join(".phora-stage/app.conf.new"),
            dst: dst.clone(),
            record: record(b"new body\n", RecordKind::Dir, false),
            swap_completed: true,
        })
        .expect("append completed intent");

    let registry = open_registry(root.path());
    recovery_sweep(&deploy, &journal, &registry).expect("recovery sweep");

    let committed = registry.artifact(&key()).expect("registry get");
    let mut doc = String::new();
    let _ = writeln!(doc, "record_committed = {}", committed.is_some());
    let _ = writeln!(
        doc,
        "committed_commit = {}",
        committed.as_ref().map_or("<none>", |r| r.commit.as_str())
    );
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    assert_golden(
        "recovery_completed_swap.golden",
        &normalize(root.path(), &doc),
    );
}

// ─── 6. recovery removes orphaned .phora-stage* dirs a crash left behind ────────

#[test]
fn recovery_sweep_removes_orphaned_staging_dirs() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let orphan = deploy.join(".phora-stage-abandoned");
    write(&orphan.join("leftover.txt"), b"crash residue\n");
    write(&deploy.join("kept.conf"), b"unrelated live file\n");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    let registry = open_registry(root.path());

    recovery_sweep(&deploy, &journal, &registry).expect("recovery sweep");

    let mut doc = String::new();
    let _ = writeln!(doc, "orphan_stage_exists = {}", orphan.exists());
    let _ = writeln!(
        doc,
        "unrelated_live_file_exists = {}",
        deploy.join("kept.conf").exists()
    );
    assert_golden(
        "recovery_orphan_staging.golden",
        &normalize(root.path(), &doc),
    );
}

// ─── 7. rollback on state-write failure: the prior target is restored ───────────

struct PutProbe {
    journal_bytes: String,
    dst_bytes: Option<Vec<u8>>,
    dst_is_symlink: bool,
}

struct ProbingRegistry {
    inner: phora::sync::state::FileStateStore,
    journal_path: PathBuf,
    dst: PathBuf,
    fail: bool,
    probe: Mutex<Option<PutProbe>>,
}

impl ProbingRegistry {
    fn new(root: &Path, locks: &Path, dst: &Path, fail: bool) -> Self {
        Self {
            inner: open_registry(root),
            journal_path: locks.join("journal.toml"),
            dst: dst.to_path_buf(),
            fail,
            probe: Mutex::new(None),
        }
    }

    fn captured(&self) -> PutProbe {
        self.probe
            .lock()
            .expect("probe lock")
            .take()
            .expect("registry.put must be reached: the write-ahead protocol commits through put")
    }
}

impl StateStore for ProbingRegistry {
    fn artifact(&self, key: &ArtifactKey) -> Result<Option<ArtifactRecord>, StateError> {
        self.inner.artifact(key)
    }
    fn put_artifact(&self, record: &ArtifactRecord) -> Result<(), StateError> {
        let dst_is_symlink =
            std::fs::symlink_metadata(&self.dst).is_ok_and(|m| m.file_type().is_symlink());
        let dst_bytes = if dst_is_symlink {
            None
        } else {
            std::fs::read(&self.dst).ok()
        };
        *self.probe.lock().expect("probe lock") = Some(PutProbe {
            journal_bytes: std::fs::read_to_string(&self.journal_path).unwrap_or_default(),
            dst_bytes,
            dst_is_symlink,
        });
        if self.fail {
            return Err(StateError::StateStore(
                "simulated state-write failure".to_owned(),
            ));
        }
        self.inner.put_artifact(record)
    }
    fn remove_artifact(&self, key: &ArtifactKey) -> Result<(), StateError> {
        self.inner.remove_artifact(key)
    }
    fn target_artifacts(&self, target: &str) -> Result<Vec<ArtifactRecord>, StateError> {
        self.inner.target_artifacts(target)
    }
    fn all_artifacts(&self) -> Result<Vec<ArtifactRecord>, StateError> {
        self.inner.all_artifacts()
    }
    fn ejections(&self, target: &str) -> Result<Vec<Ejection>, StateError> {
        self.inner.ejections(target)
    }
    fn save_ejections(&self, target: &str, ejected: &[Ejection]) -> Result<(), StateError> {
        self.inner.save_ejections(target, ejected)
    }
    fn hook_state(&self, target: &str) -> Result<Vec<HookState>, StateError> {
        self.inner.hook_state(target)
    }
    fn record_hook_success(
        &self,
        target: &str,
        hook_id: &str,
        digest_set: &std::collections::BTreeSet<String>,
    ) -> Result<(), StateError> {
        self.inner.record_hook_success(target, hook_id, digest_set)
    }
    fn acquire_lock(&self) -> Result<phora::sync::state::StateLock, StateError> {
        self.inner.acquire_lock()
    }
    fn journal_root(&self) -> PathBuf {
        self.inner.journal_root()
    }
}

#[test]
fn deploy_artifact_rolls_back_target_when_state_write_fails() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let dst = deploy.join("app.conf");
    let staging_base = deploy.join(".phora-stage");
    let staging = staging_base.join("app.conf.new");

    // A prior clean deployment already occupies dst; the failing put must restore it.
    write(&dst, b"original body\n");
    write(&staging, b"new body\n");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    let registry = ProbingRegistry::new(root.path(), &locks, &dst, true);

    let err = apply_artifact(
        &staging_base,
        &staging,
        &dst,
        record(b"new body\n", RecordKind::Dir, false),
        &journal,
        &registry,
    )
    .expect_err("a failing state write must abort the deploy");

    let probe = registry.captured();
    assert!(
        probe.journal_bytes.contains("swap_completed = true"),
        "the swap must be journaled complete BEFORE put is attempted (write-ahead), got:\n{}",
        probe.journal_bytes
    );
    assert_eq!(
        probe.dst_bytes.as_deref(),
        Some(b"new body\n".as_ref()),
        "at put time the new bytes must already be swapped onto dst, so put is the commit point"
    );

    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "put_time_dst_bytes = {:?}",
        probe.dst_bytes.as_deref().map(String::from_utf8_lossy)
    );
    let _ = write!(
        doc,
        "put_time_journal_bytes =\n{}\n",
        normalize(root.path(), &probe.journal_bytes)
    );
    let _ = writeln!(doc, "error = {err}");
    let _ = writeln!(
        doc,
        "dst_after_rollback = {:?}",
        std::fs::read_to_string(&dst).ok()
    );
    let _ = writeln!(doc, "staging_exists = {}", staging.exists());
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    let _ = writeln!(
        doc,
        "record_committed = {}",
        registry.artifact(&key()).expect("get").is_some()
    );
    assert_golden(
        "rollback_state_write_failure.golden",
        &normalize(root.path(), &doc),
    );
}

// ─── 7b. rollback with NO prior destination: the target is removed, not left behind ─

#[test]
fn deploy_artifact_rollback_removes_target_when_no_prior_destination() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let dst = deploy.join("app.conf");
    let staging_base = deploy.join(".phora-stage");
    let staging = staging_base.join("app.conf.new");

    // No prior deployment: dst is absent, so a failing put has no backup to restore.
    write(&staging, b"new body\n");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    let registry = ProbingRegistry::new(root.path(), &locks, &dst, true);

    let err = apply_artifact(
        &staging_base,
        &staging,
        &dst,
        record(b"new body\n", RecordKind::Dir, false),
        &journal,
        &registry,
    )
    .expect_err("a failing state write must abort the deploy");

    let probe = registry.captured();
    assert!(
        probe.journal_bytes.contains("swap_completed = true"),
        "the swap must be journaled complete BEFORE put is attempted (write-ahead), got:\n{}",
        probe.journal_bytes
    );
    assert_eq!(
        probe.dst_bytes.as_deref(),
        Some(b"new body\n".as_ref()),
        "at put time the new bytes must already be swapped onto a freshly-created dst"
    );

    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "put_time_dst_bytes = {:?}",
        probe.dst_bytes.as_deref().map(String::from_utf8_lossy)
    );
    let _ = write!(
        doc,
        "put_time_journal_bytes =\n{}\n",
        normalize(root.path(), &probe.journal_bytes)
    );
    let _ = writeln!(doc, "error = {err}");
    let _ = writeln!(doc, "dst_exists_after_rollback = {}", dst.exists());
    let _ = writeln!(doc, "staging_exists = {}", staging.exists());
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    let _ = writeln!(
        doc,
        "record_committed = {}",
        registry.artifact(&key()).expect("get").is_some()
    );
    assert_golden(
        "rollback_no_prior_destination.golden",
        &normalize(root.path(), &doc),
    );
}

// ─── 8. crash-safe link staging: link_artifact write-ahead-journals its symlink swap ─

#[test]
fn link_artifact_deploys_crash_safe_symlink() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let staging_base = deploy.join(".phora-stage");
    let dst = deploy.join("app.conf");
    let worktree_target = root.path().join("worktree/app.conf");
    write(&worktree_target, b"linked body\n");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    let registry = ProbingRegistry::new(root.path(), &locks, &dst, false);

    link_artifact(
        &staging_base,
        &dst,
        &worktree_target,
        record(b"linked body\n", RecordKind::File, true),
        &journal,
        &registry,
    )
    .expect("link_artifact deploys the symlink");

    let probe = registry.captured();
    assert!(
        probe.journal_bytes.contains("swap_completed = true"),
        "the link swap must be journaled complete BEFORE put (write-ahead), got:\n{}",
        probe.journal_bytes
    );
    assert!(
        probe.journal_bytes.contains("linked = true"),
        "the journaled intent at put time must record a linked artifact, got:\n{}",
        probe.journal_bytes
    );
    assert!(
        probe.dst_is_symlink,
        "at put time dst must already be the deployed symlink, so put is the commit point"
    );

    let mut doc = String::new();
    let _ = write!(
        doc,
        "put_time_journal_bytes =\n{}\n",
        normalize(root.path(), &probe.journal_bytes)
    );
    let _ = writeln!(doc, "put_time_dst_is_symlink = {}", probe.dst_is_symlink);
    let _ = writeln!(
        doc,
        "dst_is_symlink = {}",
        std::fs::symlink_metadata(&dst)
            .expect("stat dst")
            .file_type()
            .is_symlink()
    );
    let _ = writeln!(
        doc,
        "dst_link_target = {}",
        normalize(
            root.path(),
            &std::fs::read_link(&dst)
                .expect("read link")
                .to_string_lossy()
        )
    );
    let _ = writeln!(
        doc,
        "record_linked = {}",
        registry
            .artifact(&key())
            .expect("get")
            .is_some_and(|r| r.linked)
    );
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    assert_golden("link_crash_safe.golden", &normalize(root.path(), &doc));
}

// ─── 9. interrupted symlink deploy: recovery discards a pre-rename staged link ───

#[test]
fn recovery_sweep_discards_pre_rename_staged_symlink() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let dst = deploy.join("app.conf");
    let staging_base = deploy.join(".phora-stage");
    let staging = staging_base.join("link-app.conf-0");
    let worktree_target = root.path().join("worktree/app.conf");
    write(&worktree_target, b"linked body\n");
    std::fs::create_dir_all(&staging_base).expect("create staging base");
    symlink(&worktree_target, &staging).expect("stage the symlink");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    journal
        .append(&JournalEntry {
            staging_base: staging_base.clone(),
            staging: staging.clone(),
            dst: dst.clone(),
            record: record(b"linked body\n", RecordKind::File, true),
            swap_completed: false,
        })
        .expect("append pre-rename link intent");

    let registry = open_registry(root.path());
    recovery_sweep(&deploy, &journal, &registry).expect("recovery sweep");

    let mut doc = String::new();
    let _ = writeln!(doc, "dst_exists = {}", dst.try_exists().expect("stat dst"));
    let _ = writeln!(
        doc,
        "staging_exists = {}",
        staging.try_exists().expect("stat staging")
    );
    let _ = writeln!(
        doc,
        "record_committed = {}",
        registry.artifact(&key()).expect("get").is_some()
    );
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    assert_golden(
        "recovery_link_pre_rename.golden",
        &normalize(root.path(), &doc),
    );
}

// ─── 10. interrupted symlink deploy: post-rename, pre-put — the link stays ───────

#[test]
fn recovery_sweep_keeps_post_rename_symlink_uncommitted() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let dst = deploy.join("app.conf");
    let staging_base = deploy.join(".phora-stage");
    let staging = staging_base.join("link-app.conf-0");
    let worktree_target = root.path().join("worktree/app.conf");
    write(&worktree_target, b"linked body\n");
    std::fs::create_dir_all(&staging_base).expect("create staging base");
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).expect("create deploy dir");
    }
    symlink(&worktree_target, &dst).expect("place the deployed symlink");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    journal
        .append(&JournalEntry {
            staging_base: staging_base.clone(),
            staging: staging.clone(),
            dst: dst.clone(),
            record: record(b"linked body\n", RecordKind::File, true),
            swap_completed: false,
        })
        .expect("append post-rename link intent");

    let registry = open_registry(root.path());
    recovery_sweep(&deploy, &journal, &registry).expect("recovery sweep");

    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "dst_is_symlink = {}",
        std::fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_symlink())
    );
    let _ = writeln!(
        doc,
        "dst_link_target = {}",
        normalize(
            root.path(),
            &std::fs::read_link(&dst).map_or_else(
                |_| "<absent>".to_owned(),
                |p| p.to_string_lossy().into_owned()
            )
        )
    );
    let _ = writeln!(
        doc,
        "staging_exists = {}",
        staging.try_exists().expect("stat staging")
    );
    let _ = writeln!(
        doc,
        "record_committed = {}",
        registry.artifact(&key()).expect("get").is_some()
    );
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    assert_golden(
        "recovery_link_post_rename.golden",
        &normalize(root.path(), &doc),
    );
}

// ─── 11. cross-device fallback: copy_tree reproduces files + symlinks under a new mount ─

/// Sorted by relative path so the dump is order-independent; files render their content and
/// symlinks their target, both content-stable, so no wall-clock mtime enters the golden.
fn dump_tree(root: &Path) -> String {
    fn collect(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).expect("stat entry");
            let rel = path
                .strip_prefix(base)
                .expect("under base")
                .to_string_lossy()
                .replace('\\', "/");
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path)
                    .expect("read link")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel.clone(), format!("{rel} symlink -> {target}\n")));
            } else if meta.is_dir() {
                collect(&path, base, out);
            } else {
                let body = std::fs::read_to_string(&path).expect("read file");
                out.push((rel.clone(), format!("{rel} file {body:?}\n")));
            }
        }
    }
    let mut items = Vec::new();
    collect(root, root, &mut items);
    items.sort();
    items.into_iter().map(|(_, line)| line).collect()
}

#[test]
fn copy_tree_cross_device_fallback_output_is_byte_identical() {
    let root = TempDir::new().expect("tempdir");
    let src = root.path().join("src");
    let dst = root.path().join("dst");
    write(&src.join("config/app.conf"), b"app = 1\n");
    write(&src.join("config/nested/deep.toml"), b"[deep]\n");
    symlink("config/app.conf", src.join("link.conf")).expect("stage a relative symlink");

    copy_tree(&src, &dst, true).expect("copy_tree reproduces the tree across a device boundary");

    assert_golden(
        "copy_tree_cross_device.golden",
        &normalize(root.path(), &dump_tree(&dst)),
    );
}

// ─── 12. ejection directory-vs-leaf overlap: the dir ejects, the nested leaf stays managed ─

/// Sorted by relative path so the concatenated registry+meta dump is order-independent.
fn dump_state_toml(root: &Path) -> String {
    fn collect(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, base, out);
            } else if path.extension().is_some_and(|e| e == "toml") {
                let rel = path
                    .strip_prefix(base)
                    .expect("under base")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("read toml")));
            }
        }
    }
    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort();
    let mut doc = String::new();
    for (rel, body) in files {
        doc.push_str("--- ");
        doc.push_str(&rel);
        doc.push_str(" ---\n");
        doc.push_str(&body);
        if !body.ends_with('\n') {
            doc.push('\n');
        }
    }
    doc
}

fn overlap_record(artifact: &str, kind: RecordKind, contents: &[u8]) -> ArtifactRecord {
    ArtifactRecord {
        version: 1,
        key: ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: artifact.to_owned(),
        },
        source: SOURCE.to_owned(),
        commit: COMMIT.to_owned(),
        digest: format!("blake3:{}", blake3::hash(contents).to_hex()),
        projected_at: PROJECTED_AT.to_owned(),
        layout: "flat".to_owned(),
        kind,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from(artifact),
            size: contents.len() as u64,
            mtime: FIXED_MTIME,
            blake3: blake3::hash(contents).to_hex().to_string(),
        }],
        directories: None,
        linked: false,
        vars_digest: None,
        deploy_root: None,
        layout_separator: None,
    }
}

#[test]
fn eject_directory_over_leaf_overlap_metadata_is_byte_identical() {
    let root = TempDir::new().expect("tempdir");
    let registry = open_registry(root.path());

    let dir = overlap_record("editor", RecordKind::Dir, b"-- editor tree\n");
    let leaf = overlap_record("editor/init.lua", RecordKind::File, b"-- init\n");
    registry.put_artifact(&dir).expect("put directory record");
    registry
        .put_artifact(&leaf)
        .expect("put overlapping nested leaf record");

    registry
        .save_ejections(
            TARGET,
            &[Ejection {
                source: SOURCE.to_owned(),
                artifact: "editor".to_owned(),
                ejected_at: PROJECTED_AT.to_owned(),
            }],
        )
        .expect("eject the directory record");

    let dump = dump_state_toml(&root.path().join("state"));
    assert_golden(
        "eject_dir_leaf_overlap.golden",
        &normalize(root.path(), &dump),
    );
}

// ─── 13. link mode re-linking: a second link at a changed source re-points dst ──────

#[test]
fn relink_at_changed_source_pins_second_outcome() {
    let root = TempDir::new().expect("tempdir");
    let deploy = root.path().join("deploy");
    let staging_base = deploy.join(".phora-stage");
    let dst = deploy.join("app.conf");
    let source_a = root.path().join("worktree-a/app.conf");
    let source_b = root.path().join("worktree-b/app.conf");
    write(&source_a, b"first source\n");
    write(&source_b, b"second source\n");

    let locks = root.path().join("locks");
    let journal = Journal::open(&locks).expect("open journal");
    let registry = open_registry(root.path());

    link_artifact(
        &staging_base,
        &dst,
        &source_a,
        record(b"first source\n", RecordKind::File, true),
        &journal,
        &registry,
    )
    .expect("first link pins the head at source A");
    link_artifact(
        &staging_base,
        &dst,
        &source_b,
        record(b"second source\n", RecordKind::File, true),
        &journal,
        &registry,
    )
    .expect("re-link re-points dst at the changed source B");

    let committed = registry.artifact(&key()).expect("registry get");
    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "dst_link_target = {}",
        normalize(
            root.path(),
            &std::fs::read_link(&dst)
                .expect("read link")
                .to_string_lossy()
        )
    );
    let _ = writeln!(
        doc,
        "record_commit = {}",
        committed.as_ref().map_or("<none>", |r| r.commit.as_str())
    );
    let _ = writeln!(
        doc,
        "record_linked = {}",
        committed.as_ref().is_some_and(|r| r.linked)
    );
    let _ = writeln!(
        doc,
        "journal_entries = {}",
        journal.entries().expect("read entries").len()
    );
    assert_golden(
        "relink_changed_source.golden",
        &normalize(root.path(), &doc),
    );
}
