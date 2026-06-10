//! End-to-end acceptance for the ARCH-002 dotfile opt-in, driven through the real phora binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    target_path: PathBuf,
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

    std::fs::create_dir_all(home_path.join(".phora/git")).expect("seed phora git dir");

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
        _src: src,
        cwd,
        home_path,
        target_path,
    }
}

fn sync(fixture: &Fixture) {
    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(["sync"])
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs");
    assert!(
        out.status.success(),
        "sync must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
