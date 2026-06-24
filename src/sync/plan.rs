//! Registry-free, network-free plan builder shared by sync, prune, and preview.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{
    Config, DeployMode, LayoutConfig, Offer, ParsedSource, TakeEntry, Target, TemplateOptIn,
};
use crate::diagnostic::SelectionDiagnostic;
use crate::error::{Error, Result};
use crate::kernel::{
    CollapseChoice, CollapseMode, Materialization, OfferSelection, SourceName, Take, fold_dest,
    is_take_glob, plan_collapse, resolve_take,
};
use crate::lock::encode_ref;
use crate::source::SourceBackend;

use super::discover::discover_working_tree_leaves;
use super::remote_for;

/// One target's deployment plan: every binding's resolved leaf-granular plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPlan {
    pub target: String,
    pub bindings: Vec<ResolvedBindingPlan>,
}

/// One binding's leaf-granular plan: the offer-sealed, take-projected, collapse-folded
/// materializations and their layout-composed destinations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBindingPlan {
    pub identity: String,
    pub source: String,
    pub commit: String,
    pub items: Vec<PlannedItem>,
    pub warnings: Vec<PlanWarning>,
}

/// A single planned deployment unit and its destination under the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedItem {
    pub materialization: Materialization,
    pub destination: PathBuf,
    pub kept_leaves: Vec<crate::kernel::ResolvedTake>,
}

/// A non-fatal take/collapse outcome carried up from the kernel.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlanWarning {
    TakeNoMatchGlob(String),
    LostCollapseToExclude(String),
}

/// Config-typed inputs the resolver maps onto the kernel for one binding.
pub struct BindingPlanInput<'a> {
    pub identity: &'a str,
    pub source: &'a str,
    pub commit: &'a str,
    pub offer: Offer<'a>,
    pub candidate_leaves: &'a [String],
    pub take: Option<&'a [TakeEntry]>,
    pub mode: DeployMode,
    pub collapse: Option<bool>,
    pub layout: &'a LayoutConfig,
    pub target_path: &'a Path,
    pub template_opt_in: &'a TemplateOptIn,
}

/// Resolves one binding: compile the offer, seal `take` over it, fold collapse, then
/// compose each materialization's destination under the layout.
///
/// # Errors
/// Errors if the offer fails to compile, `take` references a non-offered leaf, two kept
/// leaves collide, or a demanded collapse is blocked.
pub fn resolve_binding_plan(input: &BindingPlanInput<'_>) -> Result<ResolvedBindingPlan> {
    let candidates: Vec<&str> = input.candidate_leaves.iter().map(String::as_str).collect();
    let selection = OfferSelection::compile(
        input.offer.includes(),
        input.offer.excludes(),
        input.offer.root(),
    )?;
    let offer = selection.select(&candidates);
    let physical_tree = OfferSelection::compile(&[], &[], input.offer.root())?.select(&candidates);

    let takes = input.take.map(map_take_entries);
    let resolution = resolve_take(&offer, takes.as_deref())?;

    let mode = match input.mode {
        DeployMode::Link => CollapseMode::Link,
        DeployMode::Copy => CollapseMode::Copy,
    };
    let choice = match input.collapse {
        None => CollapseChoice::Default,
        Some(false) => CollapseChoice::ForcePerLeaf,
        Some(true) => CollapseChoice::ForceCollapse,
    };
    let plan = plan_collapse(&resolution.kept, &physical_tree, mode, choice)?;
    let materializations =
        reject_partial_take_collapse(plan.items, &resolution.kept, &offer, input.collapse)?
            .into_iter()
            .map(|m| apply_deployed_name(m, input))
            .collect::<Vec<_>>();

    let items = materializations
        .into_iter()
        .map(|materialization| {
            let key = materialization.published_key();
            let destination = input
                .target_path
                .join(input.layout.artifact_path(input.identity, key));
            let kept_leaves = kept_leaves_under(&materialization, &resolution.kept);
            PlannedItem {
                materialization,
                destination,
                kept_leaves,
            }
        })
        .collect();

    let mut warnings: Vec<PlanWarning> = resolution
        .warnings
        .into_iter()
        .map(|w| {
            let crate::kernel::TakeWarning::NoMatchGlob(p) = w;
            PlanWarning::TakeNoMatchGlob(p)
        })
        .chain(plan.warnings.into_iter().map(|w| {
            let crate::kernel::CollapseWarning::LostCollapseToExclude { dir } = w;
            PlanWarning::LostCollapseToExclude(dir)
        }))
        .collect();
    warnings.sort();

    Ok(ResolvedBindingPlan {
        identity: input.identity.to_owned(),
        source: input.source.to_owned(),
        commit: input.commit.to_owned(),
        items,
        warnings,
    })
}

/// A `CollapsedDir` is sound only when every *offered* leaf under it is kept at
/// identity; a `take` that drops an offered sibling makes the dir partial. The
/// partial dir is re-collapsed against only its offered-and-kept leaves, so a
/// wholly-taken sub-dir still collapses while the dropped siblings stay out.
fn reject_partial_take_collapse(
    items: Vec<Materialization>,
    kept: &[crate::kernel::ResolvedTake],
    offer: &[String],
    collapse: Option<bool>,
) -> Result<Vec<Materialization>> {
    let kept_at_identity: std::collections::BTreeSet<&str> = kept
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
        let kept_under: Vec<crate::kernel::ResolvedTake> = kept
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
    }
    .sync()
}

fn map_take_entries(entries: &[TakeEntry]) -> Vec<Take<'_>> {
    entries
        .iter()
        .map(|entry| match entry {
            TakeEntry::Leaf(leaf) if is_take_glob(leaf) => Take::Glob(leaf),
            TakeEntry::Leaf(leaf) => Take::Literal(leaf),
            TakeEntry::Rename { src, dest } => Take::Rename { src, dest },
        })
        .collect()
}

fn apply_deployed_name(
    materialization: Materialization,
    input: &BindingPlanInput<'_>,
) -> Materialization {
    let Materialization::Leaf(mut take) = materialization else {
        return materialization;
    };
    if input.mode == DeployMode::Copy && take.source == take.dest {
        take.dest = input.template_opt_in.deployed_name(&take.source);
    }
    Materialization::Leaf(take)
}

fn kept_leaves_under(
    materialization: &Materialization,
    kept: &[crate::kernel::ResolvedTake],
) -> Vec<crate::kernel::ResolvedTake> {
    let Materialization::CollapsedDir { dir } = materialization else {
        return Vec::new();
    };
    let prefix = format!("{dir}/");
    kept.iter()
        .filter(|r| r.dest.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Resolves every binding of one target, then rejects any destination two bindings
/// land on, folded the way `take` folds within a binding (NFC + simple-lowercase).
///
/// # Errors
/// Errors if any binding fails to resolve or two bindings collide on a destination.
pub fn resolve_target_plan(
    target_name: &str,
    inputs: &[BindingPlanInput<'_>],
) -> Result<TargetPlan> {
    let bindings = inputs
        .iter()
        .map(resolve_binding_plan)
        .collect::<Result<Vec<_>>>()?;
    reject_cross_binding_dups(target_name, &bindings)?;
    Ok(TargetPlan {
        target: target_name.to_owned(),
        bindings,
    })
}

fn reject_cross_binding_dups(target_name: &str, bindings: &[ResolvedBindingPlan]) -> Result<()> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for binding in bindings {
        for item in &binding.items {
            let dest = item.destination.to_string_lossy().into_owned();
            if let Some(first) = seen.insert(fold_dest(&dest), dest.clone()) {
                return Err(cross_binding_dup_diagnostic(target_name, &first, &dest));
            }
        }
    }
    for (folded, dest) in &seen {
        for ancestor in ancestor_prefixes(folded) {
            if let Some(ancestor_dest) = seen.get(ancestor) {
                return Err(cross_binding_dup_diagnostic(
                    target_name,
                    ancestor_dest,
                    dest,
                ));
            }
        }
    }
    Ok(())
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
    }
    .sync()
}

/// The published artifact keys prune republishes for one binding: each item's
/// collapsed-dir or leaf destination key.
#[must_use]
pub fn expected_artifact_keys(plan: &ResolvedBindingPlan) -> Vec<String> {
    plan.items
        .iter()
        .map(|item| item.materialization.published_key().to_owned())
        .collect()
}

/// Plan one target's deployments: registry-free and network-free, discovering each
/// binding's candidate leaves via the source seam and resolving the leaf-granular plan,
/// taking resolved commits as a precondition; it never fetches or writes.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
#[must_use = "a plan describes deployments but performs none; consume the returned TargetPlan"]
pub fn plan_target(
    target_name: &str,
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<TargetPlan> {
    let path = target.expanded_path();
    let layout = target.layout();

    let mut discovered = Vec::new();
    for binding in target.resolve_sources(parsed) {
        let source = parsed.get(binding.source).ok_or_else(|| {
            Error::Config(format!(
                "target references undefined source: {}",
                binding.source
            ))
        })?;
        let commit_key = (
            binding.source.to_owned(),
            encode_ref(&binding.effective_ref),
        );
        let commit = resolved_commits
            .get(&commit_key)
            .ok_or_else(|| {
                Error::Sync(format!(
                    "no resolved commit for {} at {}",
                    binding.source, binding.effective_ref
                ))
            })?
            .clone();
        let name = SourceName::trusted(binding.source);
        let leaves = discover_binding_leaves(source, &name, &commit, remotes, backend)?;
        discovered.push(DiscoveredBinding {
            identity: binding.identity.to_owned(),
            source: binding.source.to_owned(),
            commit,
            offer: source.offer(),
            leaves,
            take: binding.take.map(<[TakeEntry]>::to_vec),
            mode: source.deploy_mode(),
            collapse: binding.collapse,
            template_opt_in: binding.template_opt_in,
        });
    }

    let inputs: Vec<BindingPlanInput<'_>> = discovered
        .iter()
        .map(|d| BindingPlanInput {
            identity: &d.identity,
            source: &d.source,
            commit: &d.commit,
            offer: d.offer,
            candidate_leaves: &d.leaves,
            take: d.take.as_deref(),
            mode: d.mode,
            collapse: d.collapse,
            layout: &layout,
            target_path: &path,
            template_opt_in: &d.template_opt_in,
        })
        .collect();

    resolve_target_plan(target_name, &inputs)
}

struct DiscoveredBinding<'a> {
    identity: String,
    source: String,
    commit: String,
    offer: Offer<'a>,
    leaves: Vec<String>,
    take: Option<Vec<TakeEntry>>,
    mode: DeployMode,
    collapse: Option<bool>,
    template_opt_in: TemplateOptIn,
}

/// Every candidate leaf one binding offers at `commit`, unfiltered: the offer's
/// own include/exclude is applied downstream by `OfferSelection`.
fn discover_binding_leaves(
    source: &ParsedSource,
    source_name: &SourceName,
    commit: &str,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
) -> Result<Vec<String>> {
    let git = remote_for(remotes, source_name.as_str())?;
    match source.deploy_mode() {
        DeployMode::Link => Ok(discover_working_tree_leaves(Path::new(git), None)?),
        DeployMode::Copy => Ok(backend.list_source_leaves(source_name, git, commit, None)?),
    }
}

/// Plan every target in `config`, forwarding to `plan_target` for each.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
#[must_use = "a plan describes deployments but performs none; consume the returned TargetPlans"]
pub fn plan_targets(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<Vec<TargetPlan>> {
    config
        .targets
        .iter()
        .map(|(name, target)| plan_target(name, target, parsed, remotes, backend, resolved_commits))
        .collect()
}

#[cfg(test)]
mod leaf_granular_resolver_tests {
    use std::path::{Path, PathBuf};

    use crate::config::{DeployMode, LayoutConfig, ParsedSource, Source, TakeEntry, TemplateOptIn};
    use crate::diagnostic::{MATCHED_AGAINST, REMEDY, SELECTION, TO_DEBUG};
    use crate::kernel::Materialization;

    use super::{BindingPlanInput, PlannedItem, ResolvedBindingPlan, resolve_binding_plan};

    fn leaves(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn take_leaves(items: &[&str]) -> Vec<TakeEntry> {
        items
            .iter()
            .map(|s| TakeEntry::Leaf((*s).to_string()))
            .collect()
    }

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

    struct Args<'a> {
        source: &'a ParsedSource,
        candidate_leaves: Vec<String>,
        take: Option<Vec<TakeEntry>>,
        layout: LayoutConfig,
        target_path: PathBuf,
        identity: &'a str,
    }

    impl<'a> Args<'a> {
        fn flat(source: &'a ParsedSource, candidates: &[&str]) -> Self {
            Self {
                source,
                candidate_leaves: leaves(candidates),
                take: None,
                layout: LayoutConfig::default(),
                target_path: PathBuf::from("/dst"),
                identity: "s",
            }
        }

        fn resolve(&self) -> ResolvedBindingPlan {
            self.try_resolve().expect("binding plan resolves")
        }

        fn try_resolve(&self) -> crate::error::Result<ResolvedBindingPlan> {
            let input = BindingPlanInput {
                identity: self.identity,
                source: "s",
                commit: "c0ffee",
                offer: self.source.offer(),
                candidate_leaves: &self.candidate_leaves,
                take: self.take.as_deref(),
                mode: self.source.deploy_mode(),
                collapse: None,
                layout: &self.layout,
                target_path: &self.target_path,
                template_opt_in: &TemplateOptIn::SuffixOnly,
            };
            resolve_binding_plan(&input)
        }
    }

    fn dest_paths(plan: &ResolvedBindingPlan) -> Vec<PathBuf> {
        plan.items.iter().map(|i| i.destination.clone()).collect()
    }

    fn materializations(plan: &ResolvedBindingPlan) -> Vec<Materialization> {
        plan.items
            .iter()
            .map(|i| i.materialization.clone())
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

    #[test]
    fn flat_bind_resolves_source_offer_to_root_relative_leaf_set() {
        let source = source_with(Some("editor"), &["*.lua"], &[], DeployMode::Copy);
        let args = Args::flat(
            &source,
            &["editor/init.lua", "editor/README.md", "other/x.lua"],
        );
        let plan = args.resolve();
        assert_eq!(
            materializations(&plan),
            vec![leaf("init.lua", "init.lua")],
            "the offer re-anchors at root `editor`, drops the unmatched sibling, and publishes \
             the root-relative `init.lua`; got: {:?}",
            materializations(&plan)
        );
    }

    #[test]
    fn link_bind_discovers_working_tree_leaves_with_dotfiles_matching() {
        let source = source_with(None, &[], &[], DeployMode::Link);
        let args = Args::flat(&source, &[".zshrc", ".config/nvim/init.lua", "plain.txt"]);
        let plan = args.resolve();
        assert_eq!(
            dest_paths(&plan),
            vec![
                PathBuf::from("/dst/.config"),
                PathBuf::from("/dst/.zshrc"),
                PathBuf::from("/dst/plain.txt"),
            ],
            "an implicit-full offer keeps offered dotfiles with no opt-in, and a wholly-taken \
             dot-dir collapses; got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn omitted_take_keeps_every_offered_leaf_at_identity() {
        let source = source_with(None, &["*.md"], &[], DeployMode::Copy);
        let args = Args::flat(&source, &["a.md", "b.md", "skip.txt"]);
        let plan = args.resolve();
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/a.md"), PathBuf::from("/dst/b.md")],
            "an omitted take projects every offered leaf at identity; got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn take_literal_outside_offer_is_a_hard_error_at_plan_time() {
        let source = source_with(None, &["*.md"], &[], DeployMode::Copy);
        let mut args = Args::flat(&source, &["present.md"]);
        args.take = Some(take_leaves(&["absent.md"]));
        let rendered = args
            .try_resolve()
            .expect_err("a take literal outside the offer must hard-error at plan time")
            .to_string();
        assert_named_diagnostic(&rendered, "absent.md");
    }

    #[test]
    fn take_glob_subsets_the_offer_and_never_widens_it() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut args = Args::flat(
            &source,
            &["skills/a/SKILL.md", "skills/b/SKILL.md", "editor/init.lua"],
        );
        args.take = Some(vec![TakeEntry::Leaf("skills/**".to_string())]);
        let plan = args.resolve();
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/skills")],
            "a take glob subsets the offer to the skills subtree (collapsed) and never widens to \
             `editor/init.lua`; got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn take_rename_maps_leaf_to_dest_destructively() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut args = Args::flat(&source, &["x.md", "untouched.md"]);
        args.take = Some(vec![TakeEntry::Rename {
            src: "x.md".to_string(),
            dest: "renamed.md".to_string(),
        }]);
        let plan = args.resolve();
        assert_eq!(
            materializations(&plan),
            vec![leaf("x.md", "renamed.md")],
            "a rename emits the leaf only at its destination and consumes the original; got: {:?}",
            materializations(&plan)
        );
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/renamed.md")],
            "the destination is the renamed published key joined under the target; got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn collapse_link_default_wholly_taken_dir_becomes_one_collapsed_dir() {
        let source = source_with(None, &["**"], &[], DeployMode::Link);
        let args = Args::flat(&source, &["d/a.md", "d/b.md"]);
        let plan = args.resolve();
        assert_eq!(
            materializations(&plan),
            vec![collapsed("d")],
            "a wholly-taken dir under link collapses to one CollapsedDir; got: {:?}",
            materializations(&plan)
        );
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/d")],
            "the collapsed dir destination is the dir key under the target; got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn collapse_copy_default_within_dir_exclude_still_collapses() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Copy);
        let args = Args::flat(&source, &["d/a.md", "d/secret.md"]);
        let plan = args.resolve();
        assert_eq!(
            materializations(&plan),
            vec![collapsed("d")],
            "under copy a within-dir exclude does not block collapse; got: {:?}",
            materializations(&plan)
        );
        assert!(
            plan.warnings.is_empty(),
            "copy collapse with an excluded child emits no warning; got: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn collapse_force_per_leaf_emits_each_leaf() {
        let source = source_with(None, &["**"], &[], DeployMode::Link);
        let input = BindingPlanInput {
            identity: "s",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["d/a.md", "d/b.md"]),
            take: None,
            mode: DeployMode::Link,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        let plan = resolve_binding_plan(&input).expect("plan resolves");
        assert_eq!(
            materializations(&plan),
            vec![leaf("d/a.md", "d/a.md"), leaf("d/b.md", "d/b.md")],
            "`collapse = false` (ForcePerLeaf) emits every leaf even for a wholly-taken dir; got: {:?}",
            materializations(&plan)
        );
    }

    #[test]
    fn collapse_force_collapse_blocked_under_link_is_hard_error() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Link);
        let input = BindingPlanInput {
            identity: "s",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["d/a.md", "d/secret.md"]),
            take: None,
            mode: DeployMode::Link,
            collapse: Some(true),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        let rendered = resolve_binding_plan(&input)
            .expect_err(
                "`collapse = true` blocked by a within-dir exclude under link is a hard error",
            )
            .to_string();
        assert_named_diagnostic(&rendered, "d");
        assert!(
            rendered.contains("to debug: phora preview --files"),
            "a partial-take collapse block must point at the preview command; got:\n{rendered}"
        );
    }

    #[test]
    fn deploy_mode_copy_maps_to_collapse_mode_copy() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Copy);
        let args = Args::flat(&source, &["d/a.md", "d/secret.md"]);
        let plan = args.resolve();
        assert_eq!(
            materializations(&plan),
            vec![collapsed("d")],
            "DeployMode::Copy maps to CollapseMode::Copy: the within-dir exclude does NOT block \
             collapse; got: {:?}",
            materializations(&plan)
        );
    }

    #[test]
    fn deploy_mode_link_maps_to_collapse_mode_link() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Link);
        let args = Args::flat(&source, &["d/a.md", "d/secret.md"]);
        let plan = args.resolve();
        assert_eq!(
            materializations(&plan),
            vec![leaf("d/a.md", "d/a.md")],
            "DeployMode::Link maps to CollapseMode::Link: the within-dir exclude blocks collapse \
             and falls back per-leaf; got: {:?}",
            materializations(&plan)
        );
    }

    #[test]
    fn layout_flat_destination_is_target_join_published_key() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut args = Args::flat(&source, &["top.md"]);
        args.layout = LayoutConfig::default();
        let plan = args.resolve();
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/top.md")],
            "flat layout: destination is target_path.join(published_key); got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn layout_by_source_prefixes_identity() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut args = Args::flat(&source, &["top.md"]);
        args.identity = "mysrc";
        args.layout = named_layout("by-source");
        let plan = args.resolve();
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/mysrc/top.md")],
            "by-source layout: destination joins target/identity/key; got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn layout_prefixed_joins_with_separator() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut args = Args::flat(&source, &["a/deep.md"]);
        args.identity = "mysrc";
        args.layout = named_layout("prefixed");
        let plan = args.resolve();
        assert_eq!(
            dest_paths(&plan),
            vec![PathBuf::from("/dst/mysrc-a")],
            "prefixed layout joins identity and the multi-segment published key `a` with the \
             default `-` separator (a wholly-taken `a/` collapses to `a`); got: {:?}",
            dest_paths(&plan)
        );
    }

    #[test]
    fn cross_binding_duplicate_dest_across_all_bindings_is_rejected() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let first = BindingPlanInput {
            identity: "one",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["shared.md"]),
            take: None,
            mode: DeployMode::Copy,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        let second = BindingPlanInput {
            identity: "two",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["shared.md"]),
            take: None,
            mode: DeployMode::Copy,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        let rendered = super::resolve_target_plan("home", &[first, second])
            .expect_err(
                "two bindings landing the same /dst/shared.md must be rejected target-globally",
            )
            .to_string();
        assert_named_diagnostic(&rendered, "shared.md");
        assert!(
            rendered.contains("to debug: phora preview --target home"),
            "a cross-binding dup must point at the preview command scoped to the offending \
             target; got:\n{rendered}"
        );
    }

    #[test]
    fn cross_binding_dup_dest_uses_simple_fold_not_full_casefold() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let upper = BindingPlanInput {
            identity: "one",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["C"]),
            take: None,
            mode: DeployMode::Copy,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        let lower = BindingPlanInput {
            identity: "two",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["c"]),
            take: None,
            mode: DeployMode::Copy,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        super::resolve_target_plan("home", &[upper, lower]).expect_err(
            "`C` and `c` collide under simple fold and must be rejected across bindings",
        );

        let sharp_s = BindingPlanInput {
            identity: "one",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["straße"]),
            take: None,
            mode: DeployMode::Copy,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        let ss = BindingPlanInput {
            identity: "two",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &leaves(&["strasse"]),
            take: None,
            mode: DeployMode::Copy,
            collapse: Some(false),
            layout: &LayoutConfig::default(),
            target_path: Path::new("/dst"),
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        super::resolve_target_plan("home", &[sharp_s, ss]).expect(
            "`straße` and `strasse` are distinct under simple fold (not full case-fold) and must \
             NOT collide",
        );
    }

    #[test]
    fn fold_dest_is_crate_visible_for_cross_binding_reuse() {
        let fold: fn(&str) -> String = crate::kernel::take::fold_dest;
        assert_eq!(
            fold("C"),
            fold("c"),
            "the cross-binding dup-dest check reuses take's fold; `C` and `c` collide under it"
        );
        assert_ne!(
            fold("straße"),
            fold("strasse"),
            "fold_dest must stay reachable as a crate item AND remain SIMPLE fold, not full \
             case-fold: `straße` and `strasse` are distinct"
        );
    }

    #[test]
    fn mapped_field_is_removed_from_plan_entry() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let args = Args::flat(&source, &["top.md"]);
        let plan = args.resolve();
        let item: &PlannedItem = &plan.items[0];
        assert_eq!(
            item.destination,
            PathBuf::from("/dst/top.md"),
            "a PlannedItem carries `materialization` + `destination` only — the dead `mapped` \
             field is gone; got: {item:?}"
        );
    }

    #[test]
    fn prune_expected_set_is_derived_from_leaf_granular_plan() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut args = Args::flat(&source, &["d/a.md", "d/b.md", "top.md"]);
        args.take = Some(take_leaves(&["top.md"]));
        let plan = args.resolve();
        let keys: Vec<String> = super::expected_artifact_keys(&plan);
        assert_eq!(
            keys,
            vec!["top.md".to_string()],
            "prune's expected set derives from the leaf-granular plan's published keys, not from \
             directory-granular discovery; got: {keys:?}"
        );
    }

    fn planned(materialization: Materialization, destination: &str) -> PlannedItem {
        PlannedItem {
            materialization,
            destination: PathBuf::from(destination),
            kept_leaves: Vec::new(),
        }
    }

    fn binding(identity: &str, items: Vec<PlannedItem>) -> ResolvedBindingPlan {
        ResolvedBindingPlan {
            identity: identity.to_string(),
            source: "s".to_string(),
            commit: "c0ffee".to_string(),
            items,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn cross_binding_collapsed_dir_overlapping_a_leaf_inside_it_is_rejected() {
        let dir_binding = binding("a", vec![planned(collapsed("d"), "/dst/d")]);
        let leaf_binding = binding("b", vec![planned(leaf("a.md", "d/a.md"), "/dst/d/a.md")]);

        let rendered = super::reject_cross_binding_dups("home", &[dir_binding, leaf_binding])
            .expect_err(
                "a collapsed dir at `d` and a leaf at `d/a.md` overlap as ancestor/descendant — \
                 `d` deploys as a directory symlink the leaf would escape into — and must be \
                 rejected, not pass as distinct folded keys",
            )
            .to_string();
        assert_named_diagnostic(&rendered, "d/a.md");

        let dir_binding = binding("a", vec![planned(collapsed("d"), "/dst/d")]);
        let leaf_binding = binding("b", vec![planned(leaf("a.md", "d/a.md"), "/dst/d/a.md")]);
        super::reject_cross_binding_dups("home", &[leaf_binding, dir_binding]).expect_err(
            "overlap rejection must hold with the descendant first — it is not binding-order \
             dependent",
        );
    }

    #[test]
    fn cross_binding_sibling_sharing_a_string_prefix_is_allowed() {
        let dir_binding = binding("a", vec![planned(collapsed("d"), "/dst/d")]);
        let leaf_binding = binding("b", vec![planned(leaf("d.md", "d.md"), "/dst/d.md")]);
        super::reject_cross_binding_dups("home", &[dir_binding, leaf_binding]).expect(
            "`/dst/d` and `/dst/d.md` are distinct path components — the ancestor check splits on \
             `/`, not a raw byte prefix — and must NOT be rejected",
        );
    }
}
