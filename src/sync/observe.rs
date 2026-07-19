use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::projection::model::Projection;
use crate::store::{
    ArtifactKey, EjectedEntry, HookState, Registry, RegistryRecord, StateLockGuard, StoreError,
};
use crate::sync::inspect::inspect;
use crate::sync::model::{
    ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedEntry, ObservedProjectState,
};
use crate::sync::state::StateStore;

use super::target::{self, ArtifactEntry, TargetRun};
use super::{DeployAll, StageSource, target_run};

type StoreResult<T> = std::result::Result<T, StoreError>;
type ObservationKey = (String, String, String);

pub(super) fn observe_workspace(
    ctx: &DeployAll<'_>,
    projection: &Projection,
) -> Result<ObservedProjectState<RegistryRecord>> {
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
        target::walk_projection_target(
            run,
            target_projection,
            ctx.registry,
            false,
            |run, entry| {
                let observation = observe_entry(run, entry, ctx.backend, ctx.registry)?;
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
            },
        )?;
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
) -> Result<ObservedArtifact<RegistryRecord>> {
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.identity.to_owned(),
        artifact: entry.item.materialization.published_key().to_owned(),
    };
    let vars_digest = target::expected_vars_digest(entry, backend, registry, &key, run.vars)?;
    let store = RegistryStore { registry };
    let observation = inspect(
        entry.artifact_dst,
        entry.identity,
        entry.commit,
        entry.ejected,
        &store,
        &key,
        vars_digest.as_deref(),
    )?;
    Ok(absorb_mode_transition(observation, entry.mode_transition))
}

fn absorb_mode_transition(
    observation: ObservedArtifact<RegistryRecord>,
    mode_transition: bool,
) -> ObservedArtifact<RegistryRecord> {
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

struct RegistryStore<'a> {
    registry: &'a dyn Registry,
}

impl StateStore for RegistryStore<'_> {
    fn artifact(&self, key: &ArtifactKey) -> StoreResult<Option<RegistryRecord>> {
        self.registry.get(key)
    }
    fn put_artifact(&self, record: &RegistryRecord) -> StoreResult<()> {
        self.registry.put(record)
    }
    fn remove_artifact(&self, key: &ArtifactKey) -> StoreResult<()> {
        self.registry.remove(key)
    }
    fn target_artifacts(&self, target: &str) -> StoreResult<Vec<RegistryRecord>> {
        self.registry.list_target(target)
    }
    fn all_artifacts(&self) -> StoreResult<Vec<RegistryRecord>> {
        self.registry.list_all()
    }
    fn ejections(&self, target: &str) -> StoreResult<Vec<EjectedEntry>> {
        self.registry.load_ejected(target)
    }
    fn save_ejections(&self, target: &str, entries: &[EjectedEntry]) -> StoreResult<()> {
        self.registry.save_ejected(target, entries)
    }
    fn hook_state(&self, target: &str) -> StoreResult<Vec<HookState>> {
        self.registry.load_hook_state(target)
    }
    fn record_hook_success(
        &self,
        target: &str,
        hook_id: &str,
        digest_set: &BTreeSet<String>,
    ) -> StoreResult<()> {
        self.registry
            .record_hook_success(target, hook_id, digest_set)
    }
    fn acquire_lock(&self) -> StoreResult<StateLockGuard> {
        Err(StoreError::Lock(
            "observation adapter does not acquire the state lock".to_owned(),
        ))
    }
    fn journal_root(&self) -> PathBuf {
        self.registry.locks_dir()
    }
}
