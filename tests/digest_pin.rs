//! ARCH-004 zero-churn pin: source digests stay byte-identical across the cleanup refactor.

use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use phora::source::SourceName;
use phora::source::{
    GitBackend, ResolvePolicy, ResolveRequest, ResolvedRevision, ResolvedSource, RevisionSpec,
    SnapshotId, SourceDirectoryEntry, SourceEntry, SourceInventory, SourceLocation, SourcePath,
    SourceStore, digest_snapshot,
};
use tempfile::TempDir;

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
}

mod common;

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let status = Command::new("git")
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
        status.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

struct DigestFixture {
    _src: TempDir,
    _git_dir: TempDir,
    backend: GitBackend,
    resolved: ResolvedSource,
}

fn build_fixture() -> DigestFixture {
    let src = TempDir::new().expect("src tempdir");
    let root = src.path();

    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);

    write(&root.join("editor/init.lua"), b"-- init\n");
    write(&root.join("editor/lua/opts.lua"), b"return {}\n");
    write(&root.join("README.md"), b"loose root file\n");
    write(&root.join(".config/settings.json"), b"{\"k\":1}\n");
    write(&root.join(".config/nested/app.toml"), b"a = 1\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let url = root.to_string_lossy().into_owned();
    let resolved = SourceStore::resolve(
        &backend,
        &ResolveRequest {
            name: sn("fixture"),
            location: SourceLocation::Git { url },
            revision: RevisionSpec::Branch("main".to_owned()),
        },
        ResolvePolicy::Refresh,
    )
    .expect("refresh builds and resolves the mirror");

    DigestFixture {
        _src: src,
        _git_dir: git_dir,
        backend,
        resolved,
    }
}

fn paths(values: &[&str]) -> Vec<SourcePath> {
    values
        .iter()
        .map(|value| SourcePath::new(value).expect("fixture path is safe"))
        .collect()
}

struct RootedStore<'a> {
    inner: &'a GitBackend,
    root: &'a str,
}

impl RootedStore<'_> {
    fn rooted_path(&self, path: &SourcePath) -> SourcePath {
        SourcePath::new(&format!("{}/{}", self.root, path.as_str()))
            .expect("fixture root and leaf form a safe path")
    }
}

impl SourceStore for RootedStore<'_> {
    fn resolve(
        &self,
        request: &ResolveRequest,
        policy: ResolvePolicy,
    ) -> Result<ResolvedSource, phora::source::SourceError> {
        SourceStore::resolve(self.inner, request, policy)
    }

    fn inventory(
        &self,
        snapshot: &SnapshotId,
        root: Option<&SourcePath>,
    ) -> Result<SourceInventory, phora::source::SourceError> {
        SourceStore::inventory(self.inner, snapshot, root)
    }

    fn read(
        &self,
        snapshot: &SnapshotId,
        path: &SourcePath,
    ) -> Result<SourceEntry, phora::source::SourceError> {
        let mut entry = SourceStore::read(self.inner, snapshot, &self.rooted_path(path))?;
        entry.meta.path = path.clone();
        Ok(entry)
    }

    fn list_directory(
        &self,
        snapshot: &SnapshotId,
        path: Option<&SourcePath>,
    ) -> Result<Vec<SourceDirectoryEntry>, phora::source::SourceError> {
        SourceStore::list_directory(self.inner, snapshot, path)
    }
}

fn digest(fixture: &DigestFixture, root: Option<&str>, leaves: &[&str]) -> String {
    let leaves = paths(leaves);
    match root {
        Some(root) => digest_snapshot(
            &RootedStore {
                inner: &fixture.backend,
                root,
            },
            &fixture.resolved.snapshot,
            &leaves,
        ),
        None => digest_snapshot(&fixture.backend, &fixture.resolved.snapshot, &leaves),
    }
    .expect("digest_snapshot succeeds")
}

#[test]
fn resolve_matches_committed_head() {
    let fixture = build_fixture();
    assert_eq!(
        fixture.resolved.revision,
        ResolvedRevision::Commit(fixture.resolved.snapshot.commit().clone()),
        "main must resolve to the committed HEAD"
    );
}

#[test]
fn digest_pins_full_tree_with_root_loose_and_dotfile_dir() {
    let fixture = build_fixture();
    let value = digest(
        &fixture,
        None,
        &[
            ".config/nested/app.toml",
            ".config/settings.json",
            "README.md",
            "editor/init.lua",
            "editor/lua/opts.lua",
        ],
    );
    assert_eq!(
        value,
        "blake3:920b4081e48d0500d65bb36753595c9987d6c0e7f4d3acb7b33d4e8b083ed22c"
    );
}

#[test]
fn digest_pins_tree_without_root_loose_file() {
    let fixture = build_fixture();
    let value = digest(
        &fixture,
        None,
        &[
            ".config/nested/app.toml",
            ".config/settings.json",
            "editor/init.lua",
            "editor/lua/opts.lua",
        ],
    );
    assert_eq!(
        value,
        "blake3:f5f20404d2ea56d711ec3a47f1167803300055b14e1fbb7fd84a09d360eb300f"
    );
}

#[test]
fn digest_pins_tree_without_top_level_dotfile_dir() {
    let fixture = build_fixture();
    let full = digest(
        &fixture,
        None,
        &[
            ".config/nested/app.toml",
            ".config/settings.json",
            "README.md",
            "editor/init.lua",
            "editor/lua/opts.lua",
        ],
    );
    let value = digest(
        &fixture,
        None,
        &["README.md", "editor/init.lua", "editor/lua/opts.lua"],
    );
    assert_ne!(
        value, full,
        "excluding the .config subtree must drop its leaves, changing the digest"
    );
    assert_eq!(
        value,
        "blake3:619b3b51342eb66551a9a8b5057d26b5762dadfd3a190ed9cedfb3ac1de03033"
    );
}

#[test]
fn digest_pins_editor_subtree_only() {
    let fixture = build_fixture();
    let value = digest(&fixture, None, &["editor/init.lua", "editor/lua/opts.lua"]);
    assert_eq!(
        value,
        "blake3:a4bf87d47664c474e7f850b59219ea1c918a63ca3547150365e17d54c5d1a142"
    );
}

#[test]
fn digest_pins_dotfile_subtree_via_root() {
    let fixture = build_fixture();
    let value = digest(
        &fixture,
        Some(".config"),
        &["nested/app.toml", "settings.json"],
    );
    assert_eq!(
        value,
        "blake3:27db1c96b7134c9bd8aeb6f1d8782a45c115faac31067eed4ac21bffd6ede7e5"
    );
}
