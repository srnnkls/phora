use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{DeployMode, LayoutKind, ParsedSource, Target};
use crate::deploy::{ArtifactState, Journal, check_artifact_state, deploy_artifact, link_artifact};
use crate::error::{Error, Result};
use crate::kernel::Selection;
use crate::source::{ExportRequest, SourceBackend};
use crate::store::{ArtifactKey, EjectedEntry, Registry, RegistryRecord};

use super::discover::discover_artifacts_for_source;
use super::{
    Conflict, ConflictKind, ConflictResolver, Resolution, StagingGuard, nonce, remote_for,
    target_parent,
};

#[derive(Clone, Copy)]
pub(super) struct TargetRun<'a> {
    pub(super) parsed: &'a BTreeMap<String, ParsedSource>,
    pub(super) target_name: &'a str,
    pub(super) target: &'a Target,
    pub(super) commits: &'a BTreeMap<String, String>,
    pub(super) remotes: &'a BTreeMap<String, String>,
    pub(super) force: bool,
    pub(super) interactive: bool,
    pub(super) resolver: Option<&'a dyn ConflictResolver>,
}

pub(super) fn deploy_target(
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

    for source_name in run.target.resolve_sources(run.parsed) {
        let source = run.parsed.get(source_name).ok_or_else(|| {
            Error::Config(format!("target references undefined source: {source_name}"))
        })?;
        let commit = &run.commits[source_name];
        let git = remote_for(run.remotes, source_name)?;
        let selection = Selection::new(source.includes(), source.excludes())?;
        let discovered =
            discover_artifacts_for_source(source, git, source_name, commit, backend, &selection)?;

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
                selection: &selection,
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

pub(super) struct ArtifactEntry<'a> {
    pub(super) source: &'a ParsedSource,
    pub(super) git: &'a str,
    pub(super) source_name: &'a str,
    pub(super) commit: &'a str,
    pub(super) selection: &'a Selection,
    pub(super) artifact_name: &'a str,
    pub(super) target_path: &'a Path,
    pub(super) layout_kind: LayoutKind,
    pub(super) ejected: &'a [EjectedEntry],
    pub(super) mode_transition: bool,
}

pub(super) fn deploy_artifact_entry(
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
                selection: entry.selection,
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

struct DeployContext<'a> {
    target_path: &'a Path,
    layout_kind: LayoutKind,
    source: &'a ParsedSource,
    git: &'a str,
    source_name: &'a str,
    commit: &'a str,
    selection: &'a Selection,
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
        selection: ctx.selection,
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
