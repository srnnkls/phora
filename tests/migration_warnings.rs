//! Binary-level check that deprecated source-key aliases surface a one-line
//! deprecation warning on stderr while stdout stays byte-identical (ARCH-015).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    _home: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
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

fn build_fixture(config: &str) -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    let home_path = home.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");
    write(&cwd.path().join("phora.toml"), config.as_bytes());
    Fixture {
        _home: home,
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
        .output()
        .expect("phora binary runs")
}

#[test]
fn git_localpath_alias_warns_on_stderr_without_touching_stdout() {
    let local = TempDir::new().expect("local source dir");
    let config = format!(
        "version = 1\n\n[sources.loqui]\ngit = \"{local}\"\n\n\
         [targets.home]\npath = \"~/deploy\"\nsources = [\"loqui\"]\n",
        local = local.path().display(),
    );
    let fixture = build_fixture(&config);

    // check-match is local-only: no network, so the warning is the only stderr.
    let out = run(
        &fixture,
        &["check-match", "--source", "loqui", "editor/init.lua"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let warn_line = stderr
        .lines()
        .find(|l| l.contains("loqui") && l.contains("deprecat"))
        .unwrap_or_else(|| {
            panic!("a `git = <localpath>` alias must surface a deprecation warning on stderr, got stderr: {stderr:?}")
        });
    assert!(
        warn_line.contains("path"),
        "the stderr deprecation warning must steer the user to the `path` key, got: {warn_line:?}"
    );
    assert!(
        !stdout.contains("deprecat") && !stdout.to_lowercase().contains("warning"),
        "deprecation warnings must go to stderr ONLY; stdout must stay clean, got stdout: {stdout:?}"
    );
}

#[test]
fn canonical_config_emits_no_deprecation_warning() {
    let local = TempDir::new().expect("local source dir");
    let config = format!(
        "version = 1\n\n[sources.loqui]\npath = \"{local}\"\n\n\
         [targets.home]\npath = \"~/deploy\"\nsources = [\"loqui\"]\n",
        local = local.path().display(),
    );
    let fixture = build_fixture(&config);

    let out = run(
        &fixture,
        &["check-match", "--source", "loqui", "editor/init.lua"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("deprecat"),
        "a canonical local `path` source must emit NO deprecation warning, got stderr: {stderr:?}"
    );
}

#[test]
fn mirror_resolves_under_xdg_cache() {
    let src = TempDir::new().expect("src tempdir");
    git(src.path(), &["init", "-b", "main", "."]);
    git(src.path(), &["config", "user.email", "test@example.com"]);
    git(src.path(), &["config", "user.name", "Test"]);
    write(&src.path().join("editor/init.lua"), b"-- init\n");
    git(src.path(), &["add", "-A"]);
    git(src.path(), &["commit", "-m", "fixture"]);

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\"]\n\n[targets.home]\npath = \"~/deploy\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src.path().display(),
    );
    let fixture = build_fixture(&config);

    let out = run(&fixture, &["sync"]);
    assert!(
        out.status.success(),
        "sync must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mirror = fixture.xdg_cache.join("phora/git");
    assert!(
        mirror.is_dir(),
        "git mirror must resolve under XDG_CACHE_HOME at {}",
        mirror.display()
    );
    assert!(
        !fixture.home_path.join(".phora").exists(),
        "no state may be written to the legacy ~/.phora directory"
    );
}
