use std::collections::BTreeMap;

use phora::projection::build::project_target;
use phora::projection::model::{
    ArtifactRelativePath, BindingProjectionInput, CollapsePreference, LayoutSpec, LayoutStyle,
    MaterializationPolicy, OfferSpec, ResolvedSourceRef, TakeSpec, TargetProjection,
    TemplatePolicy,
};
use phora::source::SourceInventory;
use phora::sync::{StageRequest, StagedArtifact, StagedFile};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn projected_home() -> TargetProjection {
    let inventory =
        SourceInventory::from_paths(["editor/init.lua", "editor/lua/opts.lua", "motd.md.tmpl"])
            .expect("fixture paths are lexically valid");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::ProjectAll;
    let templates = TemplatePolicy::suffix_only();
    let layout = LayoutSpec::new(LayoutStyle::Flat, "-".to_owned());
    let source = ResolvedSourceRef::new("dotfiles", COMMIT);
    let input = BindingProjectionInput {
        identity: "dotfiles",
        source: &source,
        offer: &offer,
        inventory: &inventory,
        take: &take,
        collapse: CollapsePreference::default(),
        history: false,
        materialization: MaterializationPolicy::Copy,
        layout: &layout,
        templates: &templates,
    };
    project_target("home", &[input]).expect("clean fixture projects")
}

#[test]
fn stage_request_borrows_the_projection_artifact_and_target_directly() {
    let target = projected_home();
    let artifact = target
        .artifacts
        .first()
        .expect("fixture projects at least one artifact");
    let variables: BTreeMap<String, String> = [("name".to_owned(), "world".to_owned())]
        .into_iter()
        .collect();

    let request = StageRequest {
        artifact,
        target: &target,
        variables: &variables,
    };

    assert!(
        std::ptr::eq(request.artifact, artifact),
        "StageRequest.artifact is the very &ProjectedArtifact from the projection — deploy/apply \
         consumes projection values directly, no clone, no conversion (the PR6 StageBridge is retired)"
    );
    assert!(
        std::ptr::eq(request.target, &raw const target),
        "StageRequest.target is the borrowed &TargetProjection, straight through"
    );
    assert_eq!(
        request.target.target, "home",
        "the request reads the projection it was built over"
    );
    assert_eq!(
        request.variables.get("name").map(String::as_str),
        Some("world"),
        "variables is the run's vars map — ctx.vars at today's ExportRequest build site in \
         sync/target.rs — borrowed, not re-keyed"
    );
    assert!(
        !request.artifact.destination.as_str().is_empty(),
        "the projected destination stays readable through the request"
    );
}

#[test]
fn staged_artifact_frames_the_export_result_over_typed_destinations() {
    let verbatim = StagedArtifact {
        files: vec![StagedFile {
            destination: ArtifactRelativePath::new("editor/init.lua")
                .expect("safe leaf destination"),
            size: 8,
            mtime: 1_800_000_000,
            blake3: "aa".repeat(32),
        }],
        digest: "cc".repeat(32),
        vars_digest: None,
    };
    let templated = StagedArtifact {
        files: vec![StagedFile {
            destination: ArtifactRelativePath::new("motd.md").expect("safe deployed name"),
            size: 12,
            mtime: 1_800_000_000,
            blake3: "bb".repeat(32),
        }],
        digest: "dd".repeat(32),
        vars_digest: Some("ee".repeat(32)),
    };

    assert_eq!(
        verbatim.files[0].destination.as_str(),
        "editor/init.lua",
        "a staged file's destination is the artifact-relative deployed path, typed"
    );
    assert_eq!(
        (verbatim.files[0].size, verbatim.files[0].mtime),
        (8, 1_800_000_000),
        "per-file size and mtime framing mirrors today's manifest entries"
    );
    assert_eq!(
        verbatim.files[0].blake3.len(),
        64,
        "per-file content digest framing mirrors today's manifest entries"
    );
    assert!(
        verbatim.vars_digest.is_none(),
        "vars_digest is None when no template rendered — the ExportResult framing being re-homed"
    );
    assert_eq!(
        templated.vars_digest.as_deref(),
        Some("ee".repeat(32).as_str()),
        "vars_digest is Some iff at least one template rendered, alongside the artifact digest"
    );
    assert_eq!(
        templated.files[0].destination.as_str(),
        "motd.md",
        "a rendered leaf stages under its deployed name (tmpl suffix already stripped upstream)"
    );
}

#[test]
fn staged_destination_is_the_projection_newtype_itself() {
    let target = projected_home();
    let leaf_destination = &target
        .artifacts
        .iter()
        .flat_map(|artifact| artifact.leaves.iter())
        .next()
        .expect("fixture projects leaves")
        .destination;

    let staged = StagedFile {
        destination: leaf_destination.clone(),
        size: 0,
        mtime: 0,
        blake3: String::new(),
    };

    let round_trip: ArtifactRelativePath = staged.destination;
    assert_eq!(
        &round_trip, leaf_destination,
        "stage.rs reuses crate::projection::model::ArtifactRelativePath — one nominal type, so a \
         projected leaf destination and a staged destination compare directly, never re-parsed \
         through a duplicate newtype (codex-R3-C1)"
    );
}
