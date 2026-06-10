use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{Config, DeployMode, LayoutKind, ParsedSource, Target};
use crate::error::{Error, Result};
use crate::kernel::Selection;
use crate::lock::Lock;
use crate::source::{ExportRequest, SourceBackend};
use crate::store::{ArtifactKey, ManifestFile, Registry, RegistryRecord};

use super::discover::discover_artifacts_for_source;
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

    for (target_name, target) in &config.targets {
        let mut managed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut selections: Vec<Selection> = Vec::new();

        for source_name in target.resolve_sources(&parsed) {
            let source = parsed.get(source_name).ok_or_else(|| {
                Error::Config(format!("target references undefined source: {source_name}"))
            })?;
            let locked = lock.find_source(source_name).ok_or_else(|| {
                Error::Sync(format!(
                    "no locked commit for source {source_name}; run sync first"
                ))
            })?;
            let commit = &locked.commit;
            let git = remote_for(&remotes, source_name)?;
            let selection = Selection::new(source.includes(), source.excludes())?;
            selections.push(selection.clone());
            let policy = source.export_policy();
            let discovered = discover_artifacts_for_source(
                source,
                git,
                source_name,
                commit,
                backend,
                &selection,
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
                        selection: &selection,
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

        report
            .foreign
            .extend(scan_foreign(target, &managed, &selections)?);
    }

    Ok(report)
}

struct RebuildOne<'a> {
    backend: &'a dyn SourceBackend,
    registry: &'a dyn Registry,
    source: &'a ParsedSource,
    git: &'a str,
    source_name: &'a str,
    commit: &'a str,
    selection: &'a Selection,
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
        selection,
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
        selection,
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

fn admits_for_foreign(name: &str, selections: &[Selection]) -> bool {
    !name.starts_with('.') || selections.iter().any(|s| s.selects_artifact(name))
}

/// On-disk artifact dirs under `target` that no managed `(source, artifact)` maps to.
fn scan_foreign(
    target: &Target,
    managed: &BTreeMap<String, BTreeSet<String>>,
    selections: &[Selection],
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
        if !admits_for_foreign(&name, selections) {
            continue;
        }
        foreign.extend(foreign_under(
            &entry.path(),
            &name,
            layout.kind,
            managed,
            selections,
        ));
    }

    Ok(foreign)
}

fn foreign_under(
    dir: &Path,
    name: &str,
    layout_kind: LayoutKind,
    managed: &BTreeMap<String, BTreeSet<String>>,
    selections: &[Selection],
) -> Vec<PathBuf> {
    let is_managed_artifact = managed.values().any(|arts| arts.contains(name));
    let is_managed_source = managed.contains_key(name);

    match layout_kind {
        LayoutKind::Flat | LayoutKind::Prefixed => {
            if is_managed_artifact || is_managed_prefixed(name, managed) {
                Vec::new()
            } else if name.starts_with('.') {
                vec![dir.to_path_buf()]
            } else {
                unmanaged_subdirs(dir, &BTreeSet::new(), selections)
            }
        }
        LayoutKind::BySource => {
            if is_managed_source {
                unmanaged_subdirs(dir, &managed[name], selections)
            } else {
                unmanaged_subdirs(dir, &BTreeSet::new(), selections)
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

fn unmanaged_subdirs(
    dir: &Path,
    managed_artifacts: &BTreeSet<String>,
    selections: &[Selection],
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            admits_for_foreign(&n, selections) && !managed_artifacts.contains(&n)
        })
        .map(|e| e.path())
        .collect()
}
