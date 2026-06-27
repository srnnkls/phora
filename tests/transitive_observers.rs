//! TDEP-CONFINE-OBSERVERS-001: the read-only observers (preview, verify) resolve and inject the
//! composed transitive graph, and `verify` hard-gates an untrusted, stripped `on_change` hook so
//! CI catches a composed artifact that is deployed but not post-processed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
    insteadof: Vec<(String, String)>,
}

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

fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    let home_path = home.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");
    Fixture {
        _home: home,
        cwd,
        home_path,
        xdg_cache,
        xdg_state,
        insteadof: Vec::new(),
    }
}

impl Fixture {
    fn map_url(&mut self, mock: &str, local: &Path) {
        self.insteadof
            .push((mock.to_owned(), local.display().to_string()));
    }

    fn finish_gitconfig(&self) {
        let mut body = String::new();
        for (mock, local) in &self.insteadof {
            let _ = write!(body, "[url \"{local}\"]\n\tinsteadOf = {mock}\n");
        }
        write(&self.home_path.join(".gitconfig"), body.as_bytes());
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

fn commit_repo(dir: &Path, files: &[(&str, &str)], manifest: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("phora.toml"), manifest.as_bytes());
    for (path, body) in files {
        write(&dir.join(path), body.as_bytes());
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "fixture"]);
}

fn leaf_repo(dir: &Path, file: &str, body: &str) {
    commit_repo(dir, &[(&format!("pkg/{file}"), body)], "version = 1\n");
}

fn dep_with_on_change_hook(dep: &Path, sentinel_abs: &str) {
    let manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n\n\
         [targets.nvim.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    commit_repo(dep, &[], &manifest);
}

/// A consumer importing the dep under `~/.config`; the composed target deploys at `~/.config/nvim`.
fn consumer_importing(fixture: &mut Fixture, dep: &Path, leaf: &Path) {
    fixture.map_url("https://github.com/mock/leaf.git", leaf);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
}

fn extract_first_preimage(lock_text: &str) -> Option<String> {
    for line in lock_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("preimage = \"")
            && let Some(end) = rest.find('"')
        {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

#[test]
fn verify_hard_gates_an_untrusted_stripped_transitive_hook() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let mut fixture = build_fixture();
    let sentinel = fixture.home_path.join("hook-ran.sentinel");
    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel.display().to_string());
    consumer_importing(&mut fixture, dep.path(), leaf.path());

    let synced = run(&fixture, &["sync"]);
    assert!(
        synced.status.success(),
        "the seeding sync must succeed (the untrusted hook is stripped, not fatal); stderr: {}",
        String::from_utf8_lossy(&synced.stderr)
    );

    let out = run(&fixture, &["verify"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "verify must exit non-zero so CI catches a composed artifact whose stripped hook is \
         untrusted (deployed but not post-processed); output:\n{combined}"
    );
    assert!(
        combined.contains("untrusted") && combined.contains("phora trust"),
        "verify must name the untrusted stripped hook and the `phora trust <source>` remedy; \
         output:\n{combined}"
    );
}

#[test]
fn verify_is_green_once_the_stripped_hook_is_trusted() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let mut fixture = build_fixture();
    let sentinel = fixture.home_path.join("hook-ran.sentinel");
    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel.display().to_string());
    consumer_importing(&mut fixture, dep.path(), leaf.path());

    let synced = run(&fixture, &["sync"]);
    assert!(synced.status.success(), "seeding sync must succeed");

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("sync wrote phora.lock");
    let preimage = extract_first_preimage(&lock_text)
        .expect("the sync must surface a commit-bound preimage to pin a trust approval against");
    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"approved-by-test\"\n\
         hook_id = \"composed#on_change#deadbeef\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n",
    );
    write(&lock_path, approved.as_bytes());

    let out = run(&fixture, &["verify"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "once the candidate's preimage is pinned in trusted_hooks, verify must exit zero \
         (anti-TOFU: the match is what grants trust); output:\n{combined}"
    );
    assert!(
        !combined.contains("untrusted"),
        "a trusted hook must produce NO untrusted finding; output:\n{combined}"
    );
}

#[test]
fn preview_shows_the_confined_composed_target_path() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let mut fixture = build_fixture();
    let sentinel = fixture.home_path.join("hook-ran.sentinel");
    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel.display().to_string());
    consumer_importing(&mut fixture, dep.path(), leaf.path());

    let synced = run(&fixture, &["sync"]);
    assert!(synced.status.success(), "seeding sync must succeed");

    let out = run(&fixture, &["preview"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "preview over a synced transitive setup must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(".config/nvim"),
        "preview must surface the injected composed target's CONFINED destination \
         (~/.config/nvim), proving the observer resolved and injected the graph; stdout:\n{stdout}"
    );
}

#[test]
fn preview_json_marks_a_composed_target_with_an_untrusted_stripped_hook() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let mut fixture = build_fixture();
    let sentinel = fixture.home_path.join("hook-ran.sentinel");
    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel.display().to_string());
    consumer_importing(&mut fixture, dep.path(), leaf.path());

    let synced = run(&fixture, &["sync"]);
    assert!(synced.status.success(), "seeding sync must succeed");

    let out = run(&fixture, &["preview", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "preview --json must succeed");
    assert!(
        stdout.contains("\"untrusted_stripped_hook\": true"),
        "preview --json must mark the composed target carrying an untrusted stripped hook so \
         automation can parse it; stdout:\n{stdout}"
    );
}

#[test]
fn preview_degrades_gracefully_when_the_transitive_dep_is_unsynced() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let mut fixture = build_fixture();
    let sentinel = fixture.home_path.join("hook-ran.sentinel");
    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel.display().to_string());
    consumer_importing(&mut fixture, dep.path(), leaf.path());

    let out = run(&fixture, &["preview"]);
    assert!(
        out.status.success(),
        "an observer must NOT hard-fail when a transitive dep is not yet synced/pinned — it must \
         degrade to the un-injected view; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
