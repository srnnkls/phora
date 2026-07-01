//! Offline preview: a per-target plan built from the lock alone, annotating each
//! binding's sync state without fetching, resolving, or writing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, DeployMode, LayoutConfig, Offer, ParsedSource, Target, TemplateOptIn};
use crate::diagnostic::did_you_mean;
use crate::error::{Error, Result};
use crate::kernel::{Materialization, OfferSelection, SourceName};
use crate::lock::{Lock, ref_discriminator};
use crate::source::SourceBackend;

use super::discover::discover_working_tree_leaves;
use super::plan::{
    BindingPlanInput, PlanWarning, PlannedItem, ResolvedBindingPlan, resolve_binding_plan,
};
use super::remote_for;

/// Whether a binding is renderable now or needs action before it can deploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SyncState {
    Synced,
    NotLocked,
    NeedsSync,
    LinkWorkingTreeGone,
}

/// One previewed file under an artifact: its deployed name and whether it renders.
///
/// `path` is the deployed name (a templated source has its `.tmpl` suffix stripped);
/// `templated` is true only for copy-mode files that render. Link files never render.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewFile {
    pub path: PathBuf,
    pub templated: bool,
}

/// A previewed artifact or a per-binding annotation; consumers must branch on `state`.
///
/// A `Synced` entry carries a real `artifact` and `destination`; the unsynced states
/// (NotLocked/NeedsSync/LinkWorkingTreeGone) leave both empty. Link bindings carry
/// `commit = "link"`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewEntry {
    pub identity: String,
    pub source: String,
    pub artifact: String,
    pub commit: String,
    pub destination: PathBuf,
    pub state: SyncState,
    /// Deployed file names (empty until `--files` enrichment).
    pub files: Vec<PreviewFile>,
    /// The source leaf when `take` renamed it; `None` for an identity leaf or a dir.
    pub rename: Option<String>,
    /// True when this entry is a collapsed directory deployed as one artifact.
    pub collapsed: bool,
}

/// A non-fatal preview warning; `TakeNoMatch.suggestions` are resolved at the preview
/// boundary from the offer set, never carried by the kernel `take` resolver.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PreviewWarning {
    TakeNoMatch {
        pattern: String,
        suggestions: Vec<String>,
    },
    CollapseBlocked {
        dir: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BindingWarnings {
    pub identity: String,
    pub source: String,
    pub warnings: Vec<PreviewWarning>,
}

/// A predicted flat-layout clash: two or more identities whose artifacts share one name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewCollision {
    pub artifact: String,
    pub sources: Vec<String>,
}

/// One target's offline preview: every binding's entries plus predicted collisions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreviewTargetPlan {
    pub target: String,
    pub path: PathBuf,
    pub entries: Vec<PreviewEntry>,
    pub collisions: Vec<PreviewCollision>,
    pub warnings: Vec<BindingWarnings>,
    /// True for a composed target whose dep carries a stripped, still-untrusted `on_change` hook:
    /// deployed but not post-processed, so the artifact may be incomplete until `phora trust`.
    pub untrusted_stripped_hook: bool,
}

/// Build every target's offline preview from the lock: never fetches, resolves, or writes.
///
/// # Errors
/// Errors only on configuration faults (an undefined source or unresolved remote);
/// an unfetched or unlocked binding is annotated, not propagated.
#[must_use = "a preview describes deployments but performs none; consume the returned PreviewTargetPlans"]
pub fn preview_targets(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    lock: Option<&Lock>,
    files: bool,
) -> Result<Vec<PreviewTargetPlan>> {
    let flagged = untrusted_stripped_targets(lock);
    config
        .targets
        .iter()
        .map(|(name, target)| {
            preview_target(
                name, target, parsed, remotes, backend, lock, files, &flagged,
            )
        })
        .collect()
}

/// Composed target names (the `composed_target` prefix of each `candidate_hooks` `hook_id`) whose
/// stripped `on_change` hook is still untrusted, reusing `sync`'s trust predicate (anti-TOFU).
fn untrusted_stripped_targets(lock: Option<&Lock>) -> std::collections::BTreeSet<String> {
    let Some(lock) = lock else {
        return std::collections::BTreeSet::new();
    };
    let trusted = super::trusted_preimages(Some(lock));
    lock.candidate_hooks
        .iter()
        .filter(|c| !trusted.contains(&c.preimage))
        .filter_map(|c| c.hook_id.split('#').next().map(str::to_owned))
        .collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "offline preview threads config, resolution maps, backend, lock, and the flagged set"
)]
fn preview_target(
    target_name: &str,
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    lock: Option<&Lock>,
    files: bool,
    flagged: &std::collections::BTreeSet<String>,
) -> Result<PreviewTargetPlan> {
    let path = target.expanded_path();
    let layout = target.layout();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();

    for binding in target.resolve_sources(parsed) {
        let source = parsed.get(binding.source).ok_or_else(|| {
            Error::Config(format!(
                "target references undefined source: {}",
                binding.source
            ))
        })?;
        let name = SourceName::trusted(binding.source);
        let ctx = BindingCtx {
            remotes,
            backend,
            path: &path,
            layout: &layout,
            source,
            binding: &binding,
            name: &name,
            files,
        };

        match source.deploy_mode() {
            DeployMode::Link => preview_link(&ctx, &mut entries, &mut warnings)?,
            DeployMode::Copy => preview_copy(&ctx, lock, &mut entries, &mut warnings)?,
        }
    }

    let collisions = detect_dest_collisions(&entries);
    Ok(PreviewTargetPlan {
        target: target_name.to_owned(),
        path,
        entries,
        collisions,
        warnings,
        untrusted_stripped_hook: flagged.contains(target_name),
    })
}

struct BindingCtx<'a> {
    remotes: &'a BTreeMap<String, String>,
    backend: &'a dyn SourceBackend,
    path: &'a Path,
    layout: &'a LayoutConfig,
    source: &'a ParsedSource,
    binding: &'a crate::config::ResolvedBinding<'a>,
    name: &'a SourceName,
    files: bool,
}

fn preview_link(
    ctx: &BindingCtx,
    entries: &mut Vec<PreviewEntry>,
    warnings: &mut Vec<BindingWarnings>,
) -> Result<()> {
    let git = remote_for(ctx.remotes, ctx.binding.source)?;
    let Ok(candidates) = discover_working_tree_leaves(Path::new(git), None) else {
        entries.push(annotation(ctx, "link", SyncState::LinkWorkingTreeGone));
        return Ok(());
    };
    let plan = resolve_plan(ctx, "link", &candidates)?;
    for item in &plan.items {
        push_item(ctx, item, "link", entries);
    }
    collect_warnings(ctx, &plan, &candidates, warnings);
    Ok(())
}

fn preview_copy(
    ctx: &BindingCtx,
    lock: Option<&Lock>,
    entries: &mut Vec<PreviewEntry>,
    warnings: &mut Vec<BindingWarnings>,
) -> Result<()> {
    let disc = ref_discriminator(&ctx.binding.effective_ref, &ctx.source.refspec());
    let Some(locked) = lock.and_then(|l| l.find_entry(ctx.binding.source, disc.as_deref())) else {
        entries.push(annotation(ctx, "", SyncState::NotLocked));
        return Ok(());
    };

    let git = remote_for(ctx.remotes, ctx.binding.source)?;
    let Ok(candidates) = ctx
        .backend
        .list_source_leaves(ctx.name, git, &locked.commit, None)
    else {
        entries.push(annotation(ctx, &locked.commit, SyncState::NeedsSync));
        return Ok(());
    };

    let plan = resolve_plan(ctx, &locked.commit, &candidates)?;
    for item in &plan.items {
        push_item(ctx, item, &locked.commit, entries);
    }
    collect_warnings(ctx, &plan, &candidates, warnings);
    Ok(())
}

fn collect_warnings(
    ctx: &BindingCtx,
    plan: &ResolvedBindingPlan,
    candidates: &[String],
    warnings: &mut Vec<BindingWarnings>,
) {
    if let Some(group) = binding_warnings(
        ctx.binding.identity,
        ctx.binding.source,
        plan,
        ctx.source.offer(),
        candidates,
    ) {
        warnings.push(group);
    }
}

fn binding_warnings(
    identity: &str,
    source: &str,
    plan: &ResolvedBindingPlan,
    offer: Offer<'_>,
    candidates: &[String],
) -> Option<BindingWarnings> {
    if plan.warnings.is_empty() {
        return None;
    }
    let offered = offered_leaves(&offer, candidates).unwrap_or_default();
    let warnings = plan
        .warnings
        .iter()
        .map(|w| match w {
            PlanWarning::TakeNoMatchGlob(pattern) => {
                let refs = offered.iter().map(String::as_str);
                PreviewWarning::TakeNoMatch {
                    pattern: pattern.clone(),
                    suggestions: did_you_mean(pattern, refs).unwrap_or_default(),
                }
            }
            PlanWarning::LostCollapseToExclude(dir) => {
                PreviewWarning::CollapseBlocked { dir: dir.clone() }
            }
        })
        .collect();
    Some(BindingWarnings {
        identity: identity.to_owned(),
        source: source.to_owned(),
        warnings,
    })
}

pub(crate) fn offered_leaves(offer: &Offer<'_>, candidates: &[String]) -> Result<Vec<String>> {
    let selection = OfferSelection::compile(offer.includes(), offer.excludes(), offer.root())?;
    let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
    Ok(selection.select(&refs))
}

fn resolve_plan(
    ctx: &BindingCtx,
    commit: &str,
    candidates: &[String],
) -> Result<ResolvedBindingPlan> {
    let input = BindingPlanInput {
        identity: ctx.binding.identity,
        source: ctx.binding.source,
        commit,
        offer: ctx.source.offer(),
        candidate_leaves: candidates,
        take: ctx.binding.take,
        mode: ctx.source.deploy_mode(),
        collapse: ctx.binding.collapse,
        layout: ctx.layout,
        target_path: ctx.path,
        template_opt_in: &ctx.binding.template_opt_in,
    };
    resolve_binding_plan(&input)
}

fn push_item(ctx: &BindingCtx, item: &PlannedItem, commit: &str, entries: &mut Vec<PreviewEntry>) {
    let key = item.materialization.published_key().to_owned();
    let mut entry = PreviewEntry {
        identity: ctx.binding.identity.to_owned(),
        source: ctx.binding.source.to_owned(),
        artifact: key,
        commit: commit.to_owned(),
        destination: item.destination.clone(),
        state: SyncState::Synced,
        files: Vec::new(),
        rename: rename_of(
            ctx.source.deploy_mode(),
            &ctx.binding.template_opt_in,
            &item.materialization,
        ),
        collapsed: matches!(item.materialization, Materialization::CollapsedDir { .. }),
    };
    if ctx.files {
        entry.files = item_files(ctx, item);
    }
    entries.push(entry);
}

fn rename_of(
    mode: DeployMode,
    template_opt_in: &TemplateOptIn,
    materialization: &Materialization,
) -> Option<String> {
    let Materialization::Leaf(take) = materialization else {
        return None;
    };
    let suffix_strip =
        mode == DeployMode::Copy && template_opt_in.deployed_name(&take.source) == take.dest;
    (take.source != take.dest && !suffix_strip).then(|| take.source.clone())
}

/// Deployed file names under one materialization, derived from the plan: a leaf is its
/// single dest; a collapsed dir is each kept child's dir-relative deployed name.
fn item_files(ctx: &BindingCtx, item: &PlannedItem) -> Vec<PreviewFile> {
    let templated = !matches!(ctx.source.deploy_mode(), DeployMode::Link);
    match &item.materialization {
        Materialization::Leaf(take) => {
            let dest = take.dest.rsplit('/').next().unwrap_or(&take.dest);
            vec![PreviewFile {
                path: PathBuf::from(dest),
                templated: templated && ctx.binding.template_opt_in.renders(&take.source),
            }]
        }
        Materialization::CollapsedDir { dir } => {
            let prefix = format!("{dir}/");
            let mut files: Vec<PreviewFile> = item
                .kept_leaves
                .iter()
                .filter_map(|kept| {
                    let child = kept.dest.strip_prefix(&prefix)?;
                    let deployed = if templated {
                        ctx.binding.template_opt_in.deployed_name(child)
                    } else {
                        child.to_owned()
                    };
                    Some(PreviewFile {
                        path: PathBuf::from(deployed),
                        templated: templated && ctx.binding.template_opt_in.renders(&kept.source),
                    })
                })
                .collect();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            files
        }
    }
}

fn annotation(ctx: &BindingCtx, commit: &str, state: SyncState) -> PreviewEntry {
    PreviewEntry {
        identity: ctx.binding.identity.to_owned(),
        source: ctx.binding.source.to_owned(),
        artifact: String::new(),
        commit: commit.to_owned(),
        destination: PathBuf::new(),
        state,
        files: Vec::new(),
        rename: None,
        collapsed: false,
    }
}

fn detect_dest_collisions(entries: &[PreviewEntry]) -> Vec<PreviewCollision> {
    let mut by_dest: BTreeMap<&Path, Vec<String>> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.state == SyncState::Synced) {
        by_dest
            .entry(entry.destination.as_path())
            .or_default()
            .push(entry.identity.clone());
    }

    by_dest
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(dest, sources)| PreviewCollision {
            artifact: dest
                .file_name()
                .unwrap_or(dest.as_os_str())
                .to_string_lossy()
                .into_owned(),
            sources,
        })
        .collect()
}

#[cfg(test)]
mod preview_warning_tests {
    use std::path::PathBuf;

    use crate::config::{DeployMode, LayoutConfig, ParsedSource, Source, TakeEntry, TemplateOptIn};
    use crate::sync::plan::{BindingPlanInput, ResolvedBindingPlan, resolve_binding_plan};

    use super::{BindingWarnings, PreviewWarning, binding_warnings};

    fn leaves(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn source_with(include: &[&str], mode: DeployMode) -> ParsedSource {
        use std::fmt::Write as _;
        let mut toml = String::from("git = \"https://example.com/x.git\"\n");
        if !include.is_empty() {
            let list = include
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(toml, "include = [{list}]");
        }
        match mode {
            DeployMode::Link => toml.push_str("deploy = \"link\"\n"),
            DeployMode::Copy => toml.push_str("deploy = \"copy\"\n"),
        }
        let raw = toml::from_str::<Source>(&toml).expect("source DTO deserializes");
        ParsedSource::parse("s", &raw).expect("source parses")
    }

    fn plan_for(
        source: &ParsedSource,
        candidates: &[String],
        take: Option<&[TakeEntry]>,
        collapse: Option<bool>,
    ) -> ResolvedBindingPlan {
        let layout = LayoutConfig::default();
        let target_path = PathBuf::from("/dst");
        let input = BindingPlanInput {
            identity: "s",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: candidates,
            take,
            mode: source.deploy_mode(),
            collapse,
            layout: &layout,
            target_path: &target_path,
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        resolve_binding_plan(&input).expect("plan resolves")
    }

    #[test]
    fn take_no_match_glob_carries_a_did_you_mean_over_the_offer_set() {
        let source = source_with(&["**"], DeployMode::Copy);
        let candidates = leaves(&["init.lua", "keymaps.lua"]);
        let take = [TakeEntry::Leaf("init.lus*".to_string())];
        let plan = plan_for(&source, &candidates, Some(&take), None);

        let group: BindingWarnings = binding_warnings("s", "s", &plan, source.offer(), &candidates)
            .expect("a no-match-glob take must yield a binding warning group");

        let warning = group
            .warnings
            .iter()
            .find_map(|w| match w {
                PreviewWarning::TakeNoMatch {
                    pattern,
                    suggestions,
                } => Some((pattern.clone(), suggestions.clone())),
                PreviewWarning::CollapseBlocked { .. } => None,
            })
            .expect("the group must carry a TakeNoMatch warning");

        assert_eq!(
            warning.0, "init.lus*",
            "the warning must name the unmatched take pattern; got {:?}",
            warning.0
        );
        assert_eq!(
            warning.1,
            vec!["init.lua".to_string()],
            "the nearest offered leaf `init.lua` must be suggested from the offer set; got {:?}",
            warning.1
        );
    }

    #[test]
    fn collapse_blocked_under_link_surfaces_a_per_binding_warning() {
        let source = source_with(&["d/a.md"], DeployMode::Link);
        let candidates = leaves(&["d/a.md", "d/secret.md"]);
        let plan = plan_for(&source, &candidates, None, None);

        let group = binding_warnings("s", "s", &plan, source.offer(), &candidates)
            .expect("a within-dir exclude blocking link collapse must yield a warning group");

        assert!(
            group.warnings.iter().any(|w| matches!(
                w,
                PreviewWarning::CollapseBlocked { dir } if dir == "d"
            )),
            "the blocked dir `d` must surface as a CollapseBlocked warning; got {:?}",
            group.warnings
        );
    }

    #[test]
    fn copy_mode_tmpl_suffix_strip_is_not_a_rename() {
        let source = source_with(&["**"], DeployMode::Copy);
        let candidates = leaves(&["config.lua.tmpl"]);
        let plan = plan_for(&source, &candidates, None, None);
        assert_eq!(
            super::rename_of(
                DeployMode::Copy,
                &TemplateOptIn::SuffixOnly,
                &plan.items[0].materialization,
            ),
            None,
            "a copy-mode `.tmpl` identity leaf is suffix-stripped by the template engine, not \
             renamed by `take`, so preview must not report it as a rename"
        );
    }

    #[test]
    fn a_take_rename_is_reported_as_a_rename() {
        let source = source_with(&["**"], DeployMode::Copy);
        let candidates = leaves(&["x.md"]);
        let take = [TakeEntry::Rename {
            src: "x.md".to_string(),
            dest: "renamed.md".to_string(),
        }];
        let plan = plan_for(&source, &candidates, Some(&take), None);
        assert_eq!(
            super::rename_of(
                DeployMode::Copy,
                &TemplateOptIn::SuffixOnly,
                &plan.items[0].materialization,
            ),
            Some("x.md".to_string()),
            "a genuine `take` rename must surface the source leaf as the rename origin"
        );
    }

    #[test]
    fn a_clean_binding_carries_no_warning_group() {
        let source = source_with(&["**"], DeployMode::Copy);
        let candidates = leaves(&["a.md", "b.md"]);
        let plan = plan_for(&source, &candidates, None, None);

        assert!(
            binding_warnings("s", "s", &plan, source.offer(), &candidates).is_none(),
            "a binding with no plan warnings must produce no warning group"
        );
    }
}
