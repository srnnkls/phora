use phora::projection::model::{
    BindingProjection, Materialization, ProjectedArtifact, Projection, ResolvedSourceRef,
    TargetPath, TargetProjection,
};
use phora::sync::model::{
    ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedEntry, ObservedProjectState,
    ReconciliationPolicy, RemovalReason, SyncChange,
};
use phora::sync::reconcile::reconcile;
use std::path::PathBuf;

const TARGET: &str = "vscode";
const COMMIT: &str = "abc123def456";

struct Desired {
    identity: &'static str,
    underlying: &'static str,
    published_key: &'static str,
    destination: &'static str,
}

fn projected(d: &Desired) -> ProjectedArtifact {
    ProjectedArtifact {
        destination: TargetPath::new(d.destination).expect("valid target path"),
        source: ResolvedSourceRef::new(d.underlying, COMMIT),
        materialization: Materialization::CollapsedDir {
            dir: d.published_key.to_owned(),
        },
        kept_leaves: Vec::new(),
        leaves: Vec::new(),
    }
}

fn binding(d: &Desired) -> BindingProjection {
    BindingProjection {
        identity: d.identity.to_owned(),
        source: d.underlying.to_owned(),
        commit: COMMIT.to_owned(),
        attribution: phora::projection::model::BindingAttribution::default(),
        artifacts: vec![projected(d)],
        warnings: Vec::new(),
    }
}

fn projection(bindings: Vec<BindingProjection>) -> Projection {
    Projection {
        targets: vec![TargetProjection {
            target: TARGET.to_owned(),
            bindings,
            artifacts: Vec::new(),
            warnings: Vec::new(),
        }],
        warnings: Vec::new(),
    }
}

fn entry(source: &str, artifact: &str, observation: ObservedArtifact) -> ObservedEntry {
    ObservedEntry {
        target: TARGET.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
        observation,
    }
}

fn observed(entries: Vec<ObservedEntry>) -> ObservedProjectState {
    ObservedProjectState { artifacts: entries }
}

fn managed(condition: ManagedCondition) -> ObservedArtifact {
    ObservedArtifact::Managed(ManagedArtifact {
        record: (),
        condition,
        overlay_stale: false,
    })
}

fn policy(force: bool, prune: bool) -> ReconciliationPolicy {
    ReconciliationPolicy {
        force,
        prune,
        ..ReconciliationPolicy::default()
    }
}

#[test]
fn aliased_binding_correlates_by_identity_key_not_underlying_source() {
    let d = Desired {
        identity: "alias",
        underlying: "upstream-dotfiles",
        published_key: "snippets",
        destination: "alias/snippets",
    };
    let proj = projection(vec![binding(&d)]);
    let state = observed(vec![entry("alias", "snippets", ObservedArtifact::Missing)]);

    let set = reconcile(&proj, &state, &policy(false, false)).unwrap_or_else(|e| {
        panic!(
            "R1: the observation keyed to the binding IDENTITY `alias` must correlate to the \
             desired artifact re-derived from TargetProjection.bindings — a desired side that \
             keys on the underlying source `upstream-dotfiles` (or on the empty top-level \
             TargetProjection.artifacts) sees no observation and errors spuriously: {e:?}"
        )
    });

    assert_eq!(
        set.changes,
        vec![SyncChange::Deploy {
            target: TARGET.to_owned(),
            source: "alias".to_owned(),
            artifact: "snippets".to_owned(),
        }],
        "R1: a Missing observation under an aliased, non-flat binding must reconcile to exactly \
         one Deploy keyed by (target, binding.identity, published_key) = (vscode, alias, \
         snippets) — the underlying source name `upstream-dotfiles` and the layout-derived \
         destination `alias/snippets` are OUT of the correlation key"
    );
}

#[test]
fn aliased_binding_under_by_source_layout_is_not_a_spurious_orphan() {
    let kept = Desired {
        identity: "alias",
        underlying: "upstream-dotfiles",
        published_key: "snippets",
        destination: "alias/snippets",
    };
    let proj = projection(vec![binding(&kept)]);
    let state = observed(vec![
        entry("alias", "snippets", managed(ManagedCondition::Clean)),
        entry("genuinely-gone", "old", managed(ManagedCondition::Clean)),
    ]);

    let set = reconcile(&proj, &state, &policy(false, true)).expect("R1: aliased clean is settled");

    let alias_removed = set.changes.iter().any(|c| {
        matches!(c, SyncChange::Remove { source, artifact, .. }
            if source == "alias" && artifact == "snippets")
    });
    assert!(
        !alias_removed,
        "R1: the aliased binding's observation (keyed by identity `alias`) is DESIRED and must \
         stay matched — pruning must never misclassify it as an orphan. A desired side that \
         cannot correlate the identity key leaves `alias/snippets` unmatched and prunes it. \
         got {:?}",
        set.changes
    );
    assert!(
        set.changes.contains(&SyncChange::Remove {
            target: TARGET.to_owned(),
            source: "genuinely-gone".to_owned(),
            artifact: "old".to_owned(),
            reason: RemovalReason::Pruned,
        }),
        "R1: the genuinely-undesired managed observation is the only orphan and must be pruned. \
         got {:?}",
        set.changes
    );
}

#[test]
fn orphan_managed_modified_under_prune_removes_without_conflict() {
    let proj = projection(Vec::new());
    let state = observed(vec![entry(
        "gone",
        "leftover",
        managed(ManagedCondition::Modified {
            changed: vec![PathBuf::from("a.json")],
        }),
    )]);

    let set = reconcile(&proj, &state, &policy(false, true)).expect("R2: orphan reconcile");

    assert_eq!(
        set.changes,
        vec![SyncChange::Remove {
            target: TARGET.to_owned(),
            source: "gone".to_owned(),
            artifact: "leftover".to_owned(),
            reason: RemovalReason::Pruned,
        }],
        "R2: a locally-MODIFIED managed orphan under prune is Remove{{Pruned}} with no Conflict \
         — prune overrides modification (parity with live prune_orphans, which never consults \
         modification state). got {:?}",
        set.changes
    );
}

#[test]
fn reconcile_emits_no_moved_pin_row_even_when_follow_moved_pin_is_set() {
    let d = Desired {
        identity: "src",
        underlying: "src",
        published_key: "snippets",
        destination: "snippets",
    };
    let proj = projection(vec![binding(&d)]);
    let policy = ReconciliationPolicy {
        force: false,
        prune: true,
        follow_moved_pin: true,
    };
    let state = observed(vec![
        entry("src", "snippets", managed(ManagedCondition::Clean)),
        entry("moved", "old", managed(ManagedCondition::Clean)),
    ]);

    let set = reconcile(&proj, &state, &policy).expect("R3: reconcile");

    assert!(
        !set.changes.iter().any(|c| matches!(
            c,
            SyncChange::Remove {
                reason: RemovalReason::MovedPin,
                ..
            }
        )),
        "R3: MovedPin is DROPPED from reconcile's emission surface — no desired×observed cell \
         may emit Remove{{MovedPin}}, even with follow_moved_pin=true; the fast-forward pre-pass \
         (apply_fast_forward_drops) stays authoritative. got {:?}",
        set.changes
    );
}
