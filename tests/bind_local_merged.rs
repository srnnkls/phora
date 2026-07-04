//! BUG-BIND-008: `bind --local` resolves a target against the merged config and
//! its missing-target remedy carries `--local`.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: std::path::PathBuf,
    xdg_cache: std::path::PathBuf,
    xdg_state: std::path::PathBuf,
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
         include = [\"editor\"]\n\n[targets.home]\npath = \"{target}\"\n\
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn bind_local_succeeds_for_target_defined_only_in_main_config() {
    let fixture = build_fixture();

    let out = run(&fixture, &["bind", "dotfiles", "--to", "home", "--local"]);

    let err = stderr(&out);
    assert!(
        out.status.success(),
        "bind --local to a target defined only in phora.toml must succeed \
         (the binding lands in phora.local.toml); got exit {:?}, stderr: {err}",
        out.status.code()
    );
    assert!(
        !err.contains("does not exist") && !err.contains("is not defined"),
        "the merged view must recognize the phora.toml target; \
         it must not be reported missing, stderr: {err}"
    );
}

#[test]
fn missing_target_remedy_for_local_bind_suggests_local_flag() {
    let fixture = build_fixture();

    let out = run(&fixture, &["bind", "dotfiles", "--to", "ghost", "--local"]);

    let err = stderr(&out);
    assert!(
        !out.status.success(),
        "binding to an undefined target must fail, stderr: {err}"
    );
    assert!(
        err.contains("ghost"),
        "the error must name the missing target `ghost`, stderr: {err}"
    );
    assert!(
        err.contains("phora target add ghost --path") && err.contains("--local"),
        "a --local bind's missing-target remedy must be \
         `phora target add ghost --path <path> --local`, stderr: {err}"
    );
}

#[test]
fn missing_target_remedy_for_nonlocal_bind_omits_local_flag() {
    let fixture = build_fixture();

    let out = run(&fixture, &["bind", "dotfiles", "--to", "ghost"]);

    let err = stderr(&out);
    assert!(
        !out.status.success(),
        "binding to an undefined target must fail, stderr: {err}"
    );
    assert!(
        err.contains("phora target add ghost --path"),
        "the missing-target remedy must give the `phora target add` hint, stderr: {err}"
    );
    assert!(
        !err.contains("--local"),
        "a non-local bind's remedy must NOT suggest --local, stderr: {err}"
    );
}
