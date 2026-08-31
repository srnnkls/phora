use std::collections::{BTreeMap, BTreeSet};

use crate::projection::model::Projection;
use crate::sync::model::{
    ChangeSet, ConflictKind, ManagedCondition, ObservedArtifact, ObservedEntry,
    ObservedProjectState, ReconciliationPolicy, RemovalReason, SyncChange, SyncError,
};

type Key = (String, String, String);

pub fn reconcile<R>(
    projection: &Projection,
    observed: &ObservedProjectState<R>,
    policy: &ReconciliationPolicy,
) -> Result<ChangeSet, SyncError> {
    let observations: BTreeMap<Key, &ObservedArtifact<R>> = observed
        .artifacts
        .iter()
        .map(|entry| (key_of(entry), &entry.observation))
        .collect();

    let mut changes = Vec::new();
    let mut matched: BTreeSet<Key> = BTreeSet::new();
    for desired in desired_artifacts(projection) {
        let key = (
            desired.target.clone(),
            desired.source.clone(),
            desired.artifact.clone(),
        );
        let Some(observation) = observations.get(&key) else {
            return Err(SyncError::UnmatchedArtifact {
                target: desired.target,
                source: desired.source,
                artifact: desired.artifact,
            });
        };
        matched.insert(key);
        if let Some(change) = classify_desired(desired, observation, *policy) {
            changes.push(change);
        }
    }

    for entry in &observed.artifacts {
        if matched.contains(&key_of(entry)) {
            continue;
        }
        if let Some(change) = classify_orphan(entry, *policy) {
            changes.push(change);
        }
    }

    Ok(ChangeSet { changes })
}

fn classify_desired<R>(
    desired: DesiredArtifact,
    observation: &ObservedArtifact<R>,
    policy: ReconciliationPolicy,
) -> Option<SyncChange> {
    match observation {
        ObservedArtifact::Missing => Some(deploy(desired)),
        ObservedArtifact::Foreign(_) => Some(collision(desired, ConflictKind::Foreign, policy)),
        ObservedArtifact::Ejected => None,
        ObservedArtifact::Managed(managed) => match &managed.condition {
            ManagedCondition::Outdated => Some(deploy(desired)),
            ManagedCondition::Modified { changed } => Some(collision(
                desired,
                ConflictKind::Modified {
                    changed: changed.clone(),
                },
                policy,
            )),
            ManagedCondition::Clean | ManagedCondition::MetadataChangedButContentClean { .. }
                if managed.overlay_stale =>
            {
                Some(SyncChange::RewriteOverlay {
                    target: desired.target,
                    source: desired.source,
                    artifact: desired.artifact,
                })
            }
            ManagedCondition::Clean
            | ManagedCondition::MetadataChangedButContentClean { .. }
            | ManagedCondition::Linked => None,
        },
    }
}

fn classify_orphan<R>(
    entry: &ObservedEntry<R>,
    policy: ReconciliationPolicy,
) -> Option<SyncChange> {
    match &entry.observation {
        ObservedArtifact::Managed(_) if policy.prune => Some(SyncChange::Remove {
            target: entry.target.clone(),
            source: entry.source.clone(),
            artifact: entry.artifact.clone(),
            reason: RemovalReason::Pruned,
        }),
        _ => None,
    }
}

fn deploy(desired: DesiredArtifact) -> SyncChange {
    SyncChange::Deploy {
        target: desired.target,
        source: desired.source,
        artifact: desired.artifact,
    }
}

fn collision(
    desired: DesiredArtifact,
    kind: ConflictKind,
    policy: ReconciliationPolicy,
) -> SyncChange {
    if policy.force {
        SyncChange::Overwrite {
            target: desired.target,
            source: desired.source,
            artifact: desired.artifact,
        }
    } else {
        SyncChange::Conflict {
            target: desired.target,
            source: desired.source,
            artifact: desired.artifact,
            kind,
        }
    }
}

fn key_of<R>(entry: &ObservedEntry<R>) -> Key {
    (
        entry.target.clone(),
        entry.source.clone(),
        entry.artifact.clone(),
    )
}

struct DesiredArtifact {
    target: String,
    source: String,
    artifact: String,
}

fn desired_artifacts(projection: &Projection) -> impl Iterator<Item = DesiredArtifact> + '_ {
    projection.targets.iter().flat_map(|target| {
        target.bindings.iter().flat_map(move |binding| {
            binding
                .artifacts
                .iter()
                .map(move |artifact| DesiredArtifact {
                    target: target.target.clone(),
                    source: binding.identity.clone(),
                    artifact: artifact.materialization.published_key().to_owned(),
                })
        })
    })
}
