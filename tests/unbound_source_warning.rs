use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const WARNING: &str = "phora: source `gestalt` is bound by no target, import, or build";

const BOUND: &str = "version = 1\n\n[sources.used]\npath = \"used\"\n\n\
     [targets.home]\npath = \"~/deploy\"\nsources = [\"used\"]\n";

mod common;

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed");
}

fn seed_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("editor")).expect("create repo dir");
    std::fs::write(dir.join("editor/init.lua"), "-- init\n").expect("write artifact");
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=T",
            "commit",
            "-m",
            "fixture",
        ],
    );
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    let home = TempDir::new().expect("home tempdir");
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("xdg/cache"))
        .env("XDG_STATE_HOME", home.path().join("xdg/state"))
        .output()
        .expect("phora binary runs")
}

fn project(config: &str, local: Option<&str>) -> TempDir {
    let cwd = TempDir::new().expect("cwd tempdir");
    std::fs::write(cwd.path().join("phora.toml"), config).expect("write phora.toml");
    if let Some(local) = local {
        std::fs::write(cwd.path().join("phora.local.toml"), local).expect("write phora.local.toml");
    }
    cwd
}

fn stderr_warns(out: &Output) -> bool {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .any(|line| line == WARNING)
}

#[test]
fn unbound_base_source_warns_on_sync_and_preview() {
    let cwd = project(
        "version = 1\n\n[sources.gestalt]\npath = \"gestalt\"\n",
        None,
    );
    seed_repo(&cwd.path().join("gestalt"));
    for command in ["sync", "preview"] {
        let out = run(cwd.path(), &[command]);
        assert!(
            out.status.success(),
            "`{command}` must stay non-fatal, got stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stderr_warns(&out),
            "`{command}` must warn about the unbound source, got stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("gestalt"),
            "the warning must go to stderr only"
        );
    }
}

#[test]
fn unbound_local_only_source_warns_on_preview() {
    let cwd = project(
        BOUND,
        Some("[sources.gestalt]\npath = \"/tmp/gestalt\"\ndeploy = \"link\"\n"),
    );
    let out = run(cwd.path(), &["preview"]);
    assert!(
        stderr_warns(&out),
        "a local-only unbound source must warn, got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn bound_source_emits_no_warning() {
    let cwd = project(BOUND, None);
    let out = run(cwd.path(), &["preview"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("bound by no target"),
        "a bound source must not warn, got stderr: {stderr}"
    );
}
