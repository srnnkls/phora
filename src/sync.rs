//! Top-level orchestration: sync, eject, uneject.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::config::{Config, merge_configs};
use crate::error::{Error, Result};
use crate::lock::{Lock, LockedSource, merge_locks, source_matches, split_locks};
use crate::matcher::PathMatcher;
use crate::registry::Registry;
use crate::source::SourceBackend;

/// Borrowed inputs to [`sync`]: the configs and locks plus run flags. Bundled so
/// the orchestration entry point stays stable as later phases add fields.
pub struct SyncInput<'a> {
    pub base_config: &'a Config,
    pub local_config: Option<&'a Config>,
    pub base_lock: Option<Lock>,
    pub local_lock: Option<Lock>,
    pub force: bool,
    pub interactive: bool,
    pub prune: bool,
}

/// Result of a sync run: the recomputed base and local locks, plus whether any
/// per-artifact export/deploy step failed (the CLI maps this to its exit code).
pub struct SyncOutput {
    pub base_lock: Lock,
    pub local_lock: Option<Lock>,
    pub had_failures: bool,
}

/// A source resolved during Phase 1: its commit and the artifact-tree digest.
#[expect(
    dead_code,
    reason = "Phase 2 (7b) consumes commit/digest when exporting to targets"
)]
struct ResolvedSource {
    commit: String,
    digest: String,
}

pub fn sync(
    input: &SyncInput<'_>,
    backend: &dyn SourceBackend,
    _registry: &dyn Registry,
) -> Result<SyncOutput> {
    let effective_config = merge_configs(input.base_config.clone(), input.local_config.cloned());
    let effective_lock = match (&input.base_lock, &input.local_lock) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    };

    let local_names: BTreeSet<String> = input
        .local_config
        .map(|lc| lc.sources.keys().cloned().collect())
        .unwrap_or_default();

    let mut resolved_sources: BTreeMap<String, ResolvedSource> = BTreeMap::new();
    let mut routed: Vec<(String, LockedSource)> = Vec::new();

    for (name, source) in &effective_config.sources {
        let locked = effective_lock.as_ref().and_then(|l| l.find_source(name));

        let commit = match locked {
            Some(l) if source_matches(source, l) && !input.force => l.commit.clone(),
            _ => {
                backend.fetch(name, &source.git)?;
                backend.resolve(name, &source.git, &source.refspec())?
            }
        };

        let matcher = PathMatcher::new(source.includes(), source.excludes())?;
        let digest =
            backend.compute_digest(name, &source.git, &commit, source.root.as_deref(), &matcher)?;

        routed.push((
            name.clone(),
            LockedSource {
                name: name.clone(),
                git: source.git.clone(),
                resolved: source.refspec().to_string(),
                commit: commit.clone(),
                digest: digest.clone(),
                config_digest: source.config_digest(),
            },
        ));

        resolved_sources.insert(name.clone(), ResolvedSource { commit, digest });
    }

    let (base_lock, local_lock) = split_locks(routed, &local_names);

    // Phase 2/3: deploy + prune land in 7b.

    Ok(SyncOutput {
        base_lock,
        local_lock,
        had_failures: false,
    })
}

pub fn eject(
    _config: &Config,
    _registry: &dyn Registry,
    _artifact: &str,
    _source: &str,
    _target: &str,
) -> Result<()> {
    Err(Error::NotImplemented("eject"))
}

pub fn uneject(
    _config: &Config,
    _registry: &dyn Registry,
    _artifact: &str,
    _source: &str,
    _target: &str,
) -> Result<()> {
    Err(Error::NotImplemented("uneject"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use crate::config::Refspec;
    use crate::registry::FileRegistry;
    use crate::source::{ExportRequest, ExportResult, GitBackend, SourceBackend};

    // ── git fixture ────────────────────────────────────────────────

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn run_git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
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
    /// a matching lock entry suppresses the network round-trip.
    struct CountingBackend<'a> {
        inner: &'a GitBackend,
        fetches: Cell<usize>,
        resolves: Cell<usize>,
    }

    impl<'a> CountingBackend<'a> {
        fn new(inner: &'a GitBackend) -> Self {
            Self {
                inner,
                fetches: Cell::new(0),
                resolves: Cell::new(0),
            }
        }

        fn fetch_count(&self) -> usize {
            self.fetches.get()
        }

        fn resolve_count(&self) -> usize {
            self.resolves.get()
        }
    }

    impl SourceBackend for CountingBackend<'_> {
        fn fetch(&self, source: &str, url: &str) -> Result<()> {
            self.fetches.set(self.fetches.get() + 1);
            self.inner.fetch(source, url)
        }

        fn resolve(&self, source: &str, url: &str, refspec: &Refspec) -> Result<String> {
            self.resolves.set(self.resolves.get() + 1);
            self.inner.resolve(source, url, refspec)
        }

        fn commit_time(&self, source: &str, url: &str, commit: &str) -> Result<u64> {
            self.inner.commit_time(source, url, commit)
        }

        fn discover_artifacts(
            &self,
            source: &str,
            url: &str,
            commit: &str,
            root: Option<&Path>,
            matcher: &crate::matcher::PathMatcher,
        ) -> Result<Vec<String>> {
            self.inner
                .discover_artifacts(source, url, commit, root, matcher)
        }

        fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult> {
            self.inner.export_artifact(req)
        }

        fn compute_digest(
            &self,
            source: &str,
            url: &str,
            commit: &str,
            root: Option<&Path>,
            matcher: &crate::matcher::PathMatcher,
        ) -> Result<String> {
            self.inner
                .compute_digest(source, url, commit, root, matcher)
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
        }
    }

    fn expected_digest(fx: &SyncFixture, name: &str, commit: &str) -> String {
        let m = crate::matcher::PathMatcher::new(&[], &[]).expect("empty matcher builds");
        fx.backend
            .compute_digest(name, &fx.url, commit, None, &m)
            .expect("digest computes over fixture tree")
    }

    /// Oracle digest computed with the SAME matcher and root the source declares.
    /// Differs from [`expected_digest`] whenever sync ignores include/exclude/root.
    fn expected_digest_for_source(
        fx: &SyncFixture,
        source: &crate::config::Source,
        name: &str,
        commit: &str,
    ) -> String {
        let m = crate::matcher::PathMatcher::new(source.includes(), source.excludes())
            .expect("source matcher builds");
        fx.backend
            .compute_digest(name, &fx.url, commit, source.root.as_deref(), &m)
            .expect("scoped digest computes over fixture tree")
    }

    fn config_digest_of(cfg: &Config, name: &str) -> String {
        cfg.sources
            .get(name)
            .expect("source present")
            .config_digest()
    }

    // ── Phase 1: resolve + lock build ──────────────────────────────

    #[test]
    fn resolves_source_with_no_prior_lock_into_base_lock() {
        let fx = build_sync_fixture();
        let cfg = config_with_scoped_source("editor-src", &fx.url);
        let source = cfg.sources.get("editor-src").expect("source present");
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
            expected_digest_for_source(&fx, source, "editor-src", &fx.head_sha),
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

    // ── Phase 1: source_matches skip (no refetch) ──────────────────

    #[test]
    fn matching_lock_reuses_commit_without_refetch() {
        let fx = build_sync_fixture();
        let cfg = config_with_source("editor-src", &fx.url);
        let source = cfg.sources.get("editor-src").expect("source present");

        // Pre-seed the mirror so compute_digest can read the tree without sync fetching.
        fx.backend.fetch("editor-src", &fx.url).expect("seed fetch");

        let prior = Lock {
            version: 1,
            sources: vec![crate::lock::LockedSource {
                name: "editor-src".to_owned(),
                git: fx.url.clone(),
                resolved: source.refspec().to_string(),
                commit: fx.head_sha.clone(),
                digest: expected_digest(&fx, "editor-src", &fx.head_sha),
                config_digest: source.config_digest(),
            }],
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
            }],
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

    // ── Phase 1: --force re-resolves ───────────────────────────────

    #[test]
    fn force_refetches_even_when_lock_matches() {
        let fx = build_sync_fixture();
        let cfg = config_with_source("editor-src", &fx.url);
        let source = cfg.sources.get("editor-src").expect("source present");
        fx.backend.fetch("editor-src", &fx.url).expect("seed fetch");

        let matching = Lock {
            version: 1,
            sources: vec![crate::lock::LockedSource {
                name: "editor-src".to_owned(),
                git: fx.url.clone(),
                resolved: source.refspec().to_string(),
                commit: fx.head_sha.clone(),
                digest: expected_digest(&fx, "editor-src", &fx.head_sha),
                config_digest: source.config_digest(),
            }],
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

        let out =
            sync(&in_, &fx.backend, &fx.registry).expect("sync resolves the overridden source");

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
}
