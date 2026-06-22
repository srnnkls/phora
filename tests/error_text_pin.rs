//! ARCH-012 zero-churn pin: user-visible error MESSAGES rendered at the CLI edge
//! must stay byte-identical across the per-context error-enum refactor.
//!
//! These exercise public entry points (`Config::parse`, `verify_digest`,
//! `GitBackend`/`FileRegistry` ops) and assert the exact `format!("{err}")`
//! text — the only observable behavior, since no production site matches on
//! `Error` variants. They pass today; the refactor must keep them green.

use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use phora::config::{Config, Refspec};
use phora::http::verify_digest;
use phora::kernel::{Digest, SourceName};
use phora::source::{GitBackend, SourceBackend};
use phora::store::{ArtifactKey, FileRegistry, Registry};
use tempfile::TempDir;

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
}

// ── config context ──────────────────────────────────────────────

#[test]
fn config_parse_invalid_toml_renders_config_prefixed_message() {
    let err = Config::parse("this is = = not toml").expect_err("invalid toml must error");
    let msg = err.to_string();
    assert!(
        msg.starts_with("config error: "),
        "invalid TOML must render with the `config error: ` edge prefix, got: {msg}"
    );
}

#[test]
fn config_parse_unknown_key_renders_config_prefixed_message() {
    let doc = "version = 1\nbogus_key = true\n";
    let err = Config::parse(doc).expect_err("unknown key must error (deny_unknown_fields)");
    let msg = err.to_string();
    assert!(
        msg.starts_with("config error: "),
        "unknown key must render under the config edge prefix, got: {msg}"
    );
}

#[test]
fn config_validate_unknown_host_renders_exact_message() {
    let doc = "\
version = 1

[sources.dotfiles]
host = \"ghost\"
repo = \"me/dotfiles\"
";
    let config = Config::parse(doc).expect("parses; host check is a validate-time concern");
    let err = config
        .validate()
        .expect_err("unknown host must fail validation");
    assert_eq!(
        err.to_string(),
        "config error: source `dotfiles` references unknown host `ghost`",
        "unknown-host message must stay byte-identical"
    );
}

// ── source context ──────────────────────────────────────────────

#[test]
fn source_resolve_url_source_empty_refspec_renders_exact_message() {
    let fixture = GitArtifactFixture::build();

    let err = fixture
        .backend
        .resolve(&sn("dots"), &fixture.url, &Refspec::None)
        .expect_err("an empty refspec on the git backend cannot resolve");

    assert_eq!(
        err.to_string(),
        "source error: source dots: git backend cannot resolve a url source's empty refspec",
        "empty-refspec message must stay byte-identical (source edge prefix + chained context)"
    );
}

#[test]
fn source_resolve_without_mirror_renders_source_prefixed_open_message() {
    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());

    let err = backend
        .resolve(
            &sn("dots"),
            "https://example.com/missing",
            &Refspec::Branch("main".into()),
        )
        .expect_err("resolve without a fetched mirror must error");

    let msg = err.to_string();
    assert!(
        msg.starts_with("source error: open mirror dots: "),
        "a missing mirror must render `source error: open mirror <name>: <cause>`, got: {msg}"
    );
}

#[test]
fn source_export_missing_root_renders_root_not_found() {
    let fixture = GitArtifactFixture::build();
    let staging = TempDir::new().expect("staging tempdir");

    let err = fixture
        .backend
        .compute_digest(
            &sn("dots"),
            &fixture.url,
            &fixture.commit,
            Some(Path::new("no-such-root")),
            &[],
            &[],
        )
        .expect_err("a missing root path must error");
    let _ = staging;

    assert_eq!(
        err.to_string(),
        "root path not found in tree: no-such-root",
        "RootNotFound message must stay byte-identical"
    );
}

// ── store / registry context ────────────────────────────────────

#[test]
fn registry_lock_contention_renders_exact_lock_message() {
    let dir = TempDir::new().expect("temp state root");
    let first = FileRegistry::open(dir.path().to_path_buf()).expect("open first registry");
    let second = FileRegistry::open(dir.path().to_path_buf()).expect("open second registry");

    let _held = first.lock_exclusive().expect("first acquires the lock");
    let err = second
        .lock_exclusive()
        .expect_err("second lock must fail while first held");

    assert_eq!(
        err.to_string(),
        "lock error: another phora process is running for this project (state.lock held)",
        "lock-contention message must stay byte-identical (lock edge prefix)"
    );
}

#[test]
fn registry_get_corrupt_record_renders_registry_prefixed_parse_message() {
    let dir = TempDir::new().expect("temp state root");
    let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
    let key = ArtifactKey {
        target: "vscode".to_owned(),
        source: "company-configs".to_owned(),
        artifact: "snippets".to_owned(),
    };

    let record_path = dir
        .path()
        .join("targets")
        .join("vscode")
        .join("artifacts")
        .join("company-configs")
        .join("snippets.toml");
    std::fs::create_dir_all(record_path.parent().expect("record parent"))
        .expect("create record dir");
    std::fs::write(&record_path, b"= not valid toml =").expect("write corrupt record");

    let err = reg
        .get(&key)
        .expect_err("a corrupt record must surface a parse error");
    let msg = err.to_string();
    assert!(
        msg.starts_with("registry error: parse record "),
        "a corrupt record must render `registry error: parse record <path>: <cause>`, got: {msg}"
    );
    assert!(
        msg.contains("snippets.toml"),
        "the parse-record message must name the offending record path, got: {msg}"
    );
}

// ── http / digest context ───────────────────────────────────────

#[test]
fn verify_digest_mismatch_renders_source_prefixed_mismatch_message() {
    let wrong_hex = "0".repeat(64);
    let expected = Digest::from_str(&format!("sha256:{wrong_hex}")).expect("valid sha256 digest");

    let err = verify_digest(b"payload bytes", &expected).expect_err("wrong digest must reject");
    let msg = err.to_string();

    assert!(
        msg.starts_with("source error: sha256 digest mismatch: expected "),
        "a digest mismatch must render under the source edge prefix with chained mismatch text, got: {msg}"
    );
    assert!(
        msg.contains(&wrong_hex),
        "the mismatch message must name the expected hex, got: {msg}"
    );
}

// ── fixture ──────────────────────────────────────────────────────

struct GitArtifactFixture {
    _src: TempDir,
    _git_dir: TempDir,
    backend: GitBackend,
    url: String,
    commit: String,
}

impl GitArtifactFixture {
    fn build() -> Self {
        let src = TempDir::new().expect("src tempdir");
        let root = src.path();

        git(root, &["init", "-b", "main", "."]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "core.autocrlf", "false"]);

        let editor = root.join("editor");
        std::fs::create_dir_all(&editor).expect("create editor dir");
        std::fs::write(editor.join("init.lua"), b"-- init\n").expect("write fixture file");

        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "fixture"]);
        let out = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse runs");
        let commit = String::from_utf8(out.stdout)
            .expect("utf8 sha")
            .trim()
            .to_owned();

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
            commit,
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
