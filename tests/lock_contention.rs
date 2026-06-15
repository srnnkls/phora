//! LOCK-001: mutating commands fail fast (exit 75) on a contended project lock.
//!
//! Contention is real and cross-process: the test flocks the same inode
//! (`<state_root>/locks/state.lock`) the binary opens, so the child's flock(2)
//! `try_lock` returns `WouldBlock` rather than blocking the test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use phora::kernel::ProjectId;
use tempfile::TempDir;

const CONTENDED_MSG: &str = "another phora process is running for this project";
const EX_TEMPFAIL: i32 = 75;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
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

fn state_root(fixture: &Fixture) -> PathBuf {
    let project = ProjectId::for_path(fixture.cwd.path()).expect("project id");
    fixture
        .xdg_state
        .join("phora")
        .join("projects")
        .join(project.as_str())
}

/// Flocks the project's `state.lock`; held until the returned file drops.
fn hold_project_lock(fixture: &Fixture) -> std::fs::File {
    let locks_dir = state_root(fixture).join("locks");
    std::fs::create_dir_all(&locks_dir).expect("create locks dir");
    let lock_path = locks_dir.join("state.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lock file");
    file.try_lock().expect("test acquires the project lock first");
    file
}

fn assert_contended(out: &Output, cmd: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(EX_TEMPFAIL),
        "`phora {cmd}` under contention must exit {EX_TEMPFAIL} (EX_TEMPFAIL); \
         got {:?}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains(CONTENDED_MSG),
        "`phora {cmd}` under contention must print the contended-lock message \
         ({CONTENDED_MSG:?}) to stderr; got:\n{stderr}"
    );
}

#[test]
fn sync_fails_fast_with_ex_tempfail_when_project_lock_is_held() {
    let fixture = build_fixture();
    let held = hold_project_lock(&fixture);

    let out = run(&fixture, &["sync"]);

    assert_contended(&out, "sync");
    drop(held);
}

#[test]
fn eject_fails_fast_with_ex_tempfail_when_project_lock_is_held() {
    let fixture = build_fixture();
    let held = hold_project_lock(&fixture);

    let out = run(
        &fixture,
        &["eject", "editor/init.lua", "--source", "dotfiles", "--target", "home"],
    );

    assert_contended(&out, "eject");
    drop(held);
}

#[test]
fn uneject_fails_fast_with_ex_tempfail_when_project_lock_is_held() {
    let fixture = build_fixture();
    let held = hold_project_lock(&fixture);

    let out = run(
        &fixture,
        &["uneject", "editor/init.lua", "--source", "dotfiles", "--target", "home"],
    );

    assert_contended(&out, "uneject");
    drop(held);
}

#[test]
fn rebuild_registry_fails_fast_with_ex_tempfail_when_project_lock_is_held() {
    let fixture = build_fixture();
    let held = hold_project_lock(&fixture);

    let out = run(&fixture, &["rebuild-registry"]);

    assert_contended(&out, "rebuild-registry");
    drop(held);
}

#[test]
fn sync_succeeds_once_the_lock_is_released() {
    let fixture = build_fixture();

    let blocked = {
        let _held = hold_project_lock(&fixture);
        run(&fixture, &["sync"])
    };
    assert_eq!(
        blocked.status.code(),
        Some(EX_TEMPFAIL),
        "sanity: sync must be blocked while the lock is held"
    );

    let after = run(&fixture, &["sync"]);
    assert!(
        after.status.success(),
        "after the lock is released, a fresh `phora sync` must acquire it and \
         succeed (exit 0); got {:?}\nstderr:\n{}",
        after.status.code(),
        String::from_utf8_lossy(&after.stderr)
    );
}
