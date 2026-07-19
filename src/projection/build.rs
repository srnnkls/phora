use std::collections::{BTreeMap, BTreeSet};

use crate::diagnostic::SelectionDiagnostic;
use crate::error::Error;
use crate::projection::collapse::{CollapseChoice, CollapseMode, CollapseWarning, plan_collapse};
use crate::projection::diagnostic::{ProjectionError, ProjectionWarning, unsafe_leaf};
use crate::projection::model::{
    ArtifactRelativePath, BindingAttribution, BindingProjection, BindingProjectionInput,
    ContentTransform, Materialization, MaterializationPolicy, OfferSpec, ProjectedArtifact,
    ProjectedLeaf, Projection, TakeSpec, TargetPath, TargetProjection, TemplatePolicy,
    WorkspaceTargetInput,
};
use crate::projection::offer::OfferSelection;
use crate::projection::take::{ResolvedTake, TakeWarning, fold_dest, resolve_take};
use crate::source::SourcePath;

/// Projects one binding: compile the offer over the inventory, seal `take` over it,
/// fold collapse, then compose each materialization's target-relative destination.
///
/// # Errors
/// Errors if the offer fails to compile, `take` references a non-offered leaf, two
/// kept leaves collide, or a demanded collapse is blocked.
pub fn project_binding(
    input: &BindingProjectionInput<'_>,
) -> std::result::Result<BindingProjection, ProjectionError> {
    let candidates: Vec<&str> = input
        .inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    let selection = OfferSelection::compile(
        input.offer.includes(),
        input.offer.excludes(),
        input.offer.root(),
    )
    .map_err(other)?;
    let offer = selection.select(&candidates);
    let physical_tree = OfferSelection::compile(&[], &[], input.offer.root())
        .map_err(other)?
        .select(&candidates);

    let directives = input.take.directives();
    let resolution = resolve_take(&offer, directives.as_deref())
        .map_err(|error| classify_take_error(error, &offer, input.take))?;
    let resolved_takes = resolution.kept;
    let take_warnings = resolution.warnings;

    let mode = input.materialization.collapse_mode();
    let choice = input.collapse.choice();
    let plan =
        plan_collapse(&resolved_takes, &physical_tree, mode, choice).map_err(collapse_blocked)?;
    let materializations = reject_partial_take_collapse(
        plan.items,
        &resolved_takes,
        &offer,
        input.collapse.as_bool(),
    )
    .map_err(collapse_blocked)?
    .into_iter()
    .map(|materialization| {
        apply_deployed_name(materialization, input.materialization, input.templates)
    })
    .collect::<Vec<_>>();

    let mut artifacts = Vec::with_capacity(materializations.len());
    for materialization in materializations {
        let key = materialization.published_key().to_owned();
        let layout_path = input.layout.artifact_path(input.identity, &key);
        let dest = layout_path.to_string_lossy();
        let destination = TargetPath::new(&dest)?;
        let kept_leaves = kept_leaves_under(&materialization, &resolved_takes);
        let leaves = build_leaves(
            &materialization,
            &kept_leaves,
            input.materialization,
            input.templates,
            offer_root_prefix(input.offer).as_deref(),
        )?;
        artifacts.push(ProjectedArtifact {
            destination,
            source: input.source.clone(),
            materialization,
            kept_leaves,
            leaves,
        });
    }

    let mut warnings: Vec<ProjectionWarning> = take_warnings
        .into_iter()
        .map(|warning| {
            let TakeWarning::NoMatchGlob(pattern) = warning;
            ProjectionWarning::TakeNoMatchGlob(pattern)
        })
        .chain(plan.warnings.into_iter().map(|warning| {
            let CollapseWarning::LostCollapseToExclude { dir } = warning;
            ProjectionWarning::LostCollapseToExclude(dir)
        }))
        .collect();
    warnings.sort();

    Ok(BindingProjection {
        identity: input.identity.to_owned(),
        source: input.source.name().to_owned(),
        commit: input.source.commit().to_owned(),
        attribution: BindingAttribution {
            offered_leaves: offer,
            resolved_takes,
            copy_template_suffix: input.materialization.is_copy()
                && input.templates.strips_suffix(),
        },
        artifacts,
        warnings,
    })
}

fn other(error: Error) -> ProjectionError {
    ProjectionError::Other {
        rendered: Box::new(error),
    }
}

fn collapse_blocked(error: Error) -> ProjectionError {
    ProjectionError::CollapseBlocked {
        rendered: Box::new(error),
    }
}

/// A take failure is `LeafNotOffered` when a literal or rename source is not in the
/// offer set; the offer seal rejects it first, so it dominates any other take fault.
fn classify_take_error(error: Error, offer: &[String], take: &TakeSpec) -> ProjectionError {
    if let TakeSpec::Explicit {
        literals, renames, ..
    } = take
    {
        let offered: BTreeSet<&str> = offer.iter().map(String::as_str).collect();
        let unoffered = literals.iter().any(|leaf| !offered.contains(leaf.as_str()))
            || renames
                .iter()
                .any(|(src, _)| !offered.contains(src.as_str()));
        if unoffered {
            return ProjectionError::LeafNotOffered {
                rendered: Box::new(error),
            };
        }
    }
    ProjectionError::Other {
        rendered: Box::new(error),
    }
}

fn offer_root_prefix(offer: &OfferSpec) -> Option<String> {
    offer.root().and_then(|root| {
        let trimmed = root.to_string_lossy().trim_end_matches('/').to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn build_leaves(
    materialization: &Materialization,
    kept_leaves: &[ResolvedTake],
    policy: MaterializationPolicy,
    templates: &TemplatePolicy,
    root: Option<&str>,
) -> std::result::Result<Vec<ProjectedLeaf>, ProjectionError> {
    let transform_of = |source: &str| {
        if policy.is_copy() && templates.renders(source) {
            ContentTransform::Template
        } else {
            ContentTransform::Identity
        }
    };
    let inventory_source = |source: &str| match root {
        Some(root) => format!("{root}/{source}"),
        None => source.to_owned(),
    };
    match materialization {
        Materialization::Leaf(take) => {
            let dest = take.dest.rsplit('/').next().unwrap_or(&take.dest);
            let source = inventory_source(&take.source);
            Ok(vec![ProjectedLeaf {
                source: SourcePath::new(&source).map_err(|_| unsafe_leaf(&source))?,
                destination: ArtifactRelativePath::new(dest)?,
                transform: transform_of(&take.source),
            }])
        }
        Materialization::CollapsedDir { dir } => {
            let prefix = format!("{dir}/");
            let mut leaves = Vec::new();
            for kept in kept_leaves {
                let Some(child) = kept.dest.strip_prefix(&prefix) else {
                    continue;
                };
                let deployed = if policy.is_copy() {
                    templates.deployed_name(child)
                } else {
                    child.to_owned()
                };
                let source = inventory_source(&kept.source);
                leaves.push(ProjectedLeaf {
                    source: SourcePath::new(&source).map_err(|_| unsafe_leaf(&source))?,
                    destination: ArtifactRelativePath::new(&deployed)?,
                    transform: transform_of(&kept.source),
                });
            }
            Ok(leaves)
        }
    }
}

/// A `CollapsedDir` is sound only when every *offered* leaf under it is kept at
/// identity; a `take` that drops an offered sibling makes the dir partial. The
/// partial dir is re-collapsed against only its offered-and-kept leaves, so a
/// wholly-taken sub-dir still collapses while the dropped siblings stay out.
fn reject_partial_take_collapse(
    items: Vec<Materialization>,
    kept: &[ResolvedTake],
    offer: &[String],
    collapse: Option<bool>,
) -> crate::error::Result<Vec<Materialization>> {
    let kept_at_identity: BTreeSet<&str> = kept
        .iter()
        .filter(|r| r.source == r.dest)
        .map(|r| r.source.as_str())
        .collect();
    let mode = CollapseMode::Link;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Materialization::CollapsedDir { dir } = &item else {
            out.push(item);
            continue;
        };
        let prefix = format!("{dir}/");
        let offered_orphan = offer
            .iter()
            .any(|leaf| leaf.starts_with(&prefix) && !kept_at_identity.contains(leaf.as_str()));
        if !offered_orphan {
            out.push(item);
            continue;
        }
        if collapse == Some(true) {
            return Err(partial_take_collapse_diagnostic(dir));
        }
        let kept_under: Vec<ResolvedTake> = kept
            .iter()
            .filter(|r| r.source.starts_with(&prefix))
            .cloned()
            .collect();
        let offered_under: Vec<String> = offer
            .iter()
            .filter(|leaf| leaf.starts_with(&prefix))
            .cloned()
            .collect();
        let sub = plan_collapse(&kept_under, &offered_under, mode, CollapseChoice::Default)?;
        out.extend(sub.items);
    }
    out.sort_by(|a, b| a.published_key().cmp(b.published_key()));
    Ok(out)
}

fn partial_take_collapse_diagnostic(dir: &str) -> Error {
    SelectionDiagnostic {
        entry: dir.to_owned(),
        matched_against: "the offered leaves under the collapsed directory".to_owned(),
        why: "`collapse = true` was demanded but `take` keeps only part of the offered directory"
            .to_owned(),
        did_you_mean: None,
        remedy: "take the whole directory, or omit `collapse`".to_owned(),
        debug_hint: Some("phora preview --files".to_owned()),
        details: Vec::new(),
    }
    .sync()
}

fn apply_deployed_name(
    materialization: Materialization,
    policy: MaterializationPolicy,
    templates: &TemplatePolicy,
) -> Materialization {
    let Materialization::Leaf(mut take) = materialization else {
        return materialization;
    };
    if policy.is_copy() && take.source == take.dest {
        take.dest = templates.deployed_name(&take.source);
    }
    Materialization::Leaf(take)
}

fn kept_leaves_under(
    materialization: &Materialization,
    kept: &[ResolvedTake],
) -> Vec<ResolvedTake> {
    let Materialization::CollapsedDir { dir } = materialization else {
        return Vec::new();
    };
    let prefix = format!("{dir}/");
    kept.iter()
        .filter(|r| r.dest.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Projects every binding of one target, then rejects any destination two bindings
/// land on, folded the way `take` folds within a binding (NFC + simple-lowercase).
///
/// # Errors
/// Errors if any binding fails to resolve or two bindings collide on a destination.
pub fn project_target(
    target_name: &str,
    inputs: &[BindingProjectionInput<'_>],
) -> std::result::Result<TargetProjection, ProjectionError> {
    let bindings = inputs
        .iter()
        .map(project_binding)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    reject_cross_binding_dups(target_name, &bindings)?;
    let artifacts: Vec<ProjectedArtifact> = bindings
        .iter()
        .flat_map(|binding| binding.artifacts.iter().cloned())
        .collect();
    let warnings: Vec<ProjectionWarning> = bindings
        .iter()
        .flat_map(|binding| binding.warnings.iter().cloned())
        .collect();
    Ok(TargetProjection {
        target: target_name.to_owned(),
        bindings,
        artifacts,
        warnings,
    })
}

/// Projects every target purely from resolved value inputs, aggregating each target's
/// projection and the workspace-wide warnings into one [`Projection`].
///
/// # Errors
/// Errors if any target fails to project or two of its bindings collide on a destination.
pub fn build_workspace(
    targets: &[WorkspaceTargetInput<'_>],
) -> Result<Projection, ProjectionError> {
    let projected = targets
        .iter()
        .map(|workspace_target| project_target(workspace_target.target, &workspace_target.bindings))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let warnings = projected
        .iter()
        .flat_map(|target| target.warnings.iter().cloned())
        .collect();
    Ok(Projection {
        targets: projected,
        warnings,
    })
}

fn reject_cross_binding_dups(
    target_name: &str,
    bindings: &[BindingProjection],
) -> std::result::Result<(), ProjectionError> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for binding in bindings {
        for artifact in &binding.artifacts {
            let dest = artifact.destination.as_str().to_owned();
            if let Some(first) = seen.insert(fold_dest(&dest), dest.clone()) {
                return Err(duplicate_destination(target_name, &first, &dest));
            }
        }
    }
    for (folded, dest) in &seen {
        for ancestor in ancestor_prefixes(folded) {
            if let Some(ancestor_dest) = seen.get(ancestor) {
                return Err(duplicate_destination(target_name, ancestor_dest, dest));
            }
        }
    }
    Ok(())
}

fn duplicate_destination(target_name: &str, first: &str, second: &str) -> ProjectionError {
    ProjectionError::DuplicateDestination {
        rendered: Box::new(cross_binding_dup_diagnostic(target_name, first, second)),
    }
}

fn ancestor_prefixes(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(i, _)| &path[..i])
}

fn cross_binding_dup_diagnostic(target_name: &str, first: &str, second: &str) -> Error {
    let entry = if first <= second {
        format!("{first} / {second}")
    } else {
        format!("{second} / {first}")
    };
    SelectionDiagnostic {
        entry,
        matched_against: "the target's destinations across all bindings".to_string(),
        why: "two bindings resolve to the same destination".to_string(),
        did_you_mean: None,
        remedy: "rename one source's leaf, or separate the bindings under the layout".to_string(),
        debug_hint: Some(format!("phora preview --target {target_name}")),
        details: Vec::new(),
    }
    .sync()
}

/// The published artifact keys prune republishes for one binding: each artifact's
/// collapsed-dir or leaf destination key, in native projected order.
#[must_use]
pub fn projected_artifact_keys(binding: &BindingProjection) -> Vec<String> {
    binding
        .artifacts
        .iter()
        .map(|artifact| artifact.materialization.published_key().to_owned())
        .collect()
}

#[cfg(test)]
mod projection_builder_tests {
    use std::path::Path;

    use crate::config::{DeployMode, LayoutConfig, ParsedSource, Source, TakeEntry, TemplateOptIn};
    use crate::diagnostic::{MATCHED_AGAINST, REMEDY, SELECTION, TO_DEBUG};
    use crate::kernel::Materialization;
    use crate::source::SourceInventory;

    use super::{project_binding, project_target, projected_artifact_keys};
    use crate::projection::diagnostic::ProjectionError;
    use crate::projection::model::{
        BindingProjection, BindingProjectionInput, CollapsePreference, LayoutSpec,
        MaterializationPolicy, OfferSpec, ResolvedSourceRef, TakeSpec, TemplatePolicy,
    };

    const COMMIT: &str = "c0ffee";

    fn source_with(
        root: Option<&str>,
        include: &[&str],
        exclude: &[&str],
        mode: DeployMode,
    ) -> ParsedSource {
        use std::fmt::Write as _;
        let mut toml = String::from("git = \"https://example.com/x.git\"\n");
        if let Some(r) = root {
            let _ = writeln!(toml, "root = \"{r}\"");
        }
        if !include.is_empty() {
            let list = include
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(toml, "include = [{list}]");
        }
        if !exclude.is_empty() {
            let list = exclude
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(toml, "exclude = [{list}]");
        }
        match mode {
            DeployMode::Link => toml.push_str("deploy = \"link\"\n"),
            DeployMode::Copy => toml.push_str("deploy = \"copy\"\n"),
        }
        let raw = toml::from_str::<Source>(&toml).expect("source DTO deserializes");
        ParsedSource::parse("s", &raw).expect("source parses")
    }

    fn named_layout(kind: &str) -> LayoutConfig {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            layout: LayoutConfig,
        }
        toml::from_str::<Wrapper>(&format!("layout = \"{kind}\""))
            .map(|w| w.layout)
            .expect("layout parses")
    }

    struct Case<'a> {
        source: &'a ParsedSource,
        leaves: Vec<String>,
        take: Option<Vec<TakeEntry>>,
        collapse: Option<bool>,
        layout: LayoutConfig,
        identity: &'a str,
    }

    impl<'a> Case<'a> {
        fn flat(source: &'a ParsedSource, leaves: &[&str]) -> Self {
            Self {
                source,
                leaves: leaves.iter().map(|s| (*s).to_string()).collect(),
                take: None,
                collapse: None,
                layout: LayoutConfig::default(),
                identity: "s",
            }
        }

        fn project(&self) -> BindingProjection {
            self.try_project().expect("binding projects")
        }

        fn try_project(&self) -> std::result::Result<BindingProjection, ProjectionError> {
            let inventory = SourceInventory::from_paths(self.leaves.iter().map(String::as_str))
                .expect("valid paths");
            let offer = OfferSpec::from(self.source.offer());
            let take = TakeSpec::from_entries(self.take.as_deref());
            let templates = TemplatePolicy::from(&TemplateOptIn::SuffixOnly);
            let layout = LayoutSpec::from(&self.layout);
            let source = ResolvedSourceRef::new("s", COMMIT);
            project_binding(&BindingProjectionInput {
                identity: self.identity,
                source: &source,
                offer: &offer,
                inventory: &inventory,
                take: &take,
                collapse: CollapsePreference::from(self.collapse),
                materialization: MaterializationPolicy::from(&self.source.deploy_mode()),
                layout: &layout,
                templates: &templates,
            })
        }
    }

    fn dests(binding: &BindingProjection) -> Vec<String> {
        binding
            .artifacts
            .iter()
            .map(|a| a.destination.as_str().to_owned())
            .collect()
    }

    fn materializations(binding: &BindingProjection) -> Vec<Materialization> {
        binding
            .artifacts
            .iter()
            .map(|a| a.materialization.clone())
            .collect()
    }

    fn leaf(source: &str, dest: &str) -> Materialization {
        Materialization::Leaf(crate::kernel::ResolvedTake {
            source: source.to_string(),
            dest: dest.to_string(),
        })
    }

    fn collapsed(dir: &str) -> Materialization {
        Materialization::CollapsedDir {
            dir: dir.to_string(),
        }
    }

    fn assert_named_diagnostic(rendered: &str, entry: &str) {
        for phrase in [SELECTION, MATCHED_AGAINST, REMEDY, TO_DEBUG] {
            assert!(
                rendered.contains(phrase),
                "the rejection must render `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains(entry),
            "the rejection must name `{entry}`; got:\n{rendered}"
        );
    }

    fn rendered(error: ProjectionError) -> String {
        crate::error::Error::from(error).to_string()
    }

    #[test]
    fn flat_bind_projects_source_offer_to_root_relative_leaf_set() {
        let source = source_with(Some("editor"), &["*.lua"], &[], DeployMode::Copy);
        let case = Case::flat(
            &source,
            &["editor/init.lua", "editor/README.md", "other/x.lua"],
        );
        let binding = case.project();
        assert_eq!(
            materializations(&binding),
            vec![leaf("init.lua", "init.lua")],
            "the offer re-anchors at root `editor`, drops the unmatched sibling, and publishes \
             the root-relative `init.lua`"
        );
    }

    #[test]
    fn link_bind_discovers_working_tree_leaves_with_dotfiles_matching() {
        let source = source_with(None, &[], &[], DeployMode::Link);
        let case = Case::flat(&source, &[".zshrc", ".config/nvim/init.lua", "plain.txt"]);
        let binding = case.project();
        assert_eq!(
            dests(&binding),
            vec![
                ".config".to_string(),
                ".zshrc".to_string(),
                "plain.txt".to_string()
            ],
            "an implicit-full offer keeps offered dotfiles with no opt-in, and a wholly-taken \
             dot-dir collapses"
        );
    }

    #[test]
    fn omitted_take_keeps_every_offered_leaf_at_identity() {
        let source = source_with(None, &["*.md"], &[], DeployMode::Copy);
        let case = Case::flat(&source, &["a.md", "b.md", "skip.txt"]);
        let binding = case.project();
        assert_eq!(
            dests(&binding),
            vec!["a.md".to_string(), "b.md".to_string()],
            "an omitted take projects every offered leaf at identity"
        );
    }

    #[test]
    fn take_literal_outside_offer_is_a_hard_error() {
        let source = source_with(None, &["*.md"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["present.md"]);
        case.take = Some(vec![TakeEntry::Leaf("absent.md".to_string())]);
        let err = case
            .try_project()
            .expect_err("a take literal outside the offer must hard-error");
        assert!(
            matches!(err, ProjectionError::LeafNotOffered { .. }),
            "an unoffered take literal is a structured LeafNotOffered; got {err:?}"
        );
        assert_named_diagnostic(&rendered(err), "absent.md");
    }

    #[test]
    fn take_rename_maps_leaf_to_dest_destructively() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["x.md", "untouched.md"]);
        case.take = Some(vec![TakeEntry::Rename {
            src: "x.md".to_string(),
            dest: "renamed.md".to_string(),
        }]);
        let binding = case.project();
        assert_eq!(
            materializations(&binding),
            vec![leaf("x.md", "renamed.md")],
            "a rename emits the leaf only at its destination and consumes the original"
        );
        assert_eq!(dests(&binding), vec!["renamed.md".to_string()]);
    }

    #[test]
    fn collapse_link_default_wholly_taken_dir_becomes_one_collapsed_dir() {
        let source = source_with(None, &["**"], &[], DeployMode::Link);
        let case = Case::flat(&source, &["d/a.md", "d/b.md"]);
        let binding = case.project();
        assert_eq!(materializations(&binding), vec![collapsed("d")]);
        assert_eq!(dests(&binding), vec!["d".to_string()]);
    }

    #[test]
    fn deploy_mode_copy_maps_to_collapse_mode_copy() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Copy);
        let case = Case::flat(&source, &["d/a.md", "d/secret.md"]);
        assert_eq!(
            materializations(&case.project()),
            vec![collapsed("d")],
            "DeployMode::Copy maps to CollapseMode::Copy: a within-dir exclude does NOT block collapse"
        );
    }

    #[test]
    fn deploy_mode_link_maps_to_collapse_mode_link() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Link);
        let case = Case::flat(&source, &["d/a.md", "d/secret.md"]);
        assert_eq!(
            materializations(&case.project()),
            vec![leaf("d/a.md", "d/a.md")],
            "DeployMode::Link maps to CollapseMode::Link: the within-dir exclude blocks collapse"
        );
    }

    #[test]
    fn collapse_force_collapse_blocked_under_link_is_hard_error() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Link);
        let mut case = Case::flat(&source, &["d/a.md", "d/secret.md"]);
        case.collapse = Some(true);
        let err = case.try_project().expect_err(
            "`collapse = true` blocked by a within-dir exclude under link is a hard error",
        );
        assert!(matches!(err, ProjectionError::CollapseBlocked { .. }));
        let text = rendered(err);
        assert_named_diagnostic(&text, "d");
        assert!(
            text.contains("to debug: phora preview --files"),
            "a partial-take collapse block must point at the preview command; got:\n{text}"
        );
    }

    #[test]
    fn layout_flat_destination_is_published_key() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let case = Case::flat(&source, &["top.md"]);
        assert_eq!(dests(&case.project()), vec!["top.md".to_string()]);
    }

    #[test]
    fn layout_by_source_prefixes_identity() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["top.md"]);
        case.identity = "mysrc";
        case.layout = named_layout("by-source");
        assert_eq!(dests(&case.project()), vec!["mysrc/top.md".to_string()]);
    }

    #[test]
    fn layout_prefixed_joins_with_separator() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["a/deep.md"]);
        case.identity = "mysrc";
        case.layout = named_layout("prefixed");
        assert_eq!(dests(&case.project()), vec!["mysrc-a".to_string()]);
    }

    fn binding_input<'a>(
        identity: &'a str,
        source: &'a ResolvedSourceRef,
        inventory: &'a SourceInventory,
        offer: &'a OfferSpec,
        take: &'a TakeSpec,
        templates: &'a TemplatePolicy,
        layout: &'a LayoutSpec,
    ) -> BindingProjectionInput<'a> {
        BindingProjectionInput {
            identity,
            source,
            offer,
            inventory,
            take,
            collapse: CollapsePreference::default(),
            materialization: MaterializationPolicy::Copy,
            layout,
            templates,
        }
    }

    #[test]
    fn cross_binding_duplicate_dest_across_all_bindings_is_rejected() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let offer = OfferSpec::from(source.offer());
        let take = TakeSpec::from_entries(None);
        let templates = TemplatePolicy::from(&TemplateOptIn::SuffixOnly);
        let layout = LayoutSpec::from(&LayoutConfig::default());
        let inv = SourceInventory::from_paths(["shared.md"]).expect("valid paths");
        let one = ResolvedSourceRef::new("s", COMMIT);
        let two = ResolvedSourceRef::new("s", COMMIT);
        let inputs = [
            binding_input("one", &one, &inv, &offer, &take, &templates, &layout),
            binding_input("two", &two, &inv, &offer, &take, &templates, &layout),
        ];
        let err = project_target("home", &inputs)
            .expect_err("two bindings landing the same shared.md must be rejected target-globally");
        assert!(matches!(err, ProjectionError::DuplicateDestination { .. }));
        let text = rendered(err);
        assert_named_diagnostic(&text, "shared.md");
        assert!(
            text.contains("to debug: phora preview --target home"),
            "a cross-binding dup must point at the preview command scoped to the target; got:\n{text}"
        );
    }

    #[test]
    fn prune_expected_set_is_derived_from_projected_keys() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["d/a.md", "d/b.md", "top.md"]);
        case.take = Some(vec![TakeEntry::Leaf("top.md".to_string())]);
        let binding = case.project();
        assert_eq!(
            projected_artifact_keys(&binding),
            vec!["top.md".to_string()],
            "prune's expected set derives from the projected published keys"
        );
    }

    #[test]
    fn destinations_stay_target_relative() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["a.md"]);
        case.identity = "editor";
        case.layout = named_layout("by-source");
        let binding = case.project();
        let dest = binding.artifacts[0].destination.as_str();
        assert!(
            !dest.starts_with('/'),
            "the projection keeps the destination target-relative, joining the root is sync's job; got `{dest}`"
        );
        assert_eq!(
            Path::new("/home/u/deploy").join(dest),
            Path::new("/home/u/deploy/editor/a.md"),
            "a consumer reconstructs the absolute deploy path by joining the target root"
        );
    }
}
