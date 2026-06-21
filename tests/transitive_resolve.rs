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

#[test]
fn frozen_refuses_to_fetch_an_unpinned_transitive_manifest() {
    let inner = TempDir::new().expect("inner repo");
    git(inner.path(), &["init", "-b", "main", "."]);
    git(inner.path(), &["config", "user.email", "test@example.com"]);
    git(inner.path(), &["config", "user.name", "Test"]);
    write(&inner.path().join("payload.txt"), b"inner\n");
    git(inner.path(), &["add", "-A"]);
    git(inner.path(), &["commit", "-m", "inner"]);

    let dep = TempDir::new().expect("dep repo");
    dep_repo(dep.path(), "inner", &inner.path().display().to_string());

    let fixture = build_fixture();
    let config = format!(
        "version = 1\n\n[sources.dep]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let frozen = run(&fixture, &["sync", "--frozen"]);
    let stderr = String::from_utf8_lossy(&frozen.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !frozen.status.success(),
        "--frozen with no lock must refuse to fetch the transitive manifest, got success; stderr: {stderr}"
    );
    assert!(
        stderr.contains("dep") && stderr.contains("--frozen refuses to fetch its manifest"),
        "the frozen diagnostic must name the unpinned transitive source (`dep`) and attribute the \
         refusal to --frozen, got: {stderr}"
    );
    assert!(
        !fixture.cwd.path().join("phora.lock").exists(),
        "--frozen must fail before any lock write when the manifest is unpinned"
    );

    let unfrozen = run(&fixture, &["sync"]);
    let unfrozen_stderr = String::from_utf8_lossy(&unfrozen.stderr);
    assert!(
        !unfrozen_stderr.contains("--frozen"),
        "a non-frozen run must get PAST the manifest fetch (failing later on the inner local-path \
         escape), proving --frozen alone blocked the fetch; got: {unfrozen_stderr}"
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

fn dep_repo_nested_transitive(dir: &Path, inner_name: &str, inner_mock_url: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    let manifest = format!(
        "version = 1\n\n[sources.{inner_name}]\ngit = \"{inner_mock_url}\"\ntransitive = true\n\n\
         [targets.t]\npath = \"sub\"\nimports = [\"{inner_name}\"]\n",
    );
    write(&dir.join("phora.toml"), manifest.as_bytes());
    write(&dir.join("sub/file.txt"), b"payload\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "dep"]);
}

#[test]
fn frozen_refuses_to_fetch_an_unpinned_nested_transitive_manifest() {
    let leaf = TempDir::new().expect("leaf repo");
    git(leaf.path(), &["init", "-b", "main", "."]);
    git(leaf.path(), &["config", "user.email", "test@example.com"]);
    git(leaf.path(), &["config", "user.name", "Test"]);
    write(&leaf.path().join("phora.toml"), b"version = 1\n");
    write(&leaf.path().join("file.txt"), b"leaf\n");
    git(leaf.path(), &["add", "-A"]);
    git(leaf.path(), &["commit", "-m", "leaf"]);

    let leaf_url = leaf.path().display().to_string();
    let inner_mock = "https://github.com/mock/inner.git";

    let dep = TempDir::new().expect("dep repo");
    dep_repo_nested_transitive(dep.path(), "inner", inner_mock);

    let fixture = build_fixture();
    // Redirect the mock URL to the leaf repo so the nested fetch is reachable offline:
    // the only thing that may stop it is the frozen gate, not an unreachable remote.
    let gitconfig = format!("[url \"{leaf_url}\"]\n\tinsteadOf = {inner_mock}\n");
    write(&fixture.home_path.join(".gitconfig"), gitconfig.as_bytes());

    let config = format!(
        "version = 1\n\n[sources.dep]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let seed = run(&fixture, &["sync"]);
    let seed_stderr = String::from_utf8_lossy(&seed.stderr);
    reject_unknown_field_stub(&seed_stderr);
    let lock_path = fixture.cwd.path().join("phora.lock");
    let full_lock = std::fs::read_to_string(&lock_path).unwrap_or_else(|_| {
        panic!("the unfrozen seed run must write a full lock; stderr: {seed_stderr}")
    });

    let pinned_anchor_only = strip_instance_entries(&full_lock);
    assert!(
        pinned_anchor_only.contains("name = \"dep\"") && !pinned_anchor_only.contains("instance ="),
        "the lock must pin the depth-1 anchor `dep` while leaving the nested `inner` unpinned; got:\n{pinned_anchor_only}"
    );
    write(&lock_path, pinned_anchor_only.as_bytes());
    let lock_before = std::fs::read(&lock_path).expect("lock readable before frozen run");

    let frozen = run(&fixture, &["sync", "--frozen"]);
    let stderr = String::from_utf8_lossy(&frozen.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        !frozen.status.success(),
        "--frozen must refuse to fetch an UNPINNED nested transitive manifest even when its \
         depth-1 anchor IS pinned, got success; stderr: {stderr}"
    );
    assert!(
        stderr.contains("inner") && stderr.contains("--frozen"),
        "the frozen diagnostic must name the unpinned NESTED source (`inner`) and attribute the \
         refusal to --frozen, not silently fetch it; got: {stderr}"
    );
    assert_eq!(
        std::fs::read(&lock_path).expect("lock readable after frozen run"),
        lock_before,
        "the frozen refusal must fail-fast before any lock write, leaving the anchor-only lock \
         byte-unchanged"
    );
}

fn strip_instance_entries(lock_toml: &str) -> String {
    let mut out = String::new();
    let mut block = String::new();
    let flush = |block: &str, out: &mut String| {
        if !block.contains("instance =") {
            out.push_str(block);
        }
    };
    for line in lock_toml.lines() {
        if line.trim_start().starts_with("[[sources]]") {
            flush(&block, &mut out);
            block = String::new();
        }
        block.push_str(line);
        block.push('\n');
    }
    flush(&block, &mut out);
    out
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

/// A flat git leaf holding `pkg/<file>`; reached as a depth-3 composed source so the
/// produced lock carries a nested node with `instance = Some(...)`.
fn leaf_repo(dir: &Path, file: &str, body: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("phora.toml"), b"version = 1\n");
    write(&dir.join(format!("pkg/{file}")), body.as_bytes());
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "leaf"]);
}

fn commit_manifest(dir: &Path, manifest: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("phora.toml"), manifest.as_bytes());
    write(&dir.join("anchor/keep.txt"), b"keep\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "dep"]);
}

/// Counts `[[sources]]` entries that carry an `instance = "..."` key (nested nodes).
fn count_instance_entries(lock_toml: &str) -> usize {
    lock_toml
        .lines()
        .filter(|l| l.trim_start().starts_with("instance ="))
        .count()
}

/// Removes the first `[[sources]]` block carrying an `instance =` key, simulating a
/// dropped nested pin so `--frozen` must hard-error on the now-unpinned node.
fn drop_first_instance_entry(lock_toml: &str) -> String {
    let mut out = String::new();
    let mut block = String::new();
    let mut dropped = false;
    let flush = |block: &str, out: &mut String, dropped: &mut bool| {
        if !*dropped && block.contains("instance =") {
            *dropped = true;
            return;
        }
        out.push_str(block);
    };
    for line in lock_toml.lines() {
        if line.trim_start().starts_with("[[sources]]") {
            flush(&block, &mut out, &mut dropped);
            block = String::new();
        }
        block.push_str(line);
        block.push('\n');
    }
    flush(&block, &mut out, &mut dropped);
    out
}

/// Round-trip regression guard (TDEP-LOCK-001 fix2): a REAL unfrozen sync of a depth≥2
/// composed dep must write a lock whose nested node carries `instance = Some(...)`, and
/// an UNMODIFIED `--frozen` re-run against that lock must SUCCEED at every depth — even
/// with a validation-only nested transitive source present. Tampering (dropping a nested
/// pin) must make `--frozen` hard-error naming the node.
#[test]
fn unfrozen_sync_locks_nested_instance_and_unmodified_frozen_roundtrips() {
    let leaf = TempDir::new().expect("e leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "from-e\n");

    let vonly_leaf = TempDir::new().expect("validation-only leaf repo");
    leaf_repo(vonly_leaf.path(), "vonly.txt", "vonly\n");

    let dep_e = TempDir::new().expect("dep E repo");
    let e_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/eleaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n";
    commit_manifest(dep_e.path(), e_manifest);

    let dep_d = TempDir::new().expect("dep D repo");
    let d_manifest = "version = 1\n\n\
         [sources.einner]\ngit = \"https://github.com/mock/depe.git\"\ntransitive = true\n\n\
         [sources.vonly]\ngit = \"https://github.com/mock/vonly.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d\"\nimports = [\"einner\"]\n";
    commit_manifest(dep_d.path(), d_manifest);

    let fixture = build_fixture();
    let gitconfig = format!(
        "[url \"{eleaf}\"]\n\tinsteadOf = https://github.com/mock/eleaf.git\n\
         [url \"{depe}\"]\n\tinsteadOf = https://github.com/mock/depe.git\n\
         [url \"{vonly}\"]\n\tinsteadOf = https://github.com/mock/vonly.git\n",
        eleaf = leaf.path().display(),
        depe = dep_e.path().display(),
        vonly = vonly_leaf.path().display(),
    );
    write(&fixture.home_path.join(".gitconfig"), gitconfig.as_bytes());

    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep_d}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep_d = dep_d.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let seed = run(&fixture, &["sync"]);
    let seed_stderr = String::from_utf8_lossy(&seed.stderr);
    reject_unknown_field_stub(&seed_stderr);
    assert!(
        seed.status.success(),
        "the unfrozen seed sync of a depth-2 composed dep must succeed; stderr: {seed_stderr}"
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let full_lock = std::fs::read_to_string(&lock_path).unwrap_or_else(|_| {
        panic!("the unfrozen seed run must write a lock; stderr: {seed_stderr}")
    });

    assert!(
        count_instance_entries(&full_lock) >= 1,
        "INVARIANT 1: a normal sync of a depth-2 composed dep must persist the nested node with \
         `instance = Some(<owning stable_key>)`; production hard-coded instance = None, so the \
         instance column was dead. Got lock:\n{full_lock}"
    );
    assert!(
        full_lock.contains("name = \"mydeps\"")
            && full_lock
                .split("[[sources]]")
                .any(|b| b.contains("name = \"mydeps\"") && !b.contains("instance =")),
        "the depth-1 anchor `mydeps` must remain a consumer-root node (instance = None); got:\n{full_lock}"
    );

    let lock_before = std::fs::read(&lock_path).expect("lock readable before frozen run");

    let frozen = run(&fixture, &["sync", "--frozen"]);
    let frozen_stderr = String::from_utf8_lossy(&frozen.stderr);
    reject_unknown_field_stub(&frozen_stderr);
    assert!(
        frozen.status.success(),
        "INVARIANT 2: `sync --frozen` against the UNMODIFIED, freshly-written lock must succeed at \
         every depth (no false `not pinned`), including past the validation-only nested source; \
         stderr: {frozen_stderr}"
    );
    assert!(
        !frozen_stderr.contains("not pinned") && !frozen_stderr.contains("refuses to fetch"),
        "the unmodified frozen round-trip must not emit any frozen-miss diagnostic; got: {frozen_stderr}"
    );
    assert_eq!(
        std::fs::read(&lock_path).expect("lock readable after frozen run"),
        lock_before,
        "a successful frozen round-trip must not rewrite the lock"
    );

    let tampered = drop_first_instance_entry(&full_lock);
    assert!(
        count_instance_entries(&tampered) < count_instance_entries(&full_lock),
        "the tamper helper must actually remove a nested pin"
    );
    write(&lock_path, tampered.as_bytes());
    let tampered_before = std::fs::read(&lock_path).expect("tampered lock readable");

    let drift = run(&fixture, &["sync", "--frozen"]);
    let drift_stderr = String::from_utf8_lossy(&drift.stderr);
    reject_unknown_field_stub(&drift_stderr);
    assert!(
        !drift.status.success(),
        "INVARIANT 3: a dropped nested pin must make `--frozen` hard-error, not silently re-fetch; \
         stderr: {drift_stderr}"
    );
    assert!(
        drift_stderr.contains("transitive") && drift_stderr.contains("--frozen"),
        "the frozen miss for a dropped nested pin must attribute to a transitive source and \
         --frozen — consistently, whether the walk's manifest gate (depth-N pin) or the resolve \
         gate (composed-leaf pin) catches the drop; got: {drift_stderr}"
    );
    assert_eq!(
        std::fs::read(&lock_path).expect("lock readable after drift run"),
        tampered_before,
        "the frozen refusal must fail-fast before any lock write"
    );
}

/// Offline guarantee (TDEP-LOCK-001 fix3): once a lock pins a transitive dep, `--frozen`
/// reads its manifest from the mirror at the locked commit and never fetches or re-resolves.
#[test]
fn frozen_reads_transitive_manifest_offline_without_fetching() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/offleaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n";
    commit_manifest(dep.path(), dep_manifest);

    let fixture = build_fixture();
    let reachable = format!(
        "[url \"{leaf}\"]\n\tinsteadOf = https://github.com/mock/offleaf.git\n\
         [url \"{dep}\"]\n\tinsteadOf = https://github.com/mock/offdep.git\n",
        leaf = leaf.path().display(),
        dep = dep.path().display(),
    );
    write(&fixture.home_path.join(".gitconfig"), reachable.as_bytes());

    let config = "version = 1\n\n\
         [sources.mydeps]\ngit = \"https://github.com/mock/offdep.git\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n";
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let seed = run(&fixture, &["sync"]);
    let seed_stderr = String::from_utf8_lossy(&seed.stderr);
    reject_unknown_field_stub(&seed_stderr);
    assert!(
        seed.status.success(),
        "the unfrozen seed must populate the mirrors and write a lock; stderr: {seed_stderr}"
    );

    let unreachable =
        "[url \"/nonexistent/phora-offline-guard\"]\n\tinsteadOf = https://github.com/mock/\n";
    write(
        &fixture.home_path.join(".gitconfig"),
        unreachable.as_bytes(),
    );

    let frozen = run(&fixture, &["sync", "--frozen"]);
    let frozen_stderr = String::from_utf8_lossy(&frozen.stderr);
    reject_unknown_field_stub(&frozen_stderr);
    assert!(
        frozen.status.success(),
        "--frozen must read the pinned transitive manifest offline from the mirror; with every \
         remote repointed to a missing path, any fetch would fail, so success proves no fetch; \
         stderr: {frozen_stderr}"
    );
    assert!(
        !frozen_stderr.contains("refuses to fetch") && !frozen_stderr.contains("not pinned"),
        "a fully-pinned frozen run must emit no frozen-miss diagnostic; got: {frozen_stderr}"
    );
}
