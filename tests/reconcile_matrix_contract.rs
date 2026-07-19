use phora::projection::model::{
    BindingProjection, Materialization, ProjectedArtifact, Projection, ResolvedSourceRef,
    TargetPath, TargetProjection,
};
use phora::sync::model::{
    ConflictKind, ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedEntry,
    ObservedProjectState, ReconciliationPolicy, RemovalReason, SyncChange, SyncError,
};
use phora::sync::reconcile::reconcile;
use std::path::PathBuf;

const TARGET: &str = "vscode";
const SOURCE: &str = "company-configs";
const ARTIFACT: &str = "snippets";
const COMMIT: &str = "abc123def456";

fn projected(source: &str, dest: &str) -> ProjectedArtifact {
    ProjectedArtifact {
        destination: TargetPath::new(dest).expect("valid target path"),
        source: ResolvedSourceRef::new(source, COMMIT),
        materialization: Materialization::CollapsedDir {
            dir: dest.to_owned(),
        },
        kept_leaves: Vec::new(),
        leaves: Vec::new(),
    }
}

fn binding_of(source: &str, dest: &str) -> BindingProjection {
    BindingProjection {
        identity: source.to_owned(),
        source: source.to_owned(),
        commit: COMMIT.to_owned(),
        artifacts: vec![projected(source, dest)],
        warnings: Vec::new(),
    }
}

fn projection_of(target: &str, arts: &[(&str, &str)]) -> Projection {
    Projection {
        targets: vec![TargetProjection {
            target: target.to_owned(),
            bindings: arts.iter().map(|(s, d)| binding_of(s, d)).collect(),
            artifacts: arts.iter().map(|(s, d)| projected(s, d)).collect(),
            warnings: Vec::new(),
        }],
        warnings: Vec::new(),
    }
}

fn empty_projection() -> Projection {
    Projection {
        targets: Vec::new(),
        warnings: Vec::new(),
    }
}

fn entry(
    target: &str,
    source: &str,
    artifact: &str,
    observation: ObservedArtifact,
) -> ObservedEntry {
    ObservedEntry {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
        observation,
    }
}

fn observed(entries: Vec<ObservedEntry>) -> ObservedProjectState {
    ObservedProjectState { artifacts: entries }
}

fn policy(force: bool, prune: bool) -> ReconciliationPolicy {
    ReconciliationPolicy {
        force,
        prune,
        ..ReconciliationPolicy::default()
    }
}

fn managed(condition: ManagedCondition) -> ObservedArtifact {
    ObservedArtifact::Managed(ManagedArtifact {
        record: (),
        condition,
    })
}

fn deploy(target: &str, source: &str, artifact: &str) -> SyncChange {
    SyncChange::Deploy {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
    }
}

fn overwrite(target: &str, source: &str, artifact: &str) -> SyncChange {
    SyncChange::Overwrite {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
    }
}

fn conflict(target: &str, source: &str, artifact: &str, kind: ConflictKind) -> SyncChange {
    SyncChange::Conflict {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
        kind,
    }
}

fn removal(target: &str, source: &str, artifact: &str, reason: RemovalReason) -> SyncChange {
    SyncChange::Remove {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
        reason,
    }
}

#[derive(Clone, Copy)]
enum Obs {
    /// No observation entry keyed to the desired artifact at all.
    Absent,
    Missing,
    Clean,
    MetadataClean,
    Outdated,
    ModifiedPaths,
    ModifiedEmpty,
    Linked,
    Foreign,
    Ejected,
}

impl Obs {
    fn observation(self) -> Option<ObservedArtifact> {
        Some(match self {
            Obs::Absent => return None,
            Obs::Missing => ObservedArtifact::Missing,
            Obs::Clean => managed(ManagedCondition::Clean),
            Obs::MetadataClean => managed(ManagedCondition::MetadataChangedButContentClean {
                refreshed: Vec::new(),
            }),
            Obs::Outdated => managed(ManagedCondition::Outdated),
            Obs::ModifiedPaths => managed(ManagedCondition::Modified {
                changed: vec![PathBuf::from("a.json")],
            }),
            Obs::ModifiedEmpty => managed(ManagedCondition::Modified {
                changed: Vec::new(),
            }),
            Obs::Linked => managed(ManagedCondition::Linked),
            Obs::Foreign => ObservedArtifact::Foreign(PathBuf::from("snippets")),
            Obs::Ejected => ObservedArtifact::Ejected,
        })
    }
}

enum Expect {
    NoOp,
    One(SyncChange),
    Unmatched,
}

struct Row {
    name: &'static str,
    desired: bool,
    obs: Obs,
    force: bool,
    prune: bool,
    expect: Expect,
}

fn matrix() -> Vec<Row> {
    let mut rows = present_drift_rows();
    rows.extend(present_settled_rows());
    rows.extend(orphan_rows());
    rows
}

fn present_drift_rows() -> Vec<Row> {
    let modified_one = || {
        conflict(
            TARGET,
            SOURCE,
            ARTIFACT,
            ConflictKind::Modified {
                changed: vec![PathBuf::from("a.json")],
            },
        )
    };
    let modified_empty = || {
        conflict(
            TARGET,
            SOURCE,
            ARTIFACT,
            ConflictKind::Modified {
                changed: Vec::new(),
            },
        )
    };
    vec![
        Row {
            name: "missing→deploy",
            desired: true,
            obs: Obs::Missing,
            force: false,
            prune: false,
            expect: Expect::One(deploy(TARGET, SOURCE, ARTIFACT)),
        },
        Row {
            name: "clean→noop",
            desired: true,
            obs: Obs::Clean,
            force: false,
            prune: false,
            expect: Expect::NoOp,
        },
        Row {
            name: "metadata-clean→noop",
            desired: true,
            obs: Obs::MetadataClean,
            force: false,
            prune: false,
            expect: Expect::NoOp,
        },
        Row {
            name: "outdated→deploy",
            desired: true,
            obs: Obs::Outdated,
            force: false,
            prune: false,
            expect: Expect::One(deploy(TARGET, SOURCE, ARTIFACT)),
        },
        Row {
            name: "modified-unforced→conflict-modified",
            desired: true,
            obs: Obs::ModifiedPaths,
            force: false,
            prune: false,
            expect: Expect::One(modified_one()),
        },
        Row {
            name: "modified-forced→overwrite",
            desired: true,
            obs: Obs::ModifiedPaths,
            force: true,
            prune: false,
            expect: Expect::One(overwrite(TARGET, SOURCE, ARTIFACT)),
        },
        Row {
            name: "modified-empty-unforced→conflict-modified-empty",
            desired: true,
            obs: Obs::ModifiedEmpty,
            force: false,
            prune: false,
            expect: Expect::One(modified_empty()),
        },
    ]
}

fn present_settled_rows() -> Vec<Row> {
    vec![
        Row {
            name: "linked→noop",
            desired: true,
            obs: Obs::Linked,
            force: false,
            prune: false,
            expect: Expect::NoOp,
        },
        Row {
            name: "foreign-unforced→conflict-foreign",
            desired: true,
            obs: Obs::Foreign,
            force: false,
            prune: false,
            expect: Expect::One(conflict(TARGET, SOURCE, ARTIFACT, ConflictKind::Foreign)),
        },
        Row {
            name: "foreign-forced→overwrite",
            desired: true,
            obs: Obs::Foreign,
            force: true,
            prune: false,
            expect: Expect::One(overwrite(TARGET, SOURCE, ARTIFACT)),
        },
        Row {
            name: "ejected→noop",
            desired: true,
            obs: Obs::Ejected,
            force: false,
            prune: false,
            expect: Expect::NoOp,
        },
        Row {
            name: "desired-without-observation→unmatched",
            desired: true,
            obs: Obs::Absent,
            force: false,
            prune: false,
            expect: Expect::Unmatched,
        },
    ]
}

fn orphan_rows() -> Vec<Row> {
    vec![
        Row {
            name: "orphan-managed-prune→remove-pruned",
            desired: false,
            obs: Obs::Clean,
            force: false,
            prune: true,
            expect: Expect::One(removal(TARGET, SOURCE, ARTIFACT, RemovalReason::Pruned)),
        },
        Row {
            name: "orphan-managed-no-prune→noop",
            desired: false,
            obs: Obs::Clean,
            force: false,
            prune: false,
            expect: Expect::NoOp,
        },
        Row {
            name: "orphan-foreign-prune→noop",
            desired: false,
            obs: Obs::Foreign,
            force: false,
            prune: true,
            expect: Expect::NoOp,
        },
        Row {
            name: "orphan-ejected→noop",
            desired: false,
            obs: Obs::Ejected,
            force: false,
            prune: true,
            expect: Expect::NoOp,
        },
    ]
}

#[test]
fn reconcile_matrix_maps_every_cell_to_its_change() {
    for row in matrix() {
        let projection = if row.desired {
            projection_of(TARGET, &[(SOURCE, ARTIFACT)])
        } else {
            empty_projection()
        };
        let entries = row
            .obs
            .observation()
            .map(|o| vec![entry(TARGET, SOURCE, ARTIFACT, o)])
            .unwrap_or_default();
        let result = reconcile(
            &projection,
            &observed(entries),
            &policy(row.force, row.prune),
        );

        match row.expect {
            Expect::Unmatched => {
                let err = result.expect_err(row.name);
                assert!(
                    matches!(&err, SyncError::UnmatchedArtifact { target, source, artifact }
                        if target == TARGET && source == SOURCE && artifact == ARTIFACT),
                    "[{}] a desired artifact with no keyed observation must fail as \
                     SyncError::UnmatchedArtifact carrying its (target, source, artifact) triplet, \
                     got {err:?}",
                    row.name
                );
            }
            Expect::NoOp => {
                let set =
                    result.unwrap_or_else(|e| panic!("[{}] reconcile errored: {e:?}", row.name));
                assert!(
                    set.changes.is_empty(),
                    "[{}] this cell must reconcile to an empty ChangeSet, got {:?}",
                    row.name,
                    set.changes
                );
            }
            Expect::One(expected) => {
                let set =
                    result.unwrap_or_else(|e| panic!("[{}] reconcile errored: {e:?}", row.name));
                assert_eq!(
                    set.changes,
                    vec![expected],
                    "[{}] this cell must reconcile to exactly its one SyncChange",
                    row.name
                );
            }
        }
    }
}

const SOURCE_B: &str = "dotfiles";
const ARTIFACT_B: &str = "settings";

#[test]
fn correlation_is_keyed_not_positional_under_reordering() {
    let projection = projection_of(TARGET, &[(SOURCE, ARTIFACT), (SOURCE_B, ARTIFACT_B)]);
    let state = observed(vec![
        entry(
            TARGET,
            SOURCE_B,
            ARTIFACT_B,
            managed(ManagedCondition::Modified {
                changed: vec![PathBuf::from("x.json")],
            }),
        ),
        entry(TARGET, SOURCE, ARTIFACT, ObservedArtifact::Missing),
    ]);

    let set = reconcile(&projection, &state, &policy(false, false)).expect("reconcile");

    assert!(
        set.changes.contains(&deploy(TARGET, SOURCE, ARTIFACT)),
        "A is Missing → Deploy(A); a positional zip would mis-pair A with B's Modified. got {:?}",
        set.changes
    );
    assert!(
        set.changes.contains(&conflict(
            TARGET,
            SOURCE_B,
            ARTIFACT_B,
            ConflictKind::Modified {
                changed: vec![PathBuf::from("x.json")],
            },
        )),
        "B is Modified → Conflict(B); correlation must key by (target, source, artifact), not \
         position. got {:?}",
        set.changes
    );
    assert!(
        !set.changes.iter().any(|c| matches!(c,
            SyncChange::Conflict { source, .. } if source == SOURCE)),
        "A must NEVER be classified from B's Modified observation — a positional zip would emit a \
         Conflict for A. got {:?}",
        set.changes
    );
}

#[test]
fn an_orphan_observed_tail_is_not_silently_dropped() {
    // Desired = A only; observed = A(missing) + B(managed, undesired). A positional zip truncates
    // the longer observed tail, dropping B entirely.
    let projection = projection_of(TARGET, &[(SOURCE, ARTIFACT)]);
    let state = observed(vec![
        entry(TARGET, SOURCE, ARTIFACT, ObservedArtifact::Missing),
        entry(
            TARGET,
            SOURCE_B,
            ARTIFACT_B,
            managed(ManagedCondition::Clean),
        ),
    ]);

    let set = reconcile(&projection, &state, &policy(false, true)).expect("reconcile");

    assert!(
        set.changes.contains(&deploy(TARGET, SOURCE, ARTIFACT)),
        "desired A is Missing → Deploy(A). got {:?}",
        set.changes
    );
    assert!(
        set.changes.contains(&removal(
            TARGET,
            SOURCE_B,
            ARTIFACT_B,
            RemovalReason::Pruned
        )),
        "the undesired managed observation B is an orphan the keyed correlation must surface for \
         pruning, never silently truncate as a positional zip tail. got {:?}",
        set.changes
    );
}

#[test]
fn unmatched_error_names_the_uncorrelated_desired_artifact() {
    let projection = projection_of(TARGET, &[(SOURCE, ARTIFACT)]);
    let err = reconcile(&projection, &observed(Vec::new()), &policy(false, false))
        .expect_err("a desired artifact with no observation is un-correlatable");

    let SyncError::UnmatchedArtifact {
        target,
        source,
        artifact,
    } = &err;
    assert_eq!(
        (target.as_str(), source.as_str(), artifact.as_str()),
        (TARGET, SOURCE, ARTIFACT),
        "the failure must name the exact desired triplet that had no keyed observation"
    );
}

#[test]
fn an_equal_length_wrong_key_observation_cannot_stand_in_by_position() {
    // Desired [A]; observed a SINGLE entry keyed to a DIFFERENT triplet (B). Lengths are equal, so
    // an impl that sorts both sides by key and pairs by index WITHOUT comparing keys would silently
    // hand A the observation that belongs to B. Keyed correlation must instead recognise that A has
    // no observation of its own.
    let projection = projection_of(TARGET, &[(SOURCE, ARTIFACT)]);
    let state = observed(vec![entry(
        TARGET,
        SOURCE_B,
        ARTIFACT_B,
        managed(ManagedCondition::Clean),
    )]);

    let err = reconcile(&projection, &state, &policy(false, true))
        .expect_err("A is desired but only a differently-keyed observation exists");

    let SyncError::UnmatchedArtifact {
        target,
        source,
        artifact,
    } = &err;
    assert_eq!(
        (target.as_str(), source.as_str(), artifact.as_str()),
        (TARGET, SOURCE, ARTIFACT),
        "the un-correlatable DESIRED artifact A must be named. The wrong-key observation B is a \
         would-be prune row, but the UnmatchedArtifact error takes precedence and preempts it even \
         under prune=true — a pair-by-index impl that never compares keys would instead treat B as \
         A's observation, emit no error, and read A as Clean"
    );
}

fn requires_std_error<T: std::error::Error>() {}

#[test]
fn sync_error_satisfies_the_std_error_bound() {
    requires_std_error::<SyncError>();
}

#[test]
fn unmatched_error_display_renders_its_triplet() {
    let projection = projection_of(TARGET, &[(SOURCE, ARTIFACT)]);
    let err = reconcile(&projection, &observed(Vec::new()), &policy(false, false))
        .expect_err("a desired artifact with no observation is un-correlatable");

    let rendered = err.to_string();
    for token in [TARGET, SOURCE, ARTIFACT] {
        assert!(
            rendered.contains(token),
            "Display for SyncError::UnmatchedArtifact must render its subject (B17) — the (target, \
             source, artifact) triplet; `{token}` is missing from {rendered:?}"
        );
    }
}
