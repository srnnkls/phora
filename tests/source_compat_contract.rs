use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::str::FromStr as _;

use phora::config::Refspec;
use phora::kernel::SourceName;
use phora::source::{
    GitBackend, HttpBackend, ResolvedSource, SnapshotId, SourceBackend as _, SourceEntryKind,
    SourcePath, SourceStore,
};
use tempfile::TempDir;

mod common;

/// One tree, materialized twice (git fixture and url tarball): path, bytes, exec.
const TREE: &[(&str, &[u8], bool)] = &[
    ("README.md", b"hello\n", false),
    ("bin/run.sh", b"#!/bin/sh\necho run\n", true),
    ("subdir/nested.txt", b"nested\n", false),
];

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
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct GitFixture {
    _src: TempDir,
    _git_dir: TempDir,
    backend: GitBackend,
    url: String,
    commit: String,
}

fn build_git_fixture() -> GitFixture {
    let src = TempDir::new().expect("src tempdir");
    let p = src.path();
    git(p, &["init", "-b", "main", "."]);
    git(p, &["config", "user.email", "test@example.com"]);
    git(p, &["config", "user.name", "Test"]);
    git(p, &["config", "core.autocrlf", "false"]);
    for (path, bytes, _) in TREE {
        write(&p.join(path), bytes);
    }
    git(p, &["add", "-A"]);
    for (path, _, exec) in TREE {
        if *exec {
            git(p, &["update-index", "--chmod=+x", path]);
        }
    }
    git(p, &["commit", "-m", "fixture"]);

    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let url = p.to_string_lossy().into_owned();
    backend.fetch(&sn("g"), &url).expect("fetch git fixture");
    let commit = backend
        .resolve(&sn("g"), &url, &Refspec::Branch("main".into()))
        .expect("resolve main");

    GitFixture {
        _src: src,
        _git_dir: git_dir,
        backend,
        url,
        commit,
    }
}

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

fn build_tree_tar() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes, exec) in TREE {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(if *exec { 0o755 } else { 0o644 });
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, *path, *bytes)
            .expect("append tar entry");
    }
    builder.into_inner().expect("finish tar")
}

struct UrlFixture {
    _git_dir: TempDir,
    backend: HttpBackend,
    /// The synthetic mirror the url import materializes is read back through
    /// a plain `GitBackend` over the same `git_dir` — url sources have no
    /// store of their own (INV-6: one snapshot representation).
    store: GitBackend,
    url: String,
    commit: String,
}

fn build_url_fixture() -> UrlFixture {
    let url = serve_bytes_forever(build_tree_tar());
    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());
    backend.fetch(&sn("u"), &url).expect("import url fixture");
    let commit = backend
        .resolve(&sn("u"), &url, &Refspec::None)
        .expect("resolve synthetic commit");
    let store = GitBackend::new(git_dir.path().to_path_buf());

    UrlFixture {
        _git_dir: git_dir,
        backend,
        store,
        url,
        commit,
    }
}

fn resolved(name: &str, url: &str, commit: &str) -> ResolvedSource {
    ResolvedSource {
        name: sn(name),
        url: url.to_owned(),
        snapshot: SnapshotId::Git {
            commit: commit.to_owned(),
        },
    }
}

fn inventory_pairs(
    store: &dyn SourceStore,
    source: &ResolvedSource,
) -> Vec<(String, SourceEntryKind)> {
    store
        .inventory(source)
        .expect("snapshot inventory")
        .entries
        .iter()
        .map(|entry| (entry.path.as_str().to_owned(), entry.kind))
        .collect()
}

fn expected_pairs() -> Vec<(String, SourceEntryKind)> {
    TREE.iter()
        .map(|(path, _, exec)| {
            let kind = if *exec {
                SourceEntryKind::Executable
            } else {
                SourceEntryKind::File
            };
            ((*path).to_owned(), kind)
        })
        .collect()
}

fn all_leaves() -> Vec<SourcePath> {
    TREE.iter().map(|(path, _, _)| sp(path)).collect()
}

// ─── delegation equivalence: the SourceStore path serves the same tree ──────

#[test]
fn git_snapshot_store_serves_the_fixture_tree() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);

    assert_eq!(
        inventory_pairs(&fx.backend, &source),
        expected_pairs(),
        "the snapshot inventory of a git source must list every fixture leaf, sorted, with \
         the exec bit surviving as SourceEntryKind::Executable"
    );
    for (path, bytes, _) in TREE {
        let entry = fx
            .backend
            .read(&source, &sp(path))
            .unwrap_or_else(|e| panic!("store read of `{path}`: {e}"));
        assert_eq!(
            entry.bytes, *bytes,
            "store read of `{path}` must return the committed bytes"
        );
        assert_eq!(entry.meta.path, sp(path), "the entry meta echoes its path");
    }
}

#[test]
fn url_snapshot_store_serves_the_fixture_tree() {
    let fx = build_url_fixture();
    let source = resolved("u", &fx.url, &fx.commit);

    assert_eq!(
        inventory_pairs(&fx.store, &source),
        expected_pairs(),
        "a url source's synthetic snapshot must inventory the identical tree a git source \
         would (INV-6), including the exec bit from the tar mode"
    );
    for (path, bytes, _) in TREE {
        let entry = fx
            .store
            .read(&source, &sp(path))
            .unwrap_or_else(|e| panic!("store read of `{path}` from the synthetic mirror: {e}"));
        assert_eq!(
            entry.bytes, *bytes,
            "store read of `{path}` must return the imported bytes"
        );
    }
}

#[test]
fn git_legacy_read_surfaces_match_the_snapshot_store() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);

    let legacy_leaves = fx
        .backend
        .list_source_leaves(&sn("g"), &fx.url, &fx.commit, None)
        .expect("legacy leaf walk");
    let store_paths: Vec<String> = fx
        .backend
        .inventory(&source)
        .expect("snapshot inventory")
        .entries
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();
    assert_eq!(
        legacy_leaves, store_paths,
        "legacy list_source_leaves and the SourceStore inventory must agree leaf-for-leaf: \
         the compat SourceBackend delegates, it does not fork the walk"
    );

    for path in &legacy_leaves {
        let legacy = fx
            .backend
            .read_file_at(&sn("g"), &fx.url, &fx.commit, Path::new(path))
            .unwrap_or_else(|e| panic!("legacy read of `{path}`: {e}"));
        let entry = fx
            .backend
            .read(&source, &sp(path))
            .unwrap_or_else(|e| panic!("store read of `{path}`: {e}"));
        assert_eq!(
            legacy, entry.bytes,
            "legacy read_file_at and SourceStore::read must return identical bytes for `{path}`"
        );
    }

    let legacy_digest = fx
        .backend
        .compute_digest(&sn("g"), &fx.url, &fx.commit, None, &[], &[])
        .expect("legacy full-tree digest");
    let snapshot_digest = fx
        .backend
        .digest_snapshot(&source, &all_leaves())
        .expect("digest_snapshot over the full leaf set");
    assert_eq!(
        legacy_digest, snapshot_digest,
        "digest_snapshot over the complete leaf set must equal the legacy unselected \
         compute_digest — lock digests may not drift when callers migrate (INV-4/INV-6)"
    );
}

#[test]
fn url_legacy_read_surfaces_match_the_snapshot_store() {
    let fx = build_url_fixture();
    let source = resolved("u", &fx.url, &fx.commit);

    let legacy_leaves = fx
        .backend
        .list_source_leaves(&sn("u"), &fx.url, &fx.commit, None)
        .expect("legacy leaf walk over the synthetic mirror");
    let store_paths: Vec<String> = fx
        .store
        .inventory(&source)
        .expect("snapshot inventory")
        .entries
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();
    assert_eq!(
        legacy_leaves, store_paths,
        "for a url source the legacy leaf walk and the SourceStore inventory must agree"
    );

    for path in &legacy_leaves {
        let legacy = fx
            .store
            .read_file_at(&sn("u"), &fx.url, &fx.commit, Path::new(path))
            .unwrap_or_else(|e| panic!("legacy mirror read of `{path}`: {e}"));
        let entry = fx
            .store
            .read(&source, &sp(path))
            .unwrap_or_else(|e| panic!("store read of `{path}`: {e}"));
        assert_eq!(
            legacy, entry.bytes,
            "the legacy mirror read and SourceStore::read must agree on `{path}`"
        );
    }

    let legacy_digest = fx
        .backend
        .compute_digest(&sn("u"), &fx.url, &fx.commit, None, &[], &[])
        .expect("legacy url digest");
    let snapshot_digest = fx
        .store
        .digest_snapshot(&source, &all_leaves())
        .expect("digest_snapshot over the synthetic snapshot");
    assert_eq!(
        legacy_digest, snapshot_digest,
        "url-source digests must be identical through the legacy and snapshot paths"
    );
}

// ─── INV-6: identical trees are indistinguishable across git and url ────────

#[test]
fn identical_git_and_url_trees_yield_equivalent_inventories_reads_and_digests() {
    let g = build_git_fixture();
    let u = build_url_fixture();
    let gs = resolved("g", &g.url, &g.commit);
    let us = resolved("u", &u.url, &u.commit);

    assert_eq!(
        inventory_pairs(&g.backend, &gs),
        inventory_pairs(&u.store, &us),
        "identical trees must produce identical inventories whether the source is git or url \
         (INV-6)"
    );
    for (path, _, _) in TREE {
        let from_git = g.backend.read(&gs, &sp(path)).expect("git store read");
        let from_url = u.store.read(&us, &sp(path)).expect("url store read");
        assert_eq!(
            from_git.bytes, from_url.bytes,
            "identical trees must serve identical bytes for `{path}`"
        );
        assert_eq!(
            from_git.meta.kind, from_url.meta.kind,
            "identical trees must agree on the entry kind of `{path}`"
        );
    }

    let git_digest = g
        .backend
        .compute_digest(&sn("g"), &g.url, &g.commit, None, &[], &[])
        .expect("git digest");
    let url_digest = u
        .backend
        .compute_digest(&sn("u"), &u.url, &u.commit, None, &[], &[])
        .expect("url digest");
    assert_eq!(
        git_digest, url_digest,
        "the digest is a pure function of the selected tree bytes: identical trees must \
         digest identically across git and url sources (INV-6)"
    );
    let git_snapshot = g
        .backend
        .digest_snapshot(&gs, &all_leaves())
        .expect("git digest_snapshot");
    let url_snapshot = u
        .store
        .digest_snapshot(&us, &all_leaves())
        .expect("url digest_snapshot");
    assert_eq!(
        git_snapshot, git_digest,
        "git digest_snapshot equals the legacy digest"
    );
    assert_eq!(
        url_snapshot, git_digest,
        "url digest_snapshot equals the legacy digest"
    );
}

// ─── digest_snapshot: explicit leaves, never a selection ────────────────────

#[test]
fn digest_snapshot_matches_legacy_digest_for_an_explicit_subset() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);

    let legacy_subset = fx
        .backend
        .compute_digest(
            &sn("g"),
            &fx.url,
            &fx.commit,
            None,
            &["README.md".to_owned()],
            &[],
        )
        .expect("legacy digest with an include selecting one leaf");
    let snapshot_subset = fx
        .backend
        .digest_snapshot(&source, &[sp("README.md")])
        .expect("digest_snapshot over one explicit leaf");
    assert_eq!(
        legacy_subset, snapshot_subset,
        "for the same effective leaf set, the explicit-leaves digest must equal the \
         selection-derived legacy digest: the leaf PLAN moves to the caller, the digest \
         bytes stay put (INV-2)"
    );

    let full = fx
        .backend
        .digest_snapshot(&source, &all_leaves())
        .expect("full-set digest_snapshot");
    assert_ne!(
        snapshot_subset, full,
        "a one-leaf digest must differ from the full-tree digest, proving the leaf set \
         is honored rather than ignored"
    );
}

#[test]
fn digest_snapshot_is_order_insensitive_through_dyn_source_store() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);
    let store: &dyn SourceStore = &fx.backend;

    let sorted = all_leaves();
    let reversed: Vec<SourcePath> = sorted.iter().rev().cloned().collect();
    assert_eq!(
        store
            .digest_snapshot(&source, &sorted)
            .expect("sorted-order digest"),
        store
            .digest_snapshot(&source, &reversed)
            .expect("reversed-order digest"),
        "caller-side leaf order must not leak into the digest (parity with the legacy \
         sorted walk), and digest_snapshot must stay callable through &dyn SourceStore"
    );
}

#[test]
fn digest_snapshot_of_an_empty_leaf_set_is_the_defined_empty_digest() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);

    let legacy_empty = fx
        .backend
        .compute_digest(
            &sn("g"),
            &fx.url,
            &fx.commit,
            None,
            &["matches-nothing-zzz".to_owned()],
            &[],
        )
        .expect("legacy digest with a selection matching no leaf is defined, not an error");
    assert_eq!(
        legacy_empty, "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        "guard: the legacy empty selection hashes zero frames — the blake3 of the empty stream"
    );

    let empty = fx
        .backend
        .digest_snapshot(&source, &[])
        .expect("an empty leaf set digests zero frames, mirroring the legacy empty selection");
    assert_eq!(
        empty, legacy_empty,
        "digest_snapshot over zero leaves must equal the legacy zero-selection digest — an \
         empty plan is a defined state, not an error"
    );
}

#[test]
fn digest_snapshot_rejects_a_directory_leaf_instead_of_expanding_it() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);

    let err = fx
        .backend
        .digest_snapshot(&source, &[sp("subdir")])
        .expect_err(
            "a leaf naming a tree must err: explicit leaves address blobs, they never \
             glob-expand a directory the way the legacy include selection does (INV-2)",
        );
    assert!(
        err.to_string().contains("subdir"),
        "the error must name the offending leaf, got: {err}"
    );
}

#[test]
fn digest_snapshot_names_an_absent_leaf_in_its_error() {
    let fx = build_git_fixture();
    let source = resolved("g", &fx.url, &fx.commit);

    let err = fx
        .backend
        .digest_snapshot(&source, &[sp("missing.txt")])
        .expect_err("a leaf absent from the snapshot must err, not digest a partial set");
    assert!(
        err.to_string().contains("missing.txt"),
        "the error must name the absent leaf so a mis-planned leaf set is diagnosable, \
         got: {err}"
    );
}
