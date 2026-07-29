use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::str::FromStr as _;

use phora::source::SourceName;
use phora::source::{
    GitBackend, HttpBackend, ResolvePolicy, ResolveRequest, ResolvedSource, RevisionSpec,
    SourceEntryKind, SourceLocation, SourcePath, SourceStore,
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
    let resolved = SourceStore::resolve(
        &backend,
        &ResolveRequest {
            name: sn("g"),
            location: SourceLocation::Git { url: url.clone() },
            revision: RevisionSpec::Branch("main".into()),
        },
        ResolvePolicy::Refresh,
    )
    .expect("refresh and resolve Git fixture");
    let commit = resolved.snapshot.commit().to_string();

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
    url: String,
}

fn build_url_fixture() -> UrlFixture {
    let url = serve_bytes_forever(build_tree_tar());
    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());
    SourceStore::resolve(
        &backend,
        &ResolveRequest {
            name: sn("u"),
            location: SourceLocation::Url { url: url.clone() },
            revision: RevisionSpec::None,
        },
        ResolvePolicy::Refresh,
    )
    .expect("import and resolve URL fixture");
    UrlFixture {
        _git_dir: git_dir,
        backend,
        url,
    }
}

fn resolved_git(fixture: &GitFixture) -> ResolvedSource {
    SourceStore::resolve(
        &fixture.backend,
        &ResolveRequest {
            name: sn("g"),
            location: SourceLocation::Git {
                url: fixture.url.clone(),
            },
            revision: RevisionSpec::Commit(
                fixture.commit.parse().expect("fixture commit is valid hex"),
            ),
        },
        ResolvePolicy::CachedOnly,
    )
    .expect("typed Git resolution finds the cached fixture commit")
}

fn resolved_url(fixture: &UrlFixture) -> ResolvedSource {
    SourceStore::resolve(
        &fixture.backend,
        &ResolveRequest {
            name: sn("u"),
            location: SourceLocation::Url {
                url: fixture.url.clone(),
            },
            revision: RevisionSpec::None,
        },
        ResolvePolicy::CachedOnly,
    )
    .expect("typed URL resolution finds the cached synthetic commit")
}

fn inventory_pairs(
    store: &dyn SourceStore,
    source: &ResolvedSource,
) -> Vec<(String, SourceEntryKind)> {
    SourceStore::inventory(store, &source.snapshot, None)
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
    let source = resolved_git(&fx);

    assert_eq!(
        inventory_pairs(&fx.backend, &source),
        expected_pairs(),
        "the snapshot inventory of a git source must list every fixture leaf, sorted, with \
         the exec bit surviving as SourceEntryKind::Executable"
    );
    for (path, bytes, _) in TREE {
        let entry = SourceStore::read(&fx.backend, &source.snapshot, &sp(path))
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
    let source = resolved_url(&fx);

    assert_eq!(
        inventory_pairs(&fx.backend, &source),
        expected_pairs(),
        "a url source's synthetic snapshot must inventory the identical tree a git source \
         would (INV-6), including the exec bit from the tar mode"
    );
    for (path, bytes, _) in TREE {
        let entry = SourceStore::read(&fx.backend, &source.snapshot, &sp(path))
            .unwrap_or_else(|e| panic!("store read of `{path}` from the synthetic mirror: {e}"));
        assert_eq!(
            entry.bytes, *bytes,
            "store read of `{path}` must return the imported bytes"
        );
    }
}

// ─── INV-6: identical trees are indistinguishable across git and url ────────

#[test]
fn identical_git_and_url_trees_yield_equivalent_inventories_reads_and_digests() {
    let g = build_git_fixture();
    let u = build_url_fixture();
    let gs = resolved_git(&g);
    let us = resolved_url(&u);

    assert_eq!(
        inventory_pairs(&g.backend, &gs),
        inventory_pairs(&u.backend, &us),
        "identical trees must produce identical inventories whether the source is git or url \
         (INV-6)"
    );
    for (path, _, _) in TREE {
        let from_git =
            SourceStore::read(&g.backend, &gs.snapshot, &sp(path)).expect("git store read");
        let from_url =
            SourceStore::read(&u.backend, &us.snapshot, &sp(path)).expect("url store read");
        assert_eq!(
            from_git.bytes, from_url.bytes,
            "identical trees must serve identical bytes for `{path}`"
        );
        assert_eq!(
            from_git.meta.kind, from_url.meta.kind,
            "identical trees must agree on the entry kind of `{path}`"
        );
    }

    let git_digest = phora::source::digest_snapshot(&g.backend, &gs.snapshot, &all_leaves())
        .expect("Git final-capability digest");
    let url_digest = phora::source::digest_snapshot(&u.backend, &us.snapshot, &all_leaves())
        .expect("URL final-capability digest");
    assert_eq!(
        git_digest, url_digest,
        "the digest is a pure function of the selected tree bytes: identical trees must \
         digest identically across git and url sources (INV-6)"
    );
}

// ─── digest_snapshot: explicit leaves, never a selection ────────────────────

#[test]
fn digest_snapshot_honors_an_explicit_subset() {
    let fx = build_git_fixture();
    let source = resolved_git(&fx);

    let snapshot_subset =
        phora::source::digest_snapshot(&fx.backend, &source.snapshot, &[sp("README.md")])
            .expect("digest_snapshot over one explicit leaf");

    let full = phora::source::digest_snapshot(&fx.backend, &source.snapshot, &all_leaves())
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
    let source = resolved_git(&fx);
    let store: &dyn SourceStore = &fx.backend;

    let sorted = all_leaves();
    let reversed: Vec<SourcePath> = sorted.iter().rev().cloned().collect();
    assert_eq!(
        phora::source::digest_snapshot(store, &source.snapshot, &sorted)
            .expect("sorted-order digest"),
        phora::source::digest_snapshot(store, &source.snapshot, &reversed)
            .expect("reversed-order digest"),
        "caller-side leaf order must not leak into the digest (parity with the legacy \
         sorted walk), and the free digest_snapshot operation must accept &dyn SourceStore"
    );
}

#[test]
fn digest_snapshot_of_an_empty_leaf_set_is_the_defined_empty_digest() {
    let fx = build_git_fixture();
    let source = resolved_git(&fx);

    let empty = phora::source::digest_snapshot(&fx.backend, &source.snapshot, &[])
        .expect("an empty leaf set digests zero frames");
    assert_eq!(
        empty, "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        "digest_snapshot over zero leaves is the pinned BLAKE3 empty-stream digest"
    );
}

#[test]
fn digest_snapshot_rejects_a_directory_leaf_instead_of_expanding_it() {
    let fx = build_git_fixture();
    let source = resolved_git(&fx);

    let err = phora::source::digest_snapshot(&fx.backend, &source.snapshot, &[sp("subdir")])
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
    let source = resolved_git(&fx);

    let err = phora::source::digest_snapshot(&fx.backend, &source.snapshot, &[sp("missing.txt")])
        .expect_err("a leaf absent from the snapshot must err, not digest a partial set");
    assert!(
        err.to_string().contains("missing.txt"),
        "the error must name the absent leaf so a mis-planned leaf set is diagnosable, \
         got: {err}"
    );
}
