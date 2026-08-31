use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::projection::model::Projection;
use crate::source::{
    SourceStore, WorktreeAdminId, WorktreeMirrorAddress, WorktreeObservationLevel,
    WorktreeObservationLock, WorktreeObservationRequest, WorktreeObservationResult,
};
use crate::sync::inspect::inspect;
use crate::sync::model::{
    ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedEntry, ObservedProjectState,
};
use crate::sync::state::{ArtifactKey, ArtifactRecord, StateStore};

use super::target::{self, ArtifactEntry, TargetRun};
use super::{DeployAll, target_run};

type ObservationKey = (String, String, String);

pub(super) fn observe_workspace<R>(
    ctx: &DeployAll<'_, R>,
    projection: &Projection,
) -> Result<ObservedProjectState<ArtifactRecord>>
where
    R: StateStore,
{
    let registry: &dyn StateStore = ctx.registry;
    let store: &dyn StateStore = ctx.registry;
    let mut observations: BTreeMap<ObservationKey, ObservedEntry<ArtifactRecord>> = BTreeMap::new();
    for (target_name, target) in &ctx.config.targets {
        let Some(target_projection) = projection
            .targets
            .iter()
            .find(|tp| &tp.target == target_name)
        else {
            continue;
        };
        let run = target_run(ctx, target_name, target);
        target::walk_projection_target(run, target_projection, registry, false, |run, entry| {
            let observation = observe_entry(run, entry, registry, store, ctx.backend)?;
            let published_key = entry.item.materialization.published_key().to_owned();
            let triplet = (
                run.target_name.to_owned(),
                entry.identity.to_owned(),
                published_key.clone(),
            );
            let record = ObservedEntry {
                target: run.target_name.to_owned(),
                source: entry.identity.to_owned(),
                artifact: published_key,
                observation,
            };
            if observations.insert(triplet.clone(), record).is_some() {
                return Err(Error::Sync(format!(
                    "duplicate observed artifact {}:{} in target {}",
                    triplet.1, triplet.2, triplet.0
                )));
            }
            Ok(false)
        })?;
    }
    for record in registry_only_records(ctx.input.prune(), || Ok(registry.all_artifacts()?))? {
        let triplet = (
            record.key.target.clone(),
            record.key.source.clone(),
            record.key.artifact.clone(),
        );
        if observations.contains_key(&triplet) {
            continue;
        }
        observations.insert(
            triplet,
            ObservedEntry {
                target: record.key.target.clone(),
                source: record.key.source.clone(),
                artifact: record.key.artifact.clone(),
                observation: ObservedArtifact::Managed(ManagedArtifact {
                    record,
                    condition: ManagedCondition::Clean,
                    overlay_stale: false,
                }),
            },
        );
    }
    Ok(ObservedProjectState {
        artifacts: observations.into_values().collect(),
    })
}

pub(super) fn observe_history_retirements<R>(
    ctx: &DeployAll<'_, R>,
) -> Result<Vec<ObservedEntry<ArtifactRecord>>>
where
    R: StateStore,
{
    let mut retirements = Vec::new();
    for record in ctx.registry.all_artifacts()? {
        if !record.history {
            continue;
        }
        let Some(target) = ctx.config.targets.get(&record.key.target) else {
            continue;
        };
        let Some(binding) = target
            .resolve_sources(ctx.parsed)
            .into_iter()
            .find(|binding| binding.identity == record.key.source && !binding.history)
        else {
            continue;
        };
        let path = super::prune::removal_destination(target, &record);
        let ejected = ctx.registry.ejections(&record.key.target)?;
        let observation = inspect(
            &path,
            binding.identity,
            &record.commit,
            &ejected,
            ctx.registry,
            &record.key,
            None,
        )?;
        retirements.push(ObservedEntry {
            target: record.key.target.clone(),
            source: record.key.source.clone(),
            artifact: record.key.artifact.clone(),
            observation,
        });
    }
    Ok(retirements)
}

fn registry_only_records(
    remove_orphans: bool,
    list_all: impl FnOnce() -> Result<Vec<ArtifactRecord>>,
) -> Result<Vec<ArtifactRecord>> {
    if remove_orphans {
        list_all()
    } else {
        Ok(Vec::new())
    }
}

fn observe_entry(
    run: &TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    registry: &dyn StateStore,
    store: &dyn StateStore,
    backend: &dyn SourceStore,
) -> Result<ObservedArtifact<ArtifactRecord>> {
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.identity.to_owned(),
        artifact: entry.item.materialization.published_key().to_owned(),
    };
    let vars_digest = target::expected_vars_digest(entry, registry, &key, run.vars)?;
    let observation = inspect(
        entry.artifact_dst,
        entry.identity,
        entry.commit,
        entry.ejected,
        store,
        &key,
        vars_digest.as_deref(),
    )?;
    let observation = absorb_mode_transition(observation, entry.mode_transition);
    observe_overlay(backend, entry, observation)
}

fn observe_overlay(
    backend: &dyn SourceStore,
    entry: &ArtifactEntry<'_>,
    observation: ObservedArtifact<ArtifactRecord>,
) -> Result<ObservedArtifact<ArtifactRecord>> {
    let ObservedArtifact::Managed(mut managed) = observation else {
        return Ok(observation);
    };
    let Some(request) = history_observation_request(
        &managed.record,
        entry.resolved.name.clone(),
        entry.artifact_dst,
        WorktreeObservationLock::Wait,
        WorktreeObservationLevel::Semantic,
    )?
    else {
        return Ok(ObservedArtifact::Managed(managed));
    };
    managed.overlay_stale = backend.observe_worktree(&request)? == WorktreeObservationResult::Stale;
    Ok(ObservedArtifact::Managed(managed))
}

pub(crate) fn history_observation_request(
    record: &ArtifactRecord,
    source: crate::source::SourceName,
    deploy_root: &Path,
    lock: WorktreeObservationLock,
    level: WorktreeObservationLevel,
) -> Result<Option<WorktreeObservationRequest>> {
    if record.linked {
        return Ok(None);
    }
    let Some((address, admin_id)) = history_address(record)? else {
        return Ok(None);
    };
    let deploy_root = normalize_history_deploy_root(deploy_root)?;
    match std::fs::symlink_metadata(&deploy_root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::Sync(format!(
                "stat history deployment root {}: {error}",
                deploy_root.display()
            )));
        }
    }
    let expected_commit = record.commit.parse().map_err(|error| {
        Error::Sync(format!(
            "parse history record commit {}: {error}",
            record.commit
        ))
    })?;
    Ok(Some(WorktreeObservationRequest {
        source,
        address,
        admin_id,
        deploy_root,
        expected_commit,
        lock,
        level,
    }))
}

pub(crate) fn normalize_history_deploy_root(deploy_root: &Path) -> Result<PathBuf> {
    Ok(normalize_history_deploy_root_at(
        &super::target::project_root()?,
        deploy_root,
    ))
}

pub(crate) fn normalize_history_deploy_root_at(project_root: &Path, deploy_root: &Path) -> PathBuf {
    if deploy_root.is_absolute() {
        deploy_root.to_path_buf()
    } else {
        project_root.join(deploy_root)
    }
}

pub(crate) fn history_address(
    record: &ArtifactRecord,
) -> Result<Option<(WorktreeMirrorAddress, WorktreeAdminId)>> {
    if !record.history {
        return Ok(None);
    }
    let (Some(cache_git_root), Some(mirror_key), Some(worktree_admin_id)) = (
        record.cache_git_root.as_deref(),
        record.mirror_key.as_deref(),
        record.worktree_admin_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let cache_git_root = PathBuf::from(cache_git_root);
    if !cache_git_root.is_absolute() {
        return Err(Error::Sync(format!(
            "history cache git root must be absolute: {}",
            cache_git_root.display()
        )));
    }
    let key = mirror_key
        .parse()
        .map_err(|error| Error::Sync(format!("parse history mirror key {mirror_key}: {error}")))?;
    let admin_id = worktree_admin_id.parse().map_err(|error| {
        Error::Sync(format!(
            "parse history worktree administration ID {worktree_admin_id}: {error}"
        ))
    })?;
    Ok(Some((
        WorktreeMirrorAddress {
            cache_git_root,
            key,
        },
        admin_id,
    )))
}

pub(super) fn absorb_mode_transition<R>(
    observation: ObservedArtifact<R>,
    mode_transition: bool,
) -> ObservedArtifact<R> {
    match observation {
        ObservedArtifact::Managed(managed)
            if mode_transition
                && matches!(
                    managed.condition,
                    ManagedCondition::Clean
                        | ManagedCondition::Linked
                        | ManagedCondition::MetadataChangedButContentClean { .. }
                ) =>
        {
            ObservedArtifact::Managed(ManagedArtifact {
                record: managed.record,
                condition: ManagedCondition::Outdated,
                overlay_stale: managed.overlay_stale,
            })
        }
        other => other,
    }
}
