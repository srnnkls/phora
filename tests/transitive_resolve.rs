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

fn reject_unknown_field_stub(stderr: &str) {
    assert!(
        !stderr.contains("unknown field"),
        "`transitive`/`imports` must be accepted wire keys driving real recursion, \
         not rejected by deny_unknown_fields; got a parse stub: {stderr}"
    );
}

// Contract: phora must emit this exact phrase to reject an escaping transitive remote.
const TRANSITIVE_ESCAPE_DIAGNOSTIC: &str = "transitive remote not allowed";

// Contract: a fail-fast diagnostic below top level must carry this recursion-depth marker.
const TRANSITIVE_DEPTH_MARKER: &str = "at depth";

/// A git repo whose committed `phora.toml` declares `inner` as transitive,
/// pointing at `inner_url`. The dep's own root config is what recursion fetches.
fn dep_repo(dir: &Path, inner_name: &str, inner_url: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    let manifest = format!(
        "version = 1\n\n[sources.{inner_name}]\ngit = \"{inner_url}\"\ntransitive = true\n\n\
         [targets.t]\npath = \"sub\"\nsources = [\"{inner_name}\"]\n",
    );
    write(&dir.join("phora.toml"), manifest.as_bytes());
    write(&dir.join("sub/file.txt"), b"payload\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "dep"]);
}

#[test]
fn fetch_failure_at_depth_writes_no_lock() {
    let dep = TempDir::new().expect("dep repo");
    dep_repo(
        dep.path(),
        "missing",
        "https://invalid.invalid/does-not-exist.git",
    );

    let fixture = build_fixture();
    let config = format!(
        "version = 1\n\n[sources.dep]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let lock_path = fixture.cwd.path().join("phora.lock");
    let sentinel = b"version = 2\n# pre-existing lock that fail-fast must not overwrite\n";
    write(&lock_path, sentinel);

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "a transitive fetch failure must fail the sync, got success; stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read(&lock_path).expect("lock readable"),
        sentinel,
        "a fetch failure below the top level must fail-fast before any lock write, \
         leaving the existing lock byte-unchanged"
    );
    assert!(
        stderr.contains("missing"),
        "the fail-fast diagnostic must name the failing transitive source (`missing`), got: {stderr}"
    );
    assert!(
        stderr.contains(TRANSITIVE_DEPTH_MARKER),
        "the fail-fast diagnostic must report the recursion depth so a plain top-level fetch \
         error cannot satisfy it; expected `{TRANSITIVE_DEPTH_MARKER}`, got: {stderr}"
    );
}

#[test]
fn transitive_source_with_absolute_path_remote_is_rejected() {
    let fixture = build_fixture();
    let config = "version = 1\n\n[sources.dep]\npath = \"/etc\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n";
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "a transitive source using an absolute local path must be rejected"
    );
    assert!(
        stderr.contains(TRANSITIVE_ESCAPE_DIAGNOSTIC),
        "the rejection must emit the named escape diagnostic `{TRANSITIVE_ESCAPE_DIAGNOSTIC}`, \
         not a generic git error echoing the path, got: {stderr}"
    );
    assert!(
        !fixture.cwd.path().join("phora.lock").exists(),
        "rejecting an escaping transitive remote must write no lock"
    );
}

#[test]
fn transitive_source_with_file_url_remote_is_rejected() {
    let fixture = build_fixture();
    let config = "version = 1\n\n[sources.dep]\ngit = \"file:///etc/passwd\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n";
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "a transitive source using a file:// remote must be rejected unless inside the materialized tree"
    );
    assert!(
        stderr.contains(TRANSITIVE_ESCAPE_DIAGNOSTIC),
        "the rejection must emit the named escape diagnostic `{TRANSITIVE_ESCAPE_DIAGNOSTIC}`, \
         not a generic git error echoing the file:// URL, got: {stderr}"
    );
    assert!(
        !fixture.cwd.path().join("phora.lock").exists(),
        "rejecting a file:// transitive remote must write no lock"
    );
}

#[test]
fn transitive_cycle_terminates_via_url_ref_visited_set() {
    let dep_a = TempDir::new().expect("dep a");
    let dep_b = TempDir::new().expect("dep b");
    let a_url = dep_a.path().display().to_string();
    let b_url = dep_b.path().display().to_string();

    let a_mock = "https://github.com/mock/a.git";
    let b_mock = "https://github.com/mock/b.git";

    dep_repo(dep_a.path(), "b", b_mock);
    dep_repo(dep_b.path(), "a", a_mock);

    let fixture = build_fixture();

    // Write a local .gitconfig so git resolves mock URLs to local folders offline
    let gitconfig = format!(
        "[url \"{a_url}\"]\n\tinsteadOf = {a_mock}\n[url \"{b_url}\"]\n\tinsteadOf = {b_mock}\n"
    );
    write(&fixture.home_path.join(".gitconfig"), gitconfig.as_bytes());

    let config = format!(
        "version = 1\n\n[sources.a]\ngit = \"{a_mock}\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"a\"]\n",
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "a 2-node transitive cycle must terminate via the (url, ref) visited-set and complete the sync, got stderr: {stderr}"
    );
    assert!(
        !stderr.contains("stack overflow") && !stderr.contains("recursion limit"),
        "the (url, ref) visited-set must bound a transitive cycle, not blow the stack; stderr: {stderr}"
    );
}

#[test]
fn local_overlay_can_flip_a_source_to_transitive() {
    let dep = TempDir::new().expect("dep repo");
    dep_repo(
        dep.path(),
        "missing",
        "https://invalid.invalid/does-not-exist.git",
    );

    let fixture = build_fixture();
    let base = format!(
        "version = 1\n\n[sources.dep]\ngit = \"{dep}\"\n\n\
         [targets.flatdep]\npath = \"~/deploy\"\nsources = [\"dep\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), base.as_bytes());

    let control = run(&fixture, &["sync"]);
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    reject_unknown_field_stub(&control_stderr);
    assert!(
        !control_stderr.contains(TRANSITIVE_DEPTH_MARKER),
        "CONTROL: the base config alone is FLAT (no `transitive`), so it must not recurse into \
         the inner source; the depth diagnostic must be absent, got: {control_stderr}"
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let control_lock = std::fs::read(&lock_path).expect(
        "CONTROL: the flat sync must succeed and write a lock; that lock is the baseline the \
         overlay-driven fail-fast must not mutate",
    );

    let overlay = "version = 1\n\n[sources.dep]\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n";
    write(
        &fixture.cwd.path().join("phora.local.toml"),
        overlay.as_bytes(),
    );

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "a phora.local.toml overlay flipping `transitive = true` must drive recursion into the (failing) inner source; stderr: {stderr}"
    );
    assert!(
        stderr.contains(TRANSITIVE_DEPTH_MARKER),
        "the failure must be attributable to overlay-driven recursion: the depth diagnostic \
         absent in the control must now appear, expected `{TRANSITIVE_DEPTH_MARKER}`, got: {stderr}"
    );
    assert_eq!(
        std::fs::read(&lock_path).expect("lock readable"),
        control_lock,
        "the overlay-driven recursion failure must fail-fast before any lock write, leaving the \
         control run's lock byte-unchanged"
    );
}

fn dep_repo_custom(dir: &Path, manifest_content: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("phora.toml"), manifest_content.as_bytes());
    write(&dir.join("sub/file.txt"), b"payload\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "dep"]);
}

#[test]
fn transitive_nested_flat_source_with_absolute_path_remote_is_rejected() {
    let dep = TempDir::new().expect("dep repo");
    let manifest = "\
version = 1

[sources.nested_flat]
path = \"/etc\"

[targets.t]
path = \"sub\"
sources = [\"nested_flat\"]
";
    dep_repo_custom(dep.path(), manifest);

    let fixture = build_fixture();
    let config = format!(
        "version = 1\n\n[sources.dep]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "a transitive source using a nested flat source with absolute path must be rejected"
    );
    assert!(
        stderr.contains(TRANSITIVE_ESCAPE_DIAGNOSTIC),
        "the rejection must emit the named escape diagnostic `{TRANSITIVE_ESCAPE_DIAGNOSTIC}`, got: {stderr}"
    );
}

#[test]
fn transitive_nested_transitive_source_with_absolute_path_git_remote_is_rejected() {
    let dep = TempDir::new().expect("dep repo");
    let manifest = "\
version = 1

[sources.nested_git_abs]
git = \"/etc/passwd\"
transitive = true

[targets.t]
path = \"sub\"
sources = [\"nested_git_abs\"]
";
    dep_repo_custom(dep.path(), manifest);

    let fixture = build_fixture();
    let config = format!(
        "version = 1\n\n[sources.dep]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "a transitive source using a nested transitive source with absolute git path must be rejected"
    );
    assert!(
        stderr.contains(TRANSITIVE_ESCAPE_DIAGNOSTIC),
        "the rejection must emit the named escape diagnostic `{TRANSITIVE_ESCAPE_DIAGNOSTIC}`, got: {stderr}"
    );
}
