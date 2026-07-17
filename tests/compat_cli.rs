//! T004 CLI snapshot baselines (INV-4/INV-9): byte-exact stdout/stderr/exit-code
//! fixtures pinning the command surface of the `phora` binary on unmoved `main`, so
//! every rendered line and exit code stays identical through the source → projection →
//! sync refactor. These are the unmoved-main oracle the PR9/PR11 diffs and the lock /
//! frozen / advisory work (T021/T026/T028) measure against.
//!
//! Per-run-varying tokens — the fixture's temp roots and its path-hash project-id — are
//! each scrubbed by their own literal value; content-addressed digests and commits stay
//! byte-identical. Serialized under `tests/compat/cli/`, distinct from the T002/T003/T004
//! `serialized`/`staging`/`recovery` matrices so no baseline race on a shared write.

#![cfg(unix)]

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use phora::cli::resolution_from_char;
use phora::kernel::ProjectId;
use phora::store::{FileRegistry, Registry};
use phora::sync::Resolution;
use tempfile::TempDir;

mod common;

const BASELINE_COMMIT: &str = "92c784e3b14496be25dcecc8d4500e32b52b1c50";
const EX_TEMPFAIL: i32 = 75;

// ─── golden-fixture harness (reused verbatim from the T003 staging matrix) ──────

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat/cli")
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
            "refusing to regenerate T004 CLI baselines: production (src/, Cargo.toml, Cargo.lock) \
             has diverged from the pinned baseline {BASELINE_COMMIT}, so a capture would freeze \
             post-refactor behavior instead of the baseline it must diff against."
        ),
        None => panic!(
            "refusing to regenerate T004 CLI baselines: `git diff` against {BASELINE_COMMIT} did \
             not run (git unavailable or not a repo); baselines must reflect unmoved production."
        ),
    }
    if let Some(reason) = tracked_worktree_is_dirty() {
        panic!(
            "refusing to regenerate T004 CLI baselines: the production pathspec (src/, Cargo.toml, \
             Cargo.lock) is not clean or git could not verify it, so the capture would not reflect \
             pinned production. Commit or stash production changes first (untracked fixtures and \
             edited test harnesses elsewhere are fine). Details: {reason}"
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
        "golden fixture missing: {} (regenerate the T004 CLI baseline with PHORA_UPDATE_GOLDEN=1)",
        path.display()
    );
    let expected = std::fs::read_to_string(&path).expect("read golden fixture");
    assert_eq!(
        actual,
        expected,
        "CLI snapshot bytes drifted from the pinned baseline {}",
        path.display()
    );
}

// ─── harness self-tests (invariants, not CLI goldens) ───────────────────────────

#[test]
fn line_diff_announces_final_newline_only_drift() {
    let dropped = line_diff("baseline\n", "baseline");
    assert!(
        dropped.contains("newline"),
        "line_diff must name the final-newline drift, got: {dropped:?}"
    );
    let added = line_diff("baseline", "baseline\n");
    assert!(
        added.contains("newline"),
        "line_diff must name the final-newline drift, got: {added:?}"
    );
    assert_ne!(dropped, added, "dropped vs added must render distinctly");
    assert!(!line_diff("same\n", "same\n").contains("newline"));
}

#[test]
fn normalize_scrubs_only_fixture_roots_and_preserves_unrelated_hex() {
    let fx = build_fixture();
    fx.run(&["sync"]);
    let project = fx.project_id().expect("project-id after sync");
    let unrelated = "0123456789abcdef";
    assert_ne!(project, unrelated, "self-test precondition");
    let raw = format!(
        "projects/{project}/x other={unrelated} at {}\n",
        fx.cwd.path().display()
    );
    assert_eq!(
        fx.normalize(&raw),
        format!("projects/<PROJECT>/x other={unrelated} at <CWD>\n"),
        "normalize must scrub only the project-id and fixture roots, leaving unrelated hex intact"
    );
}

// ─── subprocess fixture (reused from the T002 serialized matrix) ────────────────

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    src_path: PathBuf,
    target_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

fn build_source_repo(root: &Path) {
    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);

    write(&root.join("editor/init.lua"), b"-- init\n");
    write(&root.join("lint/rules.toml"), b"[rules]\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let src = TempDir::new().expect("src tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");

    build_source_repo(src.path());

    let home_path = home.path().to_path_buf();
    let src_path = src.path().to_path_buf();
    let target_path = home_path.join("deploy");
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\", \"lint\"]\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src_path.display(),
        target = target_path.display(),
    );
    write(&cwd.path().join("phora.toml"), config.as_bytes());

    Fixture {
        _home: home,
        _src: src,
        cwd,
        home_path,
        src_path,
        target_path,
        xdg_cache,
        xdg_state,
    }
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(self.cwd.path())
            .env("HOME", &self.home_path)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE")
            .output()
            .expect("phora binary runs")
    }

    fn write_config(&self, body: &str) {
        write(&self.cwd.path().join("phora.toml"), body.as_bytes());
    }

    fn state_root(&self) -> PathBuf {
        let id = ProjectId::for_path(self.cwd.path()).expect("project id");
        self.xdg_state
            .join("phora")
            .join("projects")
            .join(id.as_str())
    }

    fn project_id(&self) -> Option<String> {
        let base = self.xdg_state.join("phora").join("projects");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        if dirs.len() != 1 {
            return None;
        }
        dirs.pop()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    }

    fn normalize(&self, raw: &str) -> String {
        let mut s = raw.to_owned();
        for (needle, tag) in [
            (self.xdg_cache.to_string_lossy().into_owned(), "<XDG_CACHE>"),
            (self.xdg_state.to_string_lossy().into_owned(), "<XDG_STATE>"),
            (self.target_path.to_string_lossy().into_owned(), "<TARGET>"),
            (self.home_path.to_string_lossy().into_owned(), "<HOME>"),
            (self.src_path.to_string_lossy().into_owned(), "<SRC>"),
            (self.cwd.path().to_string_lossy().into_owned(), "<CWD>"),
        ] {
            s = s.replace(&needle, tag);
        }
        if let Some(project) = self.project_id() {
            s = s.replace(&project, "<PROJECT>");
        }
        s
    }
}

fn snapshot(out: &Output) -> String {
    format!(
        "exit: {}\n--- stdout ---\n{}--- stderr ---\n{}",
        out.status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string()),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

fn assert_success(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx} must succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn chmod_tree(path: &Path, dir_mode: u32, file_mode: u32) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    let ty = meta.file_type();
    if ty.is_symlink() {
        return Ok(());
    }
    if ty.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chmod_tree(&entry?.path(), dir_mode, file_mode)?;
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(dir_mode))?;
    } else {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(file_mode))?;
    }
    Ok(())
}

struct ReadOnlyTree {
    root: PathBuf,
}

impl ReadOnlyTree {
    fn lock(root: &Path) -> Self {
        chmod_tree(root, 0o555, 0o444).expect("lock state root read-only");
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl Drop for ReadOnlyTree {
    fn drop(&mut self) {
        let _ = chmod_tree(&self.root, 0o755, 0o644);
    }
}

// ─── 1. `sync`: full stdout/stderr/exit snapshot on a fresh deploy ──────────────

#[test]
fn sync_snapshot_is_byte_identical() {
    let fx = build_fixture();
    let out = fx.run(&["sync"]);
    assert_success(&out, "fresh sync");
    assert_golden("sync.golden", &fx.normalize(&snapshot(&out)));
}

// ─── 2. `sync` again: the idempotent (Clean, no-op) snapshot ────────────────────

#[test]
fn sync_idempotent_second_run_snapshot_is_byte_identical() {
    let fx = build_fixture();
    assert_success(&fx.run(&["sync"]), "first sync");
    let out = fx.run(&["sync"]);
    assert_success(&out, "second (idempotent) sync");
    assert_golden("sync_idempotent.golden", &fx.normalize(&snapshot(&out)));
}

// ─── 3. `preview`: the offline projection snapshot ──────────────────────────────

#[test]
fn preview_snapshot_is_byte_identical() {
    let fx = build_fixture();
    assert_success(&fx.run(&["sync"]), "sync");
    let out = fx.run(&["preview", "--files"]);
    assert_success(&out, "preview --files");
    assert_golden("preview_files.golden", &fx.normalize(&snapshot(&out)));
}

// ─── 4. `sync --prune`: orphan removal after a target left config ───────────────

#[test]
fn sync_prune_orphan_removal_snapshot_is_byte_identical() {
    let fx = build_fixture();
    assert_success(&fx.run(&["sync"]), "initial sync");

    let orphaned_files = [
        fx.target_path.join("editor/init.lua"),
        fx.target_path.join("lint/rules.toml"),
    ];
    for f in &orphaned_files {
        assert!(
            f.exists(),
            "precondition: the artifact must be deployed before its target is dropped: {}",
            f.display()
        );
    }
    let registry_before = FileRegistry::open(fx.state_root()).expect("open registry before");
    assert!(
        !registry_before
            .list_target("home")
            .expect("list_target before")
            .is_empty(),
        "precondition: the registry must hold the target's records before the prune"
    );

    let without_target = fx.cwd.path().join("phora.toml");
    let config = std::fs::read_to_string(&without_target).expect("read config");
    let trimmed = config
        .split_once("[targets.home]")
        .expect("config has [targets.home]")
        .0
        .trim_end()
        .to_owned()
        + "\n";
    fx.write_config(&trimmed);

    let out = fx.run(&["sync", "--prune"]);
    assert_golden("sync_prune.golden", &fx.normalize(&snapshot(&out)));

    for f in &orphaned_files {
        assert!(
            !f.exists(),
            "--prune must remove the orphan's deployed file, not merely report it: {}",
            f.display()
        );
    }
    let registry_after = FileRegistry::open(fx.state_root()).expect("open registry after");
    assert!(
        registry_after
            .list_target("home")
            .expect("list_target after")
            .is_empty(),
        "--prune must drop the orphan's registry records, so the pruned target holds none"
    );
}

// ─── 5. `rebuild-registry`: reconstruction snapshot ─────────────────────────────

#[test]
fn rebuild_registry_snapshot_is_byte_identical() {
    let fx = build_fixture();
    assert_success(&fx.run(&["sync"]), "sync");
    let out = fx.run(&["rebuild-registry"]);
    assert_golden("rebuild_registry.golden", &fx.normalize(&snapshot(&out)));
}

// ─── 6. lock-held contention: `sync` fails fast with EX_TEMPFAIL (75) ───────────

#[test]
fn lock_held_sync_snapshot_exits_ex_tempfail() {
    let fx = build_fixture();
    let locks_dir = fx.state_root().join("locks");
    std::fs::create_dir_all(&locks_dir).expect("create locks dir");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(locks_dir.join("state.lock"))
        .expect("open lock file");
    held.try_lock().expect("test holds the project lock first");

    let out = fx.run(&["sync"]);
    assert_eq!(
        out.status.code(),
        Some(EX_TEMPFAIL),
        "a contended sync must exit {EX_TEMPFAIL}; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_golden("lock_held_sync.golden", &fx.normalize(&snapshot(&out)));
    drop(held);
}

// ─── 7. frozen read-only fallback: clean `sync --frozen` succeeds lockless ──────

#[test]
fn frozen_clean_sync_on_readonly_root_snapshot() {
    let fx = build_fixture();
    assert_success(&fx.run(&["sync"]), "first writable sync");
    assert_success(&fx.run(&["sync"]), "second sync (sanity: Clean no-op)");

    let base = fx.xdg_state.join("phora");
    let _readonly = ReadOnlyTree::lock(&base);
    let out = fx.run(&["sync", "--frozen"]);
    assert_success(&out, "frozen Clean sync on a read-only state root");
    assert_golden(
        "frozen_readonly_clean.golden",
        &fx.normalize(&snapshot(&out)),
    );
}

// ─── 8. frozen read-only fallback: pending work on a read-only root fails ───────

#[test]
fn frozen_pending_on_readonly_root_diagnostic_snapshot() {
    let fx = build_fixture();
    assert_success(&fx.run(&["sync"]), "first writable sync");
    assert_success(&fx.run(&["sync"]), "second sync (Clean no-op)");

    std::fs::remove_dir_all(&fx.target_path).expect("remove deployed artifacts (pending work)");

    let base = fx.xdg_state.join("phora");
    let _readonly = ReadOnlyTree::lock(&base);
    let out = fx.run(&["sync", "--frozen"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "pending work on a read-only root must fail; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_golden(
        "frozen_readonly_pending.golden",
        &fx.normalize(&snapshot(&out)),
    );
}

// ─── 9. non-TTY conflict default: an unmanaged dest is Skipped, not clobbered ───

#[test]
fn noninteractive_foreign_conflict_defaults_to_skip_snapshot() {
    let fx = build_fixture();
    // A user-owned (Foreign) file already sits where the artifact would deploy; a
    // non-TTY sync cannot prompt, so it must default rather than clobber.
    write(
        &fx.target_path.join("editor/init.lua"),
        b"user's own edit\n",
    );

    let out = fx.run(&["sync"]);
    assert_golden(
        "conflict_noninteractive_skip.golden",
        &fx.normalize(&snapshot(&out)),
    );
    assert_eq!(
        std::fs::read(fx.target_path.join("editor/init.lua")).expect("read foreign file"),
        b"user's own edit\n",
        "a non-interactive sync must not clobber the pre-existing Foreign file"
    );
}

// ─── 10. interactive-conflict resolution mapping (deterministic pure fn) ────────

#[test]
fn interactive_resolution_char_mapping_is_byte_identical() {
    let mut doc = String::new();
    for c in ['s', 'o', 'e', 'a', 'x', 'S'] {
        let _ = writeln!(doc, "{c:?} -> {:?}", resolution_from_char(c));
    }
    assert!(
        matches!(resolution_from_char('s'), Some(Resolution::Skip)),
        "the interactive prompt's default character `s` must map to Skip"
    );
    assert_golden("resolution_char_mapping.golden", &doc);
}

// ─── 11. network-FS advisory: local storage yields no advisory (reachable) ──────

#[test]
fn lock_advisory_is_none_on_local_state_root() {
    let dir = TempDir::new().expect("state root tempdir");
    let registry = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
    let doc = format!("lock_advisory = {:?}\n", registry.lock_advisory());
    assert_golden("lock_advisory_local.golden", &doc);
}

// ─── 12. forced-color clap diagnostic: ANSI stays byte-identical without a PTY ───

#[test]
fn clap_forced_color_error_snapshot_is_byte_identical() {
    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .arg("definitely-not-a-command")
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .expect("phora binary runs");

    assert_eq!(
        out.status.code(),
        Some(2),
        "an unrecognized subcommand must exit 2 (clap usage error); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.contains(&0x1b),
        "CLICOLOR_FORCE=1 with NO_COLOR unset must force ANSI even off a PTY; stderr had no ESC:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_golden("clap_forced_color_error.golden", &snapshot(&out));
}

// ─── 13. frozen no-fetch: a --frozen sync reuses the lock without touching the source ─

#[test]
fn frozen_sync_does_not_fetch_the_deleted_source() {
    let fx = build_fixture();
    assert_success(
        &fx.run(&["sync"]),
        "first writable sync pins the lock and populates the mirror cache",
    );

    // With the source repo gone, any fetch attempt fails; only a genuine no-fetch path survives.
    std::fs::remove_dir_all(&fx.src_path).expect("remove the source repo");
    assert!(
        !fx.src_path.exists(),
        "precondition: the source snapshot must be unreachable so a fetch cannot silently succeed"
    );

    let out = fx.run(&["sync", "--frozen"]);
    assert_success(
        &out,
        "a --frozen sync must reuse the pinned lock and cached mirror without fetching the source",
    );
    assert_golden("frozen_no_fetch.golden", &fx.normalize(&snapshot(&out)));
}
