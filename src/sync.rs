//! Top-level orchestration: sync, eject, uneject.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{Config, DeployMode, LayoutKind, Protocol, Source, Target, merge_configs};
use crate::error::{Error, Result};
use crate::lock::{Lock, LockedSource, merge_locks, source_matches, split_locks};
use crate::matcher::PathMatcher;
use crate::projection::{
    ArtifactState, Journal, check_artifact_state, deploy_artifact, link_artifact, recovery_sweep,
};
use crate::registry::{ArtifactKey, EjectedEntry, ManifestFile, Registry, RegistryRecord};
use crate::source::{ExportRequest, SourceBackend, is_local_path, read_local_head};

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
    pub resolver: Option<&'a dyn ConflictResolver>,
}

/// How the user wants a single Modified/Foreign conflict handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Skip,
    Overwrite,
    Eject,
    Abort,
}

/// What kind of conflict surfaced at an artifact destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    Modified { changed: Vec<std::path::PathBuf> },
    Foreign,
}

/// A single conflict presented to a [`ConflictResolver`] during interactive sync.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub target: String,
    pub source: String,
    pub artifact: String,
    pub kind: ConflictKind,
}

/// Decides how to resolve each Modified/Foreign conflict in interactive sync.
pub trait ConflictResolver {
    fn resolve(&self, conflict: &Conflict) -> Resolution;
}

/// Result of a sync run: the recomputed base and local locks, plus whether any
/// per-artifact export/deploy step failed (the CLI maps this to its exit code).
pub struct SyncOutput {
    pub base_lock: Lock,
    pub local_lock: Option<Lock>,
    pub had_failures: bool,
}

/// A relative target path yields an empty (`""`) or absent parent; both normalize
/// to `.` so `recovery_sweep` scans exactly the dir deploy stages into.
fn target_parent(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The protocol a source resolves under: its own, else the global default, else https.
fn effective_protocol(source: &Source, config: &Config) -> Protocol {
    source
        .protocol
        .or(config.protocol)
        .unwrap_or(Protocol::Https)
}

/// Resolves every source's concrete remote once, keyed by source name. A resolution
/// failure (unknown host, missing protocol template) surfaces named by source.
fn resolved_remotes(config: &Config) -> Result<BTreeMap<String, String>> {
    let mut remotes = BTreeMap::new();
    for (name, source) in &config.sources {
        let protocol = effective_protocol(source, config);
        let remote = source
            .resolved_remote(&config.hosts, protocol)
            .map_err(|e| Error::Config(format!("source `{name}`: {e}")))?;
        remotes.insert(name.clone(), remote);
    }
    Ok(remotes)
}

fn remote_for<'a>(remotes: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    remotes
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| Error::Config(format!("no resolved remote for source `{name}`")))
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
    effective_config.validate()?;
    validate_source_references(&effective_config)?;
    let remotes = resolved_remotes(&effective_config)?;
    validate_link_mode(input.base_config, &effective_config, &remotes)?;
    let effective_lock = match (&input.base_lock, &input.local_lock) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    };

    let local_names: BTreeSet<String> = input
        .local_config
        .map(|lc| lc.sources.keys().cloned().collect())
        .unwrap_or_default();

    let journal = Journal::open(&registry.locks_dir())?;

    let mut swept_parents: BTreeSet<PathBuf> = BTreeSet::new();
    for target in effective_config.targets.values() {
        let parent = target_parent(&target.expanded_path());
        if swept_parents.insert(parent.clone()) {
            recovery_sweep(&parent, &journal, registry)?;
        }
    }

    let (routed, resolved_commits) = resolve_sources(
        &effective_config,
        &remotes,
        effective_lock.as_ref(),
        backend,
        input.force,
    )?;
    let (base_lock, local_lock) = split_locks(routed, &local_names);

    let mut had_failures = false;

    for (target_name, target) in &effective_config.targets {
        had_failures |= deploy_target(
            TargetRun {
                config: &effective_config,
                target_name,
                target,
                commits: &resolved_commits,
                remotes: &remotes,
                force: input.force,
                interactive: input.interactive,
                resolver: input.resolver,
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
            prune_orphans(
                &effective_config,
                &remotes,
                backend,
                registry,
                &resolved_commits,
            )?;
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

fn validate_link_mode(
    base: &Config,
    effective: &Config,
    remotes: &BTreeMap<String, String>,
) -> Result<()> {
    for (name, source) in &base.sources {
        if source.deploy_mode() == DeployMode::Link {
            return Err(Error::Config(format!(
                "source `{name}`: deploy = \"link\" is only allowed in phora.local.toml, \
                 not the committed config"
            )));
        }
    }
    for (name, source) in &effective.sources {
        let git = remote_for(remotes, name)?;
        if source.deploy_mode() == DeployMode::Link && !is_local_path(git) {
            return Err(Error::Config(format!(
                "source `{name}`: deploy = \"link\" requires a local filesystem path, \
                 not a remote URL `{git}`"
            )));
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
    remotes: &'a BTreeMap<String, String>,
    force: bool,
    interactive: bool,
    resolver: Option<&'a dyn ConflictResolver>,
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
        let git = remote_for(run.remotes, source_name)?;
        let matcher = PathMatcher::new(source.includes(), source.excludes())?;
        let discovered =
            discover_artifacts_for_source(source, git, source_name, commit, backend, &matcher)?;

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
            let dst_is_symlink =
                std::fs::symlink_metadata(&artifact_dst).is_ok_and(|m| m.file_type().is_symlink());
            let mode_transition = match source.deploy_mode() {
                DeployMode::Link => artifact_dst.exists() && !dst_is_symlink,
                DeployMode::Copy => dst_is_symlink,
            };

            let entry = ArtifactEntry {
                source,
                git,
                source_name,
                commit,
                matcher: &matcher,
                artifact_name: &artifact_name,
                target_path: &target_path,
                layout_kind: layout.kind,
                ejected: &ejected,
                mode_transition,
            };
            had_failures |= deploy_artifact_entry(run, &entry, backend, registry, journal)?;
        }
    }

    Ok(had_failures)
}

struct ArtifactEntry<'a> {
    source: &'a Source,
    git: &'a str,
    source_name: &'a str,
    commit: &'a str,
    matcher: &'a PathMatcher,
    artifact_name: &'a str,
    target_path: &'a Path,
    layout_kind: LayoutKind,
    ejected: &'a [EjectedEntry],
    mode_transition: bool,
}

fn deploy_artifact_entry(
    run: TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<bool> {
    let artifact_dst = entry.target_path.join(
        run.target
            .layout()
            .artifact_path(entry.source_name, entry.artifact_name),
    );
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.source_name.to_owned(),
        artifact: entry.artifact_name.to_owned(),
    };

    let state = check_artifact_state(
        &artifact_dst,
        entry.source_name,
        entry.commit,
        entry.ejected,
        entry.artifact_name,
        registry,
        &key,
    )?;

    let conflict_kind = match &state {
        _ if entry.mode_transition => None,
        ArtifactState::Modified { changed } if !run.force => Some(ConflictKind::Modified {
            changed: changed.clone(),
        }),
        ArtifactState::Foreign if !run.force => Some(ConflictKind::Foreign),
        _ => None,
    };

    let deploy = |key: ArtifactKey| match entry.source.deploy_mode() {
        DeployMode::Link => deploy_link(registry, journal, entry, &artifact_dst, key),
        DeployMode::Copy => deploy_one(
            backend,
            registry,
            journal,
            DeployContext {
                target_path: entry.target_path,
                layout_kind: entry.layout_kind,
                source: entry.source,
                git: entry.git,
                source_name: entry.source_name,
                commit: entry.commit,
                matcher: entry.matcher,
                artifact_name: entry.artifact_name,
                artifact_dst: &artifact_dst,
                key,
            },
        ),
    };

    let resolution = match conflict_kind {
        None if !entry.mode_transition
            && matches!(
                state,
                ArtifactState::Ejected | ArtifactState::Clean | ArtifactState::Linked
            ) =>
        {
            return Ok(false);
        }
        None => Resolution::Overwrite,
        Some(kind) => match run.resolver {
            Some(resolver) if run.interactive => resolver.resolve(&Conflict {
                target: run.target_name.to_owned(),
                source: entry.source_name.to_owned(),
                artifact: entry.artifact_name.to_owned(),
                kind,
            }),
            _ => {
                warn_skip(entry.source_name, entry.artifact_name, &kind, &artifact_dst);
                Resolution::Skip
            }
        },
    };

    match resolution {
        Resolution::Skip => Ok(false),
        Resolution::Overwrite => match deploy(key) {
            Ok(()) => Ok(false),
            Err(e) => {
                eprintln!(
                    "phora: failed to deploy {}:{}: {e}",
                    entry.source_name, entry.artifact_name
                );
                Ok(true)
            }
        },
        Resolution::Eject => {
            let mut ejected = registry.load_ejected(run.target_name)?;
            ejected.push(EjectedEntry {
                source: entry.source_name.to_owned(),
                artifact: entry.artifact_name.to_owned(),
                ejected_at: chrono::Utc::now().to_rfc3339(),
            });
            registry.save_ejected(run.target_name, &ejected)?;
            registry.remove(&key)?;
            Ok(false)
        }
        Resolution::Abort => Err(Error::Aborted),
    }
}

fn warn_skip(source: &str, artifact: &str, kind: &ConflictKind, dst: &Path) {
    match kind {
        ConflictKind::Modified { changed } => {
            eprintln!("phora: skipping locally modified {source}:{artifact}");
            for path in changed {
                eprintln!("    {}", path.display());
            }
            eprintln!("  use --force to overwrite");
        }
        ConflictKind::Foreign => {
            eprintln!(
                "phora: skipping foreign content at {}; use --force to overwrite",
                dst.display()
            );
        }
    }
}

type RoutedSources = (Vec<(String, LockedSource)>, BTreeMap<String, String>);

fn resolve_sources(
    config: &Config,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &dyn SourceBackend,
    force: bool,
) -> Result<RoutedSources> {
    let mut routed = Vec::new();
    let mut resolved_commits = BTreeMap::new();

    for (name, source) in &config.sources {
        let git = remote_for(remotes, name)?;
        if source.deploy_mode() == DeployMode::Link {
            let commit = read_local_head(git)?;
            routed.push((
                name.clone(),
                LockedSource {
                    name: name.clone(),
                    git: git.to_owned(),
                    resolved: "link".to_owned(),
                    commit: commit.clone(),
                    digest: "link:".to_owned(),
                    config_digest: source.config_digest(),
                },
            ));
            resolved_commits.insert(name.clone(), commit);
            continue;
        }

        let locked = effective_lock.and_then(|l| l.find_source(name));
        let commit = match locked {
            Some(l) if source_matches(source, l) && !force => l.commit.clone(),
            _ => {
                backend.fetch(name, git)?;
                backend.resolve(name, git, &source.refspec())?
            }
        };

        let matcher = PathMatcher::new(source.includes(), source.excludes())?;
        let digest =
            backend.compute_digest(name, git, &commit, source.root.as_deref(), &matcher)?;

        routed.push((
            name.clone(),
            LockedSource {
                name: name.clone(),
                git: git.to_owned(),
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
    git: &'a str,
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
    let staging_base = target_parent(ctx.target_path).join(".phora-stage");
    let staging = staging_base.join(format!("{}-{}", ctx.artifact_name, nonce()));
    let mut staging_guard = StagingGuard::new(&staging_base, &staging);

    let git = ctx.git;
    let commit_time = backend.commit_time(ctx.source_name, git, ctx.commit)?;
    let policy = ctx.source.export_policy();
    let req = ExportRequest {
        source: ctx.source_name,
        url: git,
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
        linked: false,
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

fn deploy_link(
    registry: &dyn Registry,
    journal: &Journal,
    entry: &ArtifactEntry<'_>,
    artifact_dst: &Path,
    key: ArtifactKey,
) -> Result<()> {
    let policy = entry.source.export_policy();
    let record = RegistryRecord {
        version: 1,
        key,
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: format!("{:?}", entry.layout_kind).to_lowercase(),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
    };
    let staging_base = target_parent(entry.target_path).join(".phora-stage");
    link_artifact(
        &staging_base,
        artifact_dst,
        &link_target(entry),
        record,
        journal,
        registry,
    )
}

/// Absolute working-tree path the symlink points at: `<remote>/<root>/<artifact>`.
fn link_target(entry: &ArtifactEntry<'_>) -> PathBuf {
    let base = Path::new(entry.git);
    let mut target = if base.is_absolute() {
        base.to_path_buf()
    } else {
        base.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir().map_or_else(|_| base.to_path_buf(), |c| c.join(base))
        })
    };
    if let Some(root) = &entry.source.root {
        target.push(root);
    }
    target.push(entry.artifact_name);
    target
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

/// Discover artifact directories by scanning the live working tree at
/// `<git>/<root>` (Link mode). Mirrors the ODB `discover_artifacts`: only
/// directory entries become artifacts, dotfiles are skipped, the matcher gates
/// inclusion, and the result is sorted. A missing path/root is an error.
fn discover_working_tree(
    git: &Path,
    root: Option<&Path>,
    matcher: &PathMatcher,
) -> Result<Vec<String>> {
    let base = root.map_or_else(|| git.to_path_buf(), |r| git.join(r));
    let entries = std::fs::read_dir(&base)
        .map_err(|e| Error::Sync(format!("scan working tree {}: {e}", base.display())))?;

    let mut artifacts = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read entry in {}: {e}", base.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !matcher.allows_artifact(&name) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            artifacts.push(name);
        }
    }

    artifacts.sort();
    Ok(artifacts)
}

fn discover_artifacts_for_source(
    source: &Source,
    git: &str,
    source_name: &str,
    commit: &str,
    backend: &dyn SourceBackend,
    matcher: &PathMatcher,
) -> Result<Vec<String>> {
    match source.deploy_mode() {
        DeployMode::Link => {
            discover_working_tree(Path::new(git), source.root.as_deref(), matcher)
        }
        DeployMode::Copy => backend.discover_artifacts(
            source_name,
            git,
            commit,
            source.root.as_deref(),
            matcher,
        ),
    }
}

fn prune_orphans(
    config: &Config,
    remotes: &BTreeMap<String, String>,
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
            let git = remote_for(remotes, source_name)?;
            let matcher = PathMatcher::new(source.includes(), source.excludes())?;
            let discovered =
                discover_artifacts_for_source(source, git, source_name, commit, backend, &matcher)?;
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
    config: &Config,
    registry: &dyn Registry,
    artifact: &str,
    source: &str,
    target: &str,
) -> Result<()> {
    if !config.targets.contains_key(target) {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    let key = ArtifactKey {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
    };
    if registry.get(&key)?.is_none() {
        return Err(Error::Registry(format!(
            "{source}/{artifact} is not managed in target {target}"
        )));
    }

    let mut ejected = registry.load_ejected(target)?;
    let already = ejected
        .iter()
        .any(|e| e.source == source && e.artifact == artifact);
    if !already {
        ejected.push(EjectedEntry {
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            ejected_at: chrono::Utc::now().to_rfc3339(),
        });
        registry.save_ejected(target, &ejected)?;
    }
    registry.remove(&key)
}

pub fn uneject(
    config: &Config,
    registry: &dyn Registry,
    artifact: &str,
    source: &str,
    target: &str,
) -> Result<()> {
    if !config.targets.contains_key(target) {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    let mut ejected = registry.load_ejected(target)?;
    ejected.retain(|e| !(e.source == source && e.artifact == artifact));
    registry.save_ejected(target, &ejected)
}

/// Why a deployed file failed verification against its registry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyReason {
    /// The deployed file's content hash differs from the recorded `blake3`.
    ContentMismatch { expected: String, actual: String },
    /// The recorded file is absent on disk at the deployed location.
    Missing,
}

/// A single deployed file that does not match its registry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyMismatch {
    pub key: ArtifactKey,
    pub path: std::path::PathBuf,
    pub reason: VerifyReason,
}

pub fn verify(config: &Config, registry: &dyn Registry) -> Result<Vec<VerifyMismatch>> {
    let mut mismatches = Vec::new();
    for record in registry.list_all()? {
        if record.linked {
            continue;
        }
        let Some(target) = config.targets.get(&record.key.target) else {
            continue;
        };
        let artifact_dir = target.expanded_path().join(
            target
                .layout()
                .artifact_path(&record.key.source, &record.key.artifact),
        );
        for file in &record.files {
            let dst = artifact_dir.join(&file.path);
            match std::fs::read(&dst) {
                Ok(content) => {
                    let actual = blake3::hash(&content).to_hex().to_string();
                    if actual != file.blake3 {
                        mismatches.push(VerifyMismatch {
                            key: record.key.clone(),
                            path: file.path.clone(),
                            reason: VerifyReason::ContentMismatch {
                                expected: file.blake3.clone(),
                                actual,
                            },
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    mismatches.push(VerifyMismatch {
                        key: record.key.clone(),
                        path: file.path.clone(),
                        reason: VerifyReason::Missing,
                    });
                }
                Err(e) => {
                    return Err(Error::Sync(format!("verify read {}: {e}", dst.display())));
                }
            }
        }
    }
    Ok(mismatches)
}

/// Summary of a [`rebuild_registry`] run: which artifacts were reconstructed and
/// which on-disk content failed to match the recomputed hash or lacked any config
/// match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// Managed artifacts whose registry record was reconstructed from mirror + disk.
    pub reconstructed: Vec<ArtifactKey>,
    /// Managed artifacts whose on-disk content fails the recomputed per-file hash.
    pub modified: Vec<ArtifactKey>,
    /// On-disk artifact dirs under a target with no matching config/lock source.
    pub foreign: Vec<std::path::PathBuf>,
}

pub fn rebuild_registry(
    config: &Config,
    lock: &Lock,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
) -> Result<RebuildReport> {
    let mut report = RebuildReport::default();
    let remotes = resolved_remotes(config)?;

    for (target_name, target) in &config.targets {
        let mut managed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for source_name in target.resolve_sources(&config.sources) {
            let source = config.sources.get(source_name).ok_or_else(|| {
                Error::Config(format!("target references undefined source: {source_name}"))
            })?;
            let locked = lock.find_source(source_name).ok_or_else(|| {
                Error::Sync(format!(
                    "no locked commit for source {source_name}; run sync first"
                ))
            })?;
            let commit = &locked.commit;
            let git = remote_for(&remotes, source_name)?;
            let matcher = PathMatcher::new(source.includes(), source.excludes())?;
            let policy = source.export_policy();
            let discovered =
                discover_artifacts_for_source(source, git, source_name, commit, backend, &matcher)?;

            for artifact in discovered {
                let key = ArtifactKey {
                    target: target_name.clone(),
                    source: source_name.to_owned(),
                    artifact: artifact.clone(),
                };
                let artifact_dst = target
                    .expanded_path()
                    .join(target.layout().artifact_path(source_name, &artifact));

                match source.deploy_mode() {
                    DeployMode::Link => {
                        rebuild_linked(registry, &policy, target.layout().kind, key, &mut report)?;
                    }
                    DeployMode::Copy => rebuild_one(RebuildOne {
                        backend,
                        registry,
                        source,
                        git,
                        source_name,
                        commit,
                        matcher: &matcher,
                        policy: &policy,
                        artifact: &artifact,
                        artifact_dst: &artifact_dst,
                        layout_kind: target.layout().kind,
                        key,
                        report: &mut report,
                    })?,
                }

                managed
                    .entry(source_name.to_owned())
                    .or_default()
                    .insert(artifact);
            }
        }

        report.foreign.extend(scan_foreign(target, &managed)?);
    }

    Ok(report)
}

struct RebuildOne<'a> {
    backend: &'a dyn SourceBackend,
    registry: &'a dyn Registry,
    source: &'a Source,
    git: &'a str,
    source_name: &'a str,
    commit: &'a str,
    matcher: &'a PathMatcher,
    policy: &'a crate::source::ExportPolicy,
    artifact: &'a str,
    artifact_dst: &'a Path,
    layout_kind: LayoutKind,
    key: ArtifactKey,
    report: &'a mut RebuildReport,
}

fn rebuild_one(args: RebuildOne<'_>) -> Result<()> {
    let RebuildOne {
        backend,
        registry,
        source,
        git,
        source_name,
        commit,
        matcher,
        policy,
        artifact,
        artifact_dst,
        layout_kind,
        key,
        report,
    } = args;

    let staging_base = std::env::temp_dir().join("phora-rebuild");
    let staging = staging_base.join(format!("{artifact}-{}-{}", std::process::id(), nonce()));
    let _guard = StagingGuard::new(&staging_base, &staging);

    let commit_time = backend.commit_time(source_name, git, commit)?;
    let export = backend.export_artifact(&ExportRequest {
        source: source_name,
        url: git,
        commit,
        root: source.root.as_deref(),
        artifact,
        matcher,
        policy,
        staging_dir: &staging,
        commit_time,
    })?;

    let mut modified = false;
    let mut files = Vec::with_capacity(export.files.len());
    for mf in export.files {
        let on_disk = artifact_dst.join(&mf.path);
        let (size, mtime) = if let Some(actual) = disk_hash(&on_disk)? {
            if actual.hash != mf.blake3 {
                modified = true;
            }
            (actual.size, actual.mtime)
        } else {
            modified = true;
            (mf.size, mf.mtime)
        };
        files.push(ManifestFile {
            path: mf.path,
            size,
            mtime,
            blake3: mf.blake3,
        });
    }

    let record = RegistryRecord {
        version: 1,
        key: key.clone(),
        commit: commit.to_owned(),
        digest: export.digest,
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: format!("{layout_kind:?}").to_lowercase(),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files,
        linked: false,
    };
    registry.put(&record)?;
    report.reconstructed.push(key.clone());
    if modified {
        report.modified.push(key);
    }
    Ok(())
}

/// Reconstruct a linked artifact's registry record without hashing or export:
/// a link source has no mirror, so its marker is synthesized from disk discovery.
fn rebuild_linked(
    registry: &dyn Registry,
    policy: &crate::source::ExportPolicy,
    layout_kind: LayoutKind,
    key: ArtifactKey,
    report: &mut RebuildReport,
) -> Result<()> {
    let record = RegistryRecord {
        version: 1,
        key: key.clone(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: format!("{layout_kind:?}").to_lowercase(),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
    };
    registry.put(&record)?;
    report.reconstructed.push(key);
    Ok(())
}

struct DiskHash {
    hash: String,
    size: u64,
    mtime: u64,
}

/// `Ok(None)` when the file is absent on disk.
fn disk_hash(path: &Path) -> Result<Option<DiskHash>> {
    let content = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Sync(format!("read {}: {e}", path.display()))),
    };
    let meta = std::fs::metadata(path)
        .map_err(|e| Error::Sync(format!("stat {}: {e}", path.display())))?;
    let mtime = filetime::FileTime::from_last_modification_time(&meta).unix_seconds();
    Ok(Some(DiskHash {
        hash: blake3::hash(&content).to_hex().to_string(),
        size: meta.len(),
        mtime: u64::try_from(mtime).unwrap_or(0),
    }))
}

/// On-disk artifact dirs under `target` that no managed `(source, artifact)` maps to.
fn scan_foreign(
    target: &Target,
    managed: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<PathBuf>> {
    let target_path = target.expanded_path();
    let layout = target.layout();
    let mut foreign = Vec::new();

    let entries = match std::fs::read_dir(&target_path) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(foreign),
        Err(e) => {
            return Err(Error::Sync(format!(
                "read target dir {}: {e}",
                target_path.display()
            )));
        }
    };

    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read {}: {e}", target_path.display())))?;
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        foreign.extend(foreign_under(&entry.path(), &name, layout.kind, managed));
    }

    Ok(foreign)
}

fn foreign_under(
    dir: &Path,
    name: &str,
    layout_kind: LayoutKind,
    managed: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<PathBuf> {
    let is_managed_artifact = managed.values().any(|arts| arts.contains(name));
    let is_managed_source = managed.contains_key(name);

    match layout_kind {
        LayoutKind::Flat | LayoutKind::Prefixed => {
            if is_managed_artifact || is_managed_prefixed(name, managed) {
                Vec::new()
            } else {
                unmanaged_subdirs(dir, &BTreeSet::new())
            }
        }
        LayoutKind::BySource => {
            if is_managed_source {
                unmanaged_subdirs(dir, &managed[name])
            } else {
                unmanaged_subdirs(dir, &BTreeSet::new())
            }
        }
    }
}

fn is_managed_prefixed(name: &str, managed: &BTreeMap<String, BTreeSet<String>>) -> bool {
    managed.iter().any(|(source, arts)| {
        arts.iter().any(|art| {
            name.starts_with(source.as_str()) && name.ends_with(art.as_str()) && name != art
        })
    })
}

fn unmanaged_subdirs(dir: &Path, managed_artifacts: &BTreeSet<String>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            !n.starts_with('.') && !managed_artifacts.contains(&n)
        })
        .map(|e| e.path())
        .collect()
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
        discovers: Cell<usize>,
        digests: Cell<usize>,
    }

    impl<'a> CountingBackend<'a> {
        fn new(inner: &'a GitBackend) -> Self {
            Self {
                inner,
                fetches: Cell::new(0),
                resolves: Cell::new(0),
                exports: Cell::new(0),
                commit_times: Cell::new(0),
                discovers: Cell::new(0),
                digests: Cell::new(0),
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

        fn discover_count(&self) -> usize {
            self.discovers.get()
        }

        fn digest_count(&self) -> usize {
            self.digests.get()
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
            self.discovers.set(self.discovers.get() + 1);
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
            self.digests.set(self.digests.get() + 1);
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
            resolver: None,
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
    /// directly via `gix`), and must NOT fetch or compute a mirror digest.
    #[test]
    fn link_source_resolves_without_mirror_into_audit_lock_entry() {
        let fx = build_sync_fixture();
        let cfg = config_with_link_source("dev-src", &fx.url);
        // No mirror is seeded for fx.url: a fetch/compute_digest would error.

        let counting = CountingBackend::new(&fx.backend);
        let remotes = resolved_remotes(&cfg).expect("remotes resolve");
        let (routed, _commits) = resolve_sources(&cfg, &remotes, None, &counting, false)
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
        let remotes = resolved_remotes(&cfg).expect("remotes resolve");
        let _ = resolve_sources(&cfg, &remotes, None, &counting, false)
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

    use crate::projection::JournalEntry;

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
        let source = cfg.sources.get("editor-src").expect("source present");

        let dst = td.artifact_dst(&flat_layout(), "editor-src", "editor");
        std::fs::create_dir_all(dst.parent().expect("dst parent")).expect("mkdir dst parent");
        let live = fx.src.path().join("editor");
        symlink(&live, &dst).expect("deploy artifact as a symlink to the working tree");

        let linked_record = RegistryRecord {
            version: 1,
            key: artifact_key("dest", "editor-src", "editor"),
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: true,
        };
        fx.registry
            .put(&linked_record)
            .expect("seed linked registry record");

        let counting = CountingBackend::new(&fx.backend);
        let journal = Journal::open(&fx.registry.locks_dir()).expect("open journal");
        let commits: BTreeMap<String, String> =
            std::iter::once(("editor-src".to_owned(), fx.head_sha.clone())).collect();
        let remotes = resolved_remotes(&cfg).expect("remotes resolve");

        let run = TargetRun {
            config: &cfg,
            target_name: "dest",
            target,
            commits: &commits,
            remotes: &remotes,
            force: false,
            interactive: false,
            resolver: None,
        };
        let matcher = PathMatcher::new(source.includes(), source.excludes()).expect("matcher");
        let entry = ArtifactEntry {
            source,
            git: remotes
                .get("editor-src")
                .expect("resolved_remotes covers every source"),
            source_name: "editor-src",
            commit: &fx.head_sha,
            matcher: &matcher,
            artifact_name: "editor",
            target_path: &target.expanded_path(),
            layout_kind: LayoutKind::Flat,
            ejected: &[],
            mode_transition: false,
        };

        let state = check_artifact_state(
            &dst,
            "editor-src",
            &fx.head_sha,
            &[],
            "editor",
            &fx.registry,
            &artifact_key("dest", "editor-src", "editor"),
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
            linked: false,
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
            resolver: None,
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
            resolver: None,
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

    // ── interactive conflict resolution (resolver-driven) ──────────

    /// A resolver returning a single preset [`Resolution`] for every conflict,
    /// counting how many conflicts it was consulted on.
    struct ScriptedResolver {
        verdict: Resolution,
        consulted: Cell<usize>,
        seen: std::cell::RefCell<Vec<Conflict>>,
    }

    impl ScriptedResolver {
        fn new(verdict: Resolution) -> Self {
            Self {
                verdict,
                consulted: Cell::new(0),
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn consulted(&self) -> usize {
            self.consulted.get()
        }

        /// The most recent `Conflict` the resolver was consulted on, cloned out.
        fn last_conflict(&self) -> Conflict {
            self.seen
                .borrow()
                .last()
                .cloned()
                .expect("resolver was consulted on at least one conflict")
        }
    }

    impl ConflictResolver for ScriptedResolver {
        fn resolve(&self, conflict: &Conflict) -> Resolution {
            self.consulted.set(self.consulted.get() + 1);
            self.seen.borrow_mut().push(conflict.clone());
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
            resolver: Some(resolver),
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
    fn interactive_eject_persists_entry_removes_record_keeps_files() {
        let (src, url) = build_nested_artifact_repo();
        let git_dir = TempDir::new().expect("git dir");
        let state_dir = TempDir::new().expect("state dir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let registry = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
        let td = TargetDir::new();
        let cfg =
            config_one_source_one_target("editor-src", &url, "dest", &td.target_path(), "flat");

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
                .is_none(),
            "Eject must remove the artifact's registry record (stop managing it)"
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
        let cfg =
            config_one_source_one_target("multi", &url, "dest", &td.target_path(), "by-source");

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
            commit: fx.head_sha.clone(),
            digest: "blake3:recovered".to_owned(),
            projected_at: "2026-01-01T00:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("recovered.txt"),
                size: 10,
                mtime: 1_700_000_000,
                blake3: "blake3:recovered".to_owned(),
            }],
            linked: false,
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
        fn fetch(&self, source: &str, url: &str) -> Result<()> {
            self.inner.fetch(source, url)
        }
        fn resolve(&self, _source: &str, _url: &str, _refspec: &Refspec) -> Result<String> {
            Err(Error::Source("injected resolve failure".to_owned()))
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
            commit: fx.head_sha.clone(),
            digest: "blake3:recovered".to_owned(),
            projected_at: "2026-01-01T00:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("recovered.txt"),
                size: 10,
                mtime: 1_700_000_000,
                blake3: "blake3:recovered".to_owned(),
            }],
            linked: false,
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
            commit: "feedfacefeedfacefeedfacefeedfacefeedface".to_owned(),
            digest: "blake3:seed".to_owned(),
            projected_at: "2026-01-01T00:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from(file),
                size: content.len() as u64,
                mtime: 1_700_000_000,
                blake3: blake3::hash(content).to_hex().to_string(),
            }],
            linked: false,
        };
        reg.put(&record).expect("seed managed record");
        dst
    }

    fn eject_target_config(td: &TargetDir, fx: &SyncFixture) -> Config {
        config_one_source_one_target("editor-src", &fx.url, "dest", &td.target_path(), "flat")
    }

    #[test]
    fn eject_adds_ejected_entry_removes_record_and_keeps_files() {
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
        assert!(
            fx.registry
                .get(&artifact_key("dest", "editor-src", "editor"))
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
                .get(&artifact_key("dest", "editor-src", "editor"))
                .expect("registry get must not error")
                .is_none(),
            "eject must REMOVE the registry record so the artifact is no longer managed"
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
            commit: "feedfacefeedfacefeedfacefeedfacefeedface".to_owned(),
            digest: "blake3:seed".to_owned(),
            projected_at: "2026-01-01T00:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: manifest,
            linked: false,
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
        std::fs::write(&file, &tampered).expect(
            "overwrite with same-length, different content so disk fails the recomputed hash",
        );

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
        std::fs::write(foreign_dir.join("notes.txt"), b"hand-written\n")
            .expect("write foreign file");

        let report = rebuild_registry(&cfg, &lock, &fx.backend, &fx.registry)
            .expect("rebuild must not error in the presence of a foreign dir");

        assert!(
            report.foreign.iter().any(|p| p.ends_with("scratch")
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

    fn match_all() -> PathMatcher {
        PathMatcher::new(&[], &[]).expect("empty matcher")
    }

    #[test]
    fn worktree_scan_returns_sorted_real_dirs_excluding_dotdirs_and_files() {
        let wt = build_worktree(None);

        let found = discover_working_tree(wt.path(), None, &match_all())
            .expect("scanning an existing working tree must succeed");

        assert_eq!(
            found,
            vec![
                "alpha".to_owned(),
                "uncommitted".to_owned(),
                "zeta".to_owned()
            ],
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
            vec![
                "alpha".to_owned(),
                "uncommitted".to_owned(),
                "zeta".to_owned()
            ],
            "with root set, artifacts nested under <git>/languages must be discovered"
        );
        let direct = discover_working_tree(wt.path(), None, &match_all())
            .expect("scanning the git root itself must succeed");
        assert_eq!(
            direct,
            vec!["languages".to_owned()],
            "without root, only the top-level `languages` dir is an artifact"
        );
    }

    #[test]
    fn worktree_scan_honors_matcher_exclude() {
        let wt = build_worktree(None);
        let matcher = PathMatcher::new(&[], &["zeta".to_owned()]).expect("exclude matcher");

        let found = discover_working_tree(wt.path(), None, &matcher)
            .expect("scan with an exclude must succeed");

        assert_eq!(
            found,
            vec!["alpha".to_owned(), "uncommitted".to_owned()],
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
            }],
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
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("ghost.lua"),
                size: 7,
                mtime: 1_700_000_000,
                blake3: blake3::hash(b"phantom").to_hex().to_string(),
            }],
            linked: true,
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
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: true,
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
                commit: "link".to_owned(),
                digest: "link:".to_owned(),
                projected_at: "2026-06-08T12:00:00Z".to_owned(),
                layout: "by-source".to_owned(),
                allow_symlinks: false,
                preserve_executable: true,
                files: vec![],
                linked: true,
            })
            .expect("seed the orphaned linked record");

        let commits: BTreeMap<String, String> =
            std::iter::once(("linked-src".to_owned(), "link".to_owned())).collect();
        let remotes = resolved_remotes(&cfg).expect("remotes resolve");

        prune_orphans(&cfg, &remotes, &fx_backend(), &registry, &commits)
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
    fn link_in_base_config_is_rejected_naming_the_source() {
        let wt = build_worktree(None);
        let td = TargetDir::new();
        // deploy = link committed in the BASE phora.toml over a LOCAL path: only the
        // base-overlay provenance guard can reject it (the path is local).
        let base = config_link_source_one_target("base-linked", wt.path(), &td.target_path());

        let remotes = resolved_remotes(&base).expect("remotes resolve");
        let Err(err) = validate_link_mode(&base, &base, &remotes) else {
            panic!(
                "deploy = \"link\" set in the committed base config must be rejected as a \
                 non-local-overlay setting, not silently honored"
            );
        };
        assert!(
            err.to_string().contains("base-linked"),
            "the guard error must name the offending source `base-linked`, got: {err}"
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
            effective.sources["remote-linked"].deploy_mode(),
            DeployMode::Link,
            "premise: the overlay must make the effective mode Link"
        );

        let remotes = resolved_remotes(&effective).expect("remotes resolve");
        let Err(err) = validate_link_mode(&base, &effective, &remotes) else {
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
    fn base_link_rejected_even_when_overlay_downgrades_to_copy() {
        let wt = build_worktree(None);
        let td = TargetDir::new();
        // Base commits deploy = link over a LOCAL path (provenance is the only violation),
        // then the overlay downgrades the same source to copy.
        let base = config_link_source_one_target("committed-link", wt.path(), &td.target_path());
        let local = local_copy_overlay("committed-link", wt.path(), &td.target_path());
        let effective = effective_of(&base, &local);
        assert_eq!(
            effective.sources["committed-link"].deploy_mode(),
            DeployMode::Copy,
            "premise: the overlay must downgrade the effective mode to Copy"
        );

        let remotes = resolved_remotes(&effective).expect("remotes resolve");
        let Err(err) = validate_link_mode(&base, &effective, &remotes) else {
            panic!(
                "deploy = \"link\" committed in the base config must be rejected REGARDLESS of a \
                 local overlay downgrading the source to copy"
            );
        };
        assert!(
            err.to_string().contains("committed-link"),
            "the base-provenance guard error must name the offending source `committed-link`, \
             got: {err}"
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
            effective.sources["dev-src"].deploy_mode(),
            DeployMode::Link,
            "premise: the overlay must make the effective mode Link"
        );

        let remotes = resolved_remotes(&effective).expect("remotes resolve");
        validate_link_mode(&base, &effective, &remotes).expect(
            "a link confined to phora.local.toml over a local path must pass both the \
             base-overlay and local-path guards",
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
        std::fs::create_dir_all(dst.parent().expect("dst has a parent"))
            .expect("create dst parent");
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

    /// A single host+path source resolving against `[hosts.fixturehost]` whose
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
             [sources.{source}]\nhost = \"fixturehost\"\npath = \"{base}\"\nbranch = \"main\"\n\n\
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
        let cfg_lit = config_one_source_one_target(
            "editor-src",
            &fx.url,
            "dest",
            &td_lit.target_path(),
            "flat",
        );
        let in_lit = input(&cfg_lit, None, None, None, false);
        let git_dir_lit = TempDir::new().expect("literal git dir");
        let state_dir_lit = TempDir::new().expect("literal state dir");
        let backend_lit = GitBackend::new(git_dir_lit.path().to_path_buf());
        let registry_lit =
            FileRegistry::open(state_dir_lit.path().to_path_buf()).expect("literal registry");
        let out_lit =
            sync(&in_lit, &backend_lit, &registry_lit).expect("literal-twin sync deploys");

        let td_host = TargetDir::new();
        let cfg_host =
            config_host_source_one_target("editor-src", &fx.url, "dest", &td_host.target_path());
        let in_host = input(&cfg_host, None, None, None, false);
        let git_dir_host = TempDir::new().expect("host git dir");
        let state_dir_host = TempDir::new().expect("host state dir");
        let backend_host = GitBackend::new(git_dir_host.path().to_path_buf());
        let registry_host =
            FileRegistry::open(state_dir_host.path().to_path_buf()).expect("host registry");
        let out_host =
            sync(&in_host, &backend_host, &registry_host).expect("host+path sync deploys");

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
        let bytes_host =
            std::fs::read(dst_host.join("init.lua")).expect("host+path deployed init.lua");
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
        urls: std::cell::RefCell<Vec<String>>,
    }

    impl<'a> RecordingBackend<'a> {
        fn new(inner: &'a GitBackend) -> Self {
            Self {
                inner,
                urls: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn fetched_urls(&self) -> Vec<String> {
            self.urls.borrow().clone()
        }
    }

    impl SourceBackend for RecordingBackend<'_> {
        fn fetch(&self, source: &str, url: &str) -> Result<()> {
            self.urls.borrow_mut().push(url.to_owned());
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
}
