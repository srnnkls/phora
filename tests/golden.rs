//! Golden snapshots of the real phora binary's observable output, captured as a
//! subprocess so they survive internal refactors of the render path.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: std::path::PathBuf,
    src_path: std::path::PathBuf,
    target_path: std::path::PathBuf,
    xdg_cache: std::path::PathBuf,
    xdg_state: std::path::PathBuf,
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
    write(&root.join("editor/lua/opts.lua"), b"return {}\n");
    write(&root.join("lint/rules.toml"), b"[rules]\n");
    write(&root.join("README.md"), b"loose root file\n");
    write(&root.join(".config/settings.json"), b"{\"k\":1}\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

fn build_fixture() -> Fixture {
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
        "version = 1\n\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\", \"lint\"]\n\n[targets.home]\npath = \"{target}\"\n\
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
        src_path,
        target_path,
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

fn snapshot(out: &Output) -> String {
    format!(
        "exit: {}\n--- stdout ---\n{}--- stderr ---\n{}",
        out.status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string()),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

fn settings(fixture: &Fixture) -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.add_filter(
        &regex_escape(&fixture.xdg_cache.to_string_lossy()),
        "<XDG_CACHE>",
    );
    s.add_filter(
        &regex_escape(&fixture.xdg_state.to_string_lossy()),
        "<XDG_STATE>",
    );
    s.add_filter(
        &regex_escape(&fixture.home_path.to_string_lossy()),
        "<HOME>",
    );
    s.add_filter(&regex_escape(&fixture.src_path.to_string_lossy()), "<SRC>");
    s.add_filter(
        &regex_escape(&fixture.cwd.path().to_string_lossy()),
        "<CWD>",
    );
    s.add_filter(
        &regex_escape(&fixture.target_path.to_string_lossy()),
        "<TARGET>",
    );
    s.add_filter(r"\b[0-9a-f]{40}\b", "<COMMIT>");
    s.add_filter(r"commit [0-9a-f]{8}", "commit <COMMIT8>");
    s.add_filter(r"@[0-9a-f]{8}", "@<COMMIT8>");
    s.add_filter(r"\b[0-9a-f]{16}\b", "<PROJECT>");
    s.add_filter(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z", "<TIME>");
    s
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[test]
fn golden_cli_output() {
    let fixture = build_fixture();

    let sync = run(&fixture, &["sync"]);
    assert!(
        sync.status.success(),
        "fixture sync must succeed before snapshotting downstream commands: {}",
        String::from_utf8_lossy(&sync.stderr)
    );

    settings(&fixture).bind(|| {
        insta::assert_snapshot!("sync", snapshot(&sync));
        insta::assert_snapshot!("list", snapshot(&run(&fixture, &["list"])));
        insta::assert_snapshot!("verify", snapshot(&run(&fixture, &["verify"])));
        insta::assert_snapshot!("where", snapshot(&run(&fixture, &["where"])));
        insta::assert_snapshot!(
            "check-match-included",
            snapshot(&run(
                &fixture,
                &["check-match", "--source", "dotfiles", "editor/init.lua"],
            ))
        );
        insta::assert_snapshot!(
            "check-match-excluded",
            snapshot(&run(
                &fixture,
                &["check-match", "--source", "dotfiles", "vim"],
            ))
        );
        insta::assert_snapshot!("preview", snapshot(&run(&fixture, &["preview"])));
        insta::assert_snapshot!(
            "preview-files",
            snapshot(&run(&fixture, &["preview", "--files"]))
        );
        insta::assert_snapshot!(
            "preview-json",
            snapshot(&run(&fixture, &["preview", "--json"]))
        );
    });
}
