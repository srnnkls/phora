//! CLIFF-FROZEN-004: `sync --frozen` on a read-only state root.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use phora::kernel::ProjectId;
use tempfile::TempDir;

mod common;

const EX_TEMPFAIL: i32 = 75;
const CONTENDED_MSG: &str = "another phora process is running for this project";

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
    target_path: PathBuf,
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
    let target_path = home_path.join("deploy");
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\", \"lint\"]\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src.path().display(),
        target = target_path.display(),
    );
    write(&cwd.path().join("phora.toml"), config.as_bytes());

    Fixture {
        _home: home,
        _src: src,
        cwd,
        home_path,
        xdg_cache,
        xdg_state,
        target_path,
    }
}

fn run(fixture: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env("XDG_CACHE_HOME", &fixture.xdg_cache)
        .env("XDG_STATE_HOME", &fixture.xdg_state)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs")
}

fn phora_state_base(fixture: &Fixture) -> PathBuf {
    fixture.xdg_state.join("phora")
}

fn registry_dir(fixture: &Fixture) -> PathBuf {
    let base = phora_state_base(fixture).join("projects");
    let id = match std::fs::read_to_string(fixture.cwd.path().join(".phora-id")) {
        Ok(text) => text.trim().to_owned(),
        Err(_) => ProjectId::for_path(fixture.cwd.path())
            .expect("project id")
            .as_str()
            .to_owned(),
    };
    base.join(id)
}

fn assert_success(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} must exit 0; got {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
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

fn snapshot(root: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut acc = Vec::new();
    collect(root, &mut acc);
    acc.sort_by(|a, b| a.0.cmp(&b.0));
    acc
}

fn collect(path: &Path, acc: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let ty = meta.file_type();
    if ty.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                collect(&entry.path(), acc);
            }
        }
    } else if ty.is_file() {
        let mtime = meta.modified().expect("mtime");
        acc.push((path.to_path_buf(), meta.len(), mtime));
    }
}

fn assert_frozen_fails_naming_root(out: &Output, base: &Path) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code();
    assert_ne!(
        code,
        Some(0),
        "frozen sync with pending work on a read-only root must fail; stderr:\n{stderr}"
    );
    assert_ne!(
        code,
        Some(EX_TEMPFAIL),
        "pending work on a read-only root is a hard error, not lock contention (75); stderr:\n{stderr}"
    );
    let base_str = base.display().to_string();
    assert!(
        stderr.contains(&base_str),
        "the error must name the read-only state root ({base_str}); got:\n{stderr}"
    );
    let lowered = stderr.to_lowercase();
    assert!(
        lowered.contains("read-only") || lowered.contains("read only"),
        "the error must identify the root as read-only; got:\n{stderr}"
    );
}

fn deploy_clean(fixture: &Fixture) {
    let first = run(fixture, &["sync"]);
    assert_success(&first, "initial writable sync");
    let second = run(fixture, &["sync"]);
    assert_success(&second, "second sync (sanity: fully Clean, no-op)");
}

#[test]
fn frozen_clean_sync_on_readonly_root_without_locks_dir_exits_zero() {
    let fixture = build_fixture();
    deploy_clean(&fixture);

    let locks = registry_dir(&fixture).join("locks");
    if locks.exists() {
        std::fs::remove_dir_all(&locks).expect("remove locks dir");
    }

    let base = phora_state_base(&fixture);
    let before = snapshot(&base);
    let _readonly = ReadOnlyTree::lock(&base);

    let out = run(&fixture, &["sync", "--frozen"]);

    assert_success(
        &out,
        "frozen Clean sync on a read-only root (locks dir absent)",
    );
    assert_eq!(
        before,
        snapshot(&base),
        "a Clean frozen sync must perform ZERO state-root writes"
    );
}

#[test]
fn frozen_clean_sync_on_readonly_root_with_existing_lock_file_exits_zero() {
    let fixture = build_fixture();
    deploy_clean(&fixture);

    let lock_file = registry_dir(&fixture).join("locks").join("state.lock");
    assert!(
        lock_file.exists(),
        "a prior sync must have created {}",
        lock_file.display()
    );

    let base = phora_state_base(&fixture);
    let before = snapshot(&base);
    let _readonly = ReadOnlyTree::lock(&base);

    let out = run(&fixture, &["sync", "--frozen"]);

    assert_success(
        &out,
        "frozen Clean sync on a read-only root (existing read-only lock file)",
    );
    assert_eq!(
        before,
        snapshot(&base),
        "a Clean frozen sync must perform ZERO state-root writes"
    );
}

#[test]
fn frozen_sync_without_identity_or_registry_on_readonly_root_fails_early() {
    let fixture = build_fixture();
    deploy_clean(&fixture);

    std::fs::remove_dir_all(registry_dir(&fixture)).expect("drop registry dir");
    std::fs::remove_file(fixture.cwd.path().join(".phora-id")).expect("drop .phora-id");

    let base = phora_state_base(&fixture);
    let _readonly = ReadOnlyTree::lock(&base);

    let out = run(&fixture, &["sync", "--frozen"]);

    assert_frozen_fails_naming_root(&out, &base);
}

#[test]
fn frozen_sync_with_pending_deploy_on_readonly_root_fails_early() {
    let fixture = build_fixture();
    deploy_clean(&fixture);

    std::fs::remove_dir_all(&fixture.target_path).expect("remove deployed artifacts");

    let base = phora_state_base(&fixture);
    let _readonly = ReadOnlyTree::lock(&base);

    let out = run(&fixture, &["sync", "--frozen"]);

    assert_frozen_fails_naming_root(&out, &base);
}

#[test]
fn non_frozen_sync_on_readonly_root_errors() {
    let fixture = build_fixture();
    deploy_clean(&fixture);

    let base = phora_state_base(&fixture);
    let _readonly = ReadOnlyTree::lock(&base);

    let out = run(&fixture, &["sync"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(0),
        "a non-frozen sync must not proceed lockless on a read-only root; stderr:\n{stderr}",
    );
    assert!(
        stderr.contains(&base.display().to_string()),
        "the error must name the read-only state root ({}); got:\n{stderr}",
        base.display(),
    );
    let lowered = stderr.to_lowercase();
    assert!(
        lowered.contains("permission denied")
            || lowered.contains("read-only")
            || lowered.contains("read only"),
        "the error must attribute the failure to the read-only root, not an unrelated crash; \
         got:\n{stderr}",
    );
}

#[test]
fn frozen_sync_still_exits_75_on_genuine_lock_contention() {
    let fixture = build_fixture();
    deploy_clean(&fixture);

    let lock_path = registry_dir(&fixture).join("locks").join("state.lock");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lock file");
    held.try_lock()
        .expect("test acquires the project lock first");

    let out = run(&fixture, &["sync", "--frozen"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(EX_TEMPFAIL),
        "frozen sync under genuine contention must still exit {EX_TEMPFAIL}; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(CONTENDED_MSG),
        "frozen contention must print the contended-lock message ({CONTENDED_MSG:?}); got:\n{stderr}"
    );
    drop(held);
}
