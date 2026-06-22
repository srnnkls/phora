use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{Config, LayoutKind, Target, TemplateOptIn};
use crate::error::{Error, Result};
use crate::kernel::{Materialization, SourceName};
use crate::lock::{Lock, encode_ref, ref_discriminator};
use crate::source::{ExportLeaf, ExportRequest, SourceBackend};
use crate::store::{
    ArtifactKey, ManifestFile, ProjectedRecord, RecordKind, Registry, RegistryRecord,
};

use super::plan::{ResolvedBindingPlan, plan_target};
use super::{StagingGuard, nonce, remote_for, resolved_remotes};

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
    let parsed = config.parsed_sources()?;
    let remotes = resolved_remotes(config, &parsed)?;
    let resolved_commits = locked_commits(config, lock, &parsed)?;

    for (target_name, target) in &config.targets {
        let plan = plan_target(
            target_name,
            target,
            &parsed,
            &remotes,
            backend,
            &resolved_commits,
        )?;

        let mut managed_dests: BTreeSet<PathBuf> = BTreeSet::new();
        for binding in &plan.bindings {
            managed_dests.extend(binding.items.iter().map(|item| item.destination.clone()));
            let source = parsed.get(&binding.source).ok_or_else(|| {
                Error::Config(format!(
                    "target references undefined source: {}",
                    binding.source
                ))
            })?;
            rebuild_binding(
                &BindingRun {
                    config,
                    target_name,
                    target,
                    backend,
                    registry,
                    binding,
                    source,
                    git: remote_for(&remotes, &binding.source)?,
                },
                &mut report,
            )?;
        }

        report.foreign.extend(scan_foreign(target, &managed_dests)?);
    }

    Ok(report)
}

/// The `(name, encoded_ref) -> commit` map `plan_target` needs, drawn from the lock:
/// every binding's effective ref resolves to its locked commit (link sources included).
fn locked_commits(
    config: &Config,
    lock: &Lock,
    parsed: &BTreeMap<String, crate::config::ParsedSource>,
) -> Result<BTreeMap<(String, String), String>> {
    let mut commits = BTreeMap::new();
    for target in config.targets.values() {
        for binding in target.resolve_sources(parsed) {
            let source = parsed.get(binding.source).ok_or_else(|| {
                Error::Config(format!(
                    "target references undefined source: {}",
                    binding.source
                ))
            })?;
            let discriminator = ref_discriminator(&binding.effective_ref, &source.refspec());
            let locked = lock
                .find_entry(binding.source, discriminator.as_deref())
                .ok_or_else(|| {
                    Error::Sync(format!(
                        "no locked commit for source {} at its ref; run sync first",
                        binding.source
                    ))
                })?;
            commits.insert(
                (
                    binding.source.to_owned(),
                    encode_ref(&binding.effective_ref),
                ),
                locked.commit.clone(),
            );
        }
    }
    Ok(commits)
}

struct BindingRun<'a> {
    config: &'a Config,
    target_name: &'a str,
    target: &'a Target,
    backend: &'a dyn SourceBackend,
    registry: &'a dyn Registry,
    binding: &'a ResolvedBindingPlan,
    source: &'a crate::config::ParsedSource,
    git: &'a str,
}

fn rebuild_binding(run: &BindingRun<'_>, report: &mut RebuildReport) -> Result<()> {
    let source_name = SourceName::trusted(&run.binding.source);
    let policy = run.source.export_policy();
    let template_opt_in = run
        .target
        .resolve_sources(&run.config.parsed_sources()?)
        .into_iter()
        .find(|b| b.identity == run.binding.identity)
        .map_or(TemplateOptIn::SuffixOnly, |b| b.template_opt_in);

    for item in &run.binding.items {
        let published_key = published_key(&item.materialization).to_owned();
        let key = ArtifactKey {
            target: run.target_name.to_owned(),
            source: run.binding.identity.clone(),
            artifact: published_key.clone(),
        };
        let artifact_dst = run.target.expanded_path().join(
            run.target
                .layout()
                .artifact_path(&run.binding.identity, &published_key),
        );

        if run.binding.commit == "link" {
            rebuild_linked(
                run.registry,
                &run.binding.source,
                &policy,
                run.target.layout().kind,
                record_kind(&item.materialization),
                key.clone(),
            )?;
        } else {
            let leaves = item_leaves(&item.materialization, &item.kept_leaves, &template_opt_in);
            rebuild_one(RebuildOne {
                backend: run.backend,
                registry: run.registry,
                git: run.git,
                source_name: &source_name,
                underlying_source: &run.binding.source,
                root: run.source.offer().root(),
                commit: &run.binding.commit,
                policy: &policy,
                leaves: &leaves,
                materialization: &item.materialization,
                artifact_dst: &artifact_dst,
                layout_kind: run.target.layout().kind,
                key: key.clone(),
                report,
                template_opt_in: &template_opt_in,
                vars: &run.config.vars,
            })?;
        }
        report.reconstructed.push(key);
    }
    Ok(())
}

/// The published artifact key — the collapsed-dir or the renamed/identity leaf dest.
fn published_key(materialization: &Materialization) -> &str {
    match materialization {
        Materialization::CollapsedDir { dir } => dir,
        Materialization::Leaf(take) => &take.dest,
    }
}

fn record_kind(materialization: &Materialization) -> RecordKind {
    match materialization {
        Materialization::CollapsedDir { .. } => RecordKind::Dir,
        Materialization::Leaf(_) => RecordKind::File,
    }
}

/// The export leaf plan for one materialization, mirroring `deploy_one`: a leaf maps
/// its single dest basename; a collapsed dir maps each kept child to its dir-relative
/// deployed name.
fn item_leaves(
    materialization: &Materialization,
    kept_leaves: &[crate::kernel::ResolvedTake],
    template_opt_in: &TemplateOptIn,
) -> Vec<ExportLeaf> {
    match materialization {
        Materialization::CollapsedDir { dir } => {
            let prefix = format!("{dir}/");
            kept_leaves
                .iter()
                .filter_map(|kept| {
                    let child = kept.dest.strip_prefix(&prefix)?;
                    Some(ExportLeaf {
                        source: PathBuf::from(&kept.source),
                        dest: PathBuf::from(template_opt_in.deployed_name(child)),
                    })
                })
                .collect()
        }
        Materialization::Leaf(take) => {
            let dest = take.dest.rsplit('/').next().unwrap_or(&take.dest);
            vec![ExportLeaf {
                source: PathBuf::from(&take.source),
                dest: PathBuf::from(dest),
            }]
        }
    }
}

struct RebuildOne<'a> {
    backend: &'a dyn SourceBackend,
    registry: &'a dyn Registry,
    git: &'a str,
    source_name: &'a SourceName,
    underlying_source: &'a str,
    root: Option<&'a Path>,
    commit: &'a str,
    policy: &'a crate::source::ExportPolicy,
    leaves: &'a [ExportLeaf],
    materialization: &'a Materialization,
    artifact_dst: &'a Path,
    layout_kind: LayoutKind,
    key: ArtifactKey,
    report: &'a mut RebuildReport,
    template_opt_in: &'a TemplateOptIn,
    vars: &'a BTreeMap<String, String>,
}

fn rebuild_one(args: RebuildOne<'_>) -> Result<()> {
    let RebuildOne {
        backend,
        registry,
        git,
        source_name,
        underlying_source,
        root,
        commit,
        policy,
        leaves,
        materialization,
        artifact_dst,
        layout_kind,
        key,
        report,
        template_opt_in,
        vars,
    } = args;

    let key_label = key.artifact.replace('/', "_");
    let staging_base = std::env::temp_dir().join("phora-rebuild");
    let staging = staging_base.join(format!("{key_label}-{}-{}", std::process::id(), nonce()));
    let _guard = StagingGuard::new(&staging_base, &staging);

    let commit_time = backend.commit_time(source_name, git, commit)?;
    let export = backend.export_artifact(&ExportRequest {
        source: source_name,
        url: git,
        commit,
        root,
        policy,
        staging_dir: &staging,
        commit_time,
        template_opt_in,
        vars,
        leaves,
    })?;

    let manifest_base = match materialization {
        Materialization::CollapsedDir { .. } => artifact_dst.to_path_buf(),
        Materialization::Leaf(_) => artifact_dst
            .parent()
            .map_or_else(|| artifact_dst.to_path_buf(), Path::to_path_buf),
    };

    let mut modified = false;
    let mut files = Vec::with_capacity(export.files.len());
    for mf in export.files {
        let on_disk = manifest_base.join(&mf.path);
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

    let record = RegistryRecord::projected(ProjectedRecord {
        key: key.clone(),
        underlying_source,
        commit,
        digest: export.digest,
        layout: format!("{layout_kind:?}").to_lowercase(),
        kind: record_kind(materialization),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files,
        vars_digest: export.vars_digest,
    });
    registry.put(&record)?;
    if modified {
        report.modified.push(key);
    }
    Ok(())
}

/// Reconstruct a linked artifact's registry record without hashing or export:
/// a link source has no mirror, so its marker is synthesized from disk discovery.
fn rebuild_linked(
    registry: &dyn Registry,
    underlying_source: &str,
    policy: &crate::source::ExportPolicy,
    layout_kind: LayoutKind,
    kind: RecordKind,
    key: ArtifactKey,
) -> Result<()> {
    let record = RegistryRecord {
        version: 1,
        key,
        source: underlying_source.to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: format!("{layout_kind:?}").to_lowercase(),
        kind,
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
        vars_digest: None,
    };
    registry.put(&record)?;
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

/// On-disk top-level entries under `target` that no binding deploys. `managed_dests`
/// holds every planned destination already composed under the target's layout AND
/// binding identity (so by-source nests each identity under its own dir, prefixed joins
/// identity to the key); an entry is spared when it is a managed destination or the
/// ancestor of one. A user-owned top-level dotfile is never reported foreign — an
/// orphaned managed dotfile is reclaimed by `sync --prune`, not flagged here.
fn scan_foreign(target: &Target, managed_dests: &BTreeSet<PathBuf>) -> Result<Vec<PathBuf>> {
    let target_path = target.expanded_path();

    let entries = match std::fs::read_dir(&target_path) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::Sync(format!(
                "read target dir {}: {e}",
                target_path.display()
            )));
        }
    };

    let mut foreign = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read {}: {e}", target_path.display())))?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if managed_dests.contains(&path) || is_managed_ancestor(&path, managed_dests) {
            continue;
        }
        foreign.push(path);
    }
    foreign.sort();
    Ok(foreign)
}

/// Whether `dir` is an ancestor of any managed destination, so a parent directory
/// holding only managed children is not itself reported foreign (by-source layout nests
/// each identity under its own dir).
fn is_managed_ancestor(dir: &Path, managed_dests: &BTreeSet<PathBuf>) -> bool {
    managed_dests.iter().any(|dest| dest.starts_with(dir))
}
