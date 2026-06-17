//! ARCH-012 contract: each bounded context owns a thiserror enum, ports return
//! typed errors, and the CLI-edge `Error` aggregates them via `From`. These are
//! RED by compilation until `SourceError`/`StoreError` exist and the conversions
//! are wired. The renders pinned here must equal today's text in `error_text_pin`.

use std::error::Error as StdError;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use phora::config::Refspec;
use phora::error::Error;
use phora::kernel::SourceName;
use phora::source::{GitBackend, SourceBackend, SourceError};
use phora::store::{FileRegistry, StoreError};
use tempfile::TempDir;

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
}

fn assert_std_error<E: StdError>(_e: &E) {}

#[test]
fn source_error_is_a_std_error_and_displays() {
    let fixture = GitArtifactFixture::build();

    let err: SourceError = fixture
        .backend
        .resolve(&sn("dots"), &fixture.url, &Refspec::None)
        .expect_err("empty refspec must yield a typed SourceError");

    assert_std_error(&err);
    assert!(
        !err.to_string().is_empty(),
        "a SourceError must render a non-empty Display"
    );
}

#[test]
fn store_error_is_a_std_error_and_displays() {
    let dir = TempDir::new().expect("temp state root");
    let first = FileRegistry::open(dir.path().to_path_buf()).expect("open first");
    let second = FileRegistry::open(dir.path().to_path_buf()).expect("open second");

    let _held = first.lock_exclusive().expect("first acquires the lock");
    let err: StoreError = second
        .lock_exclusive()
        .expect_err("contended lock must yield a typed StoreError");

    assert_std_error(&err);
    assert!(
        !err.to_string().is_empty(),
        "a StoreError must render a non-empty Display"
    );
}

#[test]
fn cli_edge_error_converts_from_source_error_preserving_text() {
    let fixture = GitArtifactFixture::build();

    let src_err: SourceError = fixture
        .backend
        .resolve(&sn("dots"), &fixture.url, &Refspec::None)
        .expect_err("empty refspec errors");

    let edge: Error = Error::from(src_err);

    assert_eq!(
        edge.to_string(),
        "source error: source dots: git backend cannot resolve a url source's empty refspec",
        "From<SourceError> at the CLI edge must render identically to today's chained message"
    );
}

#[test]
fn cli_edge_error_converts_from_store_error_preserving_text() {
    let dir = TempDir::new().expect("temp state root");
    let first = FileRegistry::open(dir.path().to_path_buf()).expect("open first");
    let second = FileRegistry::open(dir.path().to_path_buf()).expect("open second");

    let _held = first.lock_exclusive().expect("first acquires the lock");
    let store_err: StoreError = second.lock_exclusive().expect_err("contended lock errors");

    let edge: Error = Error::from(store_err);

    assert_eq!(
        edge.to_string(),
        "lock error: another phora process is running for this project (state.lock held)",
        "From<StoreError> at the CLI edge must render identically to today's lock-contention message"
    );
}

#[test]
fn source_backend_methods_return_source_error() {
    let fixture = GitArtifactFixture::build();

    let typed: Result<u64, SourceError> = fixture.backend.commit_time(
        &sn("dots"),
        &fixture.url,
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    );

    assert!(
        typed.is_err(),
        "an absent commit must error, and the error type must unify with SourceError"
    );
}

struct GitArtifactFixture {
    _src: TempDir,
    _git_dir: TempDir,
    backend: GitBackend,
    url: String,
}

impl GitArtifactFixture {
    fn build() -> Self {
        let src = TempDir::new().expect("src tempdir");
        let root = src.path();

        git(root, &["init", "-b", "main", "."]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("README.md"), b"hi\n").expect("write fixture file");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "fixture"]);

        let git_dir = TempDir::new().expect("git dir tempdir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = root.to_string_lossy().into_owned();
        backend
            .fetch(&sn("dots"), &url)
            .expect("fetch builds mirror");

        Self {
            _src: src,
            _git_dir: git_dir,
            backend,
            url,
        }
    }
}

mod common;

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
