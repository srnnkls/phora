//! Top-level orchestration: sync, eject, uneject.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{Config, LayoutKind, Source, Target, merge_configs};
use crate::error::{Error, Result};
use crate::lock::{Lock, LockedSource, merge_locks, source_matches, split_locks};
use crate::matcher::PathMatcher;
use crate::projection::{
    ArtifactState, Journal, check_artifact_state, deploy_artifact, recovery_sweep,
};
use crate::registry::{ArtifactKey, EjectedEntry, ManifestFile, Registry, RegistryRecord};
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

            let entry = ArtifactEntry {
                source,
                source_name,
                commit,
                matcher: &matcher,
                artifact_name: &artifact_name,
                target_path: &target_path,
                layout_kind: layout.kind,
                ejected: &ejected,
            };
            had_failures |= deploy_artifact_entry(run, &entry, backend, registry, journal)?;
        }
    }

    Ok(had_failures)
}

struct ArtifactEntry<'a> {
    source: &'a Source,
    source_name: &'a str,
    commit: &'a str,
    matcher: &'a PathMatcher,
    artifact_name: &'a str,
    target_path: &'a Path,
    layout_kind: LayoutKind,
    ejected: &'a [EjectedEntry],
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
        ArtifactState::Modified { changed } if !run.force => Some(ConflictKind::Modified {
            changed: changed.clone(),
        }),
        ArtifactState::Foreign if !run.force => Some(ConflictKind::Foreign),
        _ => None,
    };

    let deploy = |key: ArtifactKey| {
        deploy_one(
            backend,
            registry,
            journal,
            DeployContext {
                target_path: entry.target_path,
                layout_kind: entry.layout_kind,
                source: entry.source,
                source_name: entry.source_name,
                commit: entry.commit,
                matcher: entry.matcher,
                artifact_name: entry.artifact_name,
                artifact_dst: &artifact_dst,
                key,
            },
        )
    };

    let resolution = match conflict_kind {
        None if matches!(state, ArtifactState::Ejected | ArtifactState::Clean | ArtifactState::Linked) => {
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
    let staging_base = target_parent(ctx.target_path).join(".phora-stage");
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
            let matcher = PathMatcher::new(source.includes(), source.excludes())?;
            let policy = source.export_policy();
            let discovered = backend.discover_artifacts(
                source_name,
                &source.git,
                commit,
                source.root.as_deref(),
                &matcher,
            )?;

            for artifact in discovered {
                let key = ArtifactKey {
                    target: target_name.clone(),
                    source: source_name.to_owned(),
                    artifact: artifact.clone(),
                };
                let artifact_dst = target
                    .expanded_path()
                    .join(target.layout().artifact_path(source_name, &artifact));

                rebuild_one(RebuildOne {
                    backend,
                    registry,
                    source,
                    source_name,
                    commit,
                    matcher: &matcher,
                    policy: &policy,
                    artifact: &artifact,
                    artifact_dst: &artifact_dst,
                    layout_kind: target.layout().kind,
                    key,
                    report: &mut report,
                })?;

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

    let commit_time = backend.commit_time(source_name, &source.git, commit)?;
    let export = backend.export_artifact(&ExportRequest {
        source: source_name,
        url: &source.git,
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

        let run = TargetRun {
            config: &cfg,
            target_name: "dest",
            target,
            commits: &commits,
            force: false,
            interactive: false,
            resolver: None,
        };
        let matcher = PathMatcher::new(source.includes(), source.excludes()).expect("matcher");
        let entry = ArtifactEntry {
            source,
            source_name: "editor-src",
            commit: &fx.head_sha,
            matcher: &matcher,
            artifact_name: "editor",
            target_path: &target.expanded_path(),
            layout_kind: LayoutKind::Flat,
            ejected: &[],
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

        let had_failures =
            deploy_artifact_entry(run, &entry, &counting, &fx.registry, &journal)
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
}
