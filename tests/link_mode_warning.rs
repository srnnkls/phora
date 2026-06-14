//! Binary-level check that a base-defined `deploy = "link"` over an ABSOLUTE local
//! path surfaces a one-line non-portability warning on stderr while stdout stays
//! byte-clean of the warning, and that a non-absolute link source warns NOT at all
//! (LINK-001).

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

/// A committed git working tree at `dir` carrying one artifact, so sync can link it.
fn seed_source_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create source dir");
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("editor/init.lua"), b"-- init\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "fixture"]);
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
fn base_link_over_absolute_path_warns_on_stderr_without_touching_stdout() {
    let src = TempDir::new().expect("source repo tempdir");
    seed_source_repo(src.path());
    assert!(
        src.path().is_absolute(),
        "premise: a TempDir source path is absolute, the warning condition"
    );

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\"]\ndeploy = \"link\"\n\n\
         [targets.home]\npath = \"~/deploy\"\nsources = [\"dotfiles\"]\nlayout = \"by-source\"\n",
        src = src.path().display(),
    );
    let fixture = build_fixture(&config);

    let out = run(&fixture, &["sync"]);
    assert!(
        out.status.success(),
        "a base link over an absolute local path must sync successfully (exit 0); \
             the warning is non-fatal, got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Contract: the warning MUST name the source AND contain the literal word
    // `absolute` (the trigger condition IS an absolute path). Matcher and every
    // exclusion below key on this SAME token, leaving no wording escape hatch.
    let warn_line = stderr
        .lines()
        .find(|l| l.contains("dotfiles") && l.to_lowercase().contains("absolute"))
        .unwrap_or_else(|| {
            panic!(
                "a base-defined link over an absolute path must surface a warning naming the \
                 source AND containing `absolute` on stderr, got stderr: {stderr:?}"
            )
        });
    assert!(
        warn_line.contains("dotfiles"),
        "the stderr warning must name the source `dotfiles`, got: {warn_line:?}"
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.contains("dotfiles") && l.to_lowercase().contains("absolute"))
            && !stdout.to_lowercase().contains("warning"),
        "the portability warning must go to stderr ONLY; stdout must carry no line naming \
             `dotfiles` with `absolute`, nor any `warning`, got stdout: {stdout:?}"
    );
}

#[test]
fn link_over_non_absolute_path_emits_no_warning() {
    let fixture_cwd_owner = TempDir::new().expect("cwd tempdir");
    // Source lives inside cwd, referenced relatively: local-but-not-absolute, so no warning.
    seed_source_repo(&fixture_cwd_owner.path().join("relsrc"));

    let config = "version = 1\n\n[sources.dotfiles]\ngit = \"relsrc\"\nbranch = \"main\"\n\
         include = [\"editor\"]\ndeploy = \"link\"\n\n\
         [targets.home]\npath = \"~/deploy\"\nsources = [\"dotfiles\"]\nlayout = \"by-source\"\n";

    let home = TempDir::new().expect("home tempdir");
    let home_path = home.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");
    write(
        &fixture_cwd_owner.path().join("phora.toml"),
        config.as_bytes(),
    );

    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(["sync"])
        .current_dir(fixture_cwd_owner.path())
        .env("HOME", &home_path)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_STATE_HOME", &xdg_state)
        .output()
        .expect("phora binary runs");

    assert!(
        out.status.success(),
        "a link over a non-absolute local path must sync successfully, got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr
            .lines()
            .any(|l| l.contains("dotfiles") && l.to_lowercase().contains("absolute")),
        "a link over a NON-absolute path must emit NO portability warning naming the source \
             with `absolute` — symmetric with the positive matcher, got stderr: {stderr:?}"
    );
}
