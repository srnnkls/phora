//! Top-level orchestration: sync, eject, uneject.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{Config, LayoutKind, Source, Target, merge_configs};
use crate::error::{Error, Result};
use crate::lock::{Lock, LockedSource, merge_locks, source_matches, split_locks};
use crate::matcher::PathMatcher;
use crate::projection::{ArtifactState, Journal, check_artifact_state, deploy_artifact};
use crate::registry::{ArtifactKey, Registry, RegistryRecord};
use crate::source::{ExportRequest, SourceBackend};

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

/// Distinct suffix per call so sibling staging dirs in a shared base never collide.
fn nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub fn sync(
    input: &SyncInput<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
) -> Result<SyncOutput> {
    let effective_config = merge_configs(input.base_config.clone(), input.local_config.cloned());
    validate_source_references(&effective_config)?;
    let effective_lock = match (&input.base_lock, &input.local_lock) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    };

    let local_names: BTreeSet<String> = input
        .local_config
        .map(|lc| lc.sources.keys().cloned().collect())
        .unwrap_or_default();

    let (routed, resolved_commits) = resolve_sources(
        &effective_config,
        effective_lock.as_ref(),
        backend,
        input.force,
    )?;
    let (base_lock, local_lock) = split_locks(routed, &local_names);

    let journal = Journal::open(&registry.locks_dir())?;
    let mut had_failures = false;

    for (target_name, target) in &effective_config.targets {
        had_failures |= deploy_target(
            TargetRun {
                config: &effective_config,
                target_name,
                target,
                commits: &resolved_commits,
                force: input.force,
            },
            backend,
            registry,
            &journal,
        )?;
    }

    if input.prune {
        if had_failures {
            eprintln!("phora: skipping --prune because some artifacts failed to deploy");
        } else {
            prune_orphans(&effective_config, backend, registry, &resolved_commits)?;
        }
    }

    Ok(SyncOutput {
        base_lock,
        local_lock,
        had_failures,
    })
}

fn validate_source_references(config: &Config) -> Result<()> {
    for target in config.targets.values() {
        for source_name in target.resolve_sources(&config.sources) {
            if !config.sources.contains_key(source_name) {
                return Err(Error::Config(format!(
                    "target references undefined source: {source_name}"
                )));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct TargetRun<'a> {
    config: &'a Config,
    target_name: &'a str,
    target: &'a Target,
    commits: &'a BTreeMap<String, String>,
    force: bool,
}

fn deploy_target(
    run: TargetRun<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<bool> {
    let target_path = run.target.expanded_path();
    let layout = run.target.layout();
    let ejected = registry.load_ejected(run.target_name)?;
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut had_failures = false;

    for source_name in run.target.resolve_sources(&run.config.sources) {
        let source = run.config.sources.get(source_name).ok_or_else(|| {
            Error::Config(format!("target references undefined source: {source_name}"))
        })?;
        let commit = &run.commits[source_name];
        let matcher = PathMatcher::new(source.includes(), source.excludes())?;
        let discovered = backend.discover_artifacts(
            source_name,
            &source.git,
            commit,
            source.root.as_deref(),
            &matcher,
        )?;

        for artifact_name in discovered {
            if layout.kind == LayoutKind::Flat {
                if let Some(other) = seen.get(&artifact_name) {
                    return Err(Error::Collision {
                        artifact: artifact_name,
                        sources: vec![other.clone(), source_name.to_owned()],
                        target: run.target_name.to_owned(),
                    });
                }
                seen.insert(artifact_name.clone(), source_name.to_owned());
            }

            let artifact_dst = target_path.join(layout.artifact_path(source_name, &artifact_name));
            let key = ArtifactKey {
                target: run.target_name.to_owned(),
                source: source_name.to_owned(),
                artifact: artifact_name.clone(),
            };

            let state = check_artifact_state(
                &artifact_dst,
                source_name,
                commit,
                &ejected,
                &artifact_name,
                registry,
                &key,
            )?;

            match state {
                ArtifactState::Ejected | ArtifactState::Clean => {}
                ArtifactState::Modified { changed } if !run.force => {
                    eprintln!("phora: skipping locally modified {source_name}:{artifact_name}");
                    for path in &changed {
                        eprintln!("    {}", path.display());
                    }
                    eprintln!("  use --force to overwrite");
                }
                ArtifactState::Foreign if !run.force => {
                    eprintln!(
                        "phora: skipping foreign content at {}; use --force to overwrite",
                        artifact_dst.display()
                    );
                }
                ArtifactState::Missing
                | ArtifactState::Modified { .. }
                | ArtifactState::Foreign => {
                    let deploy = deploy_one(
                        backend,
                        registry,
                        journal,
                        DeployContext {
                            target_path: &target_path,
                            layout_kind: layout.kind,
                            source,
                            source_name,
                            commit,
                            matcher: &matcher,
                            artifact_name: &artifact_name,
                            artifact_dst: &artifact_dst,
                            key,
                        },
                    );
                    if let Err(e) = deploy {
                        eprintln!("phora: failed to deploy {source_name}:{artifact_name}: {e}");
                        had_failures = true;
                    }
                }
            }
        }
    }

    Ok(had_failures)
}

type RoutedSources = (Vec<(String, LockedSource)>, BTreeMap<String, String>);

fn resolve_sources(
    config: &Config,
    effective_lock: Option<&Lock>,
    backend: &dyn SourceBackend,
    force: bool,
) -> Result<RoutedSources> {
    let mut routed = Vec::new();
    let mut resolved_commits = BTreeMap::new();

    for (name, source) in &config.sources {
        let locked = effective_lock.and_then(|l| l.find_source(name));
        let commit = match locked {
            Some(l) if source_matches(source, l) && !force => l.commit.clone(),
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
                digest,
                config_digest: source.config_digest(),
            },
        ));
        resolved_commits.insert(name.clone(), commit);
    }

    Ok((routed, resolved_commits))
}

struct DeployContext<'a> {
    target_path: &'a Path,
    layout_kind: LayoutKind,
    source: &'a Source,
    source_name: &'a str,
    commit: &'a str,
    matcher: &'a PathMatcher,
    artifact_name: &'a str,
    artifact_dst: &'a Path,
    key: ArtifactKey,
}

fn deploy_one(
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
    ctx: DeployContext<'_>,
) -> Result<()> {
    let staging_base = ctx
        .target_path
        .parent()
        .unwrap_or(ctx.target_path)
        .join(".phora-stage");
    let staging = staging_base.join(format!("{}-{}", ctx.artifact_name, nonce()));
    let mut staging_guard = StagingGuard::new(&staging_base, &staging);

    let commit_time = backend.commit_time(ctx.source_name, &ctx.source.git, ctx.commit)?;
    let policy = ctx.source.export_policy();
    let req = ExportRequest {
        source: ctx.source_name,
        url: &ctx.source.git,
        commit: ctx.commit,
        root: ctx.source.root.as_deref(),
        artifact: ctx.artifact_name,
        matcher: ctx.matcher,
        policy: &policy,
        staging_dir: &staging,
        commit_time,
    };
    let export = backend.export_artifact(&req)?;

    if let Some(parent) = ctx.artifact_dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Sync(format!("create target dir {}: {e}", parent.display())))?;
    }

    let record = RegistryRecord {
        version: 1,
        key: ctx.key,
        commit: ctx.commit.to_owned(),
        digest: export.digest,
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: format!("{:?}", ctx.layout_kind).to_lowercase(),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: export.files,
    };

    staging_guard.disarm();
    deploy_artifact(
        &staging_base,
        &staging,
        ctx.artifact_dst,
        record,
        journal,
        registry,
    )
}

/// Removes a half-exported `staging` dir on drop unless [`disarm`](StagingGuard::disarm)
/// hands cleanup to [`deploy_artifact`] on the success path.
struct StagingGuard<'a> {
    staging_base: &'a Path,
    staging: &'a Path,
    armed: bool,
}

impl<'a> StagingGuard<'a> {
    fn new(staging_base: &'a Path, staging: &'a Path) -> Self {
        Self {
            staging_base,
            staging,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = remove_orphan_path(self.staging);
        let _ = std::fs::remove_dir(self.staging_base);
    }
}

fn remove_orphan_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn prune_orphans(
    config: &Config,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    resolved_commits: &BTreeMap<String, String>,
) -> Result<()> {
    let mut expected: HashSet<ArtifactKey> = HashSet::new();
    for (target_name, target) in &config.targets {
        for source_name in target.resolve_sources(&config.sources) {
            let source = config.sources.get(source_name).ok_or_else(|| {
                Error::Config(format!("target references undefined source: {source_name}"))
            })?;
            let commit = &resolved_commits[source_name];
            let matcher = PathMatcher::new(source.includes(), source.excludes())?;
            let discovered = backend.discover_artifacts(
                source_name,
                &source.git,
                commit,
                source.root.as_deref(),
                &matcher,
            )?;
            for artifact in discovered {
                expected.insert(ArtifactKey {
                    target: target_name.clone(),
                    source: source_name.to_owned(),
                    artifact,
                });
            }
        }
    }

    for record in registry.list_all()? {
        if expected.contains(&record.key) {
            continue;
        }
        if let Some(target) = config.targets.get(&record.key.target) {
            let dst = target.expanded_path().join(
                target
                    .layout()
                    .artifact_path(&record.key.source, &record.key.artifact),
            );
            if dst.exists() {
                eprintln!(
                    "phora: pruning orphaned {}:{}",
                    record.key.source, record.key.artifact
                );
                remove_orphan_path(&dst)
                    .map_err(|e| Error::Sync(format!("prune {}: {e}", dst.display())))?;
            }
        }
        registry.remove(&record.key)?;
    }
    Ok(())
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
    /// a matching lock entry suppresses the network round-trip. Also counts
    /// `export_artifact`/`commit_time` so a Clean second run can prove it did not
    /// re-export (exports use deterministic mtimes, so an mtime check alone cannot).
    struct CountingBackend<'a> {
        inner: &'a GitBackend,
        fetches: Cell<usize>,
        resolves: Cell<usize>,
        exports: Cell<usize>,
        commit_times: Cell<usize>,
    }

    impl<'a> CountingBackend<'a> {
        fn new(inner: &'a GitBackend) -> Self {
            Self {
                inner,
                fetches: Cell::new(0),
                resolves: Cell::new(0),
                exports: Cell::new(0),
                commit_times: Cell::new(0),
            }
        }

        fn fetch_count(&self) -> usize {
            self.fetches.get()
        }

        fn resolve_count(&self) -> usize {
            self.resolves.get()
        }

        fn export_count(&self) -> usize {
            self.exports.get()
        }

        fn commit_time_count(&self) -> usize {
            self.commit_times.get()
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
            self.commit_times.set(self.commit_times.get() + 1);
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
            self.exports.set(self.exports.get() + 1);
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

    // ── Phase 2/3 (7b): export/deploy, collision, skip, warn, prune ─

    use std::path::PathBuf;

    use crate::projection::ArtifactState;
    use crate::registry::{ArtifactKey, ManifestFile, RegistryRecord};

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
        fn fetch(&self, source: &str, url: &str) -> Result<()> {
            self.inner.fetch(source, url)
        }
        fn resolve(&self, source: &str, url: &str, refspec: &Refspec) -> Result<String> {
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
            if req.artifact == self.fail_artifact {
                return Err(Error::Source(format!(
                    "injected export failure for {}",
                    req.artifact
                )));
            }
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
        crate::projection::check_artifact_state(
            dst,
            source,
            commit,
            &[],
            artifact,
            reg,
            &artifact_key(target, source, artifact),
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
        let cfg =
            config_one_source_one_target("multi", &url, "dest", &td.target_path(), "by-source");
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
            commit: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
            digest: "blake3:orphan".to_owned(),
            projected_at: "2026-01-01T00:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("old.txt"),
                size: 6,
                mtime: 1_700_000_000,
                blake3: "blake3:orphan".to_owned(),
            }],
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
        fn fetch(&self, source: &str, url: &str) -> Result<()> {
            self.inner.fetch(source, url)
        }
        fn resolve(&self, source: &str, url: &str, refspec: &Refspec) -> Result<String> {
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
            if req.artifact == self.fail_artifact {
                std::fs::create_dir_all(req.staging_dir).expect("create partial staging dir");
                std::fs::write(req.staging_dir.join("partial.txt"), b"half-written\n")
                    .expect("write partial staging file");
                return Err(Error::Source(format!(
                    "injected export failure after partial staging for {}",
                    req.artifact
                )));
            }
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
}
