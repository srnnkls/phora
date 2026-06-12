//! End-to-end acceptance for the ARCH-002 dotfile opt-in, driven through the real phora binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

struct Fixture {
    _home: TempDir,
    src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    target_path: PathBuf,
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
    write(&root.join(".config/settings.json"), b"{\"k\":1}\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

fn build_fixture(include: &str) -> Fixture {
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
        "version = 1\n\n[sources.dotfiles]\ngit = \"{src}\"\nbranch = \"main\"\n\
         include = {include}\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src_path.display(),
        target = target_path.display(),
    );
    write(&cwd.path().join("phora.toml"), config.as_bytes());

    Fixture {
        _home: home,
        src,
        cwd,
        home_path,
        target_path,
        xdg_cache,
        xdg_state,
    }
}

fn sync(fixture: &Fixture) {
    run_sync(fixture, &["sync"]);
}

fn run_sync(fixture: &Fixture, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env("XDG_CACHE_HOME", &fixture.xdg_cache)
        .env("XDG_STATE_HOME", &fixture.xdg_state)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs");
    assert!(
        out.status.success(),
        "phora {args:?} must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rewrite_include(fixture: &Fixture, include: &str) {
    let src_path = fixture.src.path().to_path_buf();
    let config = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{src}\"\nbranch = \"main\"\n\
         include = {include}\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src_path.display(),
        target = fixture.target_path.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
}

#[test]
fn literal_dot_include_deploys_hidden_dir() {
    let fixture = build_fixture("[\".config\"]");

    sync(&fixture);

    let deployed = fixture.target_path.join(".config/settings.json");
    assert!(
        deployed.exists(),
        "include `.config` must deploy the hidden artifact to {}",
        deployed.display()
    );
}

#[test]
fn non_dotfile_include_does_not_deploy_hidden_dir() {
    let fixture = build_fixture("[\"editor\"]");

    sync(&fixture);

    assert!(
        fixture.target_path.join("editor/init.lua").exists(),
        "the listed non-hidden artifact must deploy"
    );
    assert!(
        !fixture.target_path.join(".config").exists(),
        "without a dotfile opt-in pattern, `.config` must NOT be deployed (gate must not leak)"
    );
}

// Green regression guard (passes today: registry-driven prune already deletes a dropped
// dotfile artifact via its record); locks the acceptance invariant against regressions.
#[test]
fn prune_deletes_dotfile_artifact_dropped_from_config() {
    let fixture = build_fixture("[\".config\"]");
    sync(&fixture);

    let deployed = fixture.target_path.join(".config");
    assert!(
        deployed.join("settings.json").exists(),
        "premise: `.config` must be deployed before it is dropped from config"
    );

    rewrite_include(&fixture, "[\"editor\"]");
    run_sync(&fixture, &["sync", "--prune"]);

    assert!(
        !deployed.exists(),
        "after `.config` is removed from the selection, `sync --prune` must delete the \
         stranded on-disk dotfile artifact at {}",
        deployed.display()
    );
}

#[test]
fn prune_leaves_unmanaged_user_dotfile_dir_untouched() {
    let fixture = build_fixture("[\"editor\"]");
    sync(&fixture);

    let user_dotfile = fixture.target_path.join(".cache");
    write(&user_dotfile.join("blob.bin"), b"user data\n");

    run_sync(&fixture, &["sync", "--prune"]);

    assert!(
        user_dotfile.join("blob.bin").exists(),
        "a hand-placed user dotfile dir that no source selects must survive `sync --prune` \
         untouched; it was at {}",
        user_dotfile.display()
    );
}
