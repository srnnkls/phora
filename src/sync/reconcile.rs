use crate::projection::model::Projection;
use crate::sync::model::{
    ChangeSet, ManagedCondition, ObservedArtifact, ObservedProjectState, ReconciliationPolicy,
    SyncChange, SyncError,
};

pub fn reconcile<R>(
    projection: &Projection,
    observed: &ObservedProjectState<R>,
    policy: &ReconciliationPolicy,
) -> Result<ChangeSet, SyncError> {
    let mut changes = Vec::new();
    for (desired, observation) in desired_artifacts(projection).zip(&observed.artifacts) {
        if let ObservedArtifact::Managed(managed) = observation
            && let ManagedCondition::Modified { changed } = &managed.condition
        {
            changes.push(if policy.force {
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
                    changed: changed.clone(),
                }
            });
        }
    }
    Ok(ChangeSet { changes })
}

struct DesiredArtifact {
    target: String,
    source: String,
    artifact: String,
}

fn desired_artifacts(projection: &Projection) -> impl Iterator<Item = DesiredArtifact> + '_ {
    projection.targets.iter().flat_map(|target| {
        target
            .artifacts
            .iter()
            .map(move |artifact| DesiredArtifact {
                target: target.target.clone(),
                source: artifact.source.name().to_owned(),
                artifact: artifact.destination.as_str().to_owned(),
            })
    })
}
