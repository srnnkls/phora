use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::str::FromStr as _;

use phora::config::Refspec;
use phora::kernel::SourceName;
use phora::source::{
    GitBackend, HttpBackend, ResolvedSource, SnapshotId, SourceBackend as _, SourceEntry,
    SourceEntryKind, SourceError, SourceInventory, SourcePath, SourceStore, capture_worktree,
};
use tempfile::TempDir;

mod common;

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
}

fn sp(path: &str) -> SourcePath {
    SourcePath::new(path).expect("safe source path")
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

fn rev_parse_head(cwd: &Path) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse runs");
    assert!(out.status.success(), "rev-parse succeeds");
    String::from_utf8(out.stdout)
        .expect("utf8 sha")
        .trim()
        .to_owned()
}

// ─── golden-fixture harness (new T012 fixtures; existing goldens read-only) ──

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat/serialized")
}

fn read_golden(name: &str) -> String {
    let path = golden_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read golden fixture {}: {e}", path.display()))
}

fn line_diff(old: &str, new: &str) -> String {
    let mut diff = String::new();
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    for i in 0..old_lines.len().max(new_lines.len()) {
        match (old_lines.get(i), new_lines.get(i)) {
            (a, b) if a == b => {}
            (a, b) => {
                if let Some(a) = a {
                    let _ = writeln!(diff, "- {a}");
                }
                if let Some(b) = b {
                    let _ = writeln!(diff, "+ {b}");
                }
            }
        }
    }
    let terminator = |s: &str| {
        if s.ends_with('\n') {
            "present"
        } else {
            "absent"
        }
    };
    if old.ends_with('\n') != new.ends_with('\n') {
        let _ = writeln!(
            diff,
            "~ final newline: old={} new={}",
            terminator(old),
            terminator(new),
        );
    }
    diff
}

fn current_head() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn tracked_worktree_is_dirty() -> Option<String> {
    let out = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let dirty = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!dirty.is_empty()).then_some(dirty)
}

fn regenerate_golden(path: &Path, actual: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "PHORA_UPDATE_GOLDEN must never be set while CI is set: a golden fixture may not \
         self-bless in CI. Unset PHORA_UPDATE_GOLDEN and commit regenerated fixtures explicitly."
    );
    assert!(
        current_head().is_some(),
        "refusing to regenerate T012 fixtures: `git rev-parse HEAD` did not succeed (git \
         unavailable or not a repo); a capture must be attributable to a commit"
    );
    assert!(
        tracked_worktree_is_dirty().is_none(),
        "refusing to regenerate T012 fixtures against a dirty worktree: tracked files carry \
         uncommitted modifications. Commit or stash tracked changes first."
    );
    if let Ok(old) = std::fs::read_to_string(path)
        && old != actual
    {
        eprintln!(
            "REGENERATING {} — bytes changed (old ↓ / new ↓):\n{}",
            path.display(),
            line_diff(&old, actual)
        );
    }
    std::fs::create_dir_all(path.parent().expect("golden parent")).expect("create golden dir");
    std::fs::write(path, actual).expect("write golden fixture");
}

fn assert_golden(name: &str, actual: &str) {
    let path = golden_dir().join(name);
    if let Some(val) = std::env::var_os("PHORA_UPDATE_GOLDEN") {
        assert_eq!(
            val,
            std::ffi::OsStr::new("1"),
            "PHORA_UPDATE_GOLDEN must be exactly \"1\" to regenerate golden fixtures, got {}; \
             refusing an ambiguous regeneration that could silently bless a regression",
            val.display()
        );
        regenerate_golden(&path, actual);
        return;
    }
    assert!(path.exists(), "golden fixture missing: {}", path.display());
    let expected = std::fs::read_to_string(&path).expect("read golden fixture");
    assert_eq!(
        actual,
        expected,
        "captured bytes drifted from the pinned fixture {}",
        path.display()
    );
}

#[test]
fn line_diff_announces_final_newline_only_drift() {
    let dropped = line_diff("baseline\n", "baseline");
    assert!(
        dropped.contains("newline"),
        "line_diff must name a final-newline-only drift, got: {dropped:?}"
    );
    let added = line_diff("baseline", "baseline\n");
    assert_ne!(
        dropped, added,
        "dropped vs added final newline must render as distinct diffs"
    );
    assert!(
        !line_diff("same\n", "same\n").contains("newline"),
        "line_diff must not announce newline drift when both terminators match"
    );
}

// ─── worktree capture fixtures ──────────────────────────────────────────────

#[cfg(unix)]
fn build_capture_matrix(root: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);

    write(&root.join(".config/settings.json"), b"{\"k\":1}\n");
    write(&root.join(".gitignore"), b"ignored.log\n");
    write(&root.join("README.md"), b"committed\n");
    write(&root.join("tracked-deleted.txt"), b"doomed\n");
    write(&root.join("bin/run.sh"), b"#!/bin/sh\n");
    let mut perms = std::fs::metadata(root.join("bin/run.sh"))
        .expect("stat run.sh")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(root.join("bin/run.sh"), perms).expect("chmod run.sh");
    std::os::unix::fs::symlink("README.md", root.join("link-to-readme")).expect("symlink fixture");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);

    write(&root.join("README.md"), b"modified\n");
    std::fs::remove_file(root.join("tracked-deleted.txt")).expect("delete tracked file");
    write(&root.join("untracked.txt"), b"untracked\n");
    write(&root.join("ignored.log"), b"ignored\n");
    write(&root.join("plain-dir/data.txt"), b"plain\n");
    std::fs::create_dir_all(root.join("nested")).expect("create nested repo dir");
    git(&root.join("nested"), &["init", "-b", "main", "."]);
    write(&root.join("nested/inner.txt"), b"inner\n");
}

fn resolved_worktree(root: &Path, snapshot: SnapshotId) -> ResolvedSource {
    ResolvedSource {
        name: sn("wt"),
        url: root.to_string_lossy().into_owned(),
        snapshot,
    }
}

#[cfg(unix)]
fn cache_entries(cache_dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(cache_dir)
        .expect("read cache dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn worktree_digest(snapshot: &SnapshotId) -> String {
    match snapshot {
        SnapshotId::Worktree { capture_digest, .. } => capture_digest.clone(),
        other @ SnapshotId::Git { .. } => panic!("expected a worktree snapshot, got {other:?}"),
    }
}

fn render_inventory(inventory: &SourceInventory) -> String {
    let mut doc = String::new();
    for entry in &inventory.entries {
        let kind = match entry.kind {
            SourceEntryKind::File => "File",
            SourceEntryKind::Executable => "Executable",
            SourceEntryKind::Symlink => "Symlink",
        };
        let _ = writeln!(doc, "{} {kind}", entry.path.as_str());
    }
    doc
}

fn read_bytes(store: &GitBackend, resolved: &ResolvedSource, path: &str) -> Vec<u8> {
    let entry: SourceEntry = store
        .read(resolved, &sp(path))
        .unwrap_or_else(|e| panic!("read captured `{path}`: {e}"));
    entry.bytes
}

// ─── worktree capture set (codex-R4-H1) ─────────────────────────────────────

#[cfg(unix)]
#[test]
fn worktree_capture_set_matches_the_pinned_fixture() {
    let src = TempDir::new().expect("src tempdir");
    build_capture_matrix(src.path());
    let cache = TempDir::new().expect("cache tempdir");

    let snapshot =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("worktree capture succeeds");
    let SnapshotId::Worktree {
        ref root, ref head, ..
    } = snapshot
    else {
        panic!("a local working tree must capture as SnapshotId::Worktree, got {snapshot:?}");
    };
    assert_eq!(
        root.canonicalize().expect("canonicalize captured root"),
        src.path()
            .canonicalize()
            .expect("canonicalize fixture root"),
        "SnapshotId::Worktree must record the captured root"
    );
    assert_eq!(
        head,
        &rev_parse_head(src.path()),
        "SnapshotId::Worktree must record the worktree's HEAD commit"
    );

    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);
    let inventory = store.inventory(&resolved).expect("captured inventory");

    let paths: Vec<&str> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(
        !paths.contains(&"link-to-readme"),
        "worktree capture must skip symlinks (parity with the current discovery walk, \
         sync::discover::collect_working_tree_leaves): {paths:?}"
    );
    assert!(
        store.read(&resolved, &sp("link-to-readme")).is_err(),
        "reading a skipped symlink path must err, not serve link bytes"
    );
    assert!(
        !paths.contains(&"tracked-deleted.txt"),
        "a tracked-then-deleted file is absent from the CURRENT worktree and must not be \
         captured: {paths:?}"
    );
    assert!(
        store.read(&resolved, &sp("tracked-deleted.txt")).is_err(),
        "reading a deleted-tracked path must err, not resurrect the committed blob"
    );
    let run_sh = inventory
        .entries
        .iter()
        .find(|entry| entry.path.as_str() == "bin/run.sh")
        .expect("bin/run.sh is captured");
    assert_eq!(
        run_sh.kind,
        SourceEntryKind::Executable,
        "the exec bit must survive capture as SourceEntryKind::Executable"
    );

    let rendered = render_inventory(&inventory);
    assert!(
        !rendered.contains(&src.path().to_string_lossy().into_owned()),
        "inventory paths must be source-relative, never absolute: {rendered}"
    );
    assert_golden("worktree_capture_set.golden", &rendered);
}

#[cfg(unix)]
#[test]
fn worktree_capture_reads_modified_content_from_the_disk_state() {
    let src = TempDir::new().expect("src tempdir");
    build_capture_matrix(src.path());
    let cache = TempDir::new().expect("cache tempdir");

    let snapshot =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("worktree capture succeeds");
    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);

    assert_eq!(
        read_bytes(&store, &resolved, "README.md"),
        b"modified\n",
        "capture must freeze the CURRENT disk state, not the committed blob"
    );
    assert_eq!(read_bytes(&store, &resolved, "ignored.log"), b"ignored\n");
    assert_eq!(
        read_bytes(&store, &resolved, "untracked.txt"),
        b"untracked\n"
    );
}

#[cfg(unix)]
#[test]
fn worktree_capture_ignores_special_files() {
    let src = TempDir::new().expect("src tempdir");
    write(&src.path().join("regular.txt"), b"regular\n");
    let _socket = std::os::unix::net::UnixListener::bind(src.path().join("live.sock"))
        .expect("bind unix socket fixture");
    let cache = TempDir::new().expect("cache tempdir");

    let snapshot = capture_worktree(cache.path(), &sn("wt"), src.path())
        .expect("a special file (socket/FIFO/device) must be skipped, not read or fail capture");
    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);
    let inventory = store.inventory(&resolved).expect("captured inventory");
    let paths: Vec<&str> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(
        paths,
        ["regular.txt"],
        "only regular files enter the capture set; the socket must be skipped like a symlink"
    );
    assert!(
        store.read(&resolved, &sp("live.sock")).is_err(),
        "reading a skipped special file's path must err, not serve bytes"
    );
}

// ─── cache-inside-source containment (codex-R4-H2) ──────────────────────────

#[cfg(unix)]
#[test]
fn contained_cache_is_excluded_from_capture_empty_and_prepopulated() {
    for prepopulate in [false, true] {
        let src = TempDir::new().expect("src tempdir");
        build_capture_matrix(src.path());
        let cache_dir = src.path().join(".phora-cache/git");
        std::fs::create_dir_all(&cache_dir).expect("create in-source cache dir");
        if prepopulate {
            write(&cache_dir.join("deadbeefdeadbeef.git/config"), b"[core]\n");
            write(
                &cache_dir.join("deadbeefdeadbeef.git/objects/pack/p.pack"),
                b"PACK",
            );
        }

        let snapshot = capture_worktree(&cache_dir, &sn("wt"), src.path())
            .expect("capture with an in-source cache succeeds");
        let store = GitBackend::new(cache_dir.clone());
        let resolved = resolved_worktree(src.path(), snapshot);
        let inventory = store.inventory(&resolved).expect("captured inventory");
        assert_golden("worktree_capture_set.golden", &render_inventory(&inventory));
    }
}

#[cfg(unix)]
#[test]
fn capture_into_a_contained_cache_publishes_atomically() {
    let src = TempDir::new().expect("src tempdir");
    build_capture_matrix(src.path());
    let cache_dir = src.path().join(".phora-cache/git");

    let first =
        capture_worktree(&cache_dir, &sn("wt"), src.path()).expect("first capture succeeds");
    let after_first = cache_entries(&cache_dir);
    assert!(
        !after_first.is_empty(),
        "the first capture must publish into the contained cache"
    );

    let second =
        capture_worktree(&cache_dir, &sn("wt"), src.path()).expect("second capture succeeds");
    let after_second = cache_entries(&cache_dir);

    assert_eq!(
        worktree_digest(&first),
        worktree_digest(&second),
        "an unchanged worktree must yield one capture_digest even though the first capture \
         grew the contained cache"
    );

    let store = GitBackend::new(cache_dir.clone());
    let inv_first = store
        .inventory(&resolved_worktree(src.path(), first))
        .expect("first inventory");
    let inv_second = store
        .inventory(&resolved_worktree(src.path(), second))
        .expect("second inventory");
    assert_eq!(
        inv_first, inv_second,
        "cache growth from the first capture must be invisible to the second capture set"
    );

    assert_eq!(
        after_second, after_first,
        "an unchanged re-capture must gain no cache entries and leave no transient debris, \
         whatever the temp-file scheme (this pins leftover detection, not a cache file set)"
    );
    let staging: Vec<&String> = after_second
        .iter()
        .filter(|name| name.contains(".staging-"))
        .collect();
    assert!(
        staging.is_empty(),
        "no staging debris may survive a successful publish: {staging:?}"
    );
}

// ─── race-free inventory→read (INV-6) ───────────────────────────────────────

#[cfg(unix)]
#[test]
fn copy_reads_resolve_against_the_captured_tree_not_the_live_worktree() {
    let src = TempDir::new().expect("src tempdir");
    build_capture_matrix(src.path());
    let cache = TempDir::new().expect("cache tempdir");

    let snapshot =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("worktree capture succeeds");
    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);
    let before = store
        .inventory(&resolved)
        .expect("inventory before mutation");

    write(&src.path().join("README.md"), b"changed-after-capture\n");
    std::fs::remove_file(src.path().join("untracked.txt")).expect("delete after capture");
    std::fs::remove_file(src.path().join("plain-dir/data.txt")).expect("remove file");
    std::fs::create_dir(src.path().join("plain-dir/data.txt")).expect("replace file with dir");
    write(&src.path().join("plain-dir/data.txt/child.txt"), b"child\n");

    let after = store
        .inventory(&resolved)
        .expect("inventory after mutation");
    assert_eq!(
        after, before,
        "the frozen inventory must not track live mutation, deletion, or file→dir replacement"
    );
    assert_eq!(
        read_bytes(&store, &resolved, "README.md"),
        b"modified\n",
        "a copy-mode read must see the captured bytes, not the mutated file"
    );
    assert_eq!(
        read_bytes(&store, &resolved, "untracked.txt"),
        b"untracked\n",
        "a copy-mode read must survive live deletion"
    );
    assert_eq!(
        read_bytes(&store, &resolved, "plain-dir/data.txt"),
        b"plain\n",
        "a copy-mode read must survive live file→dir replacement"
    );
}

#[cfg(unix)]
#[test]
fn capture_digest_tracks_content_not_capture_time() {
    let src = TempDir::new().expect("src tempdir");
    build_capture_matrix(src.path());
    let cache = TempDir::new().expect("cache tempdir");

    let first =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("first capture succeeds");
    let second =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("second capture succeeds");
    assert_eq!(
        worktree_digest(&first),
        worktree_digest(&second),
        "an unchanged worktree must capture to one deterministic capture_digest"
    );

    write(&src.path().join("README.md"), b"drifted\n");
    let third =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("third capture succeeds");
    assert_ne!(
        worktree_digest(&first),
        worktree_digest(&third),
        "changed content must change the capture_digest"
    );
}

// ─── plain dirs, non-UTF8 names, submodules ─────────────────────────────────

#[test]
fn plain_non_git_dir_captures_with_the_link_head_sentinel() {
    let src = TempDir::new().expect("src tempdir");
    write(&src.path().join("a.txt"), b"a\n");
    write(&src.path().join("sub/b.txt"), b"b\n");
    let cache = TempDir::new().expect("cache tempdir");

    let snapshot =
        capture_worktree(cache.path(), &sn("plain"), src.path()).expect("plain-dir capture");
    let SnapshotId::Worktree { ref head, .. } = snapshot else {
        panic!("a plain directory must capture as SnapshotId::Worktree, got {snapshot:?}");
    };
    assert_eq!(
        head, "link",
        "a non-repo worktree must carry the `link` head sentinel (read_local_head parity)"
    );

    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);
    let inventory = store.inventory(&resolved).expect("captured inventory");
    let paths: Vec<&str> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["a.txt", "sub/b.txt"]);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn worktree_capture_rejects_non_utf8_names_with_a_source_error() {
    use std::os::unix::ffi::OsStrExt as _;

    let src = TempDir::new().expect("src tempdir");
    write(&src.path().join("ok.txt"), b"ok\n");
    let bad = std::ffi::OsStr::from_bytes(b"bad-\xff.txt");
    std::fs::write(src.path().join(bad), b"x\n").expect("write non-utf8-named file");
    let cache = TempDir::new().expect("cache tempdir");

    let err = capture_worktree(cache.path(), &sn("wt"), src.path())
        .expect_err("a non-UTF8 entry name must fail capture, not panic or vanish silently");
    assert!(
        format!("{err}").to_lowercase().contains("utf"),
        "the error must name the non-UTF8 cause (import_tree parity), got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn submodule_files_are_captured_without_git_internals() {
    let sub = TempDir::new().expect("submodule tempdir");
    git(sub.path(), &["init", "-b", "main", "."]);
    git(sub.path(), &["config", "user.email", "test@example.com"]);
    git(sub.path(), &["config", "user.name", "Test"]);
    write(&sub.path().join("lib.txt"), b"lib\n");
    git(sub.path(), &["add", "-A"]);
    git(sub.path(), &["commit", "-m", "sub"]);

    let src = TempDir::new().expect("src tempdir");
    git(src.path(), &["init", "-b", "main", "."]);
    git(src.path(), &["config", "user.email", "test@example.com"]);
    git(src.path(), &["config", "user.name", "Test"]);
    write(&src.path().join("README.md"), b"root\n");
    git(src.path(), &["add", "-A"]);
    git(src.path(), &["commit", "-m", "root"]);
    git(
        src.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &sub.path().to_string_lossy(),
            "vendored",
        ],
    );
    git(src.path(), &["commit", "-m", "add submodule"]);

    let cache = TempDir::new().expect("cache tempdir");
    let snapshot =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("submodule capture succeeds");
    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);
    let inventory = store.inventory(&resolved).expect("captured inventory");
    let paths: Vec<&str> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(
        paths.contains(&".gitmodules") && paths.contains(&"vendored/lib.txt"),
        "submodule worktree files must be captured like nested content, got {paths:?}"
    );
    assert!(
        paths
            .iter()
            .all(|path| path.split('/').all(|segment| segment != ".git")),
        "no `.git` file or directory (super or submodule) may enter the capture set: {paths:?}"
    );
}

// ─── URL→Git snapshot unification (INV-6) ───────────────────────────────────

fn serve_bytes_forever(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut scratch = [0u8; 2048];
            let _ = stream.read(&mut scratch);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/src.tar")
}

fn build_url_fixture_tar() -> Vec<u8> {
    let staging = TempDir::new().expect("staging tempdir");
    write(&staging.path().join("a/x.txt"), b"alpha\n");
    write(&staging.path().join("b/y.txt"), b"beta\n");
    let status = Command::new("tar")
        .current_dir(staging.path())
        .args(["-cf", "src.tar", "a", "b"])
        .env("COPYFILE_DISABLE", "1")
        .status()
        .expect("tar runs");
    assert!(status.success(), "tar must build the fixture archive");
    std::fs::read(staging.path().join("src.tar")).expect("read tar")
}

#[test]
fn url_source_resolves_to_the_pinned_synthetic_git_snapshot() {
    let url = serve_bytes_forever(build_url_fixture_tar());
    let git_dir = TempDir::new().expect("git dir tempdir");
    let http = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());
    http.fetch(&sn("web"), &url).expect("url fetch imports");
    let commit = http
        .resolve(&sn("web"), &url, &Refspec::Default)
        .expect("url resolve");
    assert_eq!(
        commit,
        read_golden("url_synthetic_commit.golden").trim(),
        "the synthetic commit id must stay byte-identical to the T002 baseline (INV-6)"
    );

    let store = GitBackend::new(git_dir.path().to_path_buf());
    let resolved = ResolvedSource {
        name: sn("web"),
        url,
        snapshot: SnapshotId::Git {
            commit: commit.clone(),
        },
    };
    let inventory = store.inventory(&resolved).expect("url-source inventory");
    let entries: Vec<(&str, SourceEntryKind)> = inventory
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind))
        .collect();
    assert_eq!(
        entries,
        [
            ("a/x.txt", SourceEntryKind::File),
            ("b/y.txt", SourceEntryKind::File),
        ],
        "a url source must inventory through the SAME Git snapshot representation as git sources"
    );
    assert_eq!(read_bytes(&store, &resolved, "a/x.txt"), b"alpha\n");
}

#[test]
fn git_source_snapshot_store_agrees_with_the_backend() {
    let src = TempDir::new().expect("src tempdir");
    git(src.path(), &["init", "-b", "main", "."]);
    git(src.path(), &["config", "user.email", "test@example.com"]);
    git(src.path(), &["config", "user.name", "Test"]);
    write(&src.path().join("editor/init.lua"), b"-- init\n");
    write(&src.path().join("lint/rules.toml"), b"[rules]\n");
    git(src.path(), &["add", "-A"]);
    git(src.path(), &["commit", "-m", "fixture"]);

    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let url = src.path().to_string_lossy().into_owned();
    backend.fetch(&sn("fixture"), &url).expect("fetch mirror");
    let commit = backend
        .resolve(&sn("fixture"), &url, &Refspec::Branch("main".to_owned()))
        .expect("resolve main");

    let resolved = ResolvedSource {
        name: sn("fixture"),
        url: url.clone(),
        snapshot: SnapshotId::Git {
            commit: commit.clone(),
        },
    };
    let inventory = backend.inventory(&resolved).expect("git-source inventory");
    let paths: Vec<String> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();
    assert_eq!(
        paths,
        backend
            .list_source_leaves(&sn("fixture"), &url, &commit, None)
            .expect("list source leaves"),
        "the store inventory must equal the backend's leaf listing for one commit"
    );
    let entry = backend
        .read(&resolved, &sp("editor/init.lua"))
        .expect("store read");
    assert_eq!(entry.meta.path.as_str(), "editor/init.lua");
    assert_eq!(
        entry.bytes,
        backend
            .read_file_at(&sn("fixture"), &url, &commit, Path::new("editor/init.lua"))
            .expect("backend read"),
        "store reads and backend reads must agree byte-for-byte"
    );
}

// ─── SourcePath root-join parity ────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn source_path_root_join_parity_over_the_captured_set() {
    let src = TempDir::new().expect("src tempdir");
    build_capture_matrix(src.path());
    let cache = TempDir::new().expect("cache tempdir");

    let snapshot =
        capture_worktree(cache.path(), &sn("wt"), src.path()).expect("worktree capture succeeds");
    let store = GitBackend::new(cache.path().to_path_buf());
    let resolved = resolved_worktree(src.path(), snapshot);
    let inventory = store.inventory(&resolved).expect("captured inventory");
    assert!(!inventory.entries.is_empty(), "the fixture captures leaves");

    for entry in &inventory.entries {
        let rel = entry.path.as_str();
        assert_ne!(
            entry.kind,
            SourceEntryKind::Symlink,
            "symlinks are skipped at capture (discovery parity), yet `{rel}` captured as one"
        );
        assert!(
            !rel.starts_with('/') && rel.split('/').all(|segment| segment != ".."),
            "every captured path must be a safe relative path: {rel}"
        );
        assert_eq!(
            read_bytes(&store, &resolved, rel),
            std::fs::read(src.path().join(rel)).expect("read fixture file"),
            "root-join parity: store bytes must equal the file at root/{rel}"
        );
    }
}

// ─── lock bytes + link-mode exception (codex-R4-H3) ─────────────────────────

struct CliFixture {
    _home: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn build_cli_fixture(config: &str) -> CliFixture {
    let home = TempDir::new().expect("home tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    let home_path = home.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");
    write(&cwd.path().join("phora.toml"), config.as_bytes());
    CliFixture {
        _home: home,
        cwd,
        home_path,
        xdg_cache,
        xdg_state,
    }
}

fn run_cli(fx: &CliFixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(fx.cwd.path())
        .env("HOME", &fx.home_path)
        .env("XDG_CACHE_HOME", &fx.xdg_cache)
        .env("XDG_STATE_HOME", &fx.xdg_state)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs")
}

fn seed_link_source(dir: &Path) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("editor/init.lua"), b"-- init\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "fixture"]);
}

fn link_config(src: &Path) -> String {
    format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\"]\ndeploy = \"link\"\n\n\
         [targets.home]\npath = \"~/deploy\"\nsources = [\"dotfiles\"]\nlayout = \"by-source\"\n",
        src = src.display(),
    )
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file(&path, name)
        {
            return Some(found);
        }
    }
    None
}

#[test]
fn worktree_lock_bytes_stay_free_of_capture_digest() {
    let src = TempDir::new().expect("source tempdir");
    seed_link_source(src.path());
    let fx = build_cli_fixture(&link_config(src.path()));

    let out = run_cli(&fx, &["sync"]);
    assert!(
        out.status.success(),
        "link-mode sync must succeed, got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock = std::fs::read_to_string(fx.cwd.path().join("phora.lock")).expect("read phora.lock");
    assert!(
        lock.contains(&rev_parse_head(src.path())),
        "the lock must keep recording the worktree's HEAD commit (byte-identical lock, INV-4)"
    );
    assert!(
        !lock.contains("capture_digest"),
        "capture_digest is internal to source and must never reach the lock bytes, got:\n{lock}"
    );

    let doc: toml::Value = toml::from_str(&lock).expect("phora.lock parses as toml");
    let sources = doc
        .get("sources")
        .and_then(toml::Value::as_array)
        .expect("the lock carries a [[sources]] array");
    let entry = sources
        .iter()
        .find(|source| source.get("name").and_then(toml::Value::as_str) == Some("dotfiles"))
        .expect("the lock records the worktree source `dotfiles`");
    let keys: BTreeSet<&str> = entry
        .as_table()
        .expect("a [[sources]] entry is a table")
        .keys()
        .map(String::as_str)
        .collect();
    let required = [
        "name",
        "git",
        "resolved",
        "commit",
        "digest",
        "config_digest",
    ];
    for key in required {
        assert!(
            keys.contains(key),
            "the LockedSource schema key `{key}` must stay present (INV-4); got {keys:?}"
        );
    }
    let allowed: BTreeSet<&str> = required
        .iter()
        .copied()
        .chain(["ref", "instance"])
        .collect();
    let extras: Vec<&&str> = keys.difference(&allowed).collect();
    assert!(
        extras.is_empty(),
        "the worktree source's lock entry must carry EXACTLY the LockedSource keys — any new \
         key, however named, leaks snapshot internals into the lock bytes: {extras:?}"
    );

    let scratch = TempDir::new().expect("scratch cache tempdir");
    let digest_value = worktree_digest(
        &capture_worktree(scratch.path(), &sn("dotfiles"), src.path())
            .expect("capture the same worktree to obtain the digest value"),
    );
    assert!(
        !lock.contains(&digest_value),
        "the capture_digest VALUE must appear nowhere in the lock bytes, got:\n{lock}"
    );
    if let Some(hex) = digest_value.split(':').next_back() {
        assert!(
            !lock.contains(hex),
            "the capture_digest hex must appear nowhere in the lock bytes under any key"
        );
    }
}

#[cfg(unix)]
#[test]
fn link_mode_artifacts_track_the_live_worktree_after_capture() {
    let src = TempDir::new().expect("source tempdir");
    seed_link_source(src.path());
    let fx = build_cli_fixture(&link_config(src.path()));

    let out = run_cli(&fx, &["sync"]);
    assert!(
        out.status.success(),
        "link-mode sync must succeed, got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let deploy_root = fx.home_path.join("deploy");
    let deployed = find_file(&deploy_root, "init.lua")
        .expect("link-mode sync deploys editor/init.lua under the target");
    assert_eq!(
        std::fs::read(&deployed).expect("read deployed artifact"),
        b"-- init\n"
    );

    write(&src.path().join("editor/init.lua"), b"-- live\n");
    assert_eq!(
        std::fs::read(&deployed).expect("read deployed artifact after mutation"),
        b"-- live\n",
        "link artifacts are the documented exception to snapshot immutability: they must \
         track live mutation without a re-sync"
    );

    std::fs::remove_file(src.path().join("editor/init.lua")).expect("delete live file");
    assert!(
        std::fs::read(&deployed).is_err(),
        "after live deletion the link chain must dangle rather than serve frozen bytes"
    );

    std::fs::create_dir(src.path().join("editor/init.lua")).expect("replace file with dir");
    write(&src.path().join("editor/init.lua/child.txt"), b"child\n");
    assert!(
        deployed.is_dir(),
        "after live file→dir replacement the link chain must resolve to the directory"
    );
}

// ─── SourceStore trait shape ────────────────────────────────────────────────

#[test]
fn source_store_probe_object_is_usable_with_required_methods_only() {
    struct Probe;

    impl SourceStore for Probe {
        fn inventory(&self, _source: &ResolvedSource) -> Result<SourceInventory, SourceError> {
            Ok(SourceInventory::default())
        }

        fn read(
            &self,
            _source: &ResolvedSource,
            _path: &SourcePath,
        ) -> Result<SourceEntry, SourceError> {
            Err(SourceError::Source("probe".to_owned()))
        }
    }

    let probe = Probe;
    let store: &dyn SourceStore = &probe;
    let resolved = ResolvedSource {
        name: sn("probe"),
        url: String::new(),
        snapshot: SnapshotId::Git {
            commit: "0".repeat(40),
        },
    };
    assert!(
        store
            .inventory(&resolved)
            .expect("probe inventory")
            .entries
            .is_empty(),
        "SourceStore must stay implementable with only inventory/read and stay object-safe"
    );
    assert!(store.read(&resolved, &sp("a")).is_err());
}
