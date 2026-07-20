use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::projection::model::Projection;
use crate::store::{ArtifactKey, Registry, RegistryRecord};
use crate::sync::inspect::inspect;
use crate::sync::model::{
    ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedEntry, ObservedProjectState,
};
use crate::sync::state::StateStore;

use super::target::{self, ArtifactEntry, TargetRun};
use super::{DeployAll, StageSource, target_run};

type ObservationKey = (String, String, String);

pub(super) fn observe_workspace<R>(
    ctx: &DeployAll<'_, R>,
    projection: &Projection,
) -> Result<ObservedProjectState<RegistryRecord>>
where
    R: Registry + StateStore,
{
    let readonly_registry;
    let registry: &dyn Registry = if ctx.input.lockless {
        readonly_registry = crate::store::FrozenReadOnlyRegistry::new(ctx.registry);
        &readonly_registry
    } else {
        ctx.registry
    };
    let store: &dyn StateStore = ctx.registry;
    let mut observations: BTreeMap<ObservationKey, ObservedEntry<RegistryRecord>> = BTreeMap::new();
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
            let observation = observe_entry(run, entry, ctx.backend, registry, store)?;
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
    Ok(ObservedProjectState {
        artifacts: observations.into_values().collect(),
    })
}

fn observe_entry(
    run: &TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    backend: &dyn StageSource,
    registry: &dyn Registry,
    store: &dyn StateStore,
) -> Result<ObservedArtifact<RegistryRecord>> {
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.identity.to_owned(),
        artifact: entry.item.materialization.published_key().to_owned(),
    };
    let vars_digest = target::expected_vars_digest(entry, backend, registry, &key, run.vars)?;
    let observation = inspect(
        entry.artifact_dst,
        entry.identity,
        entry.commit,
        entry.ejected,
        store,
        &key,
        vars_digest.as_deref(),
    )?;
    Ok(absorb_mode_transition(observation, entry.mode_transition))
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
            })
        }
        other => other,
    }
}
