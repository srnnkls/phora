use phora::projection::model::Materialization;
use phora::sync::model::{
    ConflictKind, ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedProjectState,
    ReconciliationPolicy, SyncChange,
};
use phora::sync::reconcile::reconcile;
use phora::sync::{
    ConflictKind as ReexportedConflictKind, ProjectedArtifact, Projection, ResolvedSourceRef,
    TargetPath, TargetProjection,
};
use std::path::PathBuf;

fn policy(force: bool) -> ReconciliationPolicy {
    ReconciliationPolicy {
        force,
        ..ReconciliationPolicy::default()
    }
}

fn projection_of(destination: &str) -> Projection {
    Projection {
        targets: vec![TargetProjection {
            target: "vscode".to_owned(),
            bindings: Vec::new(),
            artifacts: vec![ProjectedArtifact {
                destination: TargetPath::new(destination).expect("valid target path"),
                source: ResolvedSourceRef::new("company-configs", "abc123def456"),
                materialization: Materialization::CollapsedDir {
                    dir: destination.to_owned(),
                },
                kept_leaves: Vec::new(),
                leaves: Vec::new(),
            }],
            warnings: Vec::new(),
        }],
        warnings: Vec::new(),
    }
}

fn modified_observation(changed: Vec<PathBuf>) -> ObservedProjectState {
    ObservedProjectState {
        artifacts: vec![ObservedArtifact::Managed(ManagedArtifact {
            record: (),
            condition: ManagedCondition::Modified { changed },
        })],
    }
}

#[test]
fn reconcile_conflict_carries_modified_kind_with_the_changed_paths() {
    let set = reconcile(
        &projection_of("snippets"),
        &modified_observation(vec![PathBuf::from("a.json"), PathBuf::from("b.json")]),
        &policy(false),
    )
    .expect("reconcile classifies the collision, not error");

    let conflict = set
        .changes
        .iter()
        .find_map(|change| match change {
            SyncChange::Conflict { kind, .. } => Some(kind),
            _ => None,
        })
        .expect("an unforced Modified collision must emit SyncChange::Conflict");

    let ConflictKind::Modified { changed } = conflict else {
        panic!("a drifted managed artifact must carry ConflictKind::Modified, got {conflict:?}");
    };
    assert_eq!(
        changed.as_slice(),
        [PathBuf::from("a.json"), PathBuf::from("b.json")],
        "the changed paths ride INSIDE ConflictKind::Modified, forwarded verbatim from the \
         observation"
    );
}

#[test]
fn reconcile_conflict_with_no_changed_paths_stays_modified_never_foreign() {
    let set = reconcile(
        &projection_of("snippets"),
        &modified_observation(Vec::new()),
        &policy(false),
    )
    .expect("reconcile classifies an empty-changed collision, not error");

    let conflict = set
        .changes
        .iter()
        .find_map(|change| match change {
            SyncChange::Conflict { kind, .. } => Some(kind),
            _ => None,
        })
        .expect("an empty-changed Modified collision must still emit a Conflict");

    assert!(
        matches!(conflict, ConflictKind::Modified { changed } if changed.is_empty()),
        "a Modified observation with an EMPTY changed list must reconcile to \
         ConflictKind::Modified with an empty vec — an empty list must NEVER be read as \
         ConflictKind::Foreign; got {conflict:?}"
    );
}

#[test]
fn reconcile_under_force_emits_no_kind_carrying_conflict() {
    let set = reconcile(
        &projection_of("snippets"),
        &modified_observation(vec![PathBuf::from("a.json")]),
        &policy(true),
    )
    .expect("reconcile under force classifies the collision, not error");

    assert!(
        !set.changes
            .iter()
            .any(|change| matches!(change, SyncChange::Conflict { .. })),
        "force = true resolves the same Modified collision to an Overwrite-class change; no \
         kind-carrying Conflict survives, got {:?}",
        set.changes
    );
}

#[test]
fn conflict_kind_is_reexported_from_sync_root_for_the_resolver_contract() {
    let via_model = ConflictKind::Foreign;
    let via_root: ReexportedConflictKind = via_model.clone();
    assert_eq!(
        via_model, via_root,
        "phora::sync::ConflictKind must be the SAME type as phora::sync::model::ConflictKind — the \
         re-export keeps the existing resolver/CLI call sites binding one type"
    );
}
