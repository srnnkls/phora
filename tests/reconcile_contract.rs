use phora::projection::model::Materialization;
use phora::sync::model::{
    ChangeSet, ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedProjectState,
    ReconciliationPolicy, SyncChange,
};
use phora::sync::reconcile::reconcile;
use phora::sync::{ProjectedArtifact, Projection, ResolvedSourceRef, TargetPath, TargetProjection};
use std::path::PathBuf;

fn empty_projection() -> Projection {
    Projection {
        targets: Vec::new(),
        warnings: Vec::new(),
    }
}

fn empty_observation() -> ObservedProjectState {
    ObservedProjectState {
        artifacts: Vec::new(),
    }
}

// Single coupling point: rewire to the committed ReconciliationPolicy constructor. `force`
// is the field the matrix reads to choose Conflict vs Overwrite; the rest default.
fn policy(force: bool) -> ReconciliationPolicy {
    ReconciliationPolicy {
        force,
        ..ReconciliationPolicy::default()
    }
}

// Single coupling point: build a projection that WANTS a destination the observation reports
// as a drifted (Modified) managed artifact — the minimal desired×observed collision. Rewire
// the fixture body to the committed Projection/ObservedProjectState shapes; the assertions on
// the emitted ChangeSet below are the contract.
fn conflicting_pair() -> (Projection, ObservedProjectState) {
    let projection = Projection {
        targets: vec![TargetProjection {
            target: "vscode".to_owned(),
            bindings: Vec::new(),
            artifacts: vec![ProjectedArtifact {
                destination: TargetPath::new("snippets").expect("valid target path"),
                source: ResolvedSourceRef::new("company-configs", "abc123def456"),
                materialization: Materialization::CollapsedDir {
                    dir: "snippets".to_owned(),
                },
                kept_leaves: Vec::new(),
                leaves: Vec::new(),
            }],
            warnings: Vec::new(),
        }],
        warnings: Vec::new(),
    };
    let observed = ObservedProjectState {
        artifacts: vec![ObservedArtifact::Managed(ManagedArtifact {
            record: (),
            condition: ManagedCondition::Modified {
                changed: vec![PathBuf::from("a.json")],
            },
        })],
    };
    (projection, observed)
}

// Single coupling point: rewire to the committed ChangeSet accessor — this assumes a public
// `changes: Vec<SyncChange>` field.
fn changes(set: &ChangeSet) -> Vec<SyncChange> {
    set.changes.clone()
}

#[test]
fn reconcile_of_an_empty_projection_and_observation_is_an_empty_change_set() {
    let set = reconcile(&empty_projection(), &empty_observation(), &policy(false))
        .expect("reconcile of nothing against nothing must succeed");

    assert!(
        changes(&set).is_empty(),
        "nothing desired and nothing observed yields an empty ChangeSet, got {:?}",
        changes(&set)
    );
}

#[test]
fn reconcile_emits_conflict_for_an_unforced_collision() {
    let (projection, observed) = conflicting_pair();

    let set = reconcile(&projection, &observed, &policy(false))
        .expect("reconcile must classify a collision, not error");

    assert!(
        changes(&set)
            .iter()
            .any(|change| matches!(change, SyncChange::Conflict { .. })),
        "an unforced (force = false) collision between the desired projection and a drifted \
         observation must emit SyncChange::Conflict rather than silently overwriting, got {:?}",
        changes(&set)
    );
}

#[test]
fn reconcile_overwrites_the_same_collision_under_force() {
    let (projection, observed) = conflicting_pair();

    let set = reconcile(&projection, &observed, &policy(true))
        .expect("reconcile under force must classify the collision, not error");

    assert!(
        !changes(&set)
            .iter()
            .any(|change| matches!(change, SyncChange::Conflict { .. })),
        "force = true resolves the same collision to an Overwrite-class change, so no \
         SyncChange::Conflict survives, got {:?}",
        changes(&set)
    );
}
