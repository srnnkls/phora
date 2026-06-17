//! ARCH-002 documented exception: `check-match`'s artifact-allow output changes for hidden names.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

mod common;

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

fn build_fixture(include: &str) -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let src = TempDir::new().expect("src tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");

    git(src.path(), &["init", "-b", "main", "."]);
    git(src.path(), &["config", "user.email", "test@example.com"]);
    git(src.path(), &["config", "user.name", "Test"]);
    write(&src.path().join(".config/settings.json"), b"{\"k\":1}\n");
    git(src.path(), &["add", "-A"]);
    git(src.path(), &["commit", "-m", "fixture"]);

    let home_path = home.path().to_path_buf();
    let src_path = src.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{src}\"\nbranch = \"main\"\n\
         include = {include}\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src_path.display(),
        target = home_path.join("deploy").display(),
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

fn check_match_stdout(fixture: &Fixture, path: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(["check-match", "--source", "dotfiles", path])
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env("XDG_CACHE_HOME", &fixture.xdg_cache)
        .env("XDG_STATE_HOME", &fixture.xdg_state)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn check_match_reports_dotfile_allowed_when_opted_in() {
    let fixture = build_fixture("[\".config\"]");

    let stdout = check_match_stdout(&fixture, ".config/foo");

    assert!(
        stdout.contains("artifact `.config`: allowed"),
        "include `.config` must report the hidden artifact as allowed; got:\n{stdout}"
    );
}

#[test]
fn check_match_reports_dotfile_excluded_under_star_include() {
    let fixture = build_fixture("[\"*\"]");

    let stdout = check_match_stdout(&fixture, ".config/foo");

    assert!(
        stdout.contains("artifact `.config`: excluded"),
        "include `*` must NOT opt the hidden artifact in (no dotglob); got:\n{stdout}"
    );
}
