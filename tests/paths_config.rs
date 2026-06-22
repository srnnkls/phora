use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
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

fn leaf_repo(dir: &Path, file: &str, body: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("phora.toml"), b"version = 1\n");
    write(&dir.join(format!("pkg/{file}")), body.as_bytes());
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "fixture"]);
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(cwd)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_STATE_HOME")
        .env("HOME", home)
        .output()
        .expect("phora binary runs")
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut it| it.next().is_some())
}

#[test]
fn paths_config_pins_cache_and_state_away_from_xdg_defaults() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "who.txt", "payload\n");

    let home = TempDir::new().expect("home tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    let cwd_path = cwd.path();
    let home_path = home.path();

    let config = format!(
        "version = 1\n\n[paths]\ncache = \".phora/cache\"\nstate = \".phora/state\"\n\n\
         [sources.editor]\ngit = \"{leaf}\"\nbranch = \"main\"\ninclude = [\"pkg\"]\n\n\
         [targets.own]\npath = \"~/own\"\nsources = [\"editor\"]\nlayout = \"flat\"\n",
        leaf = leaf.path().display(),
    );
    write(&cwd_path.join("phora.toml"), config.as_bytes());

    let out = run(home_path, cwd_path, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "sync with a [paths] override must succeed; stderr: {stderr}"
    );

    let deployed = home_path.join("own/pkg/who.txt");
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap_or_default(),
        "payload\n",
        "the artifact must still deploy under the target path; stderr: {stderr}"
    );

    let configured_cache_git: PathBuf = cwd_path.join(".phora/cache/git");
    let configured_state_projects: PathBuf = cwd_path.join(".phora/state/projects");
    assert!(
        has_entries(&configured_cache_git),
        "the git mirror must land under the configured cache root {}; stderr: {stderr}",
        configured_cache_git.display()
    );
    assert!(
        has_entries(&configured_state_projects),
        "the registry must land under the configured state root {}; stderr: {stderr}",
        configured_state_projects.display()
    );

    let default_cache = home_path.join(".cache/phora");
    let default_state = home_path.join(".local/state/phora");
    let default_data = home_path.join(".local/share/phora");
    let default_macos_caches = home_path.join("Library/Caches/phora");
    let default_macos_support = home_path.join("Library/Application Support/phora");
    for dir in [
        &default_cache,
        &default_state,
        &default_data,
        &default_macos_caches,
        &default_macos_support,
    ] {
        assert!(
            !dir.exists(),
            "with a [paths] override set, nothing may be written under the default location {}; \
             stderr: {stderr}",
            dir.display()
        );
    }

    let verify = run(home_path, cwd_path, &["verify"]);
    let verify_stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify.status.success(),
        "verify against the configured state root must succeed; stderr: {verify_stderr}"
    );
}
