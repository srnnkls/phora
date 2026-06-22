use super::*;

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use tempfile::TempDir;

use crate::config::Refspec;
use crate::source::{
    ExportRequest, ExportResult, GitBackend, HttpBackend, RouterBackend, SourceBackend, SourceError,
};
use crate::store::FileRegistry;

type SourceResult<T> = std::result::Result<T, SourceError>;

fn an(name: &str) -> crate::kernel::ArtifactName {
    crate::kernel::ArtifactName::trusted(name)
}

fn sn(name: &str) -> crate::kernel::SourceName {
    crate::kernel::SourceName::trusted(name)
}

/// A single-entry resolved-commit map keyed by (source, encoded default ref).
fn one_commit(
    parsed: &BTreeMap<String, ParsedSource>,
    name: &str,
    commit: &str,
) -> BTreeMap<(String, String), String> {
    let key = (
        name.to_owned(),
        crate::lock::encode_ref(&parsed[name].refspec()),
    );
    std::iter::once((key, commit.to_owned())).collect()
}

// ── git fixture ────────────────────────────────────────────────

#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn run_git(cwd: &Path, args: &[&str]) {
    crate::store::assert_git_sandboxed(cwd);
    let _serial = crate::store::guard_git_fork();
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn rev_parse(cwd: &Path, rev: &str) -> String {
    let _serial = crate::store::guard_git_fork();
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success(), "rev-parse {rev} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

struct SyncFixture {
    src: TempDir,
    _git_dir: TempDir,
    _state_dir: TempDir,
    backend: GitBackend,
    registry: FileRegistry,
    url: String,
    head_sha: String,
}

fn test_protected(cwd: &Path) -> super::confine::ProtectedPathSet {
    super::confine::ProtectedPathSet::resolve(&crate::config::Paths::default(), cwd)
        .expect("protected set")
}

fn linked_flat_record(target: &str, source: &str, artifact: &str) -> RegistryRecord {
    RegistryRecord {
        version: 1,
        key: artifact_key(target, source, artifact),
        source: source.to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: "2026-06-08T12:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![],
        linked: true,
        vars_digest: None,
    }
}

/// A repo with one commit on `main` containing an `editor/` artifact dir.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_sync_fixture() -> SyncFixture {
    let src = TempDir::new().unwrap();
    let src_path = src.path();

    run_git(src_path, &["init", "-b", "main", "."]);
    run_git(src_path, &["config", "user.email", "test@example.com"]);
    run_git(src_path, &["config", "user.name", "Test"]);

    std::fs::create_dir_all(src_path.join("editor")).unwrap();
    std::fs::write(src_path.join("editor/init.lua"), b"-- init\n").unwrap();
    std::fs::write(src_path.join("editor/notes.bak"), b"scratch\n").unwrap();
    // Sibling tree outside `editor/`: a root/include scoped to `editor` must drop it.
    std::fs::create_dir_all(src_path.join("docs")).unwrap();
    std::fs::write(src_path.join("docs/readme.md"), b"# docs\n").unwrap();
    run_git(src_path, &["add", "-A"]);
    run_git(src_path, &["commit", "-m", "initial"]);

    let head_sha = rev_parse(src_path, "HEAD");

    let git_dir = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let registry =
        FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry over tempdir");
    let url = src_path.to_string_lossy().into_owned();

    SyncFixture {
        src,
        _git_dir: git_dir,
        _state_dir: state_dir,
        backend,
        registry,
        url,
        head_sha,
    }
}

impl SyncFixture {
    /// Adds a second commit on `main`, returning its sha (distinct from HEAD).
    #[expect(
        clippy::unwrap_used,
        reason = "fixture mutation fails loudly; git CLI is assumed present"
    )]
    fn advance_head(&self) -> String {
        std::fs::write(self.src.path().join("editor/extra.lua"), b"-- extra\n").unwrap();
        run_git(self.src.path(), &["add", "-A"]);
        run_git(self.src.path(), &["commit", "-m", "second"]);
        rev_parse(self.src.path(), "HEAD")
    }
}

// ── fetch-counting backend (skip/force oracle) ─────────────────

/// Wraps a real `GitBackend`, counting `fetch` calls so a test can prove that
/// a matching lock entry suppresses the network round-trip. Also counts
/// `export_artifact`/`commit_time` so a Clean second run can prove it did not
/// re-export (exports use deterministic mtimes, so an mtime check alone cannot).
struct CountingBackend<'a> {
    inner: &'a GitBackend,
    fetches: AtomicUsize,
    resolves: AtomicUsize,
    exports: AtomicUsize,
    commit_times: AtomicUsize,
    discovers: AtomicUsize,
    digests: AtomicUsize,
}

impl<'a> CountingBackend<'a> {
    fn new(inner: &'a GitBackend) -> Self {
        Self {
            inner,
            fetches: AtomicUsize::new(0),
            resolves: AtomicUsize::new(0),
            exports: AtomicUsize::new(0),
            commit_times: AtomicUsize::new(0),
            discovers: AtomicUsize::new(0),
            digests: AtomicUsize::new(0),
        }
    }

    fn fetch_count(&self) -> usize {
        self.fetches.load(AtomicOrdering::SeqCst)
    }

    fn resolve_count(&self) -> usize {
        self.resolves.load(AtomicOrdering::SeqCst)
    }

    fn export_count(&self) -> usize {
        self.exports.load(AtomicOrdering::SeqCst)
    }

    fn commit_time_count(&self) -> usize {
        self.commit_times.load(AtomicOrdering::SeqCst)
    }

    fn discover_count(&self) -> usize {
        self.discovers.load(AtomicOrdering::SeqCst)
    }

    fn digest_count(&self) -> usize {
        self.digests.load(AtomicOrdering::SeqCst)
    }
}

impl SourceBackend for CountingBackend<'_> {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.fetches.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.fetch(source, url)
    }

    fn resolve(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> SourceResult<String> {
        self.resolves.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.resolve(source, url, refspec)
    }

    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.commit_times.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.commit_time(source, url, commit)
    }

    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.discovers.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }

    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        self.exports.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.export_artifact(req)
    }

    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.digests.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }

    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

// ── config helpers (target-less so Phase 2/3 are no-ops) ───────

fn config_with_source(name: &str, url: &str) -> Config {
    let toml = format!("version = 1\n\n[sources.{name}]\ngit = \"{url}\"\nbranch = \"main\"\n");
    Config::parse(&toml).expect("source-only config parses")
}

/// A source scoped to `root = "editor"` with a `**/*.bak` exclude, so the
/// resolved digest depends on root/include/exclude propagation, not just the commit.
fn config_with_scoped_source(name: &str, url: &str) -> Config {
    let toml = format!(
        "version = 1\n\n[sources.{name}]\ngit = \"{url}\"\nbranch = \"main\"\n\
             root = \"editor\"\ninclude = [\"init.lua\"]\nexclude = [\"**/*.bak\"]\n"
    );
    Config::parse(&toml).expect("scoped source config parses")
}

fn input<'a>(
    base: &'a Config,
    local: Option<&'a Config>,
    base_lock: Option<Lock>,
    local_lock: Option<Lock>,
    force: bool,
) -> SyncInput<'a> {
    SyncInput {
        base_config: base,
        local_config: local,
        base_lock,
        local_lock,
        force,
        interactive: false,
        prune: false,
        no_hooks: false,
        no_transitive_hooks: false,
        frozen: false,
        resolver: None,
        jobs: None,
    }
}

fn expected_digest(fx: &SyncFixture, name: &str, commit: &str) -> String {
    let m = crate::kernel::Selection::new(&[], &[]).expect("empty matcher builds");
    fx.backend
        .compute_digest(
            &crate::kernel::SourceName::trusted(name),
            &fx.url,
            commit,
            None,
            &m,
        )
        .expect("digest computes over fixture tree")
}

/// Oracle digest computed with the SAME matcher and root the source declares.
/// Differs from [`expected_digest`] whenever sync ignores include/exclude/root.
fn expected_digest_for_source(
    fx: &SyncFixture,
    source: &ParsedSource,
    name: &str,
    commit: &str,
) -> String {
    let m = crate::kernel::Selection::new(source.includes(), source.excludes())
        .expect("source matcher builds");
    fx.backend
        .compute_digest(&sn(name), &fx.url, commit, source.root.as_deref(), &m)
        .expect("scoped digest computes over fixture tree")
}

fn parsed_of(cfg: &Config, name: &str) -> ParsedSource {
    let raw = cfg.sources.get(name).expect("source present");
    ParsedSource::parse(name, raw).expect("source parses to typed form")
}

fn config_digest_of(cfg: &Config, name: &str) -> String {
    parsed_of(cfg, name).config_digest()
}

// ── Phase 1: resolve + lock build ──────────────────────────────

#[test]
fn resolves_source_with_no_prior_lock_into_base_lock() {
    let fx = build_sync_fixture();
    let cfg = config_with_scoped_source("editor-src", &fx.url);
    let source = parsed_of(&cfg, "editor-src");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("sync resolves a fresh source");

    let locked = out
        .base_lock
        .find_source("editor-src")
        .expect("resolved source lands in the base lock");

    assert_eq!(
        locked.commit, fx.head_sha,
        "branch main resolves to the fixture's HEAD commit"
    );
    assert_eq!(
        locked.commit.len(),
        40,
        "resolved commit must be a full 40-hex sha"
    );
    assert!(
        locked.commit.chars().all(|c| c.is_ascii_hexdigit()),
        "commit must be hex"
    );
    assert_eq!(
        locked.resolved, "main",
        "resolved field records the refspec string"
    );
    assert_eq!(locked.git, fx.url, "locked git url matches the source");
    assert_eq!(
        locked.digest,
        expected_digest_for_source(&fx, &source, "editor-src", &fx.head_sha),
        "digest must equal compute_digest with the source's own root + include/exclude"
    );
    assert_ne!(
        locked.digest,
        expected_digest(&fx, "editor-src", &fx.head_sha),
        "sync must propagate root/include/exclude: scoping to editor/ and dropping *.bak \
             must yield a different digest than an empty matcher over the whole tree"
    );
    assert!(
        locked.digest.starts_with("blake3:"),
        "digest carries the blake3: prefix, got {}",
        locked.digest
    );
    assert_eq!(
        locked.config_digest,
        config_digest_of(&cfg, "editor-src"),
        "config_digest must equal the source's config digest"
    );
}

// ── Phase 1: link-source resolution (DLD-009) ──────────────────

/// A link-mode source whose `git` is the local working-tree path `url`.
fn config_with_link_source(name: &str, url: &str) -> Config {
    let toml = format!(
        "version = 1\n\n[sources.{name}]\ngit = \"{url}\"\nbranch = \"main\"\ndeploy = \"link\"\n"
    );
    Config::parse(&toml).expect("link source config parses")
}

/// A link source whose local path has NO phora mirror must still resolve:
/// `resolve_sources` synthesizes an audit lock entry (local path + HEAD read
/// directly via the git object layer), and must NOT fetch or compute a mirror digest.
#[test]
fn link_source_resolves_without_mirror_into_audit_lock_entry() {
    let fx = build_sync_fixture();
    let cfg = config_with_link_source("dev-src", &fx.url);
    // No mirror is seeded for fx.url: a fetch/compute_digest would error.

    let counting = CountingBackend::new(&fx.backend);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let (routed, _commits) = resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        None,
        &counting,
        false,
        false,
        None,
    )
    .expect("link source resolves with no reachable mirror");

    let (_name, locked) = routed
        .iter()
        .find(|(n, _)| n == "dev-src")
        .expect("link source routed into a lock entry");

    assert_eq!(
        locked.resolved, "link",
        "a link entry is marked with the \"link\" sentinel in `resolved`"
    );
    assert_eq!(
        locked.git, fx.url,
        "link entry records the local working-tree path as its git source"
    );
    assert_eq!(
        locked.commit, fx.head_sha,
        "link entry commit is HEAD read directly from the local repo, not a mirror clone"
    );
    assert_eq!(
        locked.config_digest,
        config_digest_of(&cfg, "dev-src"),
        "config_digest is recorded as usual"
    );
}

/// The link carve-out is observable on the backend: zero fetches and zero
/// mirror digest computations for the link source.
#[test]
fn link_source_skips_fetch_and_mirror_digest() {
    let fx = build_sync_fixture();
    let cfg = config_with_link_source("dev-src", &fx.url);

    let counting = CountingBackend::new(&fx.backend);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let _ = resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        None,
        &counting,
        false,
        false,
        None,
    )
    .expect("link source resolves without touching the mirror");

    assert_eq!(
        counting.fetch_count(),
        0,
        "a link source must not fetch the git mirror"
    );
    assert_eq!(
        counting.digest_count(),
        0,
        "a link source must not compute a mirror digest"
    );
}

// ── Phase 1: source_matches skip (no refetch) ──────────────────

#[test]
fn matching_lock_reuses_commit_without_refetch() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);
    let source = parsed_of(&cfg, "editor-src");

    // Pre-seed the mirror so compute_digest can read the tree without sync fetching.
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed fetch");

    let prior = Lock {
        version: 1,
        sources: vec![crate::lock::LockedSource {
            name: "editor-src".to_owned(),
            git: fx.url.clone(),
            resolved: source.refspec().to_string(),
            commit: fx.head_sha.clone(),
            digest: expected_digest(&fx, "editor-src", &fx.head_sha),
            config_digest: source.config_digest(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };

    // Advance HEAD: if sync re-resolved, it would pick up the new commit.
    let new_head = fx.advance_head();
    assert_ne!(new_head, fx.head_sha, "fixture HEAD must have moved");

    let counting = CountingBackend::new(&fx.backend);
    let in_ = input(&cfg, None, Some(prior), None, false);

    let out = sync(&in_, &counting, &fx.registry).expect("sync reuses the matching lock");

    assert_eq!(
        counting.fetch_count(),
        0,
        "a matching lock must suppress fetch entirely"
    );
    assert_eq!(
        counting.resolve_count(),
        0,
        "a matching lock must reuse the locked commit, not re-resolve the cached mirror"
    );
    let locked = out
        .base_lock
        .find_source("editor-src")
        .expect("source still in base lock");
    assert_eq!(
        locked.commit, fx.head_sha,
        "matched source must keep the locked commit, not re-resolve to the new HEAD"
    );
    assert_ne!(
        locked.commit, new_head,
        "no refetch means the advanced HEAD must not leak into the lock"
    );
}

#[test]
fn non_matching_lock_triggers_fetch() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);

    let stale = Lock {
        version: 1,
        sources: vec![crate::lock::LockedSource {
            name: "editor-src".to_owned(),
            git: "https://github.com/other/repo.git".to_owned(),
            resolved: "main".to_owned(),
            commit: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
            digest: "blake3:stale".to_owned(),
            config_digest: "blake3:stale".to_owned(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };

    let counting = CountingBackend::new(&fx.backend);
    let in_ = input(&cfg, None, Some(stale), None, false);

    let out = sync(&in_, &counting, &fx.registry).expect("sync re-resolves a stale source");

    assert!(
        counting.fetch_count() >= 1,
        "a non-matching lock must fetch at least once"
    );
    let locked = out
        .base_lock
        .find_source("editor-src")
        .expect("source resolved");
    assert_eq!(
        locked.commit, fx.head_sha,
        "stale lock must be replaced by the freshly resolved HEAD"
    );
}

// ── TDEP-LOCK-001: --frozen refuses to fetch absent/drifted sources ────

struct DenyNetworkBackend<'a> {
    inner: &'a GitBackend,
}

impl SourceBackend for DenyNetworkBackend<'_> {
    fn fetch(&self, _source: &crate::kernel::SourceName, _url: &str) -> SourceResult<()> {
        Err(SourceError::Source("frozen must not fetch".to_owned()))
    }
    fn resolve(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _refspec: &Refspec,
    ) -> SourceResult<String> {
        Err(SourceError::Source("frozen must not resolve".to_owned()))
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }
    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

#[test]
fn frozen_errors_naming_source_when_no_lock_entry() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let backend = DenyNetworkBackend { inner: &fx.backend };
    let err = resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        None,
        &backend,
        false,
        true,
        None,
    )
    .expect_err("frozen with no lock entry must hard-error instead of fetching");

    assert!(
        err.to_string().contains("editor-src"),
        "the frozen diagnostic must name the missing source, got: {err}"
    );
}

#[test]
fn frozen_reuses_matching_lock_without_touching_network() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);
    let source = parsed_of(&cfg, "editor-src");
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed mirror so compute_digest can read the tree");

    let prior = Lock {
        version: crate::lock::LOCK_SCHEMA_VERSION,
        sources: vec![crate::lock::LockedSource {
            name: "editor-src".to_owned(),
            git: fx.url.clone(),
            resolved: source.refspec().to_string(),
            commit: fx.head_sha.clone(),
            digest: expected_digest(&fx, "editor-src", &fx.head_sha),
            config_digest: source.config_digest(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let backend = DenyNetworkBackend { inner: &fx.backend };
    let (routed, _commits) = resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        Some(&prior),
        &backend,
        false,
        true,
        None,
    )
    .expect("frozen with a matching lock must reuse it without fetch/resolve");

    let (_name, locked) = routed
        .iter()
        .find(|(n, _)| n == "editor-src")
        .expect("the matched source is routed from the lock");
    assert_eq!(
        locked.commit, fx.head_sha,
        "frozen must reuse the locked commit, proving it never resolved the cached mirror"
    );
}

#[test]
fn frozen_errors_on_drifted_lock_entry() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);

    let drifted = Lock {
        version: crate::lock::LOCK_SCHEMA_VERSION,
        sources: vec![crate::lock::LockedSource {
            name: "editor-src".to_owned(),
            git: "https://github.com/other/repo.git".to_owned(),
            resolved: "main".to_owned(),
            commit: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
            digest: "blake3:stale".to_owned(),
            config_digest: "blake3:stale".to_owned(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let backend = DenyNetworkBackend { inner: &fx.backend };
    let err = resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        Some(&drifted),
        &backend,
        false,
        true,
        None,
    )
    .expect_err("frozen with a drifted lock entry must hard-error, not re-resolve");

    assert!(
        err.to_string().contains("editor-src"),
        "the frozen drift diagnostic must name the drifted source, got: {err}"
    );
}

#[test]
fn non_frozen_reresolves_drifted_lock_entry() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);

    let drifted = Lock {
        version: crate::lock::LOCK_SCHEMA_VERSION,
        sources: vec![crate::lock::LockedSource {
            name: "editor-src".to_owned(),
            git: "https://github.com/other/repo.git".to_owned(),
            resolved: "main".to_owned(),
            commit: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
            digest: "blake3:stale".to_owned(),
            config_digest: "blake3:stale".to_owned(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let counting = CountingBackend::new(&fx.backend);
    let (routed, _commits) = resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        Some(&drifted),
        &counting,
        false,
        false,
        None,
    )
    .expect("without --frozen a drifted lock must re-resolve, not error");

    let (_name, locked) = routed
        .iter()
        .find(|(n, _)| n == "editor-src")
        .expect("source re-resolved");
    assert_eq!(
        locked.commit, fx.head_sha,
        "non-frozen drift must re-resolve to the fixture HEAD, replacing the stale lock"
    );
}

// ── Phase 1: --force re-resolves ───────────────────────────────

#[test]
fn force_refetches_even_when_lock_matches() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);
    let source = parsed_of(&cfg, "editor-src");
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed fetch");

    let matching = Lock {
        version: 1,
        sources: vec![crate::lock::LockedSource {
            name: "editor-src".to_owned(),
            git: fx.url.clone(),
            resolved: source.refspec().to_string(),
            commit: fx.head_sha.clone(),
            digest: expected_digest(&fx, "editor-src", &fx.head_sha),
            config_digest: source.config_digest(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };

    // Advance HEAD after seeding the matching lock: force must pick up C2.
    let new_head = fx.advance_head();
    assert_ne!(new_head, fx.head_sha, "fixture HEAD must have moved to C2");

    let counting = CountingBackend::new(&fx.backend);
    let in_ = input(&cfg, None, Some(matching), None, true);

    let out = sync(&in_, &counting, &fx.registry).expect("forced sync succeeds");

    assert!(
        counting.fetch_count() >= 1,
        "force=true must re-fetch even though the lock matches"
    );
    let locked = out
        .base_lock
        .find_source("editor-src")
        .expect("forced source still in base lock");
    assert_eq!(
        locked.commit, new_head,
        "force must re-resolve to the new HEAD (C2), not keep the stale locked commit"
    );
    assert_ne!(
        locked.commit, fx.head_sha,
        "force must not retain the pre-advance commit C"
    );
}

// ── Phase 1: lock routing (base vs local) ──────────────────────

#[test]
fn local_only_source_routes_into_the_local_lock() {
    let fx = build_sync_fixture();
    let base = config_with_source("base-src", &fx.url);
    let local = config_with_source("local-src", &fx.url);
    let in_ = input(&base, Some(&local), None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("sync resolves base and local");

    assert!(
        out.base_lock.find_source("base-src").is_some(),
        "a base-only source must land in the base lock"
    );
    assert!(
        out.base_lock.find_source("local-src").is_none(),
        "a locally-defined source must NOT appear in the base lock"
    );

    let local_lock = out
        .local_lock
        .expect("a local source produces a local lock");
    assert!(
        local_lock.find_source("local-src").is_some(),
        "the locally-defined source must route into the local lock"
    );
    assert!(
        local_lock.find_source("base-src").is_none(),
        "a base-only source must NOT appear in the local lock"
    );
}

#[test]
fn overridden_source_routes_to_local_lock() {
    let fx = build_sync_fixture();
    let base = config_with_source("editor-src", &fx.url);
    let local = {
        let toml = format!(
            "version = 1\n\n[sources.editor-src]\ngit = \"{}\"\nbranch = \"main\"\n",
            fx.url
        );
        Config::parse(&toml).expect("local override config parses")
    };
    let in_ = input(&base, Some(&local), None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("sync resolves the overridden source");

    assert!(
        out.base_lock.find_source("editor-src").is_none(),
        "a source overridden in local must NOT appear in the base lock"
    );
    let local_lock = out
        .local_lock
        .expect("an overridden source produces a local lock");
    assert!(
        local_lock.find_source("editor-src").is_some(),
        "a source present in BOTH base and local must route into the local lock"
    );
}

#[test]
fn base_only_run_produces_no_local_lock() {
    let fx = build_sync_fixture();
    let cfg = config_with_source("editor-src", &fx.url);
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("sync resolves a base-only config");

    assert!(
        out.local_lock.is_none(),
        "with no local config there must be no local lock"
    );
}

// ── Phase 2/3 (7b): export/deploy, collision, skip, warn, prune ─

use std::path::PathBuf;

use crate::deploy::JournalEntry;

use crate::deploy::ArtifactState;
use crate::store::{ArtifactKey, ManifestFile, RecordKind, RegistryRecord};

/// A target deployed beside this dir: `<root>/target` plus the `.phora-stage`
/// sibling sync owns. The tempdir is the target's parent so staging has somewhere
/// to live without polluting the target itself.
struct TargetDir {
    _parent: TempDir,
    parent_path: PathBuf,
}

impl TargetDir {
    fn new() -> Self {
        let parent = TempDir::new().expect("target parent tempdir");
        let parent_path = parent.path().to_path_buf();
        Self {
            _parent: parent,
            parent_path,
        }
    }

    fn target_path(&self) -> PathBuf {
        self.parent_path.join("dest")
    }

    fn artifact_dst(
        &self,
        layout: &crate::config::LayoutConfig,
        source: &str,
        artifact: &str,
    ) -> PathBuf {
        self.target_path()
            .join(layout.artifact_path(source, artifact))
    }

    /// True if the target parent holds any leftover `.phora-*` staging/backup entry.
    fn has_phora_leftover(&self) -> bool {
        std::fs::read_dir(&self.parent_path)
            .expect("read target parent")
            .filter_map(std::result::Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with(".phora-"))
    }
}

/// True if any path under `dir` (recursively) is named `.phora-*`. The no-target-metadata
/// invariant: a deployed target must NEVER contain phora bookkeeping.
fn contains_phora_metadata(dir: &Path) -> bool {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with(".phora"))
}

/// Config with one source scoped to `editor` and a single target whose `path` points
/// at `<parent>/dest`, exposing the `editor` artifact under `layout`.
fn config_one_source_one_target(
    source: &str,
    url: &str,
    target: &str,
    target_path: &Path,
    layout: &str,
) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
             [targets.{target}]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"{layout}\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("one-source one-target config parses")
}

/// The full export-ready key for `(target, source, artifact)`.
fn artifact_key(target: &str, source: &str, artifact: &str) -> ArtifactKey {
    ArtifactKey {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
    }
}

/// Build a second independent git fixture exposing an artifact dir of the given
/// `artifact` name (so two sources can be made to collide in a flat target).
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_named_artifact_repo(artifact: &str, file: &str, content: &[u8]) -> (TempDir, String) {
    let src = TempDir::new().unwrap();
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join(artifact)).unwrap();
    std::fs::write(p.join(artifact).join(file), content).unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let url = p.to_string_lossy().into_owned();
    (src, url)
}

fn flat_layout() -> crate::config::LayoutConfig {
    crate::config::LayoutConfig::default()
}

/// Wraps a real `GitBackend`, returning `Err` from `export_artifact` whenever the
/// request targets a specific artifact name. Lets a test prove warn-and-continue:
/// one artifact's export fails while siblings still deploy.
struct FailingExportBackend<'a> {
    inner: &'a GitBackend,
    fail_artifact: String,
}

impl SourceBackend for FailingExportBackend<'_> {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.inner.fetch(source, url)
    }
    fn resolve(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> SourceResult<String> {
        self.inner.resolve(source, url, refspec)
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        if req.artifact.as_str() == self.fail_artifact {
            return Err(SourceError::Source(format!(
                "injected export failure for {}",
                req.artifact
            )));
        }
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }
    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

// ── deploy a Missing artifact ──────────────────────────────────

#[test]
fn sync_deploys_missing_artifact_files_into_target() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("sync deploys the source");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed init.lua present"),
        b"-- init\n",
        "a Missing artifact must be exported and its files materialized at the target dst"
    );
    assert!(
        !out.had_failures,
        "a clean single-artifact deploy must report no failures"
    );
}

#[test]
fn sync_records_the_deployed_artifact_in_the_registry() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let in_ = input(&cfg, None, None, None, false);

    sync(&in_, &fx.backend, &fx.registry).expect("sync deploys the source");

    let rec = fx
        .registry
        .get(&artifact_key("dest", "editor-src", "editor"))
        .expect("registry get must not error")
        .expect("deploy must persist a registry record for (dest,editor-src,editor)");
    assert_eq!(rec.key.target, "dest");
    assert_eq!(rec.key.source, "editor-src");
    assert_eq!(rec.key.artifact, "editor");
    assert!(
        rec.files.iter().any(|f| f.path == *Path::new("init.lua")),
        "the persisted record must list the exported init.lua, got {:?}",
        rec.files
    );
}

#[test]
fn sync_leaves_no_phora_metadata_inside_the_target() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let in_ = input(&cfg, None, None, None, false);

    sync(&in_, &fx.backend, &fx.registry).expect("sync deploys the source");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        dst.join("init.lua").exists(),
        "premise: the artifact must actually be deployed before the no-metadata check is meaningful"
    );
    assert!(
        !contains_phora_metadata(&td.target_path()),
        "the deployed target must contain NO .phora-* bookkeeping (no-target-metadata invariant), \
             found phora metadata under {}",
        td.target_path().display()
    );
    assert!(
        !td.has_phora_leftover(),
        "after a successful sync the target parent must hold no .phora-* staging/backup leftover"
    );
}

// ── clean skip (no re-export) ──────────────────────────────────

#[test]
fn sync_skips_clean_artifact_on_second_run_without_re_export() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    // First run deploys (may export); count via the same wrapper so the second
    // run's delta is what proves the skip — deterministic mtimes mean an
    // mtime-equality check would pass even if sync re-exported identically.
    let counting = CountingBackend::new(&fx.backend);
    let first = sync(
        &input(&cfg, None, None, None, false),
        &counting,
        &fx.registry,
    )
    .expect("first sync deploys");
    assert!(!first.had_failures, "first deploy must succeed");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        dst.join("init.lua").exists(),
        "premise: the first sync must actually deploy the artifact"
    );
    let exports_after_first = counting.export_count();
    let commit_times_after_first = counting.commit_time_count();
    assert!(
        exports_after_first >= 1,
        "premise: the first run of a Missing artifact must export it at least once, got {exports_after_first}"
    );

    // Reuse the lock from the first run so Phase 1 also skips refetch; the second
    // run must find the artifact Clean and NOT re-export/re-deploy it.
    let second = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, false),
        &counting,
        &fx.registry,
    )
    .expect("second sync runs cleanly");

    assert!(!second.had_failures, "second clean sync must not fail");
    assert_eq!(
        counting.export_count(),
        exports_after_first,
        "a Clean artifact must NOT be re-exported on the second run: \
             export_artifact count must not increase"
    );
    assert_eq!(
        counting.commit_time_count(),
        commit_times_after_first,
        "a Clean artifact must short-circuit before commit_time: \
             commit_time count must not increase on the second run"
    );
}

// ── flat-layout collision ──────────────────────────────────────

#[test]
fn sync_errors_on_flat_layout_collision_naming_artifact_sources_and_target() {
    let fx_a = build_named_artifact_repo("shared", "a.txt", b"from-a\n");
    let fx_b = build_named_artifact_repo("shared", "b.txt", b"from-b\n");

    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let td = TargetDir::new();

    let toml = format!(
        "version = 1\n\n\
             [sources.src-a]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [sources.src-b]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"src-a\", \"src-b\"]\nlayout = \"flat\"\n",
        fx_a.1,
        fx_b.1,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two-source flat-layout config parses");
    let in_ = input(&cfg, None, None, None, false);

    let Err(err) = sync(&in_, &backend, &registry) else {
        panic!("two sources exposing `shared` into a flat target must collide (return Err)");
    };

    let Error::Collision {
        artifact,
        sources,
        target,
    } = err
    else {
        panic!("flat collision must be reported as Error::Collision, got {err:?}");
    };
    assert_eq!(
        artifact, "shared",
        "the collision must name the colliding artifact"
    );
    assert_eq!(target, "dest", "the collision must name the target");
    assert!(
        sources.contains(&"src-a".to_string()) && sources.contains(&"src-b".to_string()),
        "the collision must name BOTH contributing sources, got {sources:?}"
    );

    drop(fx_a);
    drop(fx_b);
}

// ── binding identity + aliasing + destination collision ──

/// A git repo with two top-level artifact dirs: `nvim/` and `tmux/`. A source may
/// narrow to one via its intrinsic `root`; bindings alias the source by identity.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_multi_root_repo() -> (TempDir, String) {
    let src = TempDir::new().unwrap();
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("nvim/init")).unwrap();
    std::fs::write(p.join("nvim/init/config.lua"), b"-- nvim\n").unwrap();
    std::fs::create_dir_all(p.join("tmux/conf")).unwrap();
    std::fs::write(p.join("tmux/conf/tmux.conf"), b"# tmux\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let url = p.to_string_lossy().into_owned();
    (src, url)
}

fn fresh_backend_registry() -> (TempDir, TempDir, GitBackend, FileRegistry) {
    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    (git_dir, state_dir, backend, registry)
}

/// PBR-004: an aliased binding keys its record path by IDENTITY (the `as`) yet must
/// record the UNDERLYING source name in the new `source` field, so provenance survives.
#[test]
fn aliased_binding_records_underlying_source_at_identity_path() {
    let (src, url) = build_multi_root_repo();
    let td = TargetDir::new();
    let (_g, _s, backend, registry) = fresh_backend_registry();
    let toml = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\nroot = \"nvim\"\n\n\
         [targets.dest]\npath = \"{}\"\n\
         sources = {{ nvim = {{ source = \"dotfiles\" }} }}\nlayout = \"by-source\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("aliased-binding config parses");

    sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("sync over an aliased binding must succeed");

    let rec = registry
        .get(&artifact_key("dest", "nvim", "init"))
        .expect("registry get")
        .expect("the record must be keyed by identity `nvim` at …/artifacts/nvim/init.toml");
    assert_eq!(
        rec.key.source, "nvim",
        "key.source is the binding IDENTITY (the `as`), which keys the on-disk record path"
    );
    assert_eq!(
        rec.source, "dotfiles",
        "the new `source` field must carry the UNDERLYING source name `dotfiles`, not the identity"
    );
    drop(src);
}

/// PBR-004: two aliases of ONE source must not collide — distinct record paths keyed
/// by identity, both carrying the SAME underlying `source`.
#[test]
fn two_aliases_of_one_source_each_record_the_shared_underlying_source() {
    let (src, url) = build_multi_root_repo();
    let td = TargetDir::new();
    let (_g, _s, backend, registry) = fresh_backend_registry();
    let toml = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = {{\n\
         one = {{ source = \"dotfiles\" }},\n\
         two = {{ source = \"dotfiles\" }},\n\
         }}\nlayout = \"by-source\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two-alias config parses");

    sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("two aliases of one source must deploy without collision");

    let nvim = registry
        .get(&artifact_key("dest", "one", "nvim"))
        .expect("registry get")
        .expect("the record keyed by identity `one` must exist at its own identity path");
    let tmux = registry
        .get(&artifact_key("dest", "two", "tmux"))
        .expect("registry get")
        .expect("the record keyed by identity `tmux` must exist at its own identity path");
    assert_eq!(
        nvim.source, "dotfiles",
        "the nvim alias must record the shared underlying source `dotfiles`"
    );
    assert_eq!(
        tmux.source, "dotfiles",
        "the tmux alias must record the same underlying source `dotfiles`"
    );
    drop(src);
}

/// PBR-004 back-compat: a BARE binding (identity == source) keys its record path at
/// …/artifacts/<source>/ AND records `source` == the source name, byte-identical to
/// the pre-PBR behaviour.
#[test]
fn bare_binding_records_source_equal_to_identity() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("bare-binding sync deploys");

    let rec = fx
        .registry
        .get(&artifact_key("dest", "editor-src", "editor"))
        .expect("registry get")
        .expect("bare binding must record at …/artifacts/editor-src/editor.toml");
    assert_eq!(
        rec.key.source, "editor-src",
        "a bare binding keys by the source name (identity == source)"
    );
    assert_eq!(
        rec.source, "editor-src",
        "a bare binding's underlying `source` equals its identity"
    );
}

/// PBR-004: `rebuild-registry` must round-trip an alias — after rebuild the record is
/// keyed by IDENTITY (path == …/artifacts/nvim/) and carries the UNDERLYING `source`.
#[test]
fn rebuild_round_trips_aliased_underlying_source() {
    let (src, url) = build_multi_root_repo();
    let td = TargetDir::new();
    let (_g, _s, backend, registry) = fresh_backend_registry();
    let toml = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\nroot = \"nvim\"\n\n\
         [targets.dest]\npath = \"{}\"\n\
         sources = {{ nvim = {{ source = \"dotfiles\" }} }}\nlayout = \"by-source\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("aliased-binding config parses");

    let out = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("seeding sync over an aliased binding must succeed");

    let key = artifact_key("dest", "nvim", "init");
    registry.remove(&key).expect("drop the aliased record");
    assert!(
        registry.get(&key).expect("get after remove").is_none(),
        "premise: the record must be gone before rebuild"
    );

    rebuild_registry(&cfg, &out.base_lock, &backend, &registry)
        .expect("rebuild reconstructs the aliased record");

    let rebuilt = registry
        .get(&key)
        .expect("registry get")
        .expect("rebuild must reconstruct the record at the IDENTITY path …/artifacts/nvim/");
    assert_eq!(
        rebuilt.key.source, "nvim",
        "rebuild must key by identity `nvim`, preserving the on-disk path"
    );
    assert_eq!(
        rebuilt.source, "dotfiles",
        "rebuild must reconstruct the UNDERLYING source `dotfiles` in the `source` field"
    );
    drop(src);
}

/// Two aliased slices that resolve to the SAME destination path under a flat layout
/// are a real conflict — collision detection is about the destination, not the source.
#[test]
fn flat_layout_collision_detected_between_two_aliases_sharing_a_destination() {
    let (src, url) = build_multi_root_repo();
    let td = TargetDir::new();
    let (_g, _s, backend, registry) = fresh_backend_registry();
    let toml = format!(
        "version = 1\n\n[sources.dots]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = {{\n\
         one = {{ source = \"dots\" }},\n\
         two = {{ source = \"dots\" }},\n\
         }}\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two-alias flat config parses");

    let Err(Error::Collision {
        artifact, target, ..
    }) = sync(&input(&cfg, None, None, None, false), &backend, &registry)
    else {
        panic!("two aliases of one source into a flat target must collide on the destination");
    };
    assert_eq!(
        artifact, "nvim",
        "the collision must name the shared artifact"
    );
    assert_eq!(target, "dest", "the collision must name the target");
    drop(src);
}

#[test]
fn same_identity_bindings_sharing_a_destination_still_collide() {
    let (src, url) = build_multi_root_repo();
    let td = TargetDir::new();
    let (_g, _s, backend, registry) = fresh_backend_registry();
    let toml = format!(
        "version = 1\n\n[sources.dots]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = {{\n\
         a = {{ source = \"dots\" }},\n\
         b = {{ source = \"dots\" }},\n\
         }}\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two bare same-source bindings parse");

    let Err(Error::Collision {
        artifact, target, ..
    }) = sync(&input(&cfg, None, None, None, false), &backend, &registry)
    else {
        panic!(
            "two bindings of one source sharing a default identity and projecting `nvim` into a \
             flat target must collide on the destination, not silently overwrite"
        );
    };
    assert_eq!(
        artifact, "nvim",
        "the collision must name the shared artifact"
    );
    assert_eq!(target, "dest", "the collision must name the target");
    drop(src);
}

// ── Modified / Foreign skip unless --force ─────────────────────

/// Pre-place foreign content (no registry record) at the artifact dst so
/// `check_artifact_state` reads Foreign.
fn preplace_foreign(dst: &Path, marker: &[u8]) {
    std::fs::create_dir_all(dst).expect("mkdir foreign dst");
    std::fs::write(dst.join("local-only.txt"), marker).expect("write foreign marker");
}

#[test]
fn sync_without_force_leaves_foreign_content_untouched() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    preplace_foreign(&dst, b"hand-written, not phora-managed\n");

    let st = check_state_at(
        &dst,
        &fx.registry,
        "dest",
        "editor-src",
        "editor",
        &fx.head_sha,
    );
    assert!(
        matches!(st, ArtifactState::Foreign),
        "premise: pre-placed unmanaged content must read as Foreign, got {st:?}"
    );

    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync must not error on a Foreign artifact without --force");

    assert!(
        !out.had_failures,
        "skipping a Foreign artifact is not a failure"
    );
    assert_eq!(
        std::fs::read(dst.join("local-only.txt")).expect("foreign marker still present"),
        b"hand-written, not phora-managed\n",
        "without --force a Foreign artifact must be skipped, leaving the local content intact"
    );
    assert!(
        !dst.join("init.lua").exists(),
        "without --force the upstream editor/init.lua must NOT overwrite Foreign content"
    );
}

#[test]
fn sync_with_force_overwrites_foreign_content_with_upstream() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    preplace_foreign(&dst, b"hand-written, not phora-managed\n");

    let out = sync(
        &input(&cfg, None, None, None, true),
        &fx.backend,
        &fx.registry,
    )
    .expect("forced sync deploys over Foreign content");

    assert!(!out.had_failures, "forced deploy must succeed");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("upstream init.lua deployed"),
        b"-- init\n",
        "--force must overwrite Foreign content with the upstream artifact"
    );
    assert!(
        !dst.join("local-only.txt").exists(),
        "--force replaces the whole artifact dir; the foreign-only file must be gone"
    );
}

// ── Modified (registry-backed) skip unless --force ─────────────

#[test]
fn sync_without_force_skips_modified_registry_artifact() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    let first = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys and records the artifact");
    assert!(!first.had_failures, "first deploy must succeed");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        fx.registry
            .get(&artifact_key("dest", "editor-src", "editor"))
            .expect("registry get must not error")
            .is_some(),
        "premise: the first sync must create a registry record so the artifact is MANAGED"
    );

    // Edit a recorded file in-place: a managed artifact with a changed file reads
    // Modified (record exists), distinct from Foreign (no record).
    let edited = b"-- locally edited, do not clobber\n";
    std::fs::write(dst.join("init.lua"), edited).expect("edit deployed file");

    let st = check_state_at(
        &dst,
        &fx.registry,
        "dest",
        "editor-src",
        "editor",
        &first_commit(&first),
    );
    assert!(
        matches!(st, ArtifactState::Modified { .. }),
        "premise: an edited managed artifact must read as Modified, got {st:?}"
    );

    let out = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync must not error on a Modified artifact without --force");

    assert!(
        !out.had_failures,
        "skipping a Modified artifact is not a failure"
    );
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("edited file still present"),
        edited,
        "without --force a Modified artifact must be skipped, preserving the local edit"
    );
}

#[test]
fn sync_with_force_overwrites_modified_registry_artifact() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    let first = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys and records the artifact");
    assert!(!first.had_failures, "first deploy must succeed");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        fx.registry
            .get(&artifact_key("dest", "editor-src", "editor"))
            .expect("registry get must not error")
            .is_some(),
        "premise: the first sync must create a registry record so the artifact is MANAGED"
    );

    let edited = b"-- locally edited, force should clobber\n";
    std::fs::write(dst.join("init.lua"), edited).expect("edit deployed file");

    let st = check_state_at(
        &dst,
        &fx.registry,
        "dest",
        "editor-src",
        "editor",
        &first_commit(&first),
    );
    assert!(
        matches!(st, ArtifactState::Modified { .. }),
        "premise: an edited managed artifact must read as Modified, got {st:?}"
    );

    let out = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, true),
        &fx.backend,
        &fx.registry,
    )
    .expect("forced sync redeploys over a Modified artifact");

    assert!(!out.had_failures, "forced redeploy must succeed");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("upstream init.lua redeployed"),
        b"-- init\n",
        "--force must replace the local edit with the upstream artifact content"
    );
}

// ── linked artifact idempotence (DLD-005, H1) ──────────────────

/// H1: without `Linked` in the `matches!` guard at the deploy closure, Linked falls to
/// `None => Overwrite` and re-deploys every sync; this pins the no-op.
#[test]
fn second_deploy_over_correct_link_is_a_noop() {
    use std::os::unix::fs::symlink;
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let target = cfg.targets.get("dest").expect("dest target present");
    let source = parsed_of(&cfg, "editor-src");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    std::fs::create_dir_all(dst.parent().expect("dst parent")).expect("mkdir dst parent");
    symlink(fx.src.path().join("editor"), &dst).expect("deploy artifact as a symlink");

    fx.registry
        .put(&linked_flat_record("dest", "editor-src", "editor"))
        .expect("seed linked registry record");
    let counting = CountingBackend::new(&fx.backend);
    let journal = Journal::open(&fx.registry.locks_dir()).expect("open journal");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let commits = one_commit(&parsed, "editor-src", &fx.head_sha);
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let protected = test_protected(fx.src.path());
    let run = TargetRun {
        parsed: &parsed,
        target_name: "dest",
        target,
        commits: &commits,
        remotes: &remotes,
        force: false,
        interactive: false,
        resolver: None,
        vars: &BTreeMap::new(),
        protected: &protected,
    };
    let selection = Selection::new(source.includes(), source.excludes()).expect("selection");
    let (entry_source, entry_artifact) = (sn("editor-src"), an("editor"));
    let entry = ArtifactEntry {
        source: &source,
        git: remotes
            .get("editor-src")
            .expect("resolved_remotes covers every source"),
        source_name: &entry_source,
        identity: "editor-src",
        underlying_source: "editor-src",
        root: source.root.as_deref(),
        commit: &fx.head_sha,
        selection: &selection,
        artifact_name: &entry_artifact,
        target_path: &target.expanded_path(),
        layout_kind: LayoutKind::Flat,
        ejected: &[],
        mode_transition: false,
        template_opt_in: &crate::config::TemplateOptIn::SuffixOnly,
        mapped_source_key: None,
    };
    let state = check_artifact_state(
        &dst,
        "editor-src",
        &fx.head_sha,
        &[],
        "editor",
        &fx.registry,
        &artifact_key("dest", "editor-src", "editor"),
        None,
    )
    .expect("check_artifact_state on the linked dst");
    assert!(
        matches!(state, ArtifactState::Linked),
        "premise: a deployed symlink with a linked record must read Linked, got {state:?}"
    );

    let had_failures = deploy_artifact_entry(run, &entry, &counting, &fx.registry, &journal)
        .expect("deploy pass over a linked artifact must not error");

    assert!(!had_failures, "a no-op linked pass is not a failure");
    assert_eq!(
        counting.export_count(),
        0,
        "a correct linked artifact must NOT be re-exported on a second sync (H1 re-deploy)"
    );
    assert_eq!(
        counting.commit_time_count(),
        0,
        "a no-op linked pass must not perform any backend round-trip"
    );
    assert!(
        std::fs::symlink_metadata(&dst)
            .expect("dst still present")
            .file_type()
            .is_symlink(),
        "the deployed symlink must be left intact — a re-deploy would replace it with a copy"
    );
}

/// The commit the source resolved to during a successful sync, read back from its base lock.
fn first_commit(out: &SyncOutput) -> String {
    out.base_lock
        .find_source("editor-src")
        .expect("editor-src present in base lock after sync")
        .commit
        .clone()
}

fn check_state_at(
    dst: &Path,
    reg: &FileRegistry,
    target: &str,
    source: &str,
    artifact: &str,
    commit: &str,
) -> ArtifactState {
    crate::deploy::check_artifact_state(
        dst,
        source,
        commit,
        &[],
        artifact,
        reg,
        &artifact_key(target, source, artifact),
        None,
    )
    .expect("check_artifact_state")
}

// ── warn-and-continue on a per-artifact deploy failure ─────────

#[test]
fn sync_warns_and_continues_when_one_artifact_export_fails() {
    // Two artifact dirs in one source: `editor` (deploys) and `lint` (export rigged to fail).
    let (src, url) = {
        let src = TempDir::new().expect("src tempdir");
        let p = src.path();
        run_git(p, &["init", "-b", "main", "."]);
        run_git(p, &["config", "user.email", "test@example.com"]);
        run_git(p, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(p.join("editor")).expect("mkdir editor");
        std::fs::write(p.join("editor/init.lua"), b"-- init\n").expect("write editor");
        std::fs::create_dir_all(p.join("lint")).expect("mkdir lint");
        std::fs::write(p.join("lint/rules.toml"), b"[rules]\n").expect("write lint");
        run_git(p, &["add", "-A"]);
        run_git(p, &["commit", "-m", "init"]);
        let url = p.to_string_lossy().into_owned();
        (src, url)
    };

    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let inner = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let backend = FailingExportBackend {
        inner: &inner,
        fail_artifact: "lint".to_owned(),
    };
    let td = TargetDir::new();
    let cfg = config_one_source_one_target("multi", &url, "dest", &td.target_path(), "by-source");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &backend, &registry)
        .expect("a per-artifact export failure must NOT abort the whole run");

    assert!(
        out.had_failures,
        "a failed per-artifact deploy must set had_failures=true"
    );
    let editor_dst = td.target_path().join("multi").join("editor");
    assert_eq!(
        std::fs::read(editor_dst.join("init.lua")).expect("good artifact deployed"),
        b"-- init\n",
        "the artifact whose export succeeded must still be deployed despite the sibling failure"
    );
    assert!(
        registry
            .get(&artifact_key("dest", "multi", "editor"))
            .expect("get good record")
            .is_some(),
        "the successfully deployed artifact must have a registry record"
    );
    assert!(
        registry
            .get(&artifact_key("dest", "multi", "lint"))
            .expect("get failed record")
            .is_none(),
        "the failed artifact must NOT leave a registry record"
    );

    drop(src);
}

// ── prune (--prune) ────────────────────────────────────────────

/// Seed a deployed-looking orphan: files on disk under the target plus a matching
/// registry record, for an artifact that no current source exposes.
fn seed_orphan(
    td: &TargetDir,
    reg: &FileRegistry,
    layout: &crate::config::LayoutConfig,
) -> PathBuf {
    let dst = td.artifact_dst(layout, "gone-src", "obsolete");
    std::fs::create_dir_all(&dst).expect("mkdir orphan dst");
    std::fs::write(dst.join("old.txt"), b"stale\n").expect("write orphan file");
    let record = RegistryRecord {
        version: 1,
        key: artifact_key("dest", "gone-src", "obsolete"),
        source: "gone-src".to_owned(),
        commit: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
        digest: "blake3:orphan".to_owned(),
        projected_at: "2026-01-01T00:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from("old.txt"),
            size: 6,
            mtime: 1_700_000_000,
            blake3: "blake3:orphan".to_owned(),
        }],
        linked: false,
        vars_digest: None,
    };
    reg.put(&record).expect("seed orphan record");
    dst
}

#[test]
fn sync_with_prune_removes_orphan_files_and_record_but_keeps_current() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    let orphan_dst = seed_orphan(&td, &fx.registry, &flat_layout());

    let in_ = SyncInput {
        base_config: &cfg,
        local_config: None,
        base_lock: None,
        local_lock: None,
        force: false,
        interactive: false,
        prune: true,
        no_hooks: false,
        no_transitive_hooks: false,
        frozen: false,
        resolver: None,
        jobs: None,
    };

    let out = sync(&in_, &fx.backend, &fx.registry).expect("prune sync runs");
    assert!(!out.had_failures, "prune run must succeed");

    assert!(
        !orphan_dst.exists(),
        "--prune must remove the orphan's files from disk"
    );
    assert!(
        fx.registry
            .get(&artifact_key("dest", "gone-src", "obsolete"))
            .expect("get orphan record")
            .is_none(),
        "--prune must remove the orphan's registry record"
    );
    assert!(
        fx.registry
            .get(&artifact_key("dest", "editor-src", "editor"))
            .expect("get current record")
            .is_some(),
        "a still-current artifact must NOT be pruned"
    );
    let current_dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        current_dst.join("init.lua").exists(),
        "the current artifact's deployed files must survive prune"
    );
}

#[test]
fn sync_skips_prune_when_a_deploy_failed() {
    // A source with a failing artifact (sets had_failures) PLUS a seeded orphan.
    let (src, url) = {
        let src = TempDir::new().expect("src tempdir");
        let p = src.path();
        run_git(p, &["init", "-b", "main", "."]);
        run_git(p, &["config", "user.email", "test@example.com"]);
        run_git(p, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(p.join("editor")).expect("mkdir editor");
        std::fs::write(p.join("editor/init.lua"), b"-- init\n").expect("write editor");
        run_git(p, &["add", "-A"]);
        run_git(p, &["commit", "-m", "init"]);
        let url = p.to_string_lossy().into_owned();
        (src, url)
    };

    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let inner = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let backend = FailingExportBackend {
        inner: &inner,
        fail_artifact: "editor".to_owned(),
    };
    let td = TargetDir::new();
    let cfg = config_one_source_one_target("only", &url, "dest", &td.target_path(), "flat");

    let orphan_dst = seed_orphan(&td, &registry, &flat_layout());

    let in_ = SyncInput {
        base_config: &cfg,
        local_config: None,
        base_lock: None,
        local_lock: None,
        force: false,
        interactive: false,
        prune: true,
        no_hooks: false,
        no_transitive_hooks: false,
        frozen: false,
        resolver: None,
        jobs: None,
    };

    let out = sync(&in_, &backend, &registry).expect("sync runs despite the export failure");

    assert!(
        out.had_failures,
        "premise: the only artifact's export failed, so had_failures must be true"
    );
    assert!(
        orphan_dst.exists(),
        "--prune must be SKIPPED when had_failures: the orphan's files must remain on disk"
    );
    assert!(
        registry
            .get(&artifact_key("dest", "gone-src", "obsolete"))
            .expect("get orphan record")
            .is_some(),
        "--prune must be SKIPPED on failure: the orphan's registry record must remain"
    );

    drop(src);
}

// ── undefined source reference: graceful Err, not panic ────────

#[test]
fn sync_errors_on_target_referencing_undefined_source() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();

    // The target lists a source name that has NO matching `[sources.*]` entry.
    // Indexing the source map by that name must not panic; sync must surface a
    // graceful error instead.
    let toml = format!(
        "version = 1\n\n\
             [sources.editor-src]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\", \"ghost\"]\nlayout = \"flat\"\n",
        fx.url,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("config naming an undefined source still parses");
    let in_ = input(&cfg, None, None, None, false);

    let result = sync(&in_, &fx.backend, &fx.registry);

    let Err(err) = result else {
        panic!(
            "a target referencing an undefined source `ghost` must return Err, not Ok \
                 (and must not panic while indexing the source map)"
        );
    };
    assert!(
        err.to_string().contains("ghost"),
        "the error must name the undefined source `ghost`, got: {err}"
    );
}

// ── staging cleanup when export fails ──────────────────────────

/// Wraps a real `GitBackend`, but for the rigged artifact it mirrors a real
/// partial export: it creates the staging dir and writes a partial file, then
/// returns `Err` — the way `GitBackend::export_artifact` leaves cruft when the
/// tree walk fails mid-way (e.g. a disallowed symlink). Siblings export normally.
struct PartialStagingExportBackend<'a> {
    inner: &'a GitBackend,
    fail_artifact: String,
}

impl SourceBackend for PartialStagingExportBackend<'_> {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.inner.fetch(source, url)
    }
    fn resolve(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> SourceResult<String> {
        self.inner.resolve(source, url, refspec)
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        if req.artifact.as_str() == self.fail_artifact {
            std::fs::create_dir_all(req.staging_dir).expect("create partial staging dir");
            std::fs::write(req.staging_dir.join("partial.txt"), b"half-written\n")
                .expect("write partial staging file");
            return Err(SourceError::Source(format!(
                "injected export failure after partial staging for {}",
                req.artifact
            )));
        }
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }
    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

#[test]
fn sync_cleans_staging_when_export_fails() {
    let (src, url) = build_named_artifact_repo("editor", "init.lua", b"-- init\n");

    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let inner = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let backend = PartialStagingExportBackend {
        inner: &inner,
        fail_artifact: "editor".to_owned(),
    };
    let td = TargetDir::new();
    let cfg = config_one_source_one_target("only", &url, "dest", &td.target_path(), "flat");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &backend, &registry)
        .expect("a per-artifact export failure must NOT abort the whole run");

    assert!(
        out.had_failures,
        "premise: the only artifact's export failed, so had_failures must be true"
    );
    assert!(
        !td.has_phora_leftover(),
        "a failed export must clean its partial .phora-stage; \
             no staging cruft may remain in the target parent {}",
        td.parent_path.display()
    );

    drop(src);
}

// ── interactive conflict resolution (resolver-driven) ──────────

/// A resolver returning a single preset [`Resolution`] for every conflict,
/// counting how many conflicts it was consulted on.
struct ScriptedResolver {
    verdict: Resolution,
    consulted: AtomicUsize,
    seen: Mutex<Vec<Conflict>>,
}

impl ScriptedResolver {
    fn new(verdict: Resolution) -> Self {
        Self {
            verdict,
            consulted: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn consulted(&self) -> usize {
        self.consulted.load(AtomicOrdering::SeqCst)
    }

    /// The most recent `Conflict` the resolver was consulted on, cloned out.
    fn last_conflict(&self) -> Conflict {
        self.seen
            .lock()
            .expect("seen mutex")
            .last()
            .cloned()
            .expect("resolver was consulted on at least one conflict")
    }
}

impl ConflictResolver for ScriptedResolver {
    fn resolve(&self, conflict: &Conflict) -> Resolution {
        self.consulted.fetch_add(1, AtomicOrdering::SeqCst);
        self.seen.lock().expect("seen mutex").push(conflict.clone());
        self.verdict
    }
}

fn interactive_input<'a>(
    cfg: &'a Config,
    base_lock: Option<Lock>,
    resolver: &'a dyn ConflictResolver,
) -> SyncInput<'a> {
    SyncInput {
        base_config: cfg,
        local_config: None,
        base_lock,
        local_lock: None,
        force: false,
        interactive: true,
        prune: false,
        no_hooks: false,
        no_transitive_hooks: false,
        frozen: false,
        resolver: Some(resolver),
        jobs: None,
    }
}

/// Deploy once, then edit a recorded file so the artifact reads Modified on the next run.
/// Returns the base lock from the first sync (so Phase 1 reuses the same commit).
fn deploy_then_modify(fx: &SyncFixture, td: &TargetDir, cfg: &Config) -> Lock {
    let first = sync(
        &input(cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys and records the artifact");
    assert!(!first.had_failures, "premise: first deploy must succeed");
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    std::fs::write(dst.join("init.lua"), b"-- locally edited\n").expect("edit deployed file");
    let st = check_state_at(
        &dst,
        &fx.registry,
        "dest",
        "editor-src",
        "editor",
        &first_commit(&first),
    );
    assert!(
        matches!(st, ArtifactState::Modified { .. }),
        "premise: edited managed artifact must read Modified, got {st:?}"
    );
    first.base_lock
}

#[test]
fn interactive_overwrite_redeploys_modified_with_upstream() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let base_lock = deploy_then_modify(&fx, &td, &cfg);
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");

    let resolver = ScriptedResolver::new(Resolution::Overwrite);
    let out = sync(
        &interactive_input(&cfg, Some(base_lock), &resolver),
        &fx.backend,
        &fx.registry,
    )
    .expect("interactive overwrite must not error");

    assert!(!out.had_failures, "an overwrite resolution must succeed");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("read redeployed init.lua"),
        b"-- init\n",
        "Overwrite must replace the local edit with the upstream artifact content"
    );
    assert!(
        fx.registry
            .get(&artifact_key("dest", "editor-src", "editor"))
            .expect("registry get must not error")
            .is_some(),
        "Overwrite must leave the registry record in place for the redeployed artifact"
    );
}

#[test]
fn interactive_skip_preserves_local_content() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let base_lock = deploy_then_modify(&fx, &td, &cfg);
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");

    let resolver = ScriptedResolver::new(Resolution::Skip);
    let out = sync(
        &interactive_input(&cfg, Some(base_lock), &resolver),
        &fx.backend,
        &fx.registry,
    )
    .expect("interactive skip must not error");

    assert!(!out.had_failures, "a skip resolution must not fail the run");
    assert_eq!(
        resolver.consulted(),
        1,
        "interactive sync must CONSULT the resolver for the Modified artifact (it did not)"
    );

    let conflict = resolver.last_conflict();
    assert_eq!(
        (
            conflict.target.as_str(),
            conflict.source.as_str(),
            conflict.artifact.as_str()
        ),
        ("dest", "editor-src", "editor"),
        "the Conflict handed to the resolver must identify (target, source, artifact), got {conflict:?}"
    );
    let ConflictKind::Modified { changed } = &conflict.kind else {
        panic!(
            "an edited managed artifact must surface as ConflictKind::Modified, got {:?}",
            conflict.kind
        );
    };
    assert_eq!(
        changed.as_slice(),
        [PathBuf::from("init.lua")],
        "the Modified conflict must name the changed path (the edited init.lua), got {changed:?}"
    );

    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("read preserved init.lua"),
        b"-- locally edited\n",
        "Skip must preserve the local edit, leaving the artifact untouched"
    );
}

/// A repo whose `editor` artifact holds a top-level file AND a nested file, so an eject
/// can prove it preserves the WHOLE on-disk tree, not just the top-level entry.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_nested_artifact_repo() -> (TempDir, String) {
    let src = TempDir::new().unwrap();
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor").join("lua")).unwrap();
    std::fs::write(p.join("editor/init.lua"), b"-- init\n").unwrap();
    std::fs::write(p.join("editor/lua/keymaps.lua"), b"-- keymaps\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let url = p.to_string_lossy().into_owned();
    (src, url)
}

#[test]
fn interactive_eject_persists_entry_keeps_record_and_files() {
    let (src, url) = build_nested_artifact_repo();
    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let td = TargetDir::new();
    let cfg = config_one_source_one_target("editor-src", &url, "dest", &td.target_path(), "flat");

    let first = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("first sync deploys the nested artifact");
    assert!(!first.had_failures, "premise: first deploy must succeed");
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        dst.join("lua").join("keymaps.lua").exists(),
        "premise: the nested file must deploy so the eject keep-files check is meaningful"
    );
    let edited = b"-- locally edited\n";
    std::fs::write(dst.join("init.lua"), edited).expect("edit deployed file");

    let resolver = ScriptedResolver::new(Resolution::Eject);
    let out = sync(
        &interactive_input(&cfg, Some(first.base_lock.clone()), &resolver),
        &backend,
        &registry,
    )
    .expect("interactive eject must not error");

    assert!(
        !out.had_failures,
        "an eject resolution must not fail the run"
    );
    let ejected = registry.load_ejected("dest").expect("load ejected");
    assert!(
        ejected
            .iter()
            .any(|e| e.source == "editor-src" && e.artifact == "editor"),
        "Eject must persist an EjectedEntry for (editor-src, editor), got {ejected:?}"
    );
    assert!(
        registry
            .get(&artifact_key("dest", "editor-src", "editor"))
            .expect("registry get must not error")
            .is_some(),
        "Eject must keep the artifact's registry record so list/where render it as ejected"
    );
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("read kept init.lua"),
        b"-- locally edited\n",
        "Eject must leave the edited top-level file untouched (keep local content)"
    );
    assert_eq!(
        std::fs::read(dst.join("lua").join("keymaps.lua")).expect("read kept nested file"),
        b"-- keymaps\n",
        "Eject must keep EVERY on-disk file, including the nested lua/keymaps.lua, not just the top-level one"
    );

    drop(src);
}

/// A source exposing TWO artifact dirs (`editor`, `widget`) under `by-source` layout,
/// each pre-placed with Foreign content so both would surface a conflict.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_two_artifact_repo() -> (TempDir, String) {
    let src = TempDir::new().unwrap();
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor")).unwrap();
    std::fs::write(p.join("editor/init.lua"), b"-- init\n").unwrap();
    std::fs::create_dir_all(p.join("widget")).unwrap();
    std::fs::write(p.join("widget/conf.toml"), b"[w]\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let url = p.to_string_lossy().into_owned();
    (src, url)
}

#[test]
fn interactive_abort_stops_sync_without_processing_remaining() {
    let (src, url) = build_two_artifact_repo();
    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let td = TargetDir::new();
    let cfg = config_one_source_one_target("multi", &url, "dest", &td.target_path(), "by-source");

    // Pre-place Foreign content at BOTH artifact dsts so each would surface a conflict.
    let editor_dst = td.target_path().join("multi").join("editor");
    let widget_dst = td.target_path().join("multi").join("widget");
    preplace_foreign(&editor_dst, b"hand-written editor\n");
    preplace_foreign(&widget_dst, b"hand-written widget\n");

    let resolver = ScriptedResolver::new(Resolution::Abort);
    let result = sync(
        &interactive_input(&cfg, None, &resolver),
        &backend,
        &registry,
    );

    let Err(err) = result else {
        panic!("an Abort resolution must stop the sync and return Err, got Ok");
    };
    assert!(
        matches!(err, Error::Aborted),
        "Abort must surface as Error::Aborted, got {err:?}"
    );
    assert_eq!(
        resolver.consulted(),
        1,
        "Abort must stop after the FIRST conflict: the resolver must be consulted exactly once, \
             not once per remaining artifact (got {})",
        resolver.consulted()
    );
    assert!(
        !editor_dst.join("init.lua").exists() && !widget_dst.join("conf.toml").exists(),
        "Abort must make NO changes: neither artifact may be deployed over the Foreign content"
    );
    assert_eq!(
        std::fs::read(editor_dst.join("local-only.txt"))
            .expect("preexisting editor Foreign file still present after abort"),
        b"hand-written editor\n",
        "Abort must not touch the preexisting Foreign content it stopped at"
    );
    assert_eq!(
        std::fs::read(widget_dst.join("local-only.txt"))
            .expect("preexisting widget Foreign file still present after abort"),
        b"hand-written widget\n",
        "Abort must not delete the not-yet-processed artifact's preexisting content either"
    );

    drop(src);
}

#[test]
fn non_interactive_still_warns_and_skips_modified_without_resolver() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let base_lock = deploy_then_modify(&fx, &td, &cfg);
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");

    // interactive=false (and no resolver) must keep the existing warn-and-skip behavior.
    let out = sync(
        &input(&cfg, None, Some(base_lock), None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("non-interactive sync must not error on a Modified artifact");

    assert!(
        !out.had_failures,
        "non-interactive skip of a Modified artifact is not a failure"
    );
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("read preserved init.lua"),
        b"-- locally edited\n",
        "without interactive mode a Modified artifact must still be skipped, preserving the edit"
    );
}

// ── recovery sweep wired at sync start ─────────────────────────

#[test]
fn sync_runs_recovery_sweep_finishing_a_swapped_but_unrecorded_artifact() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    // Simulate a crash between swap and registry put: the artifact's files are on disk at
    // its dst, the journal carries a swap_completed=true intent, but no record was persisted.
    let crashed_dst = td.target_path().join("orphan-artifact");
    std::fs::create_dir_all(&crashed_dst).expect("mkdir crashed dst");
    std::fs::write(crashed_dst.join("recovered.txt"), b"recovered\n").expect("write dst file");

    let crashed_key = artifact_key("dest", "editor-src", "orphan-artifact");
    let record = RegistryRecord {
        version: 1,
        key: crashed_key.clone(),
        source: "editor-src".to_owned(),
        commit: fx.head_sha.clone(),
        digest: "blake3:recovered".to_owned(),
        projected_at: "2026-01-01T00:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from("recovered.txt"),
            size: 10,
            mtime: 1_700_000_000,
            blake3: "blake3:recovered".to_owned(),
        }],
        linked: false,
        vars_digest: None,
    };

    let staging_base = td.parent_path.join(".phora-stage");
    let staging = staging_base.join("orphan-artifact-deadbeef");
    let journal = Journal::open(&fx.registry.locks_dir()).expect("open journal");
    journal
        .append(&JournalEntry {
            staging_base,
            staging,
            dst: crashed_dst,
            record,
            swap_completed: true,
        })
        .expect("seed swap-completed crash intent");

    assert!(
        fx.registry
            .get(&crashed_key)
            .expect("pre-sync get")
            .is_none(),
        "premise: the crashed artifact has no registry record yet"
    );
    assert_eq!(
        journal.entries().expect("read seeded journal").len(),
        1,
        "premise: the crash intent is the sole pending journal entry before sync"
    );

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync runs (and must sweep recovery first)");

    assert!(
        fx.registry
            .get(&crashed_key)
            .expect("post-sync get")
            .is_some(),
        "`orphan-artifact` is not a name Phase 2 discovers (the fixture exposes `editor`), so \
             its record can only exist if the start-of-sync recovery_sweep finished the crashed swap"
    );
    assert!(
        fx.registry
            .get(&artifact_key("dest", "editor-src", "editor"))
            .expect("post-sync get for editor")
            .is_some(),
        "premise: Phase 2 deploys the discovered `editor` artifact (distinct from the swept one), \
             proving the swept record was not a Phase 2 side effect"
    );
    assert!(
        journal
            .entries()
            .expect("read journal after sync")
            .is_empty(),
        "the recovery sweep must CLEAR the journal once it finishes the crashed swap"
    );
}

/// Wraps a real `GitBackend` but returns `Err` from `resolve`, forcing Phase 1
/// (`resolve_sources`) to fail. The recovery sweep touches only journal + registry +
/// filesystem, so it can still complete despite the backend error.
struct FailingResolveBackend<'a> {
    inner: &'a GitBackend,
}

impl SourceBackend for FailingResolveBackend<'_> {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.inner.fetch(source, url)
    }
    fn resolve(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _refspec: &Refspec,
    ) -> SourceResult<String> {
        Err(SourceError::Source("injected resolve failure".to_owned()))
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }
    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

#[test]
fn sync_runs_recovery_before_phase1_even_when_resolve_fails() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");

    let crashed_dst = td.target_path().join("orphan-artifact");
    std::fs::create_dir_all(&crashed_dst).expect("mkdir crashed dst");
    std::fs::write(crashed_dst.join("recovered.txt"), b"recovered\n").expect("write dst file");

    let crashed_key = artifact_key("dest", "editor-src", "orphan-artifact");
    let record = RegistryRecord {
        version: 1,
        key: crashed_key.clone(),
        source: "editor-src".to_owned(),
        commit: fx.head_sha.clone(),
        digest: "blake3:recovered".to_owned(),
        projected_at: "2026-01-01T00:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from("recovered.txt"),
            size: 10,
            mtime: 1_700_000_000,
            blake3: "blake3:recovered".to_owned(),
        }],
        linked: false,
        vars_digest: None,
    };

    let staging_base = td.parent_path.join(".phora-stage");
    let staging = staging_base.join("orphan-artifact-deadbeef");
    let journal = Journal::open(&fx.registry.locks_dir()).expect("open journal");
    journal
        .append(&JournalEntry {
            staging_base,
            staging,
            dst: crashed_dst,
            record,
            swap_completed: true,
        })
        .expect("seed swap-completed crash intent");

    assert!(
        fx.registry
            .get(&crashed_key)
            .expect("pre-sync get")
            .is_none(),
        "premise: the crashed artifact has no registry record yet"
    );

    let backend = FailingResolveBackend { inner: &fx.backend };
    let result = sync(
        &input(&cfg, None, None, None, false),
        &backend,
        &fx.registry,
    );

    assert!(
        result.is_err(),
        "premise: Phase 1 (resolve_sources) must fail when the backend's resolve errors"
    );

    assert!(
        fx.registry
            .get(&crashed_key)
            .expect("post-sync get")
            .is_some(),
        "recovery must run at the TRUE START (before Phase 1): the crashed record exists only \
             if recovery_sweep finished the swap before resolve_sources returned its error"
    );
    assert!(
        journal
            .entries()
            .expect("read journal after sync")
            .is_empty(),
        "recovery running before Phase 1 must have CLEARED the journal despite the Phase 1 error"
    );
}

// ── eject / uneject (programmatic, not interactive) ────────────

/// Seed a managed artifact: a registry record for `(dest, source, artifact)` plus its
/// deployed files on disk at the flat-layout dst. Returns the deployed artifact dir.
fn seed_managed_artifact(
    td: &TargetDir,
    reg: &FileRegistry,
    source: &str,
    artifact: &str,
    file: &str,
    content: &[u8],
) -> PathBuf {
    let dst = td.artifact_dst(&flat_layout(), source, artifact);
    std::fs::create_dir_all(&dst).expect("mkdir managed dst");
    std::fs::write(dst.join(file), content).expect("write managed file");
    let record = RegistryRecord {
        version: 1,
        key: artifact_key("dest", source, artifact),
        source: source.to_owned(),
        commit: "feedfacefeedfacefeedfacefeedfacefeedface".to_owned(),
        digest: "blake3:seed".to_owned(),
        projected_at: "2026-01-01T00:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from(file),
            size: content.len() as u64,
            mtime: 1_700_000_000,
            blake3: blake3::hash(content).to_hex().to_string(),
        }],
        linked: false,
        vars_digest: None,
    };
    reg.put(&record).expect("seed managed record");
    dst
}

fn eject_target_config(td: &TargetDir, fx: &SyncFixture) -> Config {
    config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat")
}

#[test]
fn eject_adds_ejected_entry_keeps_record_and_files() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = eject_target_config(&td, &fx);
    let dst = seed_managed_artifact(
        &td,
        &fx.registry,
        "editor-src",
        "editor",
        "init.lua",
        b"-- init\n",
    );
    let key = artifact_key("dest", "editor-src", "editor");
    assert!(
        fx.registry
            .get(&key)
            .expect("registry get must not error")
            .is_some(),
        "premise: the artifact must be MANAGED (record present) before eject"
    );

    eject(&cfg, &fx.registry, "editor", "editor-src", "dest")
        .expect("eject a managed artifact must succeed");

    let ejected = fx.registry.load_ejected("dest").expect("load ejected");
    assert!(
        ejected
            .iter()
            .any(|e| e.source == "editor-src" && e.artifact == "editor"),
        "eject must add an EjectedEntry for (editor-src, editor), got {ejected:?}"
    );
    assert!(
        fx.registry
            .get(&key)
            .expect("registry get must not error")
            .is_some(),
        "eject must KEEP the registry record so list/where can render it as ejected"
    );
    let state = crate::deploy::check_artifact_state(
        &dst,
        "editor-src",
        "any-commit",
        &ejected,
        "editor",
        &fx.registry,
        &key,
        None,
    )
    .expect("check_artifact_state");
    assert!(
        matches!(state, crate::deploy::ArtifactState::Ejected),
        "a kept record plus its ejected entry must read as Ejected, got {state:?}"
    );
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed file still present"),
        b"-- init\n",
        "eject must LEAVE the on-disk files untouched"
    );
}

#[test]
fn eject_persists_entry_across_a_reopened_registry() {
    let state_dir = TempDir::new().expect("state dir");
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = eject_target_config(&td, &fx);
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    seed_managed_artifact(&td, &reg, "editor-src", "editor", "init.lua", b"-- init\n");

    eject(&cfg, &reg, "editor", "editor-src", "dest").expect("eject must succeed");

    let reopened = FileRegistry::open(state_dir.path().to_path_buf()).expect("reopen registry");
    let ejected = reopened
        .load_ejected("dest")
        .expect("load ejected reopened");
    assert!(
        ejected
            .iter()
            .any(|e| e.source == "editor-src" && e.artifact == "editor"),
        "eject must PERSIST the ejected entry to disk so a fresh registry sees it, got {ejected:?}"
    );
}

#[test]
fn uneject_removes_matching_entry_and_leaves_others() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = eject_target_config(&td, &fx);

    let seeded = vec![
        EjectedEntry {
            source: "editor-src".to_owned(),
            artifact: "editor".to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        },
        EjectedEntry {
            source: "other-src".to_owned(),
            artifact: "widget".to_owned(),
            ejected_at: "2026-01-30T10:00:00Z".to_owned(),
        },
    ];
    fx.registry
        .save_ejected("dest", &seeded)
        .expect("seed ejected entries");

    uneject(&cfg, &fx.registry, "editor", "editor-src", "dest")
        .expect("uneject an ejected artifact must succeed");

    let ejected = fx.registry.load_ejected("dest").expect("load ejected");
    assert!(
        !ejected
            .iter()
            .any(|e| e.source == "editor-src" && e.artifact == "editor"),
        "uneject must REMOVE the matching (editor-src, editor) entry, got {ejected:?}"
    );
    assert!(
        ejected
            .iter()
            .any(|e| e.source == "other-src" && e.artifact == "widget"),
        "uneject must LEAVE unrelated ejected entries intact, got {ejected:?}"
    );
}

// ── verify (content-hash audit) ────────────────────────────────

/// Seed a managed artifact whose `ManifestFile.blake3` is the REAL blake3 of the
/// content written to disk, so a verify match case is genuine, not tautological.
/// `files`: (relative path, content) pairs; all live under the flat-layout dst.
fn seed_verifiable_artifact(
    td: &TargetDir,
    reg: &FileRegistry,
    source: &str,
    artifact: &str,
    files: &[(&str, &[u8])],
) -> PathBuf {
    let dst = td.artifact_dst(&flat_layout(), source, artifact);
    let manifest = files
        .iter()
        .map(|(rel, content)| {
            let path = dst.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir verify file parent");
            }
            std::fs::write(&path, content).expect("write verify file");
            ManifestFile {
                path: PathBuf::from(rel),
                size: content.len() as u64,
                mtime: 1_700_000_000,
                blake3: blake3::hash(content).to_hex().to_string(),
            }
        })
        .collect();
    let record = RegistryRecord {
        version: 1,
        key: artifact_key("dest", source, artifact),
        source: source.to_owned(),
        commit: "feedfacefeedfacefeedfacefeedfacefeedface".to_owned(),
        digest: "blake3:seed".to_owned(),
        projected_at: "2026-01-01T00:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: manifest,
        linked: false,
        vars_digest: None,
    };
    reg.put(&record).expect("seed verifiable record");
    dst
}

fn verify_config(td: &TargetDir, fx: &SyncFixture) -> Config {
    config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat")
}

#[test]
fn verify_reports_no_mismatch_when_content_matches_recorded_hash() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = verify_config(&td, &fx);
    seed_verifiable_artifact(
        &td,
        &fx.registry,
        "editor-src",
        "editor",
        &[
            ("init.lua", b"-- init\n"),
            ("lua/keymaps.lua", b"-- keymaps\n"),
        ],
    );

    let mismatches = verify(&cfg, &fx.registry).expect("verify must not error");

    assert!(
        !mismatches
            .iter()
            .any(|m| m.key == artifact_key("dest", "editor-src", "editor")),
        "an artifact whose deployed file contents hash to the recorded blake3 must produce \
             NO mismatch, got {mismatches:?}"
    );
}

#[test]
fn verify_skips_ejected_artifacts() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = verify_config(&td, &fx);
    let dst = seed_verifiable_artifact(
        &td,
        &fx.registry,
        "editor-src",
        "editor",
        &[("init.lua", b"-- init\n")],
    );
    std::fs::write(dst.join("init.lua"), b"locally edited after eject").expect("tamper file");
    fx.registry
        .save_ejected(
            "dest",
            &[crate::store::EjectedEntry {
                source: "editor-src".to_owned(),
                artifact: "editor".to_owned(),
                ejected_at: "2026-01-01T00:00:00Z".to_owned(),
            }],
        )
        .expect("mark the artifact ejected");

    let mismatches = verify(&cfg, &fx.registry).expect("verify must not error");

    assert!(
        mismatches.is_empty(),
        "verify must SKIP an ejected artifact: its files are the user's now, so a divergence \
         from the kept record is not a mismatch, got {mismatches:?}"
    );
}

#[test]
fn verify_reports_mismatch_for_edited_deployed_file() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = verify_config(&td, &fx);

    let original: &[u8] = b"hello world!";
    let edited: &[u8] = b"hello-world!";
    // Equal length defeats a size-only shortcut: only a content hash sees the diff.
    assert_eq!(original.len(), edited.len());

    let dst = seed_verifiable_artifact(
        &td,
        &fx.registry,
        "editor-src",
        "editor",
        &[("init.lua", original), ("notes.txt", b"keep me\n")],
    );
    let recorded_hash = blake3::hash(original).to_hex().to_string();

    std::fs::write(dst.join("init.lua"), edited).expect("edit deployed file");
    let edited_hash = blake3::hash(edited).to_hex().to_string();
    assert_ne!(recorded_hash, edited_hash);

    let mismatches = verify(&cfg, &fx.registry).expect("verify must not error");

    let hit = mismatches
        .iter()
        .find(|m| {
            m.key == artifact_key("dest", "editor-src", "editor")
                && m.path == *Path::new("init.lua")
        })
        .unwrap_or_else(|| {
            panic!("verify must report the edited init.lua as a mismatch, got {mismatches:?}")
        });
    let VerifyReason::ContentMismatch { expected, actual } = &hit.reason else {
        panic!(
            "an edited file must be reported as a content mismatch, got {:?}",
            hit.reason
        )
    };
    assert_eq!(
        *expected, recorded_hash,
        "ContentMismatch.expected must be the recorded blake3 of the original content"
    );
    assert_eq!(
        *actual, edited_hash,
        "ContentMismatch.actual must be the real blake3 of the edited deployed content, \
             proving verify hashes the actual bytes on disk (not size, not a stale/garbage hash)"
    );
    assert!(
        !mismatches.iter().any(|m| m.path == *Path::new("notes.txt")),
        "the untouched notes.txt must NOT be reported as a mismatch, got {mismatches:?}"
    );
}

#[test]
fn verify_reports_missing_recorded_file() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = verify_config(&td, &fx);
    let dst = seed_verifiable_artifact(
        &td,
        &fx.registry,
        "editor-src",
        "editor",
        &[("init.lua", b"-- init\n"), ("gone.lua", b"-- gone\n")],
    );

    // Delete a recorded file: it is in the record but absent on disk.
    std::fs::remove_file(dst.join("gone.lua")).expect("remove recorded file");

    let mismatches = verify(&cfg, &fx.registry).expect("verify must not error");

    let hit = mismatches
        .iter()
        .find(|m| {
            m.key == artifact_key("dest", "editor-src", "editor")
                && m.path == *Path::new("gone.lua")
        })
        .unwrap_or_else(|| {
            panic!("verify must report the missing gone.lua as a mismatch, got {mismatches:?}")
        });
    assert_eq!(
        hit.reason,
        VerifyReason::Missing,
        "a recorded file absent from disk must be reported as Missing, got {:?}",
        hit.reason
    );
}

// ── rebuild-registry (reconstruct from config+lock + mirror + disk) ─

/// Deploy `editor-src` into a fresh target via a real sync, returning the
/// fixture, target dir, config, and the resulting base lock. The mirror is
/// fetched and the registry populated as a side effect.
fn rebuild_setup() -> (SyncFixture, TargetDir, Config, Lock) {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys and records the artifact");
    assert!(!out.had_failures, "premise: the seeding sync must succeed");
    (fx, td, cfg, out.base_lock)
}

#[test]
fn rebuild_reconstructs_lost_record_with_same_commit_digest_and_files() {
    let (fx, td, cfg, lock) = rebuild_setup();
    let key = artifact_key("dest", "editor-src", "editor");

    let original = fx
        .registry
        .get(&key)
        .expect("registry get must not error")
        .expect("premise: sync must have recorded the artifact");
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        dst.join("init.lua").exists(),
        "premise: the deployed files must remain on disk after the record is dropped"
    );

    // Lose the registry state but keep files + mirror + config + lock.
    fx.registry.remove(&key).expect("drop the registry record");
    assert!(
        fx.registry.get(&key).expect("get after remove").is_none(),
        "premise: the record must be gone before rebuild"
    );

    let report = rebuild_registry(&cfg, &lock, &fx.backend, &fx.registry)
        .expect("rebuild reconstructs from mirror + disk");

    let rebuilt = fx
        .registry
        .get(&key)
        .expect("registry get must not error")
        .expect("rebuild must reconstruct the dropped record");

    assert_eq!(
        rebuilt.commit, original.commit,
        "reconstructed record must carry the same locked commit"
    );
    assert_eq!(
        rebuilt.digest, original.digest,
        "reconstructed export digest must equal the originally-synced digest \
             (recomputed by re-walking the mirror at the locked commit)"
    );

    let file_hashes = |rec: &RegistryRecord| -> BTreeSet<(PathBuf, String)> {
        rec.files
            .iter()
            .map(|f| (f.path.clone(), f.blake3.clone()))
            .collect()
    };
    assert_eq!(
        file_hashes(&rebuilt),
        file_hashes(&original),
        "reconstructed files must match the original path+blake3 set \
             (content hashes recomputed from the mirror)"
    );

    assert!(
        report.reconstructed.contains(&key),
        "the report must list the reconstructed artifact, got {:?}",
        report.reconstructed
    );
    assert!(
        report.modified.is_empty(),
        "an unmodified deploy must not be reported [modified], got {:?}",
        report.modified
    );
}

#[test]
fn rebuild_preserves_ejected_entries() {
    let (fx, _td, cfg, lock) = rebuild_setup();
    let key = artifact_key("dest", "editor-src", "editor");
    fx.registry.remove(&key).expect("drop the registry record");

    let ejected = EjectedEntry {
        source: "editor-src".to_owned(),
        artifact: "other".to_owned(),
        ejected_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    fx.registry
        .save_ejected("dest", std::slice::from_ref(&ejected))
        .expect("seed an ejected entry before rebuild");

    rebuild_registry(&cfg, &lock, &fx.backend, &fx.registry)
        .expect("rebuild succeeds with an ejected entry present");

    let after = fx
        .registry
        .load_ejected("dest")
        .expect("load ejected after rebuild");
    assert!(
        after.contains(&ejected),
        "rebuild must preserve the pre-existing ejected entry, got {after:?}"
    );
}

#[test]
fn rebuild_reports_modified_when_disk_content_fails_recomputed_hash() {
    let (fx, td, cfg, lock) = rebuild_setup();
    let key = artifact_key("dest", "editor-src", "editor");
    fx.registry.remove(&key).expect("drop the registry record");

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    let file = dst.join("init.lua");
    let original = std::fs::read(&file).expect("deployed init.lua present before tamper");
    let tampered: Vec<u8> = original
        .iter()
        .map(|b| if *b == b'i' { b'I' } else { *b })
        .collect();
    assert_ne!(
        tampered, original,
        "premise: the tamper must actually change the bytes"
    );
    assert_eq!(
        tampered.len(),
        original.len(),
        "premise: the tamper must preserve byte length so size alone won't reveal it"
    );
    std::fs::write(&file, &tampered)
        .expect("overwrite with same-length, different content so disk fails the recomputed hash");

    let report = rebuild_registry(&cfg, &lock, &fx.backend, &fx.registry)
        .expect("rebuild must not error on a content mismatch");

    assert!(
        report.modified.contains(&key),
        "a managed artifact whose disk content fails the recomputed hash must be \
             reported [modified], got {:?}",
        report.modified
    );
}

#[test]
fn rebuild_reports_foreign_artifact_dir_with_no_config_match() {
    let (fx, td, cfg, lock) = rebuild_setup();

    // An on-disk artifact dir under the target that no source/lock entry maps to.
    let foreign_dir = td.target_path().join("hand-made").join("scratch");
    std::fs::create_dir_all(&foreign_dir).expect("mkdir foreign artifact dir");
    std::fs::write(foreign_dir.join("notes.txt"), b"hand-written\n").expect("write foreign file");

    let report = rebuild_registry(&cfg, &lock, &fx.backend, &fx.registry)
        .expect("rebuild must not error in the presence of a foreign dir");

    assert!(
        report
            .foreign
            .iter()
            .any(|p| p.ends_with("scratch")
                || p.ends_with(std::path::Path::new("hand-made/scratch"))),
        "an on-disk artifact dir with no config/lock match must be reported [foreign], \
             got {:?}",
        report.foreign
    );

    let managed = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert!(
        !report
            .foreign
            .iter()
            .any(|p| { p == &managed || p.ends_with("editor") || p.starts_with(&managed) }),
        "the legit managed artifact ({}) must NOT be reported [foreign]; \
             only unmanaged on-disk dirs may appear, got {:?}",
        managed.display(),
        report.foreign
    );
}

/// ARCH-003 (the stranded-dotfile-orphan fix): the foreign scan must route
/// through `Selection`, not a blanket `starts_with('.')`. A hidden on-disk dir
/// the source's selection ADMITS (`include = [".config"]`) but that no managed
/// artifact maps to is an orphaned phora-shaped artifact and MUST be reported
/// foreign. A hidden dir NO selection admits (`.cache`) is the user's own and
/// MUST stay out of the foreign set. The current blanket skip drops both.
#[test]
fn foreign_scan_reports_selection_admitted_dotfile_but_spares_user_dotfile() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.editor-src]\ngit = \"{url}\"\nbranch = \"main\"\ninclude = [\".config\"]\n\n\
             [targets.dest]\npath = \"{target}\"\nsources = [\"editor-src\"]\nlayout = \"flat\"\n",
        url = fx.url,
        target = td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("dotfile-include config parses");
    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("seeding sync over a dotfile-include source succeeds");
    assert!(!out.had_failures, "premise: the seeding sync must succeed");

    let admitted =
        crate::kernel::Selection::new(&[".config".to_owned()], &[]).expect("selection builds");
    assert!(
        admitted.selects_artifact(".config"),
        "premise: include=[.config] must admit the `.config` artifact name"
    );
    assert!(
        !admitted.selects_artifact(".cache"),
        "premise: include=[.config] must NOT admit an unrelated `.cache` dotfile"
    );

    let orphan = td.target_path().join(".config");
    std::fs::create_dir_all(&orphan).expect("plant the orphaned dotfile artifact");
    std::fs::write(orphan.join("settings.json"), b"{}\n").expect("write orphan file");
    let user_dir = td.target_path().join(".cache");
    std::fs::create_dir_all(&user_dir).expect("plant the user's own dotfile dir");
    std::fs::write(user_dir.join("blob.bin"), b"mine\n").expect("write user file");

    let report = rebuild_registry(&cfg, &out.base_lock, &fx.backend, &fx.registry)
        .expect("rebuild must not error with hidden dirs present");

    assert!(
        report.foreign.iter().any(|p| p.ends_with(".config")),
        "a hidden dir the source's Selection admits but that maps to no managed \
             artifact MUST be reported [foreign] (the scan must consult Selection, not \
             blanket-skip dotfiles), got {:?}",
        report.foreign
    );
    assert!(
        !report.foreign.iter().any(|p| p.ends_with(".cache")),
        "a hidden dir no source Selection admits is the user's own and MUST NOT be \
             reported [foreign]; the fix must not over-report, got {:?}",
        report.foreign
    );
}

// ── DLD-003: mode-aware working-tree discovery ─────────────────

/// A plain on-disk working tree (not a git repo) with three real artifact
/// dirs, a `.hidden` dotdir, a regular file, and an `uncommitted` dir that
/// was never `git add`ed — proving the Link scan reads disk, not the ODB.
#[expect(clippy::unwrap_used, reason = "fixture setup fails loudly in tests")]
fn build_worktree(root_sub: Option<&str>) -> TempDir {
    let td = TempDir::new().unwrap();
    let base = match root_sub {
        Some(sub) => td.path().join(sub),
        None => td.path().to_path_buf(),
    };
    std::fs::create_dir_all(&base).unwrap();
    for art in ["alpha", "zeta", "uncommitted"] {
        std::fs::create_dir_all(base.join(art)).unwrap();
        std::fs::write(base.join(art).join("file.txt"), b"x\n").unwrap();
    }
    std::fs::create_dir_all(base.join(".hidden")).unwrap();
    std::fs::write(base.join(".hidden").join("secret"), b"s\n").unwrap();
    std::fs::write(base.join("loose.txt"), b"loose\n").unwrap();
    td
}

fn match_all() -> Selection {
    Selection::new(&[], &[]).expect("empty matcher")
}

#[test]
fn worktree_scan_returns_sorted_real_dirs_excluding_dotdirs_and_files() {
    let wt = build_worktree(None);

    let found = discover_working_tree(wt.path(), None, &match_all())
        .expect("scanning an existing working tree must succeed");

    assert_eq!(
        found,
        vec![an("alpha"), an("uncommitted"), an("zeta")],
        "the disk scan must return only real subdirectories, sorted, \
             excluding the .hidden dotdir and the loose.txt regular file; \
             the never-added `uncommitted` dir proves this is a disk scan"
    );
}

#[test]
fn worktree_scan_honors_root_subdir() {
    let wt = build_worktree(Some("languages"));

    let found = discover_working_tree(wt.path(), Some(Path::new("languages")), &match_all())
        .expect("scanning <git>/<root> must succeed");

    assert_eq!(
        found,
        vec![an("alpha"), an("uncommitted"), an("zeta")],
        "with root set, artifacts nested under <git>/languages must be discovered"
    );
    let direct = discover_working_tree(wt.path(), None, &match_all())
        .expect("scanning the git root itself must succeed");
    assert_eq!(
        direct,
        vec![an("languages")],
        "without root, only the top-level `languages` dir is an artifact"
    );
}

#[test]
fn worktree_scan_honors_matcher_exclude() {
    let wt = build_worktree(None);
    let selection = Selection::new(&[], &["zeta".to_owned()]).expect("exclude selection");

    let found = discover_working_tree(wt.path(), None, &selection)
        .expect("scan with an exclude must succeed");

    assert_eq!(
        found,
        vec![an("alpha"), an("uncommitted")],
        "the include/exclude matcher must gate disk artifacts just as it gates ODB ones"
    );
}

#[test]
fn worktree_scan_missing_path_errors() {
    let parent = TempDir::new().expect("hermetic parent for the missing path");
    let missing = parent.path().join("phora-link-source-absent");
    assert!(
        !missing.exists(),
        "premise: the child path under the tempdir must not exist"
    );

    let err = discover_working_tree(&missing, None, &match_all())
        .expect_err("an absent local path must be a clear error, not an empty list");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("phora-link-source-absent")
            || msg.contains("not found")
            || msg.contains("no such file"),
        "the error should point at the missing path, got: {msg}"
    );
}

#[test]
fn worktree_scan_missing_root_errors() {
    let wt = build_worktree(None);

    let err = discover_working_tree(wt.path(), Some(Path::new("absent-root")), &match_all())
        .expect_err("a missing root subdir must error, not silently yield nothing");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("absent-root")
            || msg.contains("not found")
            || msg.contains("no such file")
            || msg.contains("root"),
        "the error should name the missing root, got: {msg}"
    );
}

/// Cross-site invariant (review C2): a Link source must be discovered from
/// DISK in `rebuild_registry`, never via `backend.discover_artifacts`.
fn config_link_source_one_target(source: &str, link_git: &Path, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \"{}\"\nbranch = \"main\"\ndeploy = \"link\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"by-source\"\n",
        link_git.display(),
        target_path.display(),
    );
    Config::parse(&toml).expect("link-source config parses")
}

fn link_lock(source: &str, link_git: &Path) -> Lock {
    Lock {
        version: 1,
        sources: vec![LockedSource {
            name: source.to_owned(),
            git: link_git.to_string_lossy().into_owned(),
            resolved: "link".to_owned(),
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            config_digest: "blake3:link".to_owned(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    }
}

#[test]
fn rebuild_discovers_link_source_from_disk_never_via_odb() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    let cfg = config_link_source_one_target("linked-src", wt.path(), &td.target_path());
    let lock = link_lock("linked-src", wt.path());

    let counting = CountingBackend::new({
        let git_dir = TempDir::new().expect("backend mirror dir");
        // Leak a backend bound to an empty mirror dir; a link source must
        // never reach it, so it is never opened.
        Box::leak(Box::new(GitBackend::new(git_dir.path().to_path_buf())))
    });

    let report = rebuild_registry(&cfg, &lock, &counting, &fx_registry())
        .expect("rebuild over a link source must succeed using the disk scan");

    assert_eq!(
        counting.discover_count(),
        0,
        "a Link source must be discovered from disk in rebuild_registry; \
             the ODB backend.discover_artifacts must NOT be called (review C2)"
    );
    let names: BTreeSet<String> = report
        .reconstructed
        .iter()
        .map(|k| k.artifact.clone())
        .collect();
    assert!(
        names.contains("alpha") && names.contains("zeta"),
        "the link source's working-tree artifacts must be reconstructed, got {names:?}"
    );
}

fn fx_registry() -> FileRegistry {
    let state = TempDir::new().expect("registry state dir");
    let reg = FileRegistry::open(state.path().to_path_buf()).expect("open registry");
    std::mem::forget(state);
    reg
}

// ── DLD-006: integrity quarantine (verify / rebuild / prune) ────

/// Seed a `linked` registry record carrying a STRAY manifest file that does NOT
/// exist on disk. Today `verify` iterates `record.files` and would read the
/// absent file, emitting `Missing`; the explicit `if record.linked { continue; }`
/// guard must short-circuit BEFORE that read. RED until the guard lands.
#[test]
fn verify_skips_linked_record_even_with_stray_manifest_file() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = verify_config(&td, &fx);

    let stray = RegistryRecord {
        version: 1,
        key: artifact_key("dest", "editor-src", "editor"),
        source: "editor-src".to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: "2026-06-08T12:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from("ghost.lua"),
            size: 7,
            mtime: 1_700_000_000,
            blake3: blake3::hash(b"phantom").to_hex().to_string(),
        }],
        linked: true,
        vars_digest: None,
    };
    fx.registry
        .put(&stray)
        .expect("seed a linked record carrying a stray manifest file");

    let mismatches = verify(&cfg, &fx.registry).expect("verify must not error");

    assert!(
        !mismatches
            .iter()
            .any(|m| m.key == artifact_key("dest", "editor-src", "editor")),
        "verify must SKIP a linked record entirely (record.linked short-circuit), never \
             reading its files — even a stray manifest entry pointing at an absent path must \
             produce NO mismatch, got {mismatches:?}"
    );
}

/// Guard: a normal linked record (empty `files`) over an EDITED symlink target
/// yields no mismatch. Pins that linked content is outside verify even as the
/// live working tree changes.
#[cfg(unix)]
#[test]
fn verify_skips_linked_record_over_edited_symlink_target() {
    use std::os::unix::fs::symlink;

    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = verify_config(&td, &fx);

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    std::fs::create_dir_all(dst.parent().expect("dst parent")).expect("mkdir dst parent");
    let live = fx.src.path().join("editor");
    symlink(&live, &dst).expect("deploy the linked artifact as a symlink");

    let linked = RegistryRecord {
        version: 1,
        key: artifact_key("dest", "editor-src", "editor"),
        source: "editor-src".to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: "2026-06-08T12:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![],
        linked: true,
        vars_digest: None,
    };
    fx.registry.put(&linked).expect("seed linked record");

    // Edit the live working-tree target through which the symlink resolves.
    std::fs::write(live.join("init.lua"), b"-- EDITED LIVE\n")
        .expect("edit the symlink target content");

    let mismatches = verify(&cfg, &fx.registry).expect("verify must not error");

    assert!(
        !mismatches
            .iter()
            .any(|m| m.key == artifact_key("dest", "editor-src", "editor")),
        "a linked artifact is outside the content-integrity model: editing the live symlink \
             target must NOT surface as a verify mismatch, got {mismatches:?}"
    );
}

/// Guard: after `rebuild_registry` over a link source whose artifact is deployed
/// as a symlink, the record is `linked` and the deployed symlink is NOT reported
/// `foreign`. Pins `scan_foreign`'s no-follow (`file_type().is_dir()`) behavior.
#[cfg(unix)]
#[test]
fn rebuild_keeps_linked_and_excludes_symlink_from_foreign() {
    use std::os::unix::fs::symlink;

    let wt = build_worktree(None);
    let td = TargetDir::new();
    let cfg = config_link_source_one_target("linked-src", wt.path(), &td.target_path());
    let lock = link_lock("linked-src", wt.path());

    // Deploy `alpha` as a symlink at the by-source destination, as link-mode does.
    let by_source = crate::config::LayoutConfig {
        kind: LayoutKind::BySource,
        separator: String::new(),
    };
    let dst = td.artifact_dst(&by_source, "linked-src", "alpha");
    std::fs::create_dir_all(dst.parent().expect("dst parent")).expect("mkdir dst parent");
    symlink(wt.path().join("alpha"), &dst).expect("deploy the alpha artifact as a symlink");

    let registry = fx_registry();
    let report = rebuild_registry(&cfg, &lock, &fx_backend(), &registry)
        .expect("rebuild over a link source must succeed");

    let record = registry
        .get(&artifact_key("dest", "linked-src", "alpha"))
        .expect("registry read")
        .expect("rebuild must reconstruct the alpha record");
    assert!(
        record.linked,
        "rebuild must reconstruct a Link source's record as linked=true (no hashing), \
             got linked={}",
        record.linked
    );

    assert!(
        !report
            .foreign
            .iter()
            .any(|p| p == &dst || p.ends_with("alpha")),
        "the deployed linked SYMLINK must NOT be classified foreign — scan_foreign's \
             no-follow is_dir() skips it; got {:?}",
        report.foreign
    );
}

/// Guard: `prune_orphans` removes a stale linked symlink by unlinking the symlink
/// ONLY — it must never follow the link to `remove_dir_all` the target. Asserts the
/// symlink target (the working-tree dir + a file inside) survives the prune.
#[cfg(unix)]
#[test]
fn prune_removes_stale_linked_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let wt = build_worktree(None);
    let td = TargetDir::new();
    // The config discovers wt's dirs (alpha/zeta/uncommitted); `stale` is NOT among
    // them, so its registry record is orphaned and must be pruned.
    let cfg = config_link_source_one_target("linked-src", wt.path(), &td.target_path());

    let target_dir = wt.path().join("alpha");
    let inside = target_dir.join("file.txt");
    assert!(
        inside.exists(),
        "premise: the symlink target file must exist pre-prune"
    );

    let by_source = crate::config::LayoutConfig {
        kind: LayoutKind::BySource,
        separator: String::new(),
    };
    let dst = td.artifact_dst(&by_source, "linked-src", "stale");
    std::fs::create_dir_all(dst.parent().expect("dst parent")).expect("mkdir dst parent");
    symlink(&target_dir, &dst).expect("plant a stale linked symlink at the destination");

    let registry = fx_registry();
    registry
        .put(&RegistryRecord {
            version: 1,
            key: artifact_key("dest", "linked-src", "stale"),
            source: "linked-src".to_owned(),
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "by-source".to_owned(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: true,
            vars_digest: None,
        })
        .expect("seed the orphaned linked record");

    let parsed = cfg.parsed_sources().expect("sources parse");
    let commits = one_commit(&parsed, "linked-src", "link");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let protected = test_protected(&std::env::temp_dir());
    prune_orphans(
        &cfg,
        &parsed,
        &remotes,
        &fx_backend(),
        &registry,
        &commits,
        &protected,
    )
    .expect("prune must remove the orphaned linked artifact");

    assert!(
        std::fs::symlink_metadata(&dst).is_err(),
        "prune must unlink the stale symlink at {}; it must no longer exist",
        dst.display()
    );
    assert!(
        target_dir.is_dir() && inside.exists(),
        "prune must unlink the symlink ONLY (no-follow) — the working-tree target dir and the \
             file inside it must survive, never removed through the link"
    );
    assert!(
        registry
            .get(&artifact_key("dest", "linked-src", "stale"))
            .expect("registry read")
            .is_none(),
        "the orphaned linked record must be removed from the registry"
    );
}

/// A real `GitBackend` over a throwaway mirror dir; link-source rebuild/prune
/// never reach it, but the signature requires a backend.
fn fx_backend() -> GitBackend {
    let git_dir = TempDir::new().expect("backend mirror dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    std::mem::forget(git_dir);
    backend
}

// ── DLD-002: link-mode guardrails (sync-layer enforcement) ─────

/// A source with NO `deploy` line and no target, ready to receive a local
/// `deploy = "link"` overlay.
fn base_copy_source(source: &str, git: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n[sources.{source}]\ngit = \"{}\"\nbranch = \"main\"\n",
        git.display(),
    );
    Config::parse(&toml).expect("base source without deploy parses")
}

/// A local overlay setting `deploy = "link"` on `source`, pointing at a target.
fn local_link_overlay(source: &str, git: &Path, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \"{}\"\nbranch = \"main\"\ndeploy = \"link\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"by-source\"\n",
        git.display(),
        target_path.display(),
    );
    Config::parse(&toml).expect("local link overlay parses")
}

/// Mirrors how `sync` derives the effective config from base + overlay, so a
/// guard keyed on provenance (base vs effective) is exercised as in production.
fn effective_of(base: &Config, local: &Config) -> Config {
    merge_configs(base.clone(), Some(local.clone()))
}

#[test]
fn base_link_over_local_path_now_passes_the_guard() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    // deploy = link committed in the BASE phora.toml over a LOCAL path. The
    // committed-config rejection has been removed: a base-defined link over a
    // local filesystem path is now a SUPPORTED configuration and must validate.
    let base = config_link_source_one_target("base-linked", wt.path(), &td.target_path());

    let base_parsed = base.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&base, &base_parsed).expect("remotes resolve");
    let warnings = validate_link_mode(&base, &base_parsed, &remotes).expect(
        "deploy = \"link\" committed in the base config over a LOCAL path must now be \
             accepted, not rejected as a committed-config violation",
    );
    assert!(
        warnings.iter().all(|w| w.contains("base-linked")),
        "any portability warning emitted must name the source `base-linked`, got: {warnings:?}"
    );
}

#[test]
fn remote_git_with_link_is_rejected_naming_the_source() {
    let td = TargetDir::new();
    // Link comes ONLY from the overlay (base has no link), but git is a remote
    // https URL: the base-overlay guard passes, so ONLY the local-path guard
    // can reject this — isolating it.
    let remote = std::path::Path::new("https://github.com/me/dotfiles.git");
    let base = base_copy_source("remote-linked", remote);
    let local = local_link_overlay("remote-linked", remote, &td.target_path());
    let effective = effective_of(&base, &local);
    assert_eq!(
        parsed_of(&effective, "remote-linked").deploy_mode(),
        DeployMode::Link,
        "premise: the overlay must make the effective mode Link"
    );

    let parsed = effective.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&effective, &parsed).expect("remotes resolve");
    let Err(err) = validate_link_mode(&base, &parsed, &remotes) else {
        panic!(
            "deploy = \"link\" on a remote-URL git must be rejected (a remote has no \
                 working tree to symlink), even when the link is overlay-only"
        );
    };
    assert!(
        err.to_string().contains("remote-linked"),
        "the local-path guard error must name the offending source `remote-linked`, got: {err}"
    );
}

#[test]
fn remote_link_in_base_config_still_errors_naming_the_source() {
    let td = TargetDir::new();
    let remote = std::path::Path::new("https://github.com/me/dotfiles.git");
    let base = config_link_source_one_target("base-remote-link", remote, &td.target_path());

    let parsed = base.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&base, &parsed).expect("remotes resolve");
    let Err(err) = validate_link_mode(&base, &parsed, &remotes) else {
        panic!(
            "deploy = \"link\" on a remote-URL git must be rejected even when committed in \
                 the base config; a remote has no working tree to symlink"
        );
    };
    assert!(
        err.to_string().contains("base-remote-link")
            && err.to_string().contains("local filesystem path"),
        "the local-path guard error must name the source and steer to a local path, got: {err}"
    );
}

/// A local overlay downgrading `source` to `deploy = "copy"`, pointing at a target.
fn local_copy_overlay(source: &str, git: &Path, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \"{}\"\nbranch = \"main\"\ndeploy = \"copy\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"by-source\"\n",
        git.display(),
        target_path.display(),
    );
    Config::parse(&toml).expect("local copy overlay parses")
}

#[test]
fn base_link_downgraded_to_copy_locally_passes_with_no_warning() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    let base = config_link_source_one_target("committed-link", wt.path(), &td.target_path());
    let local = local_copy_overlay("committed-link", wt.path(), &td.target_path());
    let effective = effective_of(&base, &local);
    assert_eq!(
        parsed_of(&effective, "committed-link").deploy_mode(),
        DeployMode::Copy,
        "premise: the overlay must downgrade the effective mode to Copy"
    );

    let parsed = effective.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&effective, &parsed).expect("remotes resolve");
    let warnings = validate_link_mode(&base, &parsed, &remotes)
        .expect("a base link locally downgraded to copy must pass validation, not error");
    assert!(
        warnings.is_empty(),
        "no portability warning may fire when the EFFECTIVE mode is Copy, even though \
             the source is base-defined over an absolute path, got: {warnings:?}"
    );
}

#[test]
fn link_only_in_local_overlay_passes_the_guard() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    // Link confined to the overlay over a LOCAL path: both guards must pass.
    let base = base_copy_source("dev-src", wt.path());
    let local = local_link_overlay("dev-src", wt.path(), &td.target_path());
    let effective = effective_of(&base, &local);
    assert_eq!(
        parsed_of(&effective, "dev-src").deploy_mode(),
        DeployMode::Link,
        "premise: the overlay must make the effective mode Link"
    );

    let parsed = effective.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&effective, &parsed).expect("remotes resolve");
    let _warnings = validate_link_mode(&base, &parsed, &remotes).expect(
        "a link confined to phora.local.toml over a local path must pass the local-path guard",
    );
}

/// Negative-provenance control: a base with no sources, so a base-defined-source trigger flips off.
fn empty_base() -> Config {
    Config::parse("version = 1\n").expect("empty base parses")
}

#[test]
fn base_defined_link_over_absolute_path_warns_exactly_once() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    assert!(
        wt.path().is_absolute(),
        "premise: the link source path must be absolute to trigger the portability warning"
    );
    let base = config_link_source_one_target("abs-link", wt.path(), &td.target_path());

    let parsed = base.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&base, &parsed).expect("remotes resolve");
    let warnings = validate_link_mode(&base, &parsed, &remotes)
        .expect("a base link over an absolute local path is valid (non-fatal warning only)");
    assert_eq!(
        warnings.len(),
        1,
        "a base-defined link over an absolute path must emit EXACTLY one portability \
             warning (no double-emit across the two validation passes), got: {warnings:?}"
    );
    assert!(
        warnings[0].contains("abs-link"),
        "the portability warning must name the source `abs-link`, got: {:?}",
        warnings[0]
    );

    let control = validate_link_mode(&empty_base(), &parsed, &remotes)
        .expect("the same effective link is valid against an empty base too");
    assert!(
        control.is_empty(),
        "distinguishing trace: with the SAME effective link but no base-defined source, \
             the warning must NOT fire — proving the trigger is base provenance, not incidental \
             Vec length, got: {control:?}"
    );
}

#[test]
fn local_overlay_adds_link_to_base_source_warns_exactly_once() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    assert!(
        wt.path().is_absolute(),
        "premise: the source path must be absolute to trigger the portability warning"
    );
    let base = base_copy_source("overlay-link", wt.path());
    let local = local_link_overlay("overlay-link", wt.path(), &td.target_path());
    let effective = effective_of(&base, &local);
    assert_eq!(
        parsed_of(&effective, "overlay-link").deploy_mode(),
        DeployMode::Link,
        "premise: the overlay must make the effective mode Link"
    );

    let parsed = effective.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&effective, &parsed).expect("remotes resolve");
    let warnings = validate_link_mode(&base, &parsed, &remotes)
        .expect("link added by the overlay onto a base-defined local source is valid");
    assert_eq!(
        warnings.len(),
        1,
        "a base-DEFINED source (even with deploy == None) whose effective mode is Link \
             over an absolute path must warn exactly once, got: {warnings:?}"
    );
    assert!(
        warnings[0].contains("overlay-link"),
        "the portability warning must name the source `overlay-link`, got: {:?}",
        warnings[0]
    );

    let control = validate_link_mode(&empty_base(), &parsed, &remotes)
        .expect("same effective link is valid against an empty base");
    assert!(
        control.is_empty(),
        "distinguishing trace: drop the source from the base and the warning must vanish, \
             pinning base-defined-source-presence as the trigger, got: {control:?}"
    );
}

#[test]
fn link_source_only_in_local_overlay_does_not_warn() {
    let wt = build_worktree(None);
    let td = TargetDir::new();
    assert!(
        wt.path().is_absolute(),
        "premise: an absolute path would warn IF the source were base-defined; it is not"
    );
    let base = empty_base();
    let local = local_link_overlay("local-only-link", wt.path(), &td.target_path());
    let effective = effective_of(&base, &local);
    assert_eq!(
        parsed_of(&effective, "local-only-link").deploy_mode(),
        DeployMode::Link,
        "premise: the effective mode is Link over an absolute path"
    );
    assert!(
        !base.sources.contains_key("local-only-link"),
        "premise: the source must NOT be base-defined"
    );

    let parsed = effective.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&effective, &parsed).expect("remotes resolve");
    let warnings = validate_link_mode(&base, &parsed, &remotes)
        .expect("a link source confined to the overlay over a local path is valid");
    assert!(
        warnings.is_empty(),
        "a source defined ONLY in phora.local.toml is intentional and machine-specific; \
             it must emit NO portability warning even over an absolute path, got: {warnings:?}"
    );

    let provenance_flip = validate_link_mode(&local, &parsed, &remotes)
        .expect("same effective link is valid against a base that DOES define the source");
    assert_eq!(
        provenance_flip.len(),
        1,
        "distinguishing trace: feed a base that DOES define the source and the warning \
             appears — confirming the empty-base result is provenance-driven, got: {provenance_flip:?}"
    );
}

/// A base config with a `deploy = "link"` source whose `git` is the literal
/// `"."` — a LOCAL path (`Path::new(".").exists()`) that is NOT absolute. The
/// target lives in the same config so the effective mode is Link.
fn config_link_source_dot_git(source: &str, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \".\"\nbranch = \"main\"\ndeploy = \"link\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"by-source\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("dot-git link-source config parses")
}

#[test]
fn base_link_over_non_absolute_local_path_emits_no_warning() {
    let td = TargetDir::new();
    // `git = "."` resolves to the local-but-non-absolute remote `"."` (passes the
    // remote-rejection guard, yet is_absolute() is false) — the warning gate under test.
    let base = config_link_source_dot_git("dot-link", &td.target_path());

    let parsed = base.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&base, &parsed).expect("remotes resolve");
    assert!(
        !std::path::Path::new(&remotes["dot-link"]).is_absolute(),
        "premise: the resolved remote for `git = \".\"` must be NON-absolute so this \
             test exercises the is_absolute == false branch, got: {:?}",
        remotes["dot-link"]
    );
    assert_eq!(
        parsed_of(&base, "dot-link").deploy_mode(),
        DeployMode::Link,
        "premise: the effective deploy mode must be Link"
    );

    let warnings = validate_link_mode(&base, &parsed, &remotes)
        .expect("a base-defined link over a LOCAL non-absolute path is valid and non-fatal");
    assert!(
        warnings.is_empty(),
        "the portability warning is gated on an ABSOLUTE path; a base-defined link over a \
             NON-absolute local path must emit ZERO warnings, got: {warnings:?}"
    );

    let wt = build_worktree(None);
    assert!(
        wt.path().is_absolute(),
        "premise: control path must be absolute"
    );
    let abs_base = config_link_source_one_target("dot-link", wt.path(), &td.target_path());
    let abs_parsed = abs_base.parsed_sources().expect("sources parse");
    let abs_remotes = resolved_remotes(&abs_base, &abs_parsed).expect("remotes resolve");
    let abs_warnings = validate_link_mode(&abs_base, &abs_parsed, &abs_remotes)
        .expect("a base link over an absolute local path is valid (warning only)");
    assert_eq!(
        abs_warnings.len(),
        1,
        "distinguishing trace: flip the SAME base-defined link source to an ABSOLUTE path \
             and the warning fires exactly once, pinning is_absolute as the gate, got: {abs_warnings:?}"
    );
}

// ── DLD-004: link deployment (atomic symlink, dispatched at deploy) ─

/// A base config carrying `source` as a plain copy source (no `deploy`), with
/// no target. The link + target live only in the local overlay.
fn base_link_overlay_pair(source: &str, git: &Path, target_path: &Path) -> (Config, Config) {
    let base = base_copy_source(source, git);
    let local = local_link_overlay(source, git, target_path);
    (base, local)
}

/// Drives a full `sync` of a single local-path link source (confined to the
/// overlay) into one `by-source` target. Returns the sync output plus the
/// destination dir the artifact should symlink at and the absolute working-tree
/// target it should point to.
fn link_dst(target_path: &Path, source: &str, artifact: &str) -> PathBuf {
    let layout = crate::config::LayoutConfig {
        kind: LayoutKind::BySource,
        separator: String::new(),
    };
    target_path.join(layout.artifact_path(source, artifact))
}

#[test]
fn linked_artifact_deploys_as_symlink_to_working_tree() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());
    let in_ = input(&base, Some(&local), None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("link-source sync runs to deploy");
    assert!(
        !out.had_failures,
        "deploying a link source over a real local working tree must not fail"
    );

    let dst = link_dst(&td.target_path(), "dev-src", "editor");
    let meta = std::fs::symlink_metadata(&dst)
        .expect("the linked artifact must materialize at the destination");
    assert!(
        meta.file_type().is_symlink(),
        "a link-mode artifact must be deployed as a SYMLINK, not a copied directory"
    );

    let want = fx.src.path().join("editor");
    let got = std::fs::read_link(&dst).expect("the deployed symlink must be readable");
    assert_eq!(
        got, want,
        "the symlink must point to the ABSOLUTE working-tree path <source>/<artifact>"
    );

    // Edit-through: a new source-tree file is visible through the symlink with no re-sync.
    std::fs::write(fx.src.path().join("editor/live.lua"), b"-- live\n")
        .expect("write a new file into the source working tree");
    assert_eq!(
        std::fs::read(dst.join("live.lua")).expect("new file visible through the symlink"),
        b"-- live\n",
        "editing the source working tree must be visible through the destination symlink \
             without re-syncing"
    );

    let key = artifact_key("dest", "dev-src", "editor");
    let record = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("a record must be written for the linked artifact");
    assert!(
        record.linked,
        "the deployed registry record for a link artifact must be a LINKED record (linked=true)"
    );
}

#[test]
fn linked_record_is_linked_not_copy() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());
    let in_ = input(&base, Some(&local), None, None, false);

    sync(&in_, &fx.backend, &fx.registry).expect("link-source sync runs to deploy");

    let key = artifact_key("dest", "dev-src", "editor");
    let record = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("a record must be written for the linked artifact");
    assert!(record.linked, "linked record must carry linked=true");
    assert!(
        record.files.is_empty(),
        "a linked record carries no per-file manifest (it is outside the integrity model)"
    );
    assert_eq!(
        record.commit, "link",
        "a linked record uses the sentinel commit, not a copied source commit"
    );
    assert_eq!(
        record.digest, "link:",
        "a linked record uses the sentinel digest, not a copied content digest"
    );
}

#[cfg(unix)]
#[test]
fn symlink_failure_warns_skips_and_continues() {
    use std::os::unix::fs::PermissionsExt;

    let td = TargetDir::new();
    // One artifact dir per repo, so each source maps to its own by-source parent.
    let (blocked_src, blocked_git) = build_named_artifact_repo("alpha", "a.txt", b"alpha\n");
    let (ok_src, ok_git) = build_named_artifact_repo("beta", "b.txt", b"beta\n");
    let (copy_src, copy_git) = build_named_artifact_repo("widget", "w.txt", b"widget\n");
    let mirror_dir = TempDir::new().expect("backend mirror dir");
    let backend = GitBackend::new(mirror_dir.path().to_path_buf());
    let registry = fx_registry();

    let base = {
        let toml = format!(
            "version = 1\n\n\
                 [sources.dev-blocked]\ngit = \"{blocked_git}\"\nbranch = \"main\"\n\n\
                 [sources.dev-ok]\ngit = \"{ok_git}\"\nbranch = \"main\"\n\n\
                 [sources.copy-src]\ngit = \"{copy_git}\"\nbranch = \"main\"\n",
        );
        Config::parse(&toml).expect("base with link + copy sources parses")
    };
    let local = {
        let toml = format!(
            "version = 1\n\n\
                 [sources.dev-blocked]\ngit = \"{blocked_git}\"\nbranch = \"main\"\n\
                 deploy = \"link\"\n\n\
                 [sources.dev-ok]\ngit = \"{ok_git}\"\nbranch = \"main\"\ndeploy = \"link\"\n\n\
                 [targets.dest]\npath = \"{}\"\n\
                 sources = [\"dev-blocked\", \"dev-ok\", \"copy-src\"]\nlayout = \"by-source\"\n",
            td.target_path().display(),
        );
        Config::parse(&toml).expect("overlay setting links + target parses")
    };

    // Read-only parent fails the temp-symlink create inside it (EACCES) — landing in
    // the deploy closure's Err path — while try_exists(dst) still reads clean, so
    // check_artifact_state does not abort. Siblings keep their own writable parents.
    let blocked_parent = td.target_path().join("dev-blocked");
    std::fs::create_dir_all(&blocked_parent).expect("create blocked link parent dir");
    std::fs::set_permissions(&blocked_parent, std::fs::Permissions::from_mode(0o555))
        .expect("make the blocked link parent read-only");

    let in_ = input(&base, Some(&local), None, None, false);
    let out = sync(&in_, &backend, &registry).expect("sync must not abort on a link failure");

    std::fs::set_permissions(&blocked_parent, std::fs::Permissions::from_mode(0o755))
        .expect("restore perms so the tempdir can be cleaned up");

    assert!(
        out.had_failures,
        "a failed link deploy must set had_failures (warn-and-continue, not abort)"
    );

    let copy_dst = link_dst(&td.target_path(), "copy-src", "widget");
    assert_eq!(
        std::fs::read(copy_dst.join("w.txt")).expect("copy artifact deployed"),
        b"widget\n",
        "a sibling COPY artifact in the same target must deploy fine despite the link failure"
    );

    let ok_dst = link_dst(&td.target_path(), "dev-ok", "beta");
    let ok_meta = std::fs::symlink_metadata(&ok_dst)
        .expect("the healthy link source must still deploy despite the sibling failure");
    assert!(
        ok_meta.file_type().is_symlink(),
        "a healthy link artifact must deploy as a symlink even when a sibling link fails"
    );
    assert_eq!(
        std::fs::read_link(&ok_dst).expect("healthy symlink readable"),
        ok_src.path().join("beta"),
        "the healthy link must point at its absolute working-tree target"
    );
    drop((blocked_src, ok_src, copy_src));
}

// Pins the crash-orphan reclaim contract: `.phora-link-*` staging left in the dst
// dir by a hard-killed run must not wedge a fresh link deploy. link_nonce is a
// process-global counter (shared across tests), so plant a contiguous span to
// collide with whatever index this deploy picks; the fix relocates staging under
// the recovery_sweep-cleared base, making these orphans irrelevant.
#[cfg(unix)]
#[test]
fn orphaned_link_staging_does_not_wedge_next_deploy() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());

    let dst = link_dst(&td.target_path(), "dev-src", "editor");
    let parent = dst.parent().expect("dst has a parent");
    std::fs::create_dir_all(parent).expect("create dst parent");

    for n in 0..4096 {
        let orphan = parent.join(format!(".phora-link-{n}"));
        std::os::unix::fs::symlink(fx.src.path(), &orphan)
            .expect("plant a crash-orphan staging symlink");
    }

    let in_ = input(&base, Some(&local), None, None, false);
    let out = sync(&in_, &fx.backend, &fx.registry).expect("sync runs despite link orphans");

    assert!(
        !out.had_failures,
        "a `.phora-link-*` crash orphan must not wedge a fresh link deploy"
    );

    let meta = std::fs::symlink_metadata(&dst)
        .expect("the linked artifact must still materialize despite the orphans");
    assert!(
        meta.file_type().is_symlink(),
        "the deploy must produce a SYMLINK, not fail with EEXIST against an orphan"
    );
    assert_eq!(
        std::fs::read_link(&dst).expect("the deployed symlink must be readable"),
        fx.src.path().join("editor"),
        "the deploy must point at the correct absolute <source>/<artifact> target"
    );
}

#[test]
fn stale_link_with_wrong_target_is_repointed() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());

    // A wrong-target symlink with no registry record reads Foreign; force overwrites,
    // exercising the atomic re-point (a Linked record present would be a no-op per DLD-005).
    let dst = link_dst(&td.target_path(), "dev-src", "editor");
    std::fs::create_dir_all(dst.parent().expect("dst has a parent")).expect("create dst parent");
    let wrong = fx.src.path().join("docs");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&wrong, &dst).expect("plant a wrong-target symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&wrong, &dst).expect("plant a wrong-target symlink");

    assert_eq!(
        std::fs::read_link(&dst).expect("stale symlink readable"),
        wrong,
        "premise: the destination starts as a symlink to the WRONG target"
    );

    let in_ = input(&base, Some(&local), None, None, true);
    sync(&in_, &fx.backend, &fx.registry).expect("forced sync re-points the stale link");

    let meta = std::fs::symlink_metadata(&dst).expect("dst still present after re-point");
    assert!(
        meta.file_type().is_symlink(),
        "after re-point the destination must remain a symlink, never a half state"
    );
    assert_eq!(
        std::fs::read_link(&dst).expect("re-pointed symlink readable"),
        fx.src.path().join("editor"),
        "a stale link with the wrong target must be re-pointed to the CORRECT absolute target"
    );
}

// ── DLD-007: deploy-mode transitions (link <-> copy) ───────────
//
// Both legs key the SAME source name (`dev-src`) into the SAME registry and one
// by-source target, so the dst path is identical across runs; only the effective
// `deploy` flips. The copy leg exports from the ODB via `fx.url`; the link leg
// takes the same path as its `git`, symlinking the live working tree. `fx.url`
// and `fx.src.path()` are the same path — so the only on-disk difference between
// legs is the dst TYPE (real dir vs symlink), which is what these tests assert.

/// A by-source copy config for `dev-src` sharing the link leg's dst path.
fn by_source_copy_config(url: &str, target_path: &Path) -> Config {
    config_one_source_one_target("dev-src", url, "dest", target_path, "by-source")
}

#[cfg(unix)]
#[test]
fn transition_copy_to_link_materializes_symlink() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let dst = link_dst(&td.target_path(), "dev-src", "editor");

    // Leg 1: copy. dst becomes a REAL DIR; record linked=false; verify passes.
    let copy_cfg = by_source_copy_config(&fx.url, &td.target_path());
    let copy_in = input(&copy_cfg, None, None, None, false);
    let out = sync(&copy_in, &fx.backend, &fx.registry).expect("copy leg syncs");
    assert!(!out.had_failures, "the copy leg must deploy cleanly");

    let copy_meta = std::fs::symlink_metadata(&dst).expect("copy dst present");
    assert!(
        copy_meta.file_type().is_dir() && !copy_meta.file_type().is_symlink(),
        "premise: after a copy sync the dst is a REAL directory, not a symlink"
    );
    let key = artifact_key("dest", "dev-src", "editor");
    let copy_rec = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("copy leg writes a record");
    assert!(!copy_rec.linked, "premise: a copy record is linked=false");
    let copy_mismatches = verify(&copy_cfg, &fx.registry).expect("verify must not error");
    assert!(
        !copy_mismatches.iter().any(|m| m.key == key),
        "premise: the materialized copy must verify clean, got {copy_mismatches:?}"
    );

    // Leg 2: link. The SAME source resynced with deploy=link must REDEPLOY the
    // dst as a symlink to the absolute working tree (transition, not no-op).
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());
    let link_in = input(&base, Some(&local), None, None, false);
    let out = sync(&link_in, &fx.backend, &fx.registry).expect("link leg syncs");
    assert!(!out.had_failures, "the link transition must not fail");

    let link_meta = std::fs::symlink_metadata(&dst).expect("link dst present");
    assert!(
        link_meta.file_type().is_symlink(),
        "copy->link transition must REDEPLOY the dst as a SYMLINK, not leave the real copy in \
             place (an intact copy reads Clean and the no-deploy guard would no-op)"
    );
    assert_eq!(
        std::fs::read_link(&dst).expect("transitioned symlink readable"),
        fx.src.path().join("editor"),
        "the materialized symlink must point at the absolute working-tree <source>/<artifact>"
    );

    let link_rec = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("link leg rewrites the record");
    assert!(
        link_rec.linked,
        "after copy->link the record must flip to a LINKED record (linked=true)"
    );

    let effective = effective_of(&base, &local);
    let link_mismatches = verify(&effective, &fx.registry).expect("verify must not error");
    assert!(
        !link_mismatches.iter().any(|m| m.key == key),
        "a linked artifact is quarantined from verify; the transition must surface no \
             mismatch, got {link_mismatches:?}"
    );
}

#[cfg(unix)]
#[test]
fn transition_link_to_copy_materializes_real_copy() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let dst = link_dst(&td.target_path(), "dev-src", "editor");
    let key = artifact_key("dest", "dev-src", "editor");

    // Leg 1: link. dst becomes a SYMLINK; record linked=true.
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());
    let link_in = input(&base, Some(&local), None, None, false);
    let out = sync(&link_in, &fx.backend, &fx.registry).expect("link leg syncs");
    assert!(!out.had_failures, "the link leg must deploy cleanly");

    let link_meta = std::fs::symlink_metadata(&dst).expect("link dst present");
    assert!(
        link_meta.file_type().is_symlink(),
        "premise: after a link sync the dst is a symlink"
    );
    let link_rec = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("link leg writes a record");
    assert!(link_rec.linked, "premise: a link record is linked=true");

    // Leg 2: copy. The SAME source resynced with deploy=copy must REDEPLOY the
    // dst as a real materialized copy (transition, not the symlink no-op a
    // Linked record short-circuits to).
    let copy_cfg = by_source_copy_config(&fx.url, &td.target_path());
    let copy_in = input(&copy_cfg, None, None, None, false);
    let out = sync(&copy_in, &fx.backend, &fx.registry).expect("copy leg syncs");
    assert!(!out.had_failures, "the link->copy transition must not fail");

    let copy_meta = std::fs::symlink_metadata(&dst).expect("copy dst present");
    assert!(
        copy_meta.file_type().is_dir() && !copy_meta.file_type().is_symlink(),
        "link->copy transition must REDEPLOY the dst as a REAL directory (NOT a symlink); a \
             Linked record short-circuits to a no-op without this"
    );
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("materialized init.lua present"),
        b"-- init\n",
        "the materialized copy must contain the artifact's exported files on disk"
    );

    let copy_rec = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("copy leg rewrites the record");
    assert!(
        !copy_rec.linked,
        "after link->copy the record must flip to a copy record (linked=false)"
    );
    assert!(
        copy_rec
            .files
            .iter()
            .any(|f| f.path == *Path::new("init.lua")),
        "the materialized copy record must carry a per-file manifest (integrity restored), \
             got {:?}",
        copy_rec.files
    );

    let copy_mismatches = verify(&copy_cfg, &fx.registry).expect("verify must not error");
    assert!(
        !copy_mismatches.iter().any(|m| m.key == key),
        "full per-file integrity must be restored: verify over the materialized copy must \
             report no mismatch, got {copy_mismatches:?}"
    );
}

/// DLD-005-M1: a linked record whose dst was externally replaced by a real dir
/// must REDEPLOY the symlink, not no-op. `check_artifact_state` returns Linked on
/// `record.linked` alone (ignoring dst type), so the orchestration must notice the
/// effective-mode/dst-type disagreement and overwrite.
#[cfg(unix)]
#[test]
fn linked_record_over_real_dir_redeploys_symlink() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let dst = link_dst(&td.target_path(), "dev-src", "editor");
    let key = artifact_key("dest", "dev-src", "editor");

    // Deploy the link once: dst is a symlink, record linked=true.
    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());
    let in_ = input(&base, Some(&local), None, None, false);
    let out = sync(&in_, &fx.backend, &fx.registry).expect("link leg syncs");
    assert!(!out.had_failures, "the initial link deploy must not fail");
    assert!(
        std::fs::symlink_metadata(&dst)
            .expect("link dst present")
            .file_type()
            .is_symlink(),
        "premise: the artifact is first deployed as a symlink"
    );

    // Simulate external mutation: replace the symlink with a real directory.
    std::fs::remove_file(&dst).expect("remove the deployed symlink");
    std::fs::create_dir_all(&dst).expect("plant a real directory in its place");
    std::fs::write(dst.join("squatter.txt"), b"external\n").expect("write into the real dir");
    assert!(
        std::fs::symlink_metadata(&dst)
            .expect("replaced dst present")
            .file_type()
            .is_dir(),
        "premise: the dst is now a REAL directory (record still says linked=true)"
    );

    // Resync (still deploy=link): the linked-record-over-real-dir must re-link.
    let out = sync(&in_, &fx.backend, &fx.registry).expect("resync runs");
    assert!(!out.had_failures, "the re-link must not fail");

    let meta = std::fs::symlink_metadata(&dst).expect("dst present after resync");
    assert!(
        meta.file_type().is_symlink(),
        "a linked record whose dst is NOT a symlink must REDEPLOY the symlink, not no-op and \
             quarantine the externally-planted directory"
    );
    assert_eq!(
        std::fs::read_link(&dst).expect("re-linked symlink readable"),
        fx.src.path().join("editor"),
        "the re-linked dst must point at the absolute working-tree target"
    );
    let rec = fx
        .registry
        .get(&key)
        .expect("registry read")
        .expect("record present after re-link");
    assert!(rec.linked, "the re-linked record must remain linked=true");
}

/// REGRESSION GUARD (must stay green): a correctly-deployed link, resynced with
/// deploy=link UNCHANGED, is NOT redeployed. A redeploy recreates the symlink via
/// temp-symlink+rename, minting a NEW inode; a true no-op leaves the SAME symlink
/// object. Inode stability is the only observable that catches an always-redeploy —
/// `deploy_link` never exports, so an export count cannot. Guards that the transition
/// fix does not break the linked no-op (H1 idempotence).
#[cfg(unix)]
#[test]
fn correct_link_resync_is_still_a_noop() {
    use std::os::unix::fs::MetadataExt;

    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let dst = link_dst(&td.target_path(), "dev-src", "editor");

    let (base, local) = base_link_overlay_pair("dev-src", fx.src.path(), &td.target_path());
    let in_ = input(&base, Some(&local), None, None, false);
    let out = sync(&in_, &fx.backend, &fx.registry).expect("link leg syncs");
    assert!(!out.had_failures, "the initial link deploy must not fail");
    assert!(
        std::fs::symlink_metadata(&dst)
            .expect("link dst present")
            .file_type()
            .is_symlink(),
        "premise: the artifact is first deployed as a symlink"
    );
    let ino_before = std::fs::symlink_metadata(&dst)
        .expect("link dst present")
        .ino();

    let out = sync(&in_, &fx.backend, &fx.registry).expect("resync runs");
    assert!(
        !out.had_failures,
        "a correct linked re-sync is not a failure"
    );

    let meta = std::fs::symlink_metadata(&dst).expect("dst still present after resync");
    assert!(
        meta.file_type().is_symlink(),
        "the correct symlink must be left intact (not replaced by a redeploy)"
    );
    assert_eq!(
        meta.ino(),
        ino_before,
        "a correct link re-sync must NOT recreate the symlink (idempotent no-op)"
    );
    assert_eq!(
        std::fs::read_link(&dst).expect("symlink readable"),
        fx.src.path().join("editor"),
        "the correct symlink must still point at the same working-tree target"
    );
}

/// Splits a local fixture path into `(parent, basename)` so a host template
/// `<parent>/{path}` filled with `basename` reproduces the literal path verbatim.
#[expect(
    clippy::unwrap_used,
    reason = "fixture paths always have a parent + final component"
)]
fn split_remote_path(url: &str) -> (String, String) {
    let p = Path::new(url);
    let parent = p.parent().unwrap().to_string_lossy().into_owned();
    let base = p.file_name().unwrap().to_string_lossy().into_owned();
    (parent, base)
}

/// A single host+repo forge source resolving against `[hosts.fixturehost]` whose
/// `remote` template fills to the local fixture repo, plus one flat target.
fn config_host_source_one_target(
    source: &str,
    url: &str,
    target: &str,
    target_path: &Path,
) -> Config {
    let (parent, base) = split_remote_path(url);
    let toml = format!(
        "version = 1\n\n\
             [hosts.fixturehost]\nremote = \"{parent}/{{path}}\"\n\n\
             [sources.{source}]\nhost = \"fixturehost\"\nrepo = \"{base}\"\nbranch = \"main\"\n\n\
             [targets.{target}]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"flat\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("host+path one-target config parses")
}

/// A host+path source must fetch, resolve, and deploy IDENTICALLY to its
/// literal-URL twin: same resolved commit and the same deployed artifact bytes.
#[test]
fn host_path_source_syncs_identically_to_its_literal_twin() {
    let fx = build_sync_fixture();

    let td_lit = TargetDir::new();
    let cfg_lit =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td_lit.target_path(), "flat");
    let in_lit = input(&cfg_lit, None, None, None, false);
    let git_dir_lit = TempDir::new().expect("literal git dir");
    let state_dir_lit = TempDir::new().expect("literal state dir");
    let backend_lit = GitBackend::new(git_dir_lit.path().to_path_buf());
    let registry_lit =
        FileRegistry::open(state_dir_lit.path().to_path_buf()).expect("literal registry");
    let out_lit = sync(&in_lit, &backend_lit, &registry_lit).expect("literal-twin sync deploys");

    let td_host = TargetDir::new();
    let cfg_host =
        config_host_source_one_target("editor-src", &fx.url, "dest", &td_host.target_path());
    let in_host = input(&cfg_host, None, None, None, false);
    let git_dir_host = TempDir::new().expect("host git dir");
    let state_dir_host = TempDir::new().expect("host state dir");
    let backend_host = GitBackend::new(git_dir_host.path().to_path_buf());
    let registry_host =
        FileRegistry::open(state_dir_host.path().to_path_buf()).expect("host registry");
    let out_host = sync(&in_host, &backend_host, &registry_host).expect("host+path sync deploys");

    let commit_lit = out_lit
        .base_lock
        .find_source("editor-src")
        .expect("literal twin locked")
        .commit
        .clone();
    let commit_host = out_host
        .base_lock
        .find_source("editor-src")
        .expect("host+path locked")
        .commit
        .clone();
    assert_eq!(
        commit_host, commit_lit,
        "a host+path source must resolve to the SAME commit as its literal twin"
    );
    assert_eq!(
        commit_host, fx.head_sha,
        "the host+path source must resolve branch main to the fixture HEAD"
    );

    let dst_lit = td_lit.artifact_dst(&flat_layout(), "editor-src", "editor");
    let dst_host = td_host.artifact_dst(&flat_layout(), "editor-src", "editor");
    let bytes_lit =
        std::fs::read(dst_lit.join("init.lua")).expect("literal twin deployed init.lua");
    let bytes_host = std::fs::read(dst_host.join("init.lua")).expect("host+path deployed init.lua");
    assert_eq!(
        bytes_host, bytes_lit,
        "a host+path source must deploy the SAME artifact bytes as its literal twin"
    );
    assert_eq!(
        bytes_host, b"-- init\n",
        "the host+path deploy must materialize the fixture's init.lua content"
    );
}

/// Regression: a literal-`git` source still syncs and deploys unchanged.
#[test]
fn literal_git_source_still_syncs_unchanged() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &fx.backend, &fx.registry).expect("literal source still syncs");

    assert_eq!(
        out.base_lock
            .find_source("editor-src")
            .expect("literal source locked")
            .commit,
        fx.head_sha,
        "a literal-git source must keep resolving to the fixture HEAD"
    );
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("literal deployed init.lua"),
        b"-- init\n",
        "a literal-git source must keep deploying its artifact bytes"
    );
}

/// Records every URL each backend op is asked about, so a test can prove which
/// resolved remote the wiring actually fetched (protocol precedence).
struct RecordingBackend<'a> {
    inner: &'a GitBackend,
    urls: Mutex<Vec<String>>,
}

impl<'a> RecordingBackend<'a> {
    fn new(inner: &'a GitBackend) -> Self {
        Self {
            inner,
            urls: Mutex::new(Vec::new()),
        }
    }

    fn fetched_urls(&self) -> Vec<String> {
        self.urls.lock().expect("urls mutex").clone()
    }
}

impl SourceBackend for RecordingBackend<'_> {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.urls.lock().expect("urls mutex").push(url.to_owned());
        self.inner.fetch(source, url)
    }
    fn resolve(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> SourceResult<String> {
        self.inner.resolve(source, url, refspec)
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }
    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

/// A repo at `<parent>/<name>` (name shared across calls) whose `editor/init.lua`
/// holds `content`, returning the parent tempdir + the repo URL.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_repo_named(name: &str, content: &[u8]) -> (TempDir, String) {
    let parent = TempDir::new().unwrap();
    let repo = parent.path().join(name);
    std::fs::create_dir_all(repo.join("editor")).unwrap();
    run_git(&repo, &["init", "-b", "main", "."]);
    run_git(&repo, &["config", "user.email", "test@example.com"]);
    run_git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("editor/init.lua"), content).unwrap();
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-m", "init"]);
    let url = repo.to_string_lossy().into_owned();
    (parent, url)
}

/// With global=https but source protocol=ssh, the source resolves to the SSH
/// repo's HEAD; wrong precedence (global https winning) picks the https HEAD.
#[test]
fn per_source_protocol_beats_global_default() {
    let (https_src, https_url) = build_repo_named("repo", b"-- https\n");
    let (ssh_src, ssh_url) = build_repo_named("repo", b"-- ssh\n");
    let https_head = rev_parse(Path::new(&https_url), "HEAD");
    let ssh_head = rev_parse(Path::new(&ssh_url), "HEAD");
    assert_ne!(
        https_head, ssh_head,
        "premise: the two protocol repos must have distinct HEADs"
    );

    let (https_parent, https_base) = split_remote_path(&https_url);
    let (ssh_parent, ssh_base) = split_remote_path(&ssh_url);
    assert_eq!(
        https_base, ssh_base,
        "premise: both repos must share a basename so one {{path}} value selects either by protocol"
    );

    let td = TargetDir::new();
    let toml = format!(
        "version = 1\nprotocol = \"https\"\n\n\
             [hosts.dual]\nremote = {{ https = \"{https_parent}/{{path}}\", ssh = \"{ssh_parent}/{{path}}\" }}\n\n\
             [sources.editor-src]\nhost = \"dual\"\npath = \"{ssh_base}\"\nprotocol = \"ssh\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("dual-protocol config parses");
    let in_ = input(&cfg, None, None, None, false);

    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let inner = GitBackend::new(git_dir.path().to_path_buf());
    let recording = RecordingBackend::new(&inner);
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("registry");

    let out = sync(&in_, &recording, &registry).expect("dual-protocol sync deploys");

    let commit = out
        .base_lock
        .find_source("editor-src")
        .expect("editor-src locked")
        .commit
        .clone();
    assert_eq!(
        commit, ssh_head,
        "per-source protocol=ssh must win over global https: resolve via the SSH repo's HEAD"
    );
    assert!(
        recording.fetched_urls().iter().any(|u| u == &ssh_url),
        "the wiring must fetch the SSH-resolved remote `{ssh_url}`, got {:?}",
        recording.fetched_urls()
    );

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed init.lua"),
        b"-- ssh\n",
        "the deployed bytes must come from the SSH repo (per-source protocol wins)"
    );

    drop(https_src);
    drop(ssh_src);
}

/// When a source OMITS its own `protocol`, the top-level `Config.protocol`
/// must apply (the MIDDLE rung of `protocol ?? config.protocol ?? Https`).
/// Global=ssh with a silent source must resolve/fetch/deploy via the SSH
/// repo. A wrong impl that ignored config.protocol and used the Https
/// default would resolve to the https repo's HEAD and FAIL this test.
#[test]
fn global_protocol_applies_when_source_omits_protocol() {
    let (https_src, https_url) = build_repo_named("repo", b"-- https\n");
    let (ssh_src, ssh_url) = build_repo_named("repo", b"-- ssh\n");
    let https_head = rev_parse(Path::new(&https_url), "HEAD");
    let ssh_head = rev_parse(Path::new(&ssh_url), "HEAD");
    assert_ne!(
        https_head, ssh_head,
        "premise: the two protocol repos must have distinct HEADs"
    );

    let (https_parent, https_base) = split_remote_path(&https_url);
    let (ssh_parent, ssh_base) = split_remote_path(&ssh_url);
    assert_eq!(
        https_base, ssh_base,
        "premise: both repos must share a basename so one {{path}} value selects either by protocol"
    );

    let td = TargetDir::new();
    let toml = format!(
        "version = 1\nprotocol = \"ssh\"\n\n\
             [hosts.dual]\nremote = {{ https = \"{https_parent}/{{path}}\", ssh = \"{ssh_parent}/{{path}}\" }}\n\n\
             [sources.editor-src]\nhost = \"dual\"\npath = \"{ssh_base}\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("config-default-protocol config parses");
    let in_ = input(&cfg, None, None, None, false);

    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let inner = GitBackend::new(git_dir.path().to_path_buf());
    let recording = RecordingBackend::new(&inner);
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("registry");

    let out = sync(&in_, &recording, &registry).expect("config-default-protocol sync deploys");

    let commit = out
        .base_lock
        .find_source("editor-src")
        .expect("editor-src locked")
        .commit
        .clone();
    assert_eq!(
        commit, ssh_head,
        "a silent source must inherit config.protocol=ssh: resolve via the SSH repo's HEAD, not the Https default"
    );
    assert!(
        recording.fetched_urls().iter().any(|u| u == &ssh_url),
        "the wiring must fetch the config-resolved SSH remote `{ssh_url}`, got {:?}",
        recording.fetched_urls()
    );

    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed init.lua"),
        b"-- ssh\n",
        "the deployed bytes must come from the SSH repo (config.protocol applies to a silent source)"
    );

    drop(https_src);
    drop(ssh_src);
}

// ── HTP-006: url-source lock + content identity through full sync ──

/// Serves a `.tar.gz` over HTTP at a stable url, handing out one queued body per
/// connection (last body repeats once the queue drains), so two syncs of the SAME
/// url can be fed identical or changed bytes while the url identity is unchanged.
struct UrlTarServer {
    url: String,
    _handle: std::thread::JoinHandle<()>,
}

impl UrlTarServer {
    fn spawn(bodies: Vec<Vec<u8>>) -> Self {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port for url server");
        let port = listener.local_addr().expect("local addr").port();
        let url = format!("http://127.0.0.1:{port}/pkg-1.0.tar.gz");
        let handle = std::thread::spawn(move || {
            let mut idx = 0usize;
            while let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = &bodies[idx.min(bodies.len() - 1)];
                idx += 1;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
                let _ = stream.flush();
            }
        });
        Self {
            url,
            _handle: handle,
        }
    }
}

/// A `.tar.gz` holding `editor/init.lua` with the given bytes, so the archive
/// deploys the `editor` artifact through `config_one_source_one_target`.
fn editor_tar_gz(init_lua: &[u8]) -> Vec<u8> {
    use std::io::Write;

    let mut header = tar::Header::new_gnu();
    header.set_size(init_lua.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();

    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_data(&mut header, "pkg-1.0/editor/init.lua", init_lua)
        .expect("append editor/init.lua tar entry");
    let tar_bytes = builder.into_inner().expect("finish tar");

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).expect("gzip tar bytes");
    encoder.finish().expect("finish gzip")
}

/// One-source/one-target config whose single source is a url source.
fn url_config_one_target(source: &str, url: &str, target_path: &Path, layout: &str) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\nurl = \"{url}\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"{layout}\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("url source + target config parses")
}

/// A fetch-counting `RouterBackend<GitBackend, HttpBackend>`: counts every `fetch`
/// so a test can prove the second sync of an unchanged url is a no-op (0 fetches).
struct CountingRouter {
    inner: RouterBackend<GitBackend, HttpBackend>,
    fetches: AtomicUsize,
}

impl CountingRouter {
    fn new(
        git_dir: PathBuf,
        modes: BTreeMap<crate::kernel::SourceName, crate::config::SourceMode>,
    ) -> Self {
        let git = GitBackend::new(git_dir.clone());
        let http = HttpBackend::new(git_dir, BTreeMap::new());
        Self {
            inner: RouterBackend::new(git, http, modes),
            fetches: AtomicUsize::new(0),
        }
    }

    fn fetch_count(&self) -> usize {
        self.fetches.load(AtomicOrdering::SeqCst)
    }
}

impl SourceBackend for CountingRouter {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.fetches.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.fetch(source, url)
    }
    fn resolve(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> SourceResult<String> {
        self.inner.resolve(source, url, refspec)
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }

    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

fn url_modes(source: &str) -> BTreeMap<crate::kernel::SourceName, crate::config::SourceMode> {
    let mut modes = BTreeMap::new();
    modes.insert(sn(source), crate::config::SourceMode::Url);
    modes
}

#[test]
fn second_sync_of_unchanged_url_is_a_noop() {
    let server = UrlTarServer::spawn(vec![editor_tar_gz(b"-- v1\n")]);
    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let backend = CountingRouter::new(git_dir.path().to_path_buf(), url_modes("pkg"));
    let td = TargetDir::new();
    let cfg = url_config_one_target("pkg", &server.url, &td.target_path(), "flat");

    let first = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("first sync of url source deploys");
    assert!(!first.had_failures, "first url sync must succeed");
    assert_eq!(
        backend.fetch_count(),
        1,
        "premise: the first sync of a fresh url source must fetch exactly once"
    );
    assert_eq!(
        first
            .base_lock
            .find_source("pkg")
            .expect("url source in lock")
            .resolved,
        "url",
        "a url source's lock must record the 'url' sentinel in resolved, not the empty Refspec::None string"
    );

    let dst = td.artifact_dst(&flat_layout(), "pkg", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed init.lua"),
        b"-- v1\n",
        "premise: the first sync must deploy the archived editor/init.lua bytes"
    );

    let fetches_after_first = backend.fetch_count();
    let second = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, false),
        &backend,
        &registry,
    )
    .expect("second sync of unchanged url runs cleanly");

    assert!(!second.had_failures, "second clean url sync must not fail");
    // The load-bearing no-op signal is NO RE-DOWNLOAD: fetch (the network+import step) must not run again. Deploy-side verification (compute_digest/export) may still run idempotently and is not counted here.
    assert_eq!(
        backend.fetch_count(),
        fetches_after_first,
        "the second sync of an unchanged url MUST be a no-op: the matching lock must \
             suppress the re-download, so fetch_count must not increase (the bug is that a \
             url lock never matches and re-fetches every time)"
    );
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("init.lua still deployed"),
        b"-- v1\n",
        "files must remain correctly deployed after the no-op second sync"
    );
}

#[test]
fn update_with_changed_url_content_advances_lock() {
    let server = UrlTarServer::spawn(vec![editor_tar_gz(b"-- v1\n"), editor_tar_gz(b"-- v2\n")]);
    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let backend = CountingRouter::new(git_dir.path().to_path_buf(), url_modes("pkg"));
    let td = TargetDir::new();
    let cfg = url_config_one_target("pkg", &server.url, &td.target_path(), "flat");

    let first = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("first sync deploys v1");
    let first_commit = first
        .base_lock
        .find_source("pkg")
        .expect("pkg locked after first sync")
        .commit
        .clone();
    let fetches_after_first = backend.fetch_count();

    let second = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, true),
        &backend,
        &registry,
    )
    .expect("forced second sync re-fetches changed content");
    assert!(!second.had_failures, "second sync must succeed");

    assert!(
        backend.fetch_count() > fetches_after_first,
        "a forced (--force) sync of changed url content MUST re-fetch: fetch_count increases"
    );
    let second_commit = second
        .base_lock
        .find_source("pkg")
        .expect("pkg locked after second sync")
        .commit
        .clone();
    assert_ne!(
        second_commit, first_commit,
        "changed archive bytes must yield a different synthetic commit id: the lock advances"
    );

    let dst = td.artifact_dst(&flat_layout(), "pkg", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed init.lua after advance"),
        b"-- v2\n",
        "advancing the lock must re-deploy the new archive bytes, not merely bump the commit id"
    );
}

#[test]
fn reimport_identical_bytes_does_not_churn() {
    let server = UrlTarServer::spawn(vec![
        editor_tar_gz(b"-- same\n"),
        editor_tar_gz(b"-- same\n"),
    ]);
    let git_dir = TempDir::new().expect("git dir");
    let state_dir = TempDir::new().expect("state dir");
    let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let backend = CountingRouter::new(git_dir.path().to_path_buf(), url_modes("pkg"));
    let td = TargetDir::new();
    let cfg = url_config_one_target("pkg", &server.url, &td.target_path(), "flat");

    let first = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("first sync imports the archive");
    let first_commit = first
        .base_lock
        .find_source("pkg")
        .expect("pkg locked after first sync")
        .commit
        .clone();

    let second = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, true),
        &backend,
        &registry,
    )
    .expect("forced re-import of identical bytes");
    let second_commit = second
        .base_lock
        .find_source("pkg")
        .expect("pkg locked after second sync")
        .commit
        .clone();

    assert_eq!(
        second_commit, first_commit,
        "re-importing identical archive bytes must yield the SAME content-addressed synthetic \
             commit id — no lock churn even when forced to re-fetch"
    );
}

// ── PBR-005: lock / digest split (selection-neutral lock; per-binding digest) ──

/// Byte-identical serialized form of a lock — the spec's "byte-identical phora.lock"
/// is observable as identical TOML, since `Lock` is the on-disk artifact.
fn lock_bytes(lock: &Lock) -> String {
    toml::to_string(lock).expect("lock serializes to toml")
}

/// Two bindings of ONE source (distinct roots) must share exactly ONE lock entry:
/// the lock is per source, selection is per binding.
#[test]
fn two_bindings_of_one_source_share_a_single_lock_entry() {
    let (src, url) = build_multi_root_repo();
    let td = TargetDir::new();
    let (_g, _s, backend, registry) = fresh_backend_registry();
    let cfg = Config::parse(&format!(
        "version = 1\n\n[sources.dots]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = {{\n\
         nvim = {{ source = \"dots\" }},\n\
         tmux = {{ source = \"dots\" }},\n\
         }}\nlayout = \"by-source\"\n",
        td.target_path().display(),
    ))
    .expect("two-binding config parses");

    let out = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("two bindings of one source must sync");

    assert_eq!(
        out.base_lock.sources.len(),
        1,
        "two bindings of one source must collapse to exactly ONE lock entry: the lock keys per \
         source, not per binding"
    );
    assert_eq!(
        out.base_lock
            .find_source("dots")
            .expect("the single lock entry is keyed by the source name")
            .name,
        "dots",
        "the shared lock entry is keyed by the underlying source `dots`, never a binding identity"
    );

    drop(src);
}

/// A `.tar.gz` with TWO top-level slices (`editor/init.lua`, `shell/profile.sh`)
/// under a single `pkg-1.0/` prefix that extraction strips, so the extracted tree
/// is `editor/` + `shell/`. A url source scoped via include/exclude selects only
/// one slice — exposing whether the lock digest stays full-archive (selection-neutral)
/// or leaks the selection.
fn two_slice_tar_gz() -> Vec<u8> {
    use std::io::Write;

    let mut builder = tar::Builder::new(Vec::new());
    for (path, body) in [
        ("pkg-1.0/editor/init.lua", b"-- editor\n".as_slice()),
        ("pkg-1.0/shell/profile.sh", b"# shell\n".as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, path, body)
            .expect("append tar entry");
    }
    let tar_bytes = builder.into_inner().expect("finish tar");

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).expect("gzip tar bytes");
    encoder.finish().expect("finish gzip")
}

/// THE SHARP EDGE: a url source's lock `digest` must become SELECTION-NEUTRAL —
/// the digest of the FULL extracted archive, NOT the include/exclude-filtered subtree.
/// Two url sources over IDENTICAL bytes but DIFFERENT include scopes must lock the
/// SAME digest. Today `resolve_sources` feeds the source's include/exclude into
/// `compute_digest`, so the filtered url digest differs by selection — this is the RED.
#[test]
fn url_source_lock_digest_is_full_archive_regardless_of_selection() {
    let server_a = UrlTarServer::spawn(vec![two_slice_tar_gz()]);
    let server_b = UrlTarServer::spawn(vec![two_slice_tar_gz()]);
    let (_g1, _s1, _b1, registry_a) = fresh_backend_registry();
    let (_g2, _s2, _b2, registry_b) = fresh_backend_registry();
    let git_a = TempDir::new().expect("git dir a");
    let git_b = TempDir::new().expect("git dir b");
    let backend_a = CountingRouter::new(git_a.path().to_path_buf(), url_modes("pkg"));
    let backend_b = CountingRouter::new(git_b.path().to_path_buf(), url_modes("pkg"));
    let td_a = TargetDir::new();
    let td_b = TargetDir::new();

    // Source-level include is the only legal way a url source carries selection today:
    // binding-level slices on url sources are rejected (reject_url_slice), source-level is not.
    let cfg_editor = Config::parse(&format!(
        "version = 1\n\n[sources.pkg]\nurl = \"{}\"\ninclude = [\"editor/**\"]\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"pkg\"]\nlayout = \"by-source\"\n",
        server_a.url,
        td_a.target_path().display(),
    ))
    .expect("editor-scoped url config parses");
    let cfg_shell = Config::parse(&format!(
        "version = 1\n\n[sources.pkg]\nurl = \"{}\"\ninclude = [\"shell/**\"]\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"pkg\"]\nlayout = \"by-source\"\n",
        server_b.url,
        td_b.target_path().display(),
    ))
    .expect("shell-scoped url config parses");

    let out_editor = sync(
        &input(&cfg_editor, None, None, None, false),
        &backend_a,
        &registry_a,
    )
    .expect("editor-scoped url sync");
    let out_shell = sync(
        &input(&cfg_shell, None, None, None, false),
        &backend_b,
        &registry_b,
    )
    .expect("shell-scoped url sync");

    let editor_lock = out_editor
        .base_lock
        .find_source("pkg")
        .expect("editor-scoped url locked");
    let shell_lock = out_shell
        .base_lock
        .find_source("pkg")
        .expect("shell-scoped url locked");

    assert_eq!(
        editor_lock.commit, shell_lock.commit,
        "premise: identical archive bytes give the same content-addressed synthetic commit, so \
         the url commit is already selection-neutral"
    );
    assert_eq!(
        editor_lock.digest, shell_lock.digest,
        "a url source's lock digest must be the FULL extracted archive (selection-neutral): two \
         scopes over identical bytes must lock the same digest. The filtered subtree digest leaks \
         the selection into the lock and must not be used for url sources"
    );
}

/// A url source carrying a source-level selection is still ONE source with ONE lock
/// entry whose digest must be the FULL-archive digest — the selection scopes only what
/// is projected, never the lock. The oracle is the empty-selection digest over the
/// synthetic commit; today the lock records the include-filtered digest (the RED).
#[test]
fn url_source_lock_digest_equals_full_archive_oracle() {
    let server = UrlTarServer::spawn(vec![two_slice_tar_gz()]);
    let git_dir = TempDir::new().expect("git dir");
    let (_g, _s, _b, registry) = fresh_backend_registry();
    let backend = CountingRouter::new(git_dir.path().to_path_buf(), url_modes("pkg"));
    let td = TargetDir::new();
    let cfg = Config::parse(&format!(
        "version = 1\n\n[sources.pkg]\nurl = \"{}\"\ninclude = [\"editor/**\"]\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"pkg\"]\nlayout = \"by-source\"\n",
        server.url,
        td.target_path().display(),
    ))
    .expect("scoped url config parses");

    let out = sync(&input(&cfg, None, None, None, false), &backend, &registry)
        .expect("scoped url source must sync");

    assert_eq!(
        out.base_lock.sources.len(),
        1,
        "one url source must produce exactly ONE lock entry"
    );
    let pkg = out
        .base_lock
        .find_source("pkg")
        .expect("the single url lock entry is keyed by source `pkg`");

    let full_archive = backend
        .compute_digest(
            &sn("pkg"),
            &server.url,
            &pkg.commit,
            None,
            &crate::kernel::Selection::new(&[], &[]).expect("empty selection builds"),
        )
        .expect("full-archive digest computes");
    assert_eq!(
        pkg.digest, full_archive,
        "the url lock digest must be the FULL-archive digest (no root/include/exclude), not the \
         include-filtered subtree digest: selection scopes projection, never the lock"
    );
}

/// A BARE-binding config (no refinement) must produce a lock byte-identical to a
/// plain source-only config — the per-binding feature must not perturb the lock for
/// configs that use no refinements.
#[test]
fn bare_binding_lock_is_byte_identical_to_source_only_config() {
    let (src, url) = build_multi_root_repo();
    let (_g1, _s1, backend_a, registry_a) = fresh_backend_registry();
    let (_g2, _s2, backend_b, registry_b) = fresh_backend_registry();
    let td_a = TargetDir::new();
    let td_b = TargetDir::new();

    // Bare string binding `sources = ["dots"]` vs no target at all: both must lock identically.
    let cfg_bare = Config::parse(&format!(
        "version = 1\n\n[sources.dots]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"dots\"]\nlayout = \"by-source\"\n",
        td_a.target_path().display(),
    ))
    .expect("bare-binding config parses");
    let cfg_plain = Config::parse(&format!(
        "version = 1\n\n[sources.dots]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nlayout = \"by-source\"\n",
        td_b.target_path().display(),
    ))
    .expect("source-only (no explicit bindings) config parses");

    let out_bare = sync(
        &input(&cfg_bare, None, None, None, false),
        &backend_a,
        &registry_a,
    )
    .expect("bare-binding sync");
    let out_plain = sync(
        &input(&cfg_plain, None, None, None, false),
        &backend_b,
        &registry_b,
    )
    .expect("source-only sync");

    assert_eq!(
        lock_bytes(&out_bare.base_lock),
        lock_bytes(&out_plain.base_lock),
        "a bare binding must lock byte-identically to a source-only config: the per-binding \
         feature must not perturb the lock when no refinement is present"
    );

    drop(src);
}

// ── PTV-004+005: lock keyed by (source, resolved commit); per-binding ref ──

struct VersionedFixture {
    _src: TempDir,
    _git_dir: TempDir,
    _state_dir: TempDir,
    backend: GitBackend,
    registry: FileRegistry,
    url: String,
    sha_a: String,
    sha_b: String,
}

/// A repo with two tags at DISTINCT commits, each carrying an `editor/` artifact:
/// `v0.55.0` -> editor/init.lua = "v55", `v0.56.0` -> editor/init.lua = "v56".
/// One source can thus be bound at two versions; the two tags resolve to two SHAs.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_versioned_sync_fixture() -> VersionedFixture {
    let src = TempDir::new().unwrap();
    let p = src.path();

    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);

    std::fs::create_dir_all(p.join("editor")).unwrap();
    std::fs::write(p.join("editor/init.lua"), b"v55\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "v55"]);
    run_git(p, &["tag", "v0.55.0"]);
    let sha_a = rev_parse(p, "HEAD");

    std::fs::write(p.join("editor/init.lua"), b"v56\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "v56"]);
    run_git(p, &["tag", "v0.56.0"]);
    let sha_b = rev_parse(p, "HEAD");

    assert_ne!(
        sha_a, sha_b,
        "fixture premise: the two tags pin distinct commits"
    );

    let git_dir = TempDir::new().unwrap();
    let state_dir = TempDir::new().unwrap();
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let registry =
        FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry over tempdir");
    let url = p.to_string_lossy().into_owned();

    VersionedFixture {
        _src: src,
        _git_dir: git_dir,
        _state_dir: state_dir,
        backend,
        registry,
        url,
        sha_a,
        sha_b,
    }
}

/// Two bindings of one source `fzf` at two distinct tags into one target.
fn config_two_versions(url: &str, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
         [sources.fzf]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = {{\n\
         stable = {{ source = \"fzf\", tag = \"v0.55.0\" }},\n\
         canary = {{ source = \"fzf\", tag = \"v0.56.0\" }},\n\
         }}\nlayout = \"by-source\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("two-version config parses")
}

fn fzf_entries(lock: &Lock) -> Vec<&crate::lock::LockedSource> {
    lock.sources.iter().filter(|s| s.name == "fzf").collect()
}

/// (a) Two distinct (source, commit) lock entries for one twice-bound source.
#[test]
fn two_version_bindings_produce_two_lock_entries_with_distinct_commits() {
    let fx = build_versioned_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_two_versions(&fx.url, &td.target_path());

    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("two-version sync resolves both refs");

    let entries = fzf_entries(&out.base_lock);
    assert_eq!(
        entries.len(),
        2,
        "one source bound at two distinct refs must produce TWO lock entries \
         (one per resolved commit), not one collapsed by source name; got {entries:?}"
    );
    let commits: std::collections::BTreeSet<&str> =
        entries.iter().map(|e| e.commit.as_str()).collect();
    let expected: std::collections::BTreeSet<&str> =
        [fx.sha_a.as_str(), fx.sha_b.as_str()].into_iter().collect();
    assert_eq!(
        commits, expected,
        "the two `fzf` lock entries must carry the two tag SHAs \
         (v0.55.0 -> {}, v0.56.0 -> {}), one commit each",
        fx.sha_a, fx.sha_b
    );
    drop(fx);
}

/// (b) Each identity projects ITS OWN commit's tree: deployed bytes + record.commit differ.
#[test]
fn two_version_bindings_project_each_their_own_commit_tree() {
    let fx = build_versioned_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_two_versions(&fx.url, &td.target_path());

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("two-version sync deploys both slices");

    let stable = fx
        .registry
        .get(&artifact_key("dest", "stable", "editor"))
        .expect("registry get")
        .expect("the slice keyed by identity `stable` must record under identity `stable`");
    let canary = fx
        .registry
        .get(&artifact_key("dest", "canary", "editor"))
        .expect("registry get")
        .expect("the slice keyed by identity `canary` must record under identity `canary`");

    assert_eq!(
        stable.commit, fx.sha_a,
        "the stable (tag v0.55.0) record must pin commit A, not the shared/last commit"
    );
    assert_eq!(
        canary.commit, fx.sha_b,
        "the canary (tag v0.56.0) record must pin commit B, not the shared/last commit"
    );
    assert_ne!(
        stable.commit, canary.commit,
        "the two identities must record DISTINCT commits; equality means both bindings \
         read the same (wrong) per-source-name commit"
    );

    let stable_lua = std::fs::read(
        td.target_path()
            .join("stable")
            .join("editor")
            .join("init.lua"),
    )
    .expect("stable init.lua deployed");
    let canary_lua = std::fs::read(
        td.target_path()
            .join("canary")
            .join("editor")
            .join("init.lua"),
    )
    .expect("canary init.lua deployed");
    assert_eq!(
        stable_lua, b"v55\n",
        "stable must materialize v0.55.0's tree (init.lua == v55)"
    );
    assert_eq!(
        canary_lua, b"v56\n",
        "canary must materialize v0.56.0's tree (init.lua == v56)"
    );
    drop(fx);
}

/// (c) One fetch of `fzf` covers both refs, yet the two commits still differ.
#[test]
fn two_version_bindings_fetch_source_once_but_resolve_two_commits() {
    let fx = build_versioned_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_two_versions(&fx.url, &td.target_path());

    let counting = CountingBackend::new(&fx.backend);
    let out = sync(
        &input(&cfg, None, None, None, false),
        &counting,
        &fx.registry,
    )
    .expect("two-version sync over a counting backend");

    assert_eq!(
        counting.fetch_count(),
        1,
        "one source fetched once must cover both refs: fetch_count must be exactly 1, got {}",
        counting.fetch_count()
    );
    let commits: std::collections::BTreeSet<&str> = fzf_entries(&out.base_lock)
        .iter()
        .map(|e| e.commit.as_str())
        .collect();
    assert_eq!(
        commits.len(),
        2,
        "a single fetch must still yield two DISTINCT resolved commits, one per ref"
    );
    drop(fx);
}

/// (d) Ref splits share `config_digest` (source-derived) but differ in commit and digest.
#[test]
fn ref_split_entries_share_config_digest_but_differ_in_commit_and_digest() {
    let fx = build_versioned_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_two_versions(&fx.url, &td.target_path());

    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("two-version sync resolves both refs");

    let entries = fzf_entries(&out.base_lock);
    assert_eq!(
        entries.len(),
        2,
        "premise: two ref-split entries must exist"
    );
    let (a, b) = (entries[0], entries[1]);

    assert_eq!(
        a.config_digest, b.config_digest,
        "config_digest is source-derived and SHARED across ref splits"
    );
    assert_ne!(a.commit, b.commit, "ref splits must pin distinct commits");
    assert_ne!(
        a.digest, b.digest,
        "each entry recomputes its export digest at its own commit; v55 and v56 trees \
         differ, so the export digests must differ"
    );
    drop(fx);
}

/// (f) Back-compat: a bare config still serializes a lock with no ref discriminator.
#[test]
fn bare_config_lock_has_no_ref_discriminator_field() {
    let fx = build_versioned_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_one_source_one_target("fzf", &fx.url, "dest", &td.target_path(), "flat");

    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("bare-config sync succeeds");

    let text = toml::to_string(&out.base_lock).expect("lock serializes");
    for forbidden in ["ref =", "tag =", "branch =", "rev ="] {
        assert!(
            !text.contains(forbidden),
            "a bare lock must carry NO per-binding ref discriminator: found `{forbidden}` in:\n{text}"
        );
    }
    drop(fx);
}

/// (g) PTV-005: rebuild round-trips BOTH ref splits, selecting the locked entry by
/// the binding's effective ref (not just by source name).
#[test]
fn rebuild_round_trips_both_ref_splits_at_their_own_commits() {
    let fx = build_versioned_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_two_versions(&fx.url, &td.target_path());

    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("seeding two-version sync");

    let stable_key = artifact_key("dest", "stable", "editor");
    let canary_key = artifact_key("dest", "canary", "editor");
    fx.registry.remove(&stable_key).expect("drop stable record");
    fx.registry.remove(&canary_key).expect("drop canary record");

    rebuild_registry(&cfg, &out.base_lock, &fx.backend, &fx.registry)
        .expect("rebuild reconstructs both ref splits");

    let stable = fx
        .registry
        .get(&stable_key)
        .expect("registry get")
        .expect("rebuild must reconstruct the stable record");
    let canary = fx
        .registry
        .get(&canary_key)
        .expect("registry get")
        .expect("rebuild must reconstruct the canary record");

    assert_eq!(
        stable.commit, fx.sha_a,
        "rebuild must select the locked entry by the binding's effective ref: \
         stable -> tag v0.55.0 -> commit A"
    );
    assert_eq!(
        canary.commit, fx.sha_b,
        "rebuild must select the locked entry by the binding's effective ref: \
         canary -> tag v0.56.0 -> commit B"
    );
    drop(fx);
}

// ── preview (offline, lock-driven) ─────────────────────────────

/// A lock holding one source pinned at `commit`, matching `cfg`'s source identity so
/// preview reuses it via `find_source` without re-resolving.
fn lock_with(cfg: &Config, name: &str, git: &str, commit: &str) -> Lock {
    let source = parsed_of(cfg, name);
    Lock {
        version: 1,
        sources: vec![LockedSource {
            name: name.to_owned(),
            git: git.to_owned(),
            resolved: source.refspec().to_string(),
            commit: commit.to_owned(),
            digest: "blake3:locked".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
            instance: None,
        }],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    }
}

/// The single entry for `artifact` in a preview plan's entries.
fn preview_entry<'a>(plan: &'a PreviewTargetPlan, artifact: &str) -> &'a PreviewEntry {
    plan.entries
        .iter()
        .find(|e| e.artifact == artifact)
        .unwrap_or_else(|| panic!("preview plan must contain an entry for artifact `{artifact}`"))
}

fn only_plan<'a>(plans: &'a [PreviewTargetPlan], target: &str) -> &'a PreviewTargetPlan {
    plans
        .iter()
        .find(|p| p.target == target)
        .unwrap_or_else(|| panic!("preview must produce a plan for target `{target}`"))
}

#[test]
fn preview_with_empty_mirror_and_no_lock_performs_no_fetch_and_annotates_not_locked() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    // No mirror seeded, no lock: a Copy binding with no lock entry must NOT fetch.
    let counting = CountingBackend::new(&fx.backend);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let plans = preview_targets(&cfg, &parsed, &remotes, &counting, None, false)
        .expect("an empty mirror + absent lock must NOT abort preview");

    assert_eq!(
        counting.fetch_count(),
        0,
        "preview must perform NO fetch: commits come from the lock only"
    );
    let plan = only_plan(&plans, "dest");
    assert_eq!(
        plan.entries.len(),
        1,
        "an un-locked source still produces one per-binding annotation entry, got {:?}",
        plan.entries
    );
    assert_eq!(
        plan.entries[0].source, "editor-src",
        "the annotation entry names the un-locked source"
    );
    assert_eq!(
        plan.entries[0].state,
        SyncState::NotLocked,
        "a source with no lock entry must be annotated NotLocked, not fetched"
    );
}

#[test]
fn preview_reads_the_commit_from_the_lock_not_a_fresh_resolution() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    // Seed the mirror at the real HEAD so discovery succeeds offline.
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed fetch");

    let locked_commit = fx.head_sha.clone();
    let lock = lock_with(&cfg, "editor-src", &fx.url, &locked_commit);

    // Advance HEAD: a fresh resolution would pick up the new commit; preview must not.
    let new_head = fx.advance_head();
    assert_ne!(new_head, locked_commit, "fixture HEAD must have moved");

    let counting = CountingBackend::new(&fx.backend);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let plans = preview_targets(&cfg, &parsed, &remotes, &counting, Some(&lock), false)
        .expect("preview reads the locked commit");

    assert_eq!(
        counting.fetch_count(),
        0,
        "preview must not fetch when the commit is in the lock"
    );
    assert_eq!(
        counting.resolve_count(),
        0,
        "preview must reuse the locked commit, never re-resolve a refspec"
    );
    let entry = preview_entry(only_plan(&plans, "dest"), "editor");
    assert_eq!(
        entry.commit, locked_commit,
        "the entry's commit must equal the LOCKED commit, not the advanced HEAD"
    );
    assert_ne!(
        entry.commit, new_head,
        "the advanced HEAD must not leak into the preview entry"
    );
}

#[test]
fn preview_annotates_locked_but_unfetched_source_as_needs_sync_without_fetching() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    // Lock pins a commit, but the mirror is EMPTY (never fetched): discovery would fail.
    let lock = lock_with(&cfg, "editor-src", &fx.url, &fx.head_sha);

    let counting = CountingBackend::new(&fx.backend);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let plans = preview_targets(&cfg, &parsed, &remotes, &counting, Some(&lock), false)
        .expect("a locked-but-absent commit must annotate, not abort");

    assert_eq!(
        counting.fetch_count(),
        0,
        "preview must NOT fetch a locked-but-absent commit — it annotates NeedsSync"
    );
    let plan = only_plan(&plans, "dest");
    assert_eq!(
        plan.entries.len(),
        1,
        "a locked-but-unfetched source yields one annotation entry, got {:?}",
        plan.entries
    );
    assert_eq!(
        plan.entries[0].state,
        SyncState::NeedsSync,
        "locked but commit/mirror absent must be NeedsSync, distinct from NotLocked"
    );
}

#[test]
fn preview_still_plans_other_bindings_when_one_source_needs_sync() {
    let synced = build_named_artifact_repo("alpha", "a.txt", b"from-alpha\n");
    let unsynced = build_named_artifact_repo("beta", "b.txt", b"from-beta\n");

    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let td = TargetDir::new();

    let toml = format!(
        "version = 1\n\n\
             [sources.src-a]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [sources.src-b]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"src-a\", \"src-b\"]\nlayout = \"by-source\"\n",
        synced.1,
        unsynced.1,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two-source config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    // Seed only src-a's mirror; lock both. src-b's commit is absent => NeedsSync.
    backend.fetch(&sn("src-a"), &synced.1).expect("seed src-a");
    let a_head = rev_parse(synced.0.path(), "HEAD");
    let b_head = rev_parse(unsynced.0.path(), "HEAD");
    let lock = Lock {
        version: 1,
        sources: vec![
            lock_with(&cfg, "src-a", &synced.1, &a_head)
                .sources
                .remove(0),
            lock_with(&cfg, "src-b", &unsynced.1, &b_head)
                .sources
                .remove(0),
        ],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };

    let plans = preview_targets(&cfg, &parsed, &remotes, &backend, Some(&lock), false)
        .expect("one unsynced source must not blank the whole plan");

    let plan = only_plan(&plans, "dest");
    let alpha = preview_entry(plan, "alpha");
    assert_eq!(
        alpha.state,
        SyncState::Synced,
        "the synced source's artifact must still be planned (state Synced)"
    );
    assert_eq!(alpha.source, "src-a", "the synced entry belongs to src-a");
    assert!(
        plan.entries
            .iter()
            .any(|e| e.source == "src-b" && e.state == SyncState::NeedsSync),
        "the unsynced source must still appear, annotated NeedsSync, got {:?}",
        plan.entries
    );

    drop(synced);
    drop(unsynced);
}

#[test]
fn preview_link_binding_reads_the_working_tree_ignoring_the_lock() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.dev-src]\ngit = \"{}\"\nbranch = \"main\"\ndeploy = \"link\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"dev-src\"]\nlayout = \"flat\"\n",
        fx.url,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("link-source config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    // No mirror, no lock: link must read the live working tree regardless.
    let plans = preview_targets(&cfg, &parsed, &remotes, &fx.backend, None, false)
        .expect("a link binding reads the working tree without lock or mirror");

    let entry = preview_entry(only_plan(&plans, "dest"), "editor");
    assert_eq!(
        entry.commit, "link",
        "a link binding has no resolved commit; its entry carries the \"link\" sentinel"
    );
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "a link binding with a present working tree is renderable (Synced), not unsynced"
    );
}

#[test]
fn preview_link_binding_with_missing_working_tree_is_annotated() {
    let missing = TempDir::new().expect("scratch tempdir");
    let gone_path = missing.path().join("not-there");
    let gone = gone_path.to_string_lossy().into_owned();
    drop(missing); // ensure the path does not exist

    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.dev-src]\ngit = \"{gone}\"\nbranch = \"main\"\ndeploy = \"link\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"dev-src\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("link-source config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes: BTreeMap<String, String> =
        std::iter::once(("dev-src".to_owned(), gone.clone())).collect();

    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());

    let plans = preview_targets(&cfg, &parsed, &remotes, &backend, None, false)
        .expect("a link binding whose working tree is gone must annotate, not abort");

    let plan = only_plan(&plans, "dest");
    assert_eq!(
        plan.entries.len(),
        1,
        "a gone working tree yields one annotation entry, got {:?}",
        plan.entries
    );
    assert_eq!(
        plan.entries[0].state,
        SyncState::LinkWorkingTreeGone,
        "a link binding whose working tree directory is gone must be LinkWorkingTreeGone"
    );
}

#[test]
fn preview_writes_nothing_to_the_registry_or_the_target() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg =
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat");
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed fetch");

    // Seed a registry record and a target file unrelated to the plan.
    let seeded = RegistryRecord {
        version: 1,
        key: artifact_key("dest", "editor-src", "editor"),
        source: "editor-src".to_owned(),
        commit: fx.head_sha.clone(),
        digest: "blake3:seeded".to_owned(),
        projected_at: "2026-01-01T00:00:00Z".to_owned(),
        layout: "flat".to_owned(),
        kind: RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![ManifestFile {
            path: PathBuf::from("init.lua"),
            size: 8,
            mtime: 1_700_000_000,
            blake3: "blake3:seeded".to_owned(),
        }],
        linked: false,
        vars_digest: None,
    };
    fx.registry.put(&seeded).expect("seed registry record");
    let before = fx.registry.list_all().expect("snapshot registry");

    std::fs::create_dir_all(td.target_path()).expect("mkdir target");
    std::fs::write(td.target_path().join("pre-existing.txt"), b"keep\n")
        .expect("seed a pre-existing target file");
    let target_before = read_target_dir(&td.target_path());

    let counting = CountingBackend::new(&fx.backend);
    let lock = lock_with(&cfg, "editor-src", &fx.url, &fx.head_sha);
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let plans = preview_targets(&cfg, &parsed, &remotes, &counting, Some(&lock), false)
        .expect("preview builds a plan");
    assert!(
        !preview_entry(only_plan(&plans, "dest"), "editor")
            .destination
            .as_os_str()
            .is_empty(),
        "premise: preview must actually produce the editor entry so the no-write scan is meaningful"
    );

    assert_eq!(counting.export_count(), 0, "preview must export nothing");
    let after = fx.registry.list_all().expect("re-read registry");
    assert_eq!(
        before.len(),
        after.len(),
        "preview must not add or remove registry records"
    );
    assert_eq!(
        before[0].key, after[0].key,
        "the seeded registry record must be byte-for-byte unchanged after preview"
    );
    assert_eq!(
        before[0].digest, after[0].digest,
        "preview must not rewrite the seeded record's digest"
    );
    assert_eq!(
        read_target_dir(&td.target_path()),
        target_before,
        "preview must not create, remove, or modify any file under the target dir"
    );
}

/// Every file path (relative to `dir`) plus its bytes, for a whole-tree no-write diff.
fn read_target_dir(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if entry.file_type().is_file() {
            let rel = entry
                .path()
                .strip_prefix(dir)
                .expect("entry under dir")
                .to_path_buf();
            out.insert(rel, std::fs::read(entry.path()).expect("read target file"));
        }
    }
    out
}

#[test]
fn preview_annotates_predicted_flat_collision_as_warning_not_error() {
    let fx_a = build_named_artifact_repo("shared", "a.txt", b"from-a\n");
    let fx_b = build_named_artifact_repo("shared", "b.txt", b"from-b\n");

    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let td = TargetDir::new();

    let toml = format!(
        "version = 1\n\n\
             [sources.src-a]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [sources.src-b]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"src-a\", \"src-b\"]\nlayout = \"flat\"\n",
        fx_a.1,
        fx_b.1,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two-source flat config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    backend.fetch(&sn("src-a"), &fx_a.1).expect("seed src-a");
    backend.fetch(&sn("src-b"), &fx_b.1).expect("seed src-b");
    let a_head = rev_parse(fx_a.0.path(), "HEAD");
    let b_head = rev_parse(fx_b.0.path(), "HEAD");
    let lock = Lock {
        version: 1,
        sources: vec![
            lock_with(&cfg, "src-a", &fx_a.1, &a_head).sources.remove(0),
            lock_with(&cfg, "src-b", &fx_b.1, &b_head).sources.remove(0),
        ],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };

    let result = preview_targets(&cfg, &parsed, &remotes, &backend, Some(&lock), false);

    let plans = match result {
        Ok(plans) => plans,
        Err(Error::Collision { .. }) => {
            panic!("a predicted flat collision must be a warning in preview, NOT Error::Collision")
        }
        Err(e) => panic!("preview must not error on a predicted collision, got {e:?}"),
    };

    let plan = only_plan(&plans, "dest");
    assert_eq!(
        plan.entries
            .iter()
            .filter(|e| e.artifact == "shared")
            .count(),
        2,
        "preview must render BOTH colliding entries, got {:?}",
        plan.entries
    );
    let collision = plan
        .collisions
        .iter()
        .find(|c| c.artifact == "shared")
        .expect("the flat collision on `shared` must be annotated on the target");
    assert!(
        collision.sources.contains(&"src-a".to_owned())
            && collision.sources.contains(&"src-b".to_owned()),
        "the collision warning must name BOTH contributing sources, got {:?}",
        collision.sources
    );

    drop(fx_a);
    drop(fx_b);
}

#[test]
fn preview_happy_path_renders_synced_artifacts_at_literal_layout_destinations() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let cfg = config_one_source_one_target(
        "editor-src",
        &fx.url,
        "dest",
        &td.target_path(),
        "by-source",
    );
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed fetch");
    let lock = lock_with(&cfg, "editor-src", &fx.url, &fx.head_sha);

    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let plans = preview_targets(&cfg, &parsed, &remotes, &fx.backend, Some(&lock), false)
        .expect("a synced source previews its artifacts");

    let plan = only_plan(&plans, "dest");
    let editor = preview_entry(plan, "editor");
    assert_eq!(
        editor.state,
        SyncState::Synced,
        "a synced source's artifact must be Synced"
    );
    assert_eq!(
        editor.source, "editor-src",
        "the entry names its source binding"
    );
    assert_eq!(
        editor.commit, fx.head_sha,
        "the entry carries the locked commit"
    );
    assert_eq!(
        editor.destination,
        td.target_path().join("editor-src").join("editor"),
        "by-source layout must place `editor` at <target>/editor-src/editor"
    );
}

// ── preview plan builder (selectors + offline + --files) ──

use crate::cli::{
    PreviewPlan, PreviewSelectors, preview_plan, render_preview_json, render_preview_tree,
};

/// Two synced sources (`editor-src`→repo A artifact `editor`, `lint-src`→repo B
/// artifact `lint`) plus an unlocked `ghost-src`, across two by-source targets:
/// `home` carries all three bindings, `work` carries only `lint-src`. Mirrors for
/// the synced sources are seeded; a two-entry lock pins them. Returns everything a
/// preview-plan test needs to call [`preview_plan`] offline.
struct PreviewFixture {
    _fx_a: SyncFixture,
    _fx_b: TempDir,
    home: TargetDir,
    work: TargetDir,
    _git_dir: TempDir,
    cfg: Config,
    lock: Lock,
    counting_inner: GitBackend,
    head_a: String,
}

fn build_preview_fixture() -> PreviewFixture {
    let fx_a = build_sync_fixture();
    let (fx_b, url_b) = build_named_artifact_repo("lint", "rules.toml", b"[rules]\n");

    let git_dir = TempDir::new().expect("shared git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    backend
        .fetch(&sn("editor-src"), &fx_a.url)
        .expect("seed editor-src mirror");
    backend
        .fetch(&sn("lint-src"), &url_b)
        .expect("seed lint-src mirror");
    let head_a = fx_a.head_sha.clone();
    let head_b = backend
        .resolve(&sn("lint-src"), &url_b, &Refspec::Branch("main".into()))
        .expect("resolve lint-src HEAD from the seeded mirror");

    let home = TargetDir::new();
    let work = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.editor-src]\ngit = \"{url_a}\"\nbranch = \"main\"\n\n\
             [sources.lint-src]\ngit = \"{url_b}\"\nbranch = \"main\"\n\n\
             [sources.ghost-src]\ngit = \"{url_a}\"\nbranch = \"main\"\n\n\
             [targets.home]\npath = \"{home_path}\"\nsources = [\"editor-src\", \"lint-src\", \"ghost-src\"]\nlayout = \"by-source\"\n\n\
             [targets.work]\npath = \"{work_path}\"\nsources = [\"lint-src\"]\nlayout = \"by-source\"\n",
        url_a = fx_a.url,
        url_b = url_b,
        home_path = home.target_path().display(),
        work_path = work.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("multi-target multi-source config parses");

    let lock = Lock {
        version: 1,
        sources: vec![
            LockedSource {
                name: "editor-src".to_owned(),
                git: fx_a.url.clone(),
                resolved: "main".to_owned(),
                commit: head_a.clone(),
                digest: "blake3:a".to_owned(),
                config_digest: parsed_of(&cfg, "editor-src").config_digest(),
                r#ref: None,
                instance: None,
            },
            LockedSource {
                name: "lint-src".to_owned(),
                git: url_b.clone(),
                resolved: "main".to_owned(),
                commit: head_b.clone(),
                digest: "blake3:b".to_owned(),
                config_digest: parsed_of(&cfg, "lint-src").config_digest(),
                r#ref: None,
                instance: None,
            },
        ],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };

    PreviewFixture {
        _fx_a: fx_a,
        _fx_b: fx_b,
        home,
        work,
        _git_dir: git_dir,
        cfg,
        lock,
        counting_inner: backend,
        head_a,
    }
}

impl PreviewFixture {
    fn parsed(&self) -> BTreeMap<String, ParsedSource> {
        self.cfg.parsed_sources().expect("sources parse")
    }

    fn remotes(&self) -> BTreeMap<String, String> {
        let parsed = self.parsed();
        resolved_remotes(&self.cfg, &parsed).expect("remotes resolve")
    }
}

/// All entries for `target` across the plan.
fn plan_target_of<'a>(plan: &'a PreviewPlan, target: &str) -> &'a PreviewTargetPlan {
    plan.targets
        .iter()
        .find(|t| t.target == target)
        .unwrap_or_else(|| panic!("preview plan must include target `{target}`"))
}

fn entry_of<'a>(tp: &'a PreviewTargetPlan, source: &str, artifact: &str) -> &'a PreviewEntry {
    tp.entries
        .iter()
        .find(|e| e.source == source && e.artifact == artifact)
        .unwrap_or_else(|| {
            panic!(
                "target `{}` must carry an entry for {source}/{artifact}, got {:?}",
                tp.target, tp.entries
            )
        })
}

#[test]
fn preview_plan_includes_all_targets_with_synced_entries_at_literal_destinations() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: false,
    };

    let plan = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("an unfiltered plan builds over the seeded mirrors");

    let mut targets: Vec<&str> = plan.targets.iter().map(|t| t.target.as_str()).collect();
    targets.sort_unstable();
    assert_eq!(
        targets,
        vec!["home", "work"],
        "an unfiltered plan must include every configured target, got {targets:?}"
    );

    let home = plan_target_of(&plan, "home");
    let editor = entry_of(home, "editor-src", "editor");
    assert_eq!(
        editor.state,
        SyncState::Synced,
        "editor-src/editor must be Synced"
    );
    assert_eq!(
        editor.commit, pf.head_a,
        "the synced entry carries the locked commit of editor-src"
    );
    assert_eq!(
        editor.destination,
        pf.home.target_path().join("editor-src").join("editor"),
        "by-source layout must place editor at <home>/editor-src/editor (literal path, not via layout helper)"
    );

    let lint = entry_of(home, "lint-src", "lint");
    assert_eq!(
        lint.state,
        SyncState::Synced,
        "lint-src/lint must be Synced"
    );
    assert_eq!(
        lint.destination,
        pf.home.target_path().join("lint-src").join("lint"),
        "lint must land at <home>/lint-src/lint"
    );

    let work = plan_target_of(&plan, "work");
    assert_eq!(
        entry_of(work, "lint-src", "lint").destination,
        pf.work.target_path().join("lint-src").join("lint"),
        "the work target's lint entry must land at <work>/lint-src/lint"
    );
}

#[test]
fn preview_plan_keeps_unsynced_binding_in_the_unfiltered_plan() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: false,
    };

    let plan = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("plan builds");

    let home = plan_target_of(&plan, "home");
    let ghost = home
        .entries
        .iter()
        .find(|e| e.source == "ghost-src")
        .expect("an unlocked source must still appear as a per-binding annotation entry");
    assert_eq!(
        ghost.state,
        SyncState::NotLocked,
        "ghost-src has no lock entry, so it must be annotated NotLocked (not dropped, not fetched)"
    );
}

#[test]
fn preview_plan_target_selector_keeps_only_that_target() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: Some("work".to_owned()),
        files: false,
    };

    let plan = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("plan builds filtered to work");

    let targets: Vec<&str> = plan.targets.iter().map(|t| t.target.as_str()).collect();
    assert_eq!(
        targets,
        vec!["work"],
        "--target=work must filter the plan to exactly that target, got {targets:?}"
    );
    let work = plan_target_of(&plan, "work");
    assert!(
        entry_of(work, "lint-src", "lint").state == SyncState::Synced,
        "the surviving work target must still carry its lint-src/lint entry"
    );
}

#[test]
fn preview_plan_source_selector_keeps_only_that_underlying_source_across_targets() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: Some("lint-src".to_owned()),
        target: None,
        files: false,
    };

    let plan = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("plan builds filtered to lint-src");

    for tp in &plan.targets {
        assert!(
            tp.entries.iter().all(|e| e.source == "lint-src"),
            "--source=lint-src must drop every entry whose underlying source != lint-src, \
                 target `{}` still has {:?}",
            tp.target,
            tp.entries
        );
    }
    let home = plan_target_of(&plan, "home");
    assert!(
        entry_of(home, "lint-src", "lint").state == SyncState::Synced,
        "the lint-src binding under home must survive the source filter"
    );
    assert!(
        home.entries
            .iter()
            .all(|e| e.source != "editor-src" && e.source != "ghost-src"),
        "editor-src and ghost-src entries must be dropped under --source=lint-src, got {:?}",
        home.entries
    );
}

#[test]
fn preview_plan_unknown_target_selector_errors() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: Some("nope".to_owned()),
        files: false,
    };

    let err = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect_err("an unknown --target must be rejected, not silently produce an empty plan");
    assert!(
        err.to_string().contains("nope"),
        "the error must name the offending target `nope`, got `{err}`"
    );
}

#[test]
fn preview_plan_unknown_source_selector_errors() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: Some("nope-src".to_owned()),
        target: None,
        files: false,
    };

    let err = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect_err("an unknown --source must be rejected against the merged config");
    assert!(
        err.to_string().contains("nope-src"),
        "the error must name the offending source `nope-src`, got `{err}`"
    );
}

#[test]
fn preview_plan_is_offline_and_writes_nothing() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let counting = CountingBackend::new(&pf.counting_inner);
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: true,
    };

    let home_before = read_target_dir(&pf.home.target_path());
    let work_before = read_target_dir(&pf.work.target_path());

    let plan = preview_plan(&pf.cfg, &parsed, &remotes, &counting, Some(&pf.lock), &sel)
        .expect("plan builds offline");
    assert!(
        !plan.targets.is_empty(),
        "premise: the plan must be non-empty so the offline assertions are meaningful"
    );

    assert_eq!(
        counting.fetch_count(),
        0,
        "preview_plan must perform NO fetch; commits come from the lock"
    );
    assert_eq!(
        counting.resolve_count(),
        0,
        "preview_plan must reuse the locked commit, never re-resolve a refspec"
    );
    assert_eq!(
        counting.export_count(),
        0,
        "even with --files, preview_plan must not export/write artifacts"
    );
    assert_eq!(
        read_target_dir(&pf.home.target_path()),
        home_before,
        "preview_plan must not create, remove, or modify any file under the home target"
    );
    assert_eq!(
        read_target_dir(&pf.work.target_path()),
        work_before,
        "preview_plan must not write under the work target"
    );
}

#[test]
fn preview_plan_with_files_enriches_synced_entries_with_their_file_lists() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: true,
    };

    let plan = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("a --files plan builds");

    let editor = entry_of(plan_target_of(&plan, "home"), "editor-src", "editor");
    let mut editor_files: Vec<PathBuf> = editor.files.iter().map(|f| f.path.clone()).collect();
    editor_files.sort();
    assert_eq!(
        editor_files,
        vec![PathBuf::from("init.lua"), PathBuf::from("notes.bak")],
        "with --files the editor entry must list EXACTLY the seeded editor tree \
         (init.lua + the sibling notes.bak), with no over-listing or duplication, got {:?}",
        editor.files
    );

    let lint = entry_of(plan_target_of(&plan, "home"), "lint-src", "lint");
    let mut lint_files: Vec<PathBuf> = lint.files.iter().map(|f| f.path.clone()).collect();
    lint_files.sort();
    assert_eq!(
        lint_files,
        vec![PathBuf::from("rules.toml")],
        "with --files the lint entry must list EXACTLY rules.toml, got {:?}",
        lint.files
    );
}

#[test]
fn preview_plan_without_files_leaves_entries_unenriched() {
    let pf = build_preview_fixture();
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: false,
    };

    let plan = preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("a no-files plan builds");

    let editor = entry_of(plan_target_of(&plan, "home"), "editor-src", "editor");
    assert!(
        editor.files.is_empty(),
        "without --files a synced entry must carry no file list, got {:?}",
        editor.files
    );
}

// ── offline file lister (SourceBackend::list_artifact_files) ──

#[test]
fn list_artifact_files_returns_artifact_files_offline_without_fetching() {
    let fx = build_sync_fixture();
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed mirror so the lister can read the tree without fetching");
    let counting = CountingBackend::new(&fx.backend);
    let selection = Selection::new(&[], &[]).expect("empty selection builds");

    let files = counting
        .list_artifact_files(
            &sn("editor-src"),
            &fx.url,
            &fx.head_sha,
            None,
            &an("editor"),
            &selection,
        )
        .expect("listing a synced artifact's files must succeed");

    assert!(
        files.contains(&PathBuf::from("init.lua")),
        "the editor artifact's files must include init.lua, got {files:?}"
    );
    assert!(
        files.contains(&PathBuf::from("notes.bak")),
        "with an empty selection notes.bak is included too, got {files:?}"
    );
    assert_eq!(
        counting.fetch_count(),
        0,
        "list_artifact_files must read the seeded mirror, performing NO fetch"
    );
    assert_eq!(
        counting.export_count(),
        0,
        "list_artifact_files must not export/write anything"
    );
}

#[test]
fn list_artifact_files_respects_selection_exclude() {
    let fx = build_sync_fixture();
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed mirror");
    let selection =
        Selection::new(&[], &["**/*.bak".to_owned()]).expect("exclude selection builds");

    let files = fx
        .backend
        .list_artifact_files(
            &sn("editor-src"),
            &fx.url,
            &fx.head_sha,
            None,
            &an("editor"),
            &selection,
        )
        .expect("listing with an exclude must succeed");

    assert!(
        files.contains(&PathBuf::from("init.lua")),
        "init.lua is not excluded and must be listed, got {files:?}"
    );
    assert!(
        !files.contains(&PathBuf::from("notes.bak")),
        "a **/*.bak exclude must drop notes.bak from the listing, got {files:?}"
    );
}

// ── tree + json rendering ─────────────────────────────

/// Builds the unfiltered, file-enriched plan once for the rendering tests.
fn rendered_plan(pf: &PreviewFixture) -> PreviewPlan {
    let parsed = pf.parsed();
    let remotes = pf.remotes();
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: true,
    };
    preview_plan(
        &pf.cfg,
        &parsed,
        &remotes,
        &pf.counting_inner,
        Some(&pf.lock),
        &sel,
    )
    .expect("the rendering fixture plan builds")
}

#[test]
fn render_preview_tree_shows_targets_bindings_destinations_and_files() {
    let pf = build_preview_fixture();
    let plan = rendered_plan(&pf);

    let out = render_preview_tree(&plan);

    assert!(
        out.contains("home"),
        "the tree must name the home target, got:\n{out}"
    );
    assert!(
        out.contains("work"),
        "the tree must name the work target, got:\n{out}"
    );
    assert!(
        out.contains("editor-src"),
        "the tree must name the editor-src binding, got:\n{out}"
    );
    assert!(
        out.contains("editor"),
        "the tree must name the editor artifact, got:\n{out}"
    );
    assert!(
        out.contains("editor-src/editor")
            || out.contains(
                pf.home
                    .target_path()
                    .join("editor-src")
                    .join("editor")
                    .to_string_lossy()
                    .as_ref()
            ),
        "the tree must show the artifact's destination path, got:\n{out}"
    );
    assert!(
        out.contains("init.lua"),
        "with files enriched the tree must list init.lua under its artifact, got:\n{out}"
    );
    assert!(
        out.contains("ghost-src"),
        "the tree must surface the unsynced ghost-src binding, got:\n{out}"
    );
    let lower = out.to_lowercase();
    assert!(
        lower.contains("not locked") || lower.contains("needs sync") || lower.contains("notlocked"),
        "an unsynced binding must render a visible annotation, got:\n{out}"
    );
}

#[test]
fn render_preview_tree_warns_about_a_predicted_collision_naming_both_sources() {
    let fx_a = build_named_artifact_repo("shared", "a.txt", b"from-a\n");
    let fx_b = build_named_artifact_repo("shared", "b.txt", b"from-b\n");

    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    backend.fetch(&sn("src-a"), &fx_a.1).expect("seed src-a");
    backend.fetch(&sn("src-b"), &fx_b.1).expect("seed src-b");
    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.src-a]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [sources.src-b]\ngit = \"{}\"\nbranch = \"main\"\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"src-a\", \"src-b\"]\nlayout = \"flat\"\n",
        fx_a.1,
        fx_b.1,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("two-source flat config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let commit_a = backend
        .resolve(&sn("src-a"), &fx_a.1, &Refspec::Branch("main".into()))
        .expect("resolve src-a");
    let commit_b = backend
        .resolve(&sn("src-b"), &fx_b.1, &Refspec::Branch("main".into()))
        .expect("resolve src-b");
    let lock = Lock {
        version: 1,
        sources: vec![
            LockedSource {
                name: "src-a".to_owned(),
                git: fx_a.1.clone(),
                resolved: "main".to_owned(),
                commit: commit_a,
                digest: "blake3:a".to_owned(),
                config_digest: parsed_of(&cfg, "src-a").config_digest(),
                r#ref: None,
                instance: None,
            },
            LockedSource {
                name: "src-b".to_owned(),
                git: fx_b.1.clone(),
                resolved: "main".to_owned(),
                commit: commit_b,
                digest: "blake3:b".to_owned(),
                config_digest: parsed_of(&cfg, "src-b").config_digest(),
                r#ref: None,
                instance: None,
            },
        ],
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    };
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: false,
    };

    let plan = preview_plan(&cfg, &parsed, &remotes, &backend, Some(&lock), &sel)
        .expect("a colliding plan builds without error");
    let out = render_preview_tree(&plan);

    let lower = out.to_lowercase();
    assert!(
        lower.contains("collision") || lower.contains("warn") || lower.contains("conflict"),
        "a predicted flat collision must render a visible warning, got:\n{out}"
    );
    assert!(
        out.contains("src-a") && out.contains("src-b"),
        "the collision warning must name BOTH contributing sources, got:\n{out}"
    );

    drop(fx_a);
    drop(fx_b);
}

#[test]
fn render_preview_json_is_valid_json_with_targets_entries_and_collisions() {
    let pf = build_preview_fixture();
    let plan = rendered_plan(&pf);

    let json = render_preview_json(&plan).expect("preview JSON must serialize");
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("preview JSON must parse back into a Value");

    let targets = value
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .expect("JSON must carry a `targets` array");
    let home = targets
        .iter()
        .find(|t| t.get("target").and_then(serde_json::Value::as_str) == Some("home"))
        .expect("the targets array must include the home target");

    let entries = home
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .expect("each target must carry an `entries` array");
    let editor = entries
        .iter()
        .find(|e| {
            e.get("source").and_then(serde_json::Value::as_str) == Some("editor-src")
                && e.get("artifact").and_then(serde_json::Value::as_str) == Some("editor")
        })
        .expect("home entries must include editor-src/editor");
    let dest_str = editor
        .get("destination")
        .and_then(serde_json::Value::as_str)
        .expect("each entry must serialize its destination as a string");
    assert!(
        Path::new(dest_str).ends_with("editor-src/editor"),
        "the editor entry's destination must resolve under editor-src/editor, got {dest_str:?}"
    );

    let state_str = editor
        .get("state")
        .and_then(serde_json::Value::as_str)
        .expect("each entry must serialize its sync state as a string");
    assert_eq!(
        state_str.to_lowercase(),
        "synced",
        "the editor entry's state must mean Synced, got {state_str:?}"
    );

    let collisions = home
        .get("collisions")
        .and_then(serde_json::Value::as_array)
        .expect("each target must serialize collisions as an array");
    assert!(
        collisions.is_empty(),
        "the unfiltered by-source home plan has no flat collision, got {collisions:?}"
    );
}

// ── preview templating annotation (M004): --files shows deployed name + flag ──

#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_templated_artifact_repo() -> (TempDir, String) {
    let src = TempDir::new().unwrap();
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor")).unwrap();
    std::fs::write(p.join("editor/motd.tmpl"), b"hello {{ greeting }}!\n").unwrap();
    std::fs::write(p.join("editor/static.txt"), b"plain content\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let url = p.to_string_lossy().into_owned();
    (src, url)
}

fn file_named<'a>(entry: &'a PreviewEntry, name: &str) -> &'a crate::sync::PreviewFile {
    entry
        .files
        .iter()
        .find(|f| f.path == Path::new(name))
        .unwrap_or_else(|| {
            panic!(
                "entry for `{}` must carry a file with deployed path `{name}`, got {:?}",
                entry.artifact, entry.files
            )
        })
}

#[test]
fn preview_files_strips_tmpl_suffix_and_marks_templated_in_copy_mode() {
    let (src, url) = build_templated_artifact_repo();
    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    backend.fetch(&sn("dotfiles"), &url).expect("seed mirror");
    let commit = backend
        .resolve(&sn("dotfiles"), &url, &Refspec::Branch("main".into()))
        .expect("resolve HEAD from the seeded mirror");

    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\ninclude = [\"editor\"]\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("templated copy config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let lock = lock_with(&cfg, "dotfiles", &url, &commit);

    let plans = preview_targets(&cfg, &parsed, &remotes, &backend, Some(&lock), true)
        .expect("a --files preview over a templated source builds");
    let entry = preview_entry(only_plan(&plans, "dest"), "editor");

    let motd = file_named(entry, "motd");
    assert!(
        motd.templated,
        "a default-opt-in `.tmpl` file must be marked templated=true, got {:?}",
        entry.files
    );
    assert!(
        !entry.files.iter().any(|f| f.path == Path::new("motd.tmpl")),
        "preview must show the DEPLOYED name `motd`, never the source `motd.tmpl`, got {:?}",
        entry.files
    );

    let static_txt = file_named(entry, "static.txt");
    assert!(
        !static_txt.templated,
        "a plain sibling must be templated=false and keep its name, got {:?}",
        entry.files
    );

    drop(src);
}

#[test]
fn preview_files_in_link_mode_never_marks_templated() {
    let (src, url) = build_templated_artifact_repo();
    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\ndeploy = \"link\"\ninclude = [\"editor\"]\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("link-mode templated config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let plans = preview_targets(&cfg, &parsed, &remotes, &backend, None, true)
        .expect("a link binding reads the working tree without lock or mirror");
    let entry = preview_entry(only_plan(&plans, "dest"), "editor");

    let motd = file_named(entry, "motd.tmpl");
    assert!(
        !motd.templated,
        "link mode never renders, so a `.tmpl`-named file must stay templated=false, got {:?}",
        entry.files
    );
    assert!(
        !entry.files.iter().any(|f| f.path == Path::new("motd")),
        "link mode must NOT strip the `.tmpl` suffix; the source name `motd.tmpl` deploys as-is, got {:?}",
        entry.files
    );

    drop(src);
}

#[test]
fn render_preview_tree_annotates_templated_files_only() {
    let (src, url) = build_templated_artifact_repo();
    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    backend.fetch(&sn("dotfiles"), &url).expect("seed mirror");
    let commit = backend
        .resolve(&sn("dotfiles"), &url, &Refspec::Branch("main".into()))
        .expect("resolve HEAD");

    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\ninclude = [\"editor\"]\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("templated config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let lock = lock_with(&cfg, "dotfiles", &url, &commit);
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: true,
    };
    let plan = preview_plan(&cfg, &parsed, &remotes, &backend, Some(&lock), &sel)
        .expect("a --files plan builds");

    let out = render_preview_tree(&plan);

    assert!(
        out.contains("motd (templated)"),
        "the tree must annotate the templated file as `motd (templated)`, got:\n{out}"
    );
    assert!(
        !out.contains("motd.tmpl"),
        "the tree must not leak the source name `motd.tmpl`, got:\n{out}"
    );
    let static_line = out
        .lines()
        .find(|l| l.contains("static.txt"))
        .unwrap_or_else(|| panic!("the tree must list static.txt, got:\n{out}"));
    assert!(
        !static_line.contains("(templated)"),
        "a plain file must render with NO `(templated)` annotation, got line `{static_line}`"
    );

    drop(src);
}

#[test]
fn render_preview_json_carries_deployed_name_and_templated_flag() {
    let (src, url) = build_templated_artifact_repo();
    let git_dir = TempDir::new().expect("git dir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    backend.fetch(&sn("dotfiles"), &url).expect("seed mirror");
    let commit = backend
        .resolve(&sn("dotfiles"), &url, &Refspec::Branch("main".into()))
        .expect("resolve HEAD");

    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
             [sources.dotfiles]\ngit = \"{url}\"\nbranch = \"main\"\ninclude = [\"editor\"]\n\n\
             [targets.dest]\npath = \"{}\"\nsources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("templated config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    let lock = lock_with(&cfg, "dotfiles", &url, &commit);
    let sel = PreviewSelectors {
        source: None,
        target: None,
        files: true,
    };
    let plan = preview_plan(&cfg, &parsed, &remotes, &backend, Some(&lock), &sel)
        .expect("a --files plan builds");

    let json = render_preview_json(&plan).expect("preview JSON must serialize");
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("preview JSON must parse back into a Value");

    let files = value
        .pointer("/targets/0/entries/0/files")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("JSON must carry a per-entry `files` array, got:\n{json}"));

    let find = |name: &str| {
        files
            .iter()
            .find(|f| f.get("path").and_then(serde_json::Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("files must include `{name}` by deployed path, got:\n{json}"))
    };

    let motd = find("motd");
    assert_eq!(
        motd.get("templated").and_then(serde_json::Value::as_bool),
        Some(true),
        "the templated file must serialize `\"templated\": true`, got {motd:?}"
    );

    let static_txt = find("static.txt");
    assert_eq!(
        static_txt
            .get("templated")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "the plain file must serialize `\"templated\": false`, got {static_txt:?}"
    );

    drop(src);
}

// ── binding-selection parity: plan/preview must discover the binding's effective
//    include/exclude (matching deploy_target), not the source-level selection ──

/// Premise guard: WITHOUT the override, `editor-src` discovers BOTH `docs` and
/// `editor` — proving the override above genuinely narrows the set.
#[test]
fn plan_target_without_override_discovers_full_source_level_set() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let toml = format!(
        "version = 1\n\n\
         [sources.editor-src]\ngit = \"{}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\"]\nlayout = \"by-source\"\n",
        fx.url,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("plain-binding config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");
    fx.backend
        .fetch(&sn("editor-src"), &fx.url)
        .expect("seed editor-src mirror");
    let commits = one_commit(&parsed, "editor-src", &fx.head_sha);

    let plans = plan_targets(&cfg, &parsed, &remotes, &fx.backend, &commits)
        .expect("plan builds over the seeded mirror");

    let dest = plans
        .iter()
        .find(|p| p.target == "dest")
        .expect("plan must include target `dest`");
    let mut artifacts: Vec<&str> = dest.entries.iter().map(|e| e.artifact.as_str()).collect();
    artifacts.sort_unstable();
    assert_eq!(
        artifacts,
        vec!["docs", "editor"],
        "a plain binding (no override) inherits the source's full set, got {artifacts:?}"
    );
}

// ── hook dispatch (TPH-003): post-commit digest diff + run ─────

/// A target-hook command that appends one marker line to `log` then exits 0.
/// Shell-quoted so the path may contain tempdir punctuation.
fn append_cmd(log: &Path, marker: &str) -> String {
    format!("printf '%s\\n' '{marker}' >> '{}'", log.display())
}

/// A target-hook command that appends a marker then exits non-zero.
fn append_then_fail_cmd(log: &Path, marker: &str) -> String {
    format!("printf '%s\\n' '{marker}' >> '{}'; exit 7", log.display())
}

/// Reads the hook log into trimmed non-empty lines (empty if the log is absent).
fn log_lines(log: &Path) -> Vec<String> {
    match std::fs::read_to_string(log) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// One source scoped to `editor`, one target `dest`, plus an inline
/// `[targets.dest.hooks] on_change = <commands>` table.
fn config_with_target_hooks(url: &str, target_path: &Path, on_change_toml: &str) -> Config {
    let toml = format!(
        "version = 1\n\n\
         [sources.editor-src]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\"]\nlayout = \"flat\"\n\n\
         [targets.dest.hooks]\non_change = {on_change_toml}\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("target-hooks config parses")
}

/// The single `on_change` hook id recorded under `target` (panics unless exactly
/// one exists): lets a test reuse the id without reconstructing TOML escaping.
fn sole_hook_id(reg: &FileRegistry, target: &str) -> String {
    let mut states = reg.load_hook_state(target).expect("load hook state");
    assert_eq!(states.len(), 1, "expected exactly one recorded hook id");
    states.remove(0).hook_id
}

fn recorded_hook(reg: &FileRegistry, target: &str, id: &str) -> Option<crate::store::HookState> {
    reg.load_hook_state(target)
        .expect("load hook state")
        .into_iter()
        .find(|h| h.hook_id == id)
}

/// A changing sync fires the target's `on_change` hook exactly once and records
/// its last-success so the next no-op sync stays quiet (INV-3, success path).
#[test]
fn changing_sync_fires_on_change_hook_once_and_records_success() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("hook.log");
    let cfg = config_with_target_hooks(
        &fx.url,
        &td.target_path(),
        &format!("\"{}\"", append_cmd(&log, "fired").replace('"', "\\\"")),
    );

    let out = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys and fires the on_change hook");

    assert!(
        !out.had_failures,
        "a clean deploy + successful hook is no failure"
    );
    assert_eq!(
        log_lines(&log),
        vec!["fired".to_owned()],
        "a sync that changed the target's digest-set must run on_change EXACTLY once"
    );
    let id = sole_hook_id(&fx.registry, "dest");
    assert!(
        id.starts_with("dest#") && recorded_hook(&fx.registry, "dest", &id).is_some(),
        "after the hook exits 0 its last-success digest-set must be recorded under a \
         command-keyed `dest#<run>` id (INV-4); got {id:?}"
    );
}

/// INV-3: a second sync with no digest change runs NO hook and records nothing new.
#[test]
fn noop_sync_fires_no_hook() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("hook.log");
    let cfg = config_with_target_hooks(
        &fx.url,
        &td.target_path(),
        &format!("\"{}\"", append_cmd(&log, "fired").replace('"', "\\\"")),
    );

    let first = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys and fires once");
    assert_eq!(
        log_lines(&log).len(),
        1,
        "premise: the first changing sync fires the hook once"
    );

    let second = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("second sync is a clean no-op");

    assert!(!second.had_failures, "a clean no-op sync must not fail");
    assert_eq!(
        log_lines(&log).len(),
        1,
        "INV-3: a no-op sync (digest-set unchanged since last hook success) must fire NOTHING — \
         the log must still hold exactly the one line from the first run"
    );
}

/// Failure semantics: a non-zero hook makes the sync report failure, leaves the
/// deployed files intact (INV-2), does NOT record last-success (INV-4), and so the
/// hook re-fires on the next sync even though the digest-set did not change again.
#[test]
fn failed_hook_fails_sync_keeps_files_and_refires_next_sync() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("hook.log");
    let cfg = config_with_target_hooks(
        &fx.url,
        &td.target_path(),
        &format!(
            "\"{}\"",
            append_then_fail_cmd(&log, "fired").replace('"', "\\\"")
        ),
    );

    let first = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("a hook failure surfaces via had_failures, not a hard Err");

    assert!(
        first.had_failures,
        "a hook exiting non-zero must make the sync report failure (CLI exit non-zero)"
    );
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    assert_eq!(
        std::fs::read(dst.join("init.lua")).expect("deployed init.lua present"),
        b"-- init\n",
        "INV-2: a hook failure NEVER rolls back files — the artifact stays deployed"
    );
    assert_eq!(
        fx.registry
            .load_hook_state("dest")
            .expect("load hook state"),
        vec![],
        "INV-4: a failed hook must NOT record last-success — with this target's single \
         hook failing, the registry holds NO recorded success under ANY id"
    );
    assert_eq!(
        log_lines(&log).len(),
        1,
        "premise: the failing hook ran exactly once on the first sync"
    );

    let second = sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("second sync re-runs the never-succeeded hook");

    assert!(
        second.had_failures,
        "the still-failing hook keeps the sync in a failed state"
    );
    assert_eq!(
        log_lines(&log).len(),
        2,
        "INV-4 retry-on-next-sync: a hook with no recorded success must re-fire next sync, \
         appending a second line"
    );
}

/// Collection + dedup + order: a target reachable via the same command twice runs
/// it ONCE, and distinct commands run in declaration order.
#[test]
fn on_change_hooks_run_in_declaration_order_and_dedupe() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("hook.log");
    let dup = append_cmd(&log, "alpha").replace('"', "\\\"");
    let beta = append_cmd(&log, "beta").replace('"', "\\\"");
    let on_change = format!("[\"{dup}\", \"{beta}\", \"{dup}\"]");
    let cfg = config_with_target_hooks(&fx.url, &td.target_path(), &on_change);

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync runs the deduped, ordered hook list");

    assert_eq!(
        log_lines(&log),
        vec!["alpha".to_owned(), "beta".to_owned()],
        "duplicate commands collapse to one run and survivors keep declaration order: \
         alpha (once) then beta — got {:?}",
        log_lines(&log)
    );
}

/// Two hooks sharing one `run` but differing in `shell` are distinct: both
/// survive dedup (key is `(run, shell)`) and the hook id embeds the shell, so
/// each records its own last-success slot instead of colliding (INV-4).
#[test]
fn same_run_different_shell_are_distinct_hooks() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("hook.log");
    let run = append_cmd(&log, "fired").replace('"', "\\\"");
    let on_change = format!(
        "[{{ run = \"{run}\", shell = \"sh -c\" }}, {{ run = \"{run}\", shell = \"bash -c\" }}]"
    );
    let cfg = config_with_target_hooks(&fx.url, &td.target_path(), &on_change);

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync runs both same-run/different-shell hooks");

    assert_eq!(
        log_lines(&log).len(),
        2,
        "two hooks with the same run but different shell must BOTH run — they are not \
         duplicates; got {:?}",
        log_lines(&log)
    );
    let mut ids: Vec<String> = fx
        .registry
        .load_hook_state("dest")
        .expect("load hook state")
        .into_iter()
        .map(|h| h.hook_id)
        .collect();
    ids.sort();
    assert_eq!(
        ids.len(),
        2,
        "each shell variant records under a DISTINCT hook id (shell is part of the id, \
         matching the dedup key) — no id collision; got {ids:?}"
    );
    assert_ne!(ids[0], ids[1], "the two recorded hook ids must differ");
}

/// The hook process environment carries `PHORA_TARGET` (the target name),
/// `PHORA_CHANGED` (the deployed file paths of changed artifacts), and
/// `PHORA_CHANGED_NAMES` (their registry-key names), so a hook can react.
#[test]
fn hook_environment_exposes_phora_target_and_changed() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let target_log = td.parent_path.join("target.log");
    let changed_log = td.parent_path.join("changed.log");
    let names_log = td.parent_path.join("names.log");
    // Each var goes to its own file: PHORA_CHANGED is newline-separated, so a
    // shared log would split its paths across unrelated lines.
    let cmd = format!(
        "printf '%s' \"$PHORA_TARGET\" > '{}'; \
         printf '%s' \"$PHORA_CHANGED\" > '{}'; \
         printf '%s' \"$PHORA_CHANGED_NAMES\" > '{}'",
        target_log.display(),
        changed_log.display(),
        names_log.display(),
    );
    let cfg = config_with_target_hooks(
        &fx.url,
        &td.target_path(),
        &format!("\"{}\"", cmd.replace('"', "\\\"")),
    );

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync runs the env-reporting hook");

    let read = |p: &Path| std::fs::read_to_string(p).expect("hook wrote the env var");
    assert_eq!(
        read(&target_log),
        "dest",
        "the hook environment must set PHORA_TARGET to the target name `dest`"
    );

    let deployed = td
        .artifact_dst(&flat_layout(), "editor-src", "editor")
        .to_string_lossy()
        .into_owned();
    let changed = read(&changed_log);
    assert!(
        changed.contains(&deployed),
        "PHORA_CHANGED must hold the DEPLOYED PATH of the changed `editor` artifact \
         ({deployed}), got {changed:?}"
    );

    let names = read(&names_log);
    assert!(
        names.lines().any(|n| n == "editor"),
        "PHORA_CHANGED_NAMES must name the changed artifact member(s) — the `editor` \
         artifact whose digest moved must appear, got {names:?}"
    );
}

/// INV-2 ordering: files are durable BEFORE any hook runs. The hook observes the
/// already-deployed artifact on disk, proving dispatch is strictly post-commit.
#[test]
fn hook_runs_strictly_after_files_are_deployed() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("hook.log");
    let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
    let cmd = format!(
        "if [ -f '{}' ]; then printf 'deployed\\n' >> '{}'; \
         else printf 'missing\\n' >> '{}'; fi",
        dst.join("init.lua").display(),
        log.display(),
        log.display(),
    );
    let cfg = config_with_target_hooks(
        &fx.url,
        &td.target_path(),
        &format!("\"{}\"", cmd.replace('"', "\\\"")),
    );

    sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("sync deploys then fires the hook");

    assert_eq!(
        log_lines(&log),
        vec!["deployed".to_owned()],
        "INV-2: the hook must see the artifact already on disk — files are durable \
         BEFORE any hook is dispatched (strictly post-commit)"
    );
}

/// The global `[hooks] post_sync` with the default `when = always` runs after a
/// sync regardless of any digest change (the escape-hatch).
#[test]
fn global_post_sync_runs_even_on_noop_sync() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let log = td.parent_path.join("post.log");
    let post = append_cmd(&log, "post").replace('"', "\\\"");
    let toml = format!(
        "version = 1\n\n\
         [sources.editor-src]\ngit = \"{}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\"]\nlayout = \"flat\"\n\n\
         [hooks]\npost_sync = \"{post}\"\nwhen = \"always\"\n",
        fx.url,
        td.target_path().display(),
    );
    let cfg = Config::parse(&toml).expect("global post_sync config parses");

    let first = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync runs post_sync");
    assert_eq!(
        log_lines(&log).len(),
        1,
        "post_sync must run after the first sync"
    );

    sync(
        &input(&cfg, None, Some(first.base_lock.clone()), None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("second no-op sync still runs post_sync");

    assert_eq!(
        log_lines(&log).len(),
        2,
        "global post_sync with when=always must run after EVERY sync, including a no-op — \
         the log must hold a second line"
    );
}

/// D1: a removal-only sync (prune drops an artifact, nothing added) must NOT fire
/// `on_change` — the directional changed set is empty — while the global
/// `post_sync` escape hatch still runs, since removal reactions are its job.
#[test]
fn prune_only_sync_skips_on_change_but_runs_post_sync() {
    let fx = build_sync_fixture();
    let td = TargetDir::new();
    let on_change_log = td.parent_path.join("on_change.log");
    let post_log = td.parent_path.join("post.log");
    let on_change_run = append_cmd(&on_change_log, "changed");
    let post_run = append_cmd(&post_log, "post");
    let toml = format!(
        "version = 1\n\n\
         [sources.editor-src]\ngit = \"{}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"editor-src\"]\nlayout = \"flat\"\n\n\
         [targets.dest.hooks]\non_change = \"{}\"\n\n\
         [hooks]\npost_sync = \"{}\"\nwhen = \"always\"\n",
        fx.url,
        td.target_path().display(),
        on_change_run.replace('"', "\\\""),
        post_run.replace('"', "\\\""),
    );
    let cfg = Config::parse(&toml).expect("removal-scenario config parses");

    let first = sync(
        &input(&cfg, None, None, None, false),
        &fx.backend,
        &fx.registry,
    )
    .expect("first sync deploys, fires on_change, runs post_sync");
    assert_eq!(
        log_lines(&on_change_log).len(),
        1,
        "premise: the first changing sync fires on_change once"
    );
    assert_eq!(
        log_lines(&post_log).len(),
        1,
        "premise: post_sync runs after the first sync"
    );

    // Record an extra (now-removed) digest into last-success so the post-prune
    // current set is a strict subset of it: a pure removal, no additions.
    let id = sole_hook_id(&fx.registry, "dest");
    let mut recorded: std::collections::BTreeSet<String> = recorded_hook(&fx.registry, "dest", &id)
        .expect("on_change recorded last-success on first sync")
        .last_success;
    recorded.insert("blake3:orphan".to_owned());
    fx.registry
        .record_hook_success("dest", &id, &recorded)
        .expect("seed last-success with the soon-to-be-removed digest");
    let orphan_dst = seed_orphan(&td, &fx.registry, &flat_layout());

    let in_ = SyncInput {
        base_config: &cfg,
        local_config: None,
        base_lock: Some(first.base_lock.clone()),
        local_lock: None,
        force: false,
        interactive: false,
        prune: true,
        no_hooks: false,
        no_transitive_hooks: false,
        frozen: false,
        resolver: None,
        jobs: None,
    };
    sync(&in_, &fx.backend, &fx.registry).expect("prune-only sync runs");

    assert!(!orphan_dst.exists(), "premise: --prune removed the orphan");
    assert_eq!(
        log_lines(&on_change_log).len(),
        1,
        "D1: a removal-only sync must NOT fire on_change — the directional changed \
         set is empty, so the log keeps exactly the one line from the first run"
    );
    assert_eq!(
        log_lines(&post_log).len(),
        2,
        "D1: the global post_sync escape hatch must still run on a removal-only sync"
    );
}

// ── TPH-009: minijinja render pipeline (Phase 2 core) ──────────
//
// Rendering inserts at STAGE time (before per-file hashing + atomic swap):
//   - a file that `renders()` is fed through minijinja with Config.vars as the
//     context and deploys under its `.tmpl`-stripped `deployed_name()`;
//   - the registry manifest hashes the RENDERED bytes (INV-5), so `verify`
//     re-hashing the deployed file matches;
//   - phora.lock hashes SOURCE bytes, independent of vars (INV-6): two machines
//     with different vars produce the same lock entry;
//   - a render error (undefined var / syntax error) aborts that artifact
//     PRE-swap: it is not deployed and leaves no record, the run reports
//     failure, and sibling artifacts still deploy (warn-and-continue model);
//   - a vars-free, opt-in-free config deploys byte-identically (INV-8);
//   - two in-artifact files mapping to one deployed name is a clear failure,
//     never last-writer-wins.
//
// Every test drives the observable sync/deploy boundary, robust to how the
// template_opt_in / vars plumbing lands internally.

/// A live `GitBackend` + `FileRegistry` whose backing tempdirs outlive the run,
/// paired with a source repo. Lets a render test sync end-to-end against real
/// staging/swap and then read back deployed files, the manifest, and the lock.
struct RenderHarness {
    src: TempDir,
    _git_dir: TempDir,
    _state_dir: TempDir,
    backend: GitBackend,
    registry: FileRegistry,
    url: String,
    #[expect(
        dead_code,
        reason = "constructor records the resolved head for harness symmetry"
    )]
    head: String,
}

impl RenderHarness {
    fn over(src: TempDir, url: String, head: String) -> Self {
        let git_dir = TempDir::new().expect("git dir");
        let state_dir = TempDir::new().expect("state dir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
        Self {
            src,
            _git_dir: git_dir,
            _state_dir: state_dir,
            backend,
            registry,
            url,
            head,
        }
    }
}

/// A repo with one `editor/` artifact holding a `.tmpl` file referencing
/// `{{ greeting }}` plus a plain non-template `keep.txt`. The default `.tmpl`
/// suffix opt-in renders `motd.tmpl` -> `motd`; `keep.txt` is byte-copied.
#[expect(
    clippy::unwrap_used,
    reason = "fixture setup fails loudly; git CLI is assumed present"
)]
fn build_template_repo() -> (TempDir, String, String) {
    let src = TempDir::new().unwrap();
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor")).unwrap();
    std::fs::write(p.join("editor/motd.tmpl"), b"hello {{ greeting }}!\n").unwrap();
    std::fs::write(p.join("editor/keep.txt"), b"verbatim\n").unwrap();
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let head = rev_parse(p, "HEAD");
    let url = p.to_string_lossy().into_owned();
    (src, url, head)
}

/// Config: an optional `[vars]` table plus a `dest` target binding `source`
/// under a flat layout. The bare-string binding uses the default `.tmpl`
/// suffix opt-in (so `.tmpl` files render and are renamed).
fn config_with_vars(source: &str, url: &str, target_path: &Path, vars_toml: &str) -> Config {
    let vars_section = if vars_toml.is_empty() {
        String::new()
    } else {
        format!("[vars]\n{vars_toml}\n")
    };
    let toml = format!(
        "version = 1\n\n{vars_section}\
         [sources.{source}]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
         [targets.dest]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"flat\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("vars + target config parses")
}

#[test]
fn sync_renders_tmpl_file_with_vars_and_strips_suffix() {
    let (src, url, head) = build_template_repo();
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    let cfg = config_with_vars("ed", &h.url, &td.target_path(), "greeting = \"world\"\n");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &h.backend, &h.registry).expect("render sync deploys");

    let dst = td.artifact_dst(&flat_layout(), "ed", "editor");
    assert_eq!(
        std::fs::read(dst.join("motd")).expect("rendered file deployed under stripped name"),
        b"hello world!\n",
        "a .tmpl file must render through minijinja with Config.vars and deploy under its \
         .tmpl-stripped name (deployed_name), substituting {{{{ greeting }}}} -> world"
    );
    assert!(
        !dst.join("motd.tmpl").exists(),
        "the .tmpl suffix must be stripped: no `motd.tmpl` may land at the target"
    );
    assert!(
        !out.had_failures,
        "a clean render deploy must report no failures"
    );
}

#[test]
fn sync_byte_copies_non_template_file_alongside_a_rendered_sibling() {
    let (src, url, head) = build_template_repo();
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    let cfg = config_with_vars("ed", &h.url, &td.target_path(), "greeting = \"world\"\n");
    let in_ = input(&cfg, None, None, None, false);

    sync(&in_, &h.backend, &h.registry).expect("render sync deploys");

    let dst = td.artifact_dst(&flat_layout(), "ed", "editor");
    assert_eq!(
        std::fs::read(dst.join("keep.txt")).expect("plain file deployed verbatim"),
        b"verbatim\n",
        "a non-templated file must be byte-copied unchanged, with no name rewrite, even when a \
         templated sibling in the same artifact renders"
    );
    assert_eq!(
        std::fs::read(dst.join("motd")).expect("rendered sibling deployed"),
        b"hello world!\n",
        "the templated sibling must render and deploy under its stripped name in the same pass \
         that byte-copies the plain file"
    );
}

#[test]
fn manifest_hashes_rendered_bytes_so_verify_passes_on_rendered_output() {
    // INV-5: the registry manifest hashes the RENDERED (deployed) bytes. A `verify`
    // pass re-hashes the on-disk deployed file and compares to record.blake3, so a
    // clean verify is the end-to-end proof the manifest hashed rendered bytes.
    let (src, url, head) = build_template_repo();
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    let cfg = config_with_vars("ed", &h.url, &td.target_path(), "greeting = \"world\"\n");
    let in_ = input(&cfg, None, None, None, false);

    sync(&in_, &h.backend, &h.registry).expect("render sync deploys");

    let rec = h
        .registry
        .get(&artifact_key("dest", "ed", "editor"))
        .expect("registry get")
        .expect("deployed record present");
    let motd = rec
        .files
        .iter()
        .find(|f| f.path == *Path::new("motd"))
        .expect("manifest lists the rendered file under its stripped name `motd`");
    let rendered_hash = blake3::hash(b"hello world!\n").to_hex().to_string();
    assert_eq!(
        motd.blake3, rendered_hash,
        "INV-5: the manifest blake3 for `motd` must hash the RENDERED bytes (hello world!\\n), \
         not the source template bytes; got {}",
        motd.blake3
    );

    let mismatches = crate::sync::verify(&cfg, &h.registry).expect("verify runs");
    assert!(
        mismatches.is_empty(),
        "INV-5: verify re-hashes the deployed rendered file and must match the manifest; \
         mismatches: {mismatches:?}"
    );
}

#[test]
fn lock_hashes_source_bytes_independent_of_vars() {
    // INV-6: phora.lock hashes SOURCE bytes; rendering must not touch it. Two syncs
    // with DIFFERENT vars over the same source/commit must yield the SAME locked
    // digest (and same commit) — the lock is vars-independent.
    let (src_a, url_a, head_a) = build_template_repo();
    let ha = RenderHarness::over(src_a, url_a, head_a);
    let tda = TargetDir::new();
    let cfg_a = config_with_vars("ed", &ha.url, &tda.target_path(), "greeting = \"world\"\n");
    let out_a = sync(
        &input(&cfg_a, None, None, None, false),
        &ha.backend,
        &ha.registry,
    )
    .expect("sync with vars A");
    let locked_a = out_a
        .base_lock
        .find_source("ed")
        .expect("source A locked")
        .clone();

    let (src_b, url_b, head_b) = build_template_repo();
    let hb = RenderHarness::over(src_b, url_b, head_b);
    let tdb = TargetDir::new();
    let cfg_b = config_with_vars("ed", &hb.url, &tdb.target_path(), "greeting = \"galaxy\"\n");
    let out_b = sync(
        &input(&cfg_b, None, None, None, false),
        &hb.backend,
        &hb.registry,
    )
    .expect("sync with vars B");
    let locked_b = out_b
        .base_lock
        .find_source("ed")
        .expect("source B locked")
        .clone();

    assert_eq!(
        locked_a.commit, locked_b.commit,
        "premise: both syncs are over identical source content at the same commit"
    );
    assert_eq!(
        locked_a.digest, locked_b.digest,
        "INV-6: the lock digest hashes SOURCE bytes and must be IDENTICAL across two syncs whose \
         only difference is [vars] (world vs galaxy); rendering must not leak into the lock"
    );
}

#[test]
fn render_error_on_undefined_var_aborts_that_artifact_and_sibling_still_deploys() {
    // A `.tmpl` referencing an UNDEFINED var must fail to render (strict undefined),
    // aborting that artifact PRE-swap: it is not deployed and leaves no record. The
    // sibling plain artifact still deploys; the run reports failure (non-zero).
    let (src, url, head) = {
        let src = TempDir::new().expect("src tempdir");
        let p = src.path();
        run_git(p, &["init", "-b", "main", "."]);
        run_git(p, &["config", "user.email", "test@example.com"]);
        run_git(p, &["config", "user.name", "Test"]);
        // `bad` artifact: a .tmpl referencing an undefined var -> render must fail.
        std::fs::create_dir_all(p.join("bad")).expect("mkdir bad");
        std::fs::write(p.join("bad/conf.tmpl"), b"value = {{ missing }}\n").expect("write tmpl");
        // `good` artifact: plain file, must still deploy despite the sibling failure.
        std::fs::create_dir_all(p.join("good")).expect("mkdir good");
        std::fs::write(p.join("good/plain.txt"), b"ok\n").expect("write plain");
        run_git(p, &["add", "-A"]);
        run_git(p, &["commit", "-m", "init"]);
        let head = rev_parse(p, "HEAD");
        let url = p.to_string_lossy().into_owned();
        (src, url, head)
    };
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    // by-source layout keeps each artifact at its own dst so the sibling check is clean.
    let cfg = config_one_source_one_target("multi", &h.url, "dest", &td.target_path(), "by-source");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &h.backend, &h.registry)
        .expect("a render failure must warn-and-continue, not abort the whole run");

    assert!(
        out.had_failures,
        "a strict-undefined render error must set had_failures=true (sync exits non-zero)"
    );
    let bad_dst = td.target_path().join("multi").join("bad");
    assert!(
        !bad_dst.join("conf").exists() && !bad_dst.join("conf.tmpl").exists(),
        "the failing artifact must NOT be deployed (no partial/garbage file lands pre-swap), \
         found something at {}",
        bad_dst.display()
    );
    assert!(
        h.registry
            .get(&artifact_key("dest", "multi", "bad"))
            .expect("get bad record")
            .is_none(),
        "a render-aborted artifact must leave no registry record"
    );
    let good_dst = td.target_path().join("multi").join("good");
    assert_eq!(
        std::fs::read(good_dst.join("plain.txt")).expect("good artifact deployed"),
        b"ok\n",
        "the sibling artifact must still deploy despite the render failure (warn-and-continue)"
    );
}

#[test]
fn render_error_preserves_prior_deployed_content_pre_swap() {
    // The artifact was deployed clean once; a later commit turns it into a `.tmpl`
    // referencing an undefined var. The failed redeploy must abort PRE-swap, leaving
    // the previously deployed content intact (no clobber with garbage/partial).
    let src = TempDir::new().expect("src tempdir");
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor")).expect("mkdir editor");
    std::fs::write(p.join("editor/conf.txt"), b"first\n").expect("write v1");
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "v1"]);
    let url = p.to_string_lossy().into_owned();
    let h = RenderHarness::over(src, url, String::new());
    let td = TargetDir::new();
    let cfg = config_with_vars("ed", &h.url, &td.target_path(), "");

    let first = sync(
        &input(&cfg, None, None, None, true),
        &h.backend,
        &h.registry,
    )
    .expect("first clean deploy");
    assert!(!first.had_failures, "premise: first deploy is clean");
    let dst = td.artifact_dst(&flat_layout(), "ed", "editor");
    assert_eq!(
        std::fs::read(dst.join("conf.txt")).expect("v1 deployed"),
        b"first\n",
        "premise: the clean v1 content must be deployed"
    );

    // Advance: replace the plain file with a `.tmpl` that references an undefined var.
    std::fs::remove_file(h.src.path().join("editor/conf.txt")).expect("rm v1");
    std::fs::write(h.src.path().join("editor/conf.tmpl"), b"x = {{ nope }}\n")
        .expect("write bad tmpl");
    run_git(h.src.path(), &["add", "-A"]);
    run_git(h.src.path(), &["commit", "-m", "v2-bad-template"]);

    let out = sync(
        &input(&cfg, None, None, None, true),
        &h.backend,
        &h.registry,
    )
    .expect("a render failure must not abort the run");

    assert!(
        out.had_failures,
        "the undefined-var render must fail the run (had_failures=true)"
    );
    assert_eq!(
        std::fs::read(dst.join("conf.txt")).expect("prior content intact"),
        b"first\n",
        "a render error must abort PRE-swap: the previously deployed conf.txt must remain intact, \
         never clobbered by partial/garbage rendered output"
    );
    assert!(
        !dst.join("conf").exists(),
        "no partial rendered `conf` file may land when the render fails pre-swap"
    );
}

#[test]
fn feature_free_config_deploys_byte_identically_with_unchanged_lock_and_manifest() {
    // INV-8: a config using NO vars and NO template opt-in deploys byte-identically
    // to the non-template path. A source whose file simply ENDS in template syntax
    // but is NOT opted in (`template = false` disables even the .tmpl suffix) must
    // be byte-copied verbatim, its name unchanged, and lock/manifest unaffected.
    let src = TempDir::new().expect("src tempdir");
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor")).expect("mkdir editor");
    // Content that LOOKS like a template but must NOT be rendered (no opt-in).
    std::fs::write(p.join("editor/raw.conf"), b"literal {{ greeting }} stays\n")
        .expect("write raw");
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let head = rev_parse(p, "HEAD");
    let url = p.to_string_lossy().into_owned();
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    // No [vars], no template opt-in: the feature-free path.
    let cfg = config_with_vars("ed", &h.url, &td.target_path(), "");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &h.backend, &h.registry).expect("feature-free sync deploys");

    let dst = td.artifact_dst(&flat_layout(), "ed", "editor");
    assert_eq!(
        std::fs::read(dst.join("raw.conf")).expect("raw.conf deployed"),
        b"literal {{ greeting }} stays\n",
        "INV-8: with no vars and no template opt-in, a file containing template-looking syntax \
         must be byte-copied verbatim, NOT rendered"
    );
    assert!(!out.had_failures, "the feature-free deploy must succeed");

    // Literal is valid because run_git pins GIT_*_DATE, making the tree digest deterministic.
    let locked = out.base_lock.find_source("ed").expect("ed locked");
    assert_eq!(
        locked.digest, "blake3:15bd5ad64b9159e3dc1f4352478767ceea090eff38ca6d61d0354c0f0c7fcd18",
        "INV-8: feature-free lock digest is a fixed source-derived value, independent of compute_digest"
    );
}

impl RenderHarness {
    /// Oracle source-bytes digest for INV-8 / INV-6 reuse, mirroring the sync
    /// fixture's `expected_digest_for_source` but over a `RenderHarness` url.
    #[expect(
        dead_code,
        reason = "source-bytes digest oracle kept available for INV-6/INV-8 checks"
    )]
    fn digest_for(&self, source: &ParsedSource, name: &str, commit: &str) -> String {
        let m = crate::kernel::Selection::new(source.includes(), source.excludes())
            .expect("source matcher builds");
        self.backend
            .compute_digest(&sn(name), &self.url, commit, source.root.as_deref(), &m)
            .expect("source digest computes")
    }
}

#[test]
fn deployed_name_collision_within_an_artifact_is_a_clear_failure_not_last_writer_wins() {
    // A single artifact tree holding BOTH `foo` and `foo.tmpl` -> both map to the
    // deployed name `foo`. This must be a clear failure (the artifact is not
    // deployed, leaves no record, run reports failure), never last-writer-wins.
    let src = TempDir::new().expect("src tempdir");
    let p = src.path();
    run_git(p, &["init", "-b", "main", "."]);
    run_git(p, &["config", "user.email", "test@example.com"]);
    run_git(p, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(p.join("editor")).expect("mkdir editor");
    std::fs::write(p.join("editor/foo"), b"plain\n").expect("write foo");
    std::fs::write(p.join("editor/foo.tmpl"), b"templated {{ x }}\n").expect("write foo.tmpl");
    run_git(p, &["add", "-A"]);
    run_git(p, &["commit", "-m", "init"]);
    let head = rev_parse(p, "HEAD");
    let url = p.to_string_lossy().into_owned();
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    let cfg = config_with_vars("ed", &h.url, &td.target_path(), "x = \"v\"\n");
    let in_ = input(&cfg, None, None, None, false);

    let out = sync(&in_, &h.backend, &h.registry)
        .expect("a deployed-name collision must be a per-artifact failure, not a whole-run abort");

    assert!(
        out.had_failures,
        "a deployed-name collision (foo and foo.tmpl both -> foo) must report failure"
    );
    assert!(
        h.registry
            .get(&artifact_key("dest", "ed", "editor"))
            .expect("get record")
            .is_none(),
        "a collided artifact must NOT be deployed (no record), never last-writer-wins"
    );
    let dst = td.artifact_dst(&flat_layout(), "ed", "editor");
    assert!(
        !dst.join("foo").exists(),
        "no `foo` may land when `foo` and `foo.tmpl` collide on the deployed name"
    );
}

#[test]
fn sync_redeploys_when_vars_change_without_new_commit() {
    let (src, url, head) = build_template_repo();
    let h = RenderHarness::over(src, url, head);
    let td = TargetDir::new();
    let dst = td.artifact_dst(&flat_layout(), "ed", "editor");

    let cfg_a = config_with_vars("ed", &h.url, &td.target_path(), "greeting = \"world\"\n");
    let out_a = sync(
        &input(&cfg_a, None, None, None, false),
        &h.backend,
        &h.registry,
    )
    .expect("first vars deploy succeeds");
    assert!(!out_a.had_failures, "premise: the first deploy is clean");
    assert_eq!(
        std::fs::read(dst.join("motd")).expect("rendered motd deployed"),
        b"hello world!\n",
        "premise: greeting=world renders `hello world!`"
    );

    let cfg_b = config_with_vars("ed", &h.url, &td.target_path(), "greeting = \"galaxy\"\n");
    let out_b = sync(
        &input(&cfg_b, None, Some(out_a.base_lock.clone()), None, false),
        &h.backend,
        &h.registry,
    )
    .expect("second deploy with changed vars succeeds");

    assert!(
        !out_b.had_failures,
        "a vars-only redeploy (no new commit) must succeed"
    );
    assert_eq!(
        std::fs::read(dst.join("motd")).expect("re-rendered motd deployed"),
        b"hello galaxy!\n",
        "a [vars] change at the SAME commit must trigger a redeploy: motd must re-render to \
         `hello galaxy!`, not stay pinned at the prior `hello world!` because the commit/source \
         digest is unchanged"
    );
}

// ── PAR-001: parallel fetch/resolve/digest ─────────────────────

/// `Send + Sync` recording backend for the parallel path: counts fetches per URL
/// using `Mutex`/`AtomicUsize` (not `Cell`/`RefCell`) so it can be shared across
/// rayon worker threads. Records the order-insensitive set of fetched URLs.
struct SyncRecordingBackend<'a> {
    inner: &'a GitBackend,
    fetched_urls: Mutex<Vec<String>>,
    total_fetches: AtomicUsize,
}

impl<'a> SyncRecordingBackend<'a> {
    fn new(inner: &'a GitBackend) -> Self {
        Self {
            inner,
            fetched_urls: Mutex::new(Vec::new()),
            total_fetches: AtomicUsize::new(0),
        }
    }

    fn fetch_count_for(&self, url: &str) -> usize {
        self.fetched_urls
            .lock()
            .expect("fetched_urls mutex")
            .iter()
            .filter(|u| u.as_str() == url)
            .count()
    }

    fn total_fetches(&self) -> usize {
        self.total_fetches.load(AtomicOrdering::SeqCst)
    }
}

impl SourceBackend for SyncRecordingBackend<'_> {
    fn fetch(&self, source: &crate::kernel::SourceName, url: &str) -> SourceResult<()> {
        self.total_fetches.fetch_add(1, AtomicOrdering::SeqCst);
        self.fetched_urls
            .lock()
            .expect("fetched_urls mutex")
            .push(url.to_owned());
        self.inner.fetch(source, url)
    }
    fn resolve(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> SourceResult<String> {
        self.inner.resolve(source, url, refspec)
    }
    fn commit_time(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
    ) -> SourceResult<u64> {
        self.inner.commit_time(source, url, commit)
    }
    fn discover_artifacts(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        self.inner
            .discover_artifacts(source, url, commit, root, selection)
    }
    fn export_artifact(&self, req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        self.inner.export_artifact(req)
    }
    fn compute_digest(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        self.inner
            .compute_digest(source, url, commit, root, selection)
    }
    fn list_artifact_files(
        &self,
        source: &crate::kernel::SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &crate::kernel::ArtifactName,
        selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        self.inner
            .list_artifact_files(source, url, commit, root, artifact, selection)
    }
}

/// A [`SyncInput`] carrying an explicit `jobs` pool size. Mirrors [`input`] but
/// sets the PAR-001 `jobs` knob so a test can drive the serial (1) vs parallel (4) paths.
fn input_with_jobs(base: &Config, jobs: usize) -> SyncInput<'_> {
    SyncInput {
        base_config: base,
        local_config: None,
        base_lock: None,
        local_lock: None,
        force: false,
        interactive: false,
        prune: false,
        no_hooks: false,
        no_transitive_hooks: false,
        frozen: false,
        resolver: None,
        jobs: Some(jobs),
    }
}

/// Two distinct sources at distinct config paths but sharing ONE upstream URL,
/// each bound into its own flat target, plus the shared fixture repo URL.
fn config_two_sources_one_url(url: &str, td_a: &Path, td_b: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.alpha]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
             [sources.beta]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
             [targets.ta]\npath = \"{}\"\nsources = [\"alpha\"]\nlayout = \"flat\"\n\n\
             [targets.tb]\npath = \"{}\"\nsources = [\"beta\"]\nlayout = \"flat\"\n",
        td_a.display(),
        td_b.display(),
    );
    Config::parse(&toml).expect("two-source shared-url config parses")
}

/// PAR-001 dedup-by-URL: two DIFFERENT sources that resolve to the SAME upstream URL
/// share ONE bare mirror, so the fetch must run AT MOST ONCE for that URL. Today the
/// dedup is keyed by source name, so this URL is fetched twice (behavioral RED).
#[test]
fn two_sources_sharing_one_url_fetch_the_shared_mirror_once() {
    let fx = build_sync_fixture();
    let td_a = TargetDir::new();
    let td_b = TargetDir::new();
    let cfg = config_two_sources_one_url(&fx.url, &td_a.target_path(), &td_b.target_path());

    let recording = SyncRecordingBackend::new(&fx.backend);
    let in_ = input_with_jobs(&cfg, 4);

    sync(&in_, &recording, &fx.registry).expect("two sources sharing one url sync");

    assert_eq!(
        recording.fetch_count_for(&fx.url),
        1,
        "two sources resolving to the SAME upstream url must fetch the shared mirror EXACTLY once \
         (dedup keyed by normalized url, not source name); got {} fetches of {:?}",
        recording.fetch_count_for(&fx.url),
        recording.fetched_urls.lock().expect("urls").clone(),
    );
    assert_eq!(
        recording.total_fetches(),
        1,
        "the only upstream is shared, so the run must perform exactly one fetch overall"
    );
}

/// `Send + Sync` canned backend recording the per-source `SourceName` handed to
/// `fetch`; the `Mutex` makes it shareable across rayon workers.
struct UrlFetchRecordingBackend {
    fetched_sources: Mutex<Vec<String>>,
}

impl UrlFetchRecordingBackend {
    fn new() -> Self {
        Self {
            fetched_sources: Mutex::new(Vec::new()),
        }
    }

    fn fetched_source_names(&self) -> Vec<String> {
        self.fetched_sources
            .lock()
            .expect("fetched_sources mutex")
            .clone()
    }

    fn was_fetched(&self, source: &str) -> bool {
        self.fetched_source_names().iter().any(|s| s == source)
    }
}

impl SourceBackend for UrlFetchRecordingBackend {
    fn fetch(&self, source: &crate::kernel::SourceName, _url: &str) -> SourceResult<()> {
        self.fetched_sources
            .lock()
            .expect("fetched_sources mutex")
            .push(source.as_str().to_owned());
        Ok(())
    }
    fn resolve(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _refspec: &Refspec,
    ) -> SourceResult<String> {
        Ok("0000000000000000000000000000000000000000".to_owned())
    }
    fn commit_time(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _commit: &str,
    ) -> SourceResult<u64> {
        Ok(1)
    }
    fn discover_artifacts(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<crate::kernel::ArtifactName>> {
        Ok(Vec::new())
    }
    fn export_artifact(&self, _req: &ExportRequest<'_>) -> SourceResult<ExportResult> {
        Ok(ExportResult {
            files: Vec::new(),
            digest: "canned".to_owned(),
            vars_digest: None,
        })
    }
    fn compute_digest(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _selection: &crate::kernel::Selection,
    ) -> SourceResult<String> {
        Ok("canned-digest".to_owned())
    }
    fn list_artifact_files(
        &self,
        _source: &crate::kernel::SourceName,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _artifact: &crate::kernel::ArtifactName,
        _selection: &crate::kernel::Selection,
    ) -> SourceResult<Vec<std::path::PathBuf>> {
        Ok(Vec::new())
    }
}

/// Two URL-mode sources sharing ONE upstream url but with DISTINCT source names,
/// each bound into its own flat target.
fn config_two_url_sources_one_url(url: &str, td_a: &Path, td_b: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.alpha]\nurl = \"{url}\"\n\n\
             [sources.beta]\nurl = \"{url}\"\n\n\
             [targets.ta]\npath = \"{}\"\nsources = [\"alpha\"]\nlayout = \"flat\"\n\n\
             [targets.tb]\npath = \"{}\"\nsources = [\"beta\"]\nlayout = \"flat\"\n",
        td_a.display(),
        td_b.display(),
    );
    Config::parse(&toml).expect("two url-source shared-url config parses")
}

/// PAR-001 integrity-pin regression: for URL-mode (HTTP) sources, `fetch` carries a
/// PER-SOURCE side effect — it validates that source's declared integrity digest
/// against the downloaded bytes. So unlike idempotent git mirror sync, fetch must
/// run for EVERY url-source sharing an upstream url; deduping by normalized url
/// drops the second source's fetch and SILENTLY SKIPS its digest validation
/// (an integrity-pin bypass). Both `alpha` and `beta` must be fetched.
#[test]
fn url_sources_sharing_one_url_each_fetch_so_per_source_digest_is_validated() {
    let td_a = TargetDir::new();
    let td_b = TargetDir::new();
    let url = "https://example.com/pkg.tar.gz";
    let cfg = config_two_url_sources_one_url(url, &td_a.target_path(), &td_b.target_path());

    let recording = UrlFetchRecordingBackend::new();
    let parsed = cfg.parsed_sources().expect("sources parse");
    let remotes = resolved_remotes(&cfg, &parsed).expect("remotes resolve");

    resolve_sources(
        &cfg,
        &parsed,
        &remotes,
        &BTreeMap::new(),
        None,
        &recording,
        false,
        false,
        Some(4),
    )
    .expect("two url sources sharing one url resolve");

    assert!(
        recording.was_fetched("alpha"),
        "url-source `alpha` must be fetched so its per-source digest is validated; \
         fetched sources were {:?}",
        recording.fetched_source_names()
    );
    assert!(
        recording.was_fetched("beta"),
        "url-source `beta` shares the upstream url with `alpha`, but its fetch must NOT be \
         deduped away: each url-source's fetch validates ITS OWN integrity digest, so dropping \
         beta's fetch silently skips beta's digest check (integrity-pin bypass); \
         fetched sources were {:?}",
        recording.fetched_source_names()
    );
}

/// PAR-001 ordering invariant: the SAME multi-source fixture synced effectively
/// serial (jobs=1) and parallel (jobs=4) must produce a BYTE-IDENTICAL base lock
/// (serialized TOML) AND an identical registry record set (keys, commits, digests).
/// Parallel completion order must not leak into recorded state.
#[test]
fn serial_and_parallel_runs_produce_identical_lock_and_registry() {
    let (src_a, url_a) = build_repo_named("alpha-repo", b"-- alpha\n");
    let (src_b, url_b) = build_repo_named("beta-repo", b"-- beta\n");
    let (src_c, url_c) = build_repo_named("gamma-repo", b"-- gamma\n");

    let cfg_toml = |ta: &Path, tb: &Path, tc: &Path| {
        let toml = format!(
            "version = 1\n\n\
                 [sources.alpha]\ngit = \"{url_a}\"\nbranch = \"main\"\n\n\
                 [sources.beta]\ngit = \"{url_b}\"\nbranch = \"main\"\n\n\
                 [sources.gamma]\ngit = \"{url_c}\"\nbranch = \"main\"\n\n\
                 [targets.ta]\npath = \"{}\"\nsources = [\"alpha\"]\nlayout = \"flat\"\n\n\
                 [targets.tb]\npath = \"{}\"\nsources = [\"beta\"]\nlayout = \"flat\"\n\n\
                 [targets.tc]\npath = \"{}\"\nsources = [\"gamma\"]\nlayout = \"flat\"\n",
            ta.display(),
            tb.display(),
            tc.display(),
        );
        Config::parse(&toml).expect("three-source config parses")
    };

    let run = |jobs: usize| -> (String, Vec<(ArtifactKey, String, String)>) {
        let ta = TargetDir::new();
        let tb = TargetDir::new();
        let tc = TargetDir::new();
        let cfg = cfg_toml(&ta.target_path(), &tb.target_path(), &tc.target_path());

        let git_dir = TempDir::new().expect("git dir");
        let state_dir = TempDir::new().expect("state dir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let registry =
            FileRegistry::open(state_dir.path().to_path_buf()).expect("registry over tempdir");

        let in_ = input_with_jobs(&cfg, jobs);
        let out = sync(&in_, &backend, &registry).expect("multi-source sync");

        let lock_toml = toml::to_string(&out.base_lock).expect("base lock serializes to toml");
        let mut records: Vec<(ArtifactKey, String, String)> = registry
            .list_all()
            .expect("registry records")
            .into_iter()
            .map(|r| (r.key, r.commit, r.digest))
            .collect();
        records.sort_by(|a, b| {
            (&a.0.target, &a.0.source, &a.0.artifact).cmp(&(
                &b.0.target,
                &b.0.source,
                &b.0.artifact,
            ))
        });
        (lock_toml, records)
    };

    let (lock_serial, recs_serial) = run(1);
    let (lock_parallel, recs_parallel) = run(4);

    assert_eq!(
        lock_serial, lock_parallel,
        "serial (jobs=1) and parallel (jobs=4) runs must serialize to a byte-identical base lock; \
         parallel completion order must not reorder locked sources"
    );
    assert_eq!(
        recs_serial, recs_parallel,
        "serial and parallel runs must record an identical registry record set \
         (same keys, commits, digests)"
    );

    drop((src_a, src_b, src_c));
}
