use phora::projection::model::{
    BindingProjection, Materialization, ProjectedArtifact, Projection, ResolvedSourceRef,
    TargetPath, TargetProjection,
};
use phora::sync::model::{
    ChangeSet, ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedEntry,
    ObservedProjectState, ReconciliationPolicy, SyncChange,
};
use phora::sync::reconcile::reconcile;
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

fn policy(force: bool) -> ReconciliationPolicy {
    ReconciliationPolicy {
        force,
        ..ReconciliationPolicy::default()
    }
}

fn conflicting_pair() -> (Projection, ObservedProjectState) {
    let artifact = || ProjectedArtifact {
        destination: TargetPath::new("snippets").expect("valid target path"),
        source: ResolvedSourceRef::new("company-configs", "abc123def456"),
        materialization: Materialization::CollapsedDir {
            dir: "snippets".to_owned(),
        },
        kept_leaves: Vec::new(),
        leaves: Vec::new(),
    };
    let projection = Projection {
        targets: vec![TargetProjection {
            target: "vscode".to_owned(),
            bindings: vec![BindingProjection {
                identity: "company-configs".to_owned(),
                source: "company-configs".to_owned(),
                commit: "abc123def456".to_owned(),
                attribution: phora::projection::model::BindingAttribution::default(),
                artifacts: vec![artifact()],
                warnings: Vec::new(),
            }],
            artifacts: vec![artifact()],
            warnings: Vec::new(),
        }],
        warnings: Vec::new(),
    };
    let observed = ObservedProjectState {
        artifacts: vec![ObservedEntry {
            target: "vscode".to_owned(),
            source: "company-configs".to_owned(),
            artifact: "snippets".to_owned(),
            observation: ObservedArtifact::Managed(ManagedArtifact {
                record: (),
                condition: ManagedCondition::Modified {
                    changed: vec![PathBuf::from("a.json")],
                },
                overlay_stale: false,
            }),
        }],
    };
    (projection, observed)
}

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
