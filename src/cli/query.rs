//! Read-only commands over config and registry: `list`, `where`, `check-match`, `explain`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{
    Config, DeployMode, LayoutConfig, Offer, ParsedSource, Protocol, SourceFields, TakeEntry,
    Target, TemplateOptIn, merge_configs,
};
use crate::deploy::check_artifact_state;
use crate::diagnostic::{SelectionDiagnostic, did_you_mean};
use crate::error::{Error, Result};
use crate::kernel::{Materialization, OfferSelection};
use crate::lock::{Lock, merge_locks, ref_discriminator};
use crate::paths::cache_root_for;
use crate::source::SourceBackend;
use crate::store::Registry;
use crate::sync::{
    BindingPlanInput, PlanWarning, PreviewTargetPlan, ResolvedBindingPlan, offered_leaves,
    preview_targets, resolve_binding_plan, resolved_remotes,
};

use super::render::{print_listings, render_preview_json, render_preview_tree, state_label};
use super::{build_router, load_config, load_local_config, load_locks, open_project_registry};

pub(super) fn run_list(plan: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = merge_configs(load_config()?, load_local_config(&cwd)?);
    let registry = open_project_registry(&config)?;
    if plan {
        println!("plan: run `phora sync` to apply pending changes");
        return Ok(());
    }
    let listings = list_statuses(&config, &registry)?;
    print_listings(&listings);
    Ok(())
}

pub(super) fn run_preview(sel: &PreviewSelectors, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let config = merge_configs(base, local);

    let mut config = config;
    let mut parsed = config.parsed_sources()?;
    let mut remotes = resolved_remotes(&config, &parsed)?;
    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = base_lock.map_or_else(
        || local_lock.clone(),
        |base| Some(merge_locks(&base, local_lock.as_ref())),
    );

    let cache_git = cache_root_for(config.paths.cache.as_deref(), &cwd)?.join("git");
    let backend = build_router(&config, cache_git)?;
    crate::sync::inject_composed_graph(
        &mut config,
        &mut parsed,
        &mut remotes,
        &backend,
        lock.as_ref(),
    );
    let plan = preview_plan(&config, &parsed, &remotes, &backend, lock.as_ref(), sel)?;

    print!(
        "{}",
        if json {
            render_preview_json(&plan)?
        } else {
            render_preview_tree(&plan)
        }
    );
    Ok(())
}

pub(super) fn run_explain(target: &str, source: &str, path: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = merge_configs(load_config()?, load_local_config(&cwd)?);

    let parsed = config.parsed_sources()?;
    let remotes = resolved_remotes(&config, &parsed)?;
    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = base_lock.map_or_else(
        || local_lock.clone(),
        |base| Some(merge_locks(&base, local_lock.as_ref())),
    );

    let cache_git = cache_root_for(config.paths.cache.as_deref(), &cwd)?.join("git");
    let backend = build_router(&config, cache_git)?;
    let ctx = OfflineCtx {
        config: &config,
        parsed: &parsed,
        remotes: &remotes,
        backend: &backend,
        lock: lock.as_ref(),
    };
    let report = explain_cmd(&ctx, target, source, path)?;
    print!("{}", super::render::render_explain(&report));
    Ok(())
}

/// Which slice of the preview to render: optional source/target filters and the
/// `--files` enrichment toggle.
#[derive(Debug, Default, Clone)]
pub(crate) struct PreviewSelectors {
    pub source: Option<String>,
    pub target: Option<String>,
    pub files: bool,
}

/// A filtered, optionally file-enriched offline preview across targets.
#[derive(Debug, Clone)]
pub(crate) struct PreviewPlan {
    pub targets: Vec<PreviewTargetPlan>,
}

/// Returns the offline preview filtered by the selectors, with synced entries
/// optionally enriched with their file lists.
///
/// # Errors
/// Errors if a selector names an unknown source/target or the preview build fails.
pub(crate) fn preview_plan(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    lock: Option<&Lock>,
    sel: &PreviewSelectors,
) -> Result<PreviewPlan> {
    if let Some(target) = &sel.target
        && !config.targets.contains_key(target)
    {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    if let Some(source) = &sel.source
        && !config.sources.contains_key(source)
    {
        return Err(Error::Config(format!("unknown source: {source}")));
    }

    let mut targets = preview_targets(config, parsed, remotes, backend, lock, sel.files)?;
    if let Some(target) = &sel.target {
        targets.retain(|t| &t.target == target);
    }
    if let Some(source) = &sel.source {
        for tp in &mut targets {
            tp.entries.retain(|e| &e.source == source);
        }
    }

    Ok(PreviewPlan { targets })
}

/// Reverse-lookup filter over the registry: every `Some` field is an AND constraint.
#[derive(Debug, Default, Clone)]
pub struct WhereFilter {
    pub digest: Option<String>,
    pub source: Option<String>,
    pub artifact: Option<String>,
    pub commit: Option<String>,
}

impl WhereFilter {
    fn matches(&self, record: &crate::store::RegistryRecord) -> bool {
        let eq = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        eq(&self.digest, &record.digest)
            && eq(&self.source, &record.key.source)
            && eq(&self.artifact, &record.key.artifact)
            && eq(&self.commit, &record.commit)
    }
}

/// One (source, artifact) deployment grouped across the targets it lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhereMatch {
    pub source: String,
    pub artifact: String,
    pub commit: String,
    pub digest: String,
    pub targets: Vec<String>,
}

/// Outcome of debugging include/exclude matching for a path under a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckMatchReport {
    pub artifact_allowed: bool,
    pub path_allowed: bool,
}

/// Filters the registry by the constraints in `filter`, grouping survivors by
/// (source, artifact) and listing the targets each is deployed to.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn where_cmd(registry: &dyn Registry, filter: &WhereFilter) -> Result<Vec<WhereMatch>> {
    let records = registry.list_all()?;
    let ejected = crate::store::ejected_index(registry, &records)?;
    let mut groups: BTreeMap<(String, String), WhereMatch> = BTreeMap::new();

    for record in records {
        if !filter.matches(&record) {
            continue;
        }
        let entry = groups
            .entry((record.key.source.clone(), record.key.artifact.clone()))
            .or_insert_with(|| WhereMatch {
                source: record.key.source.clone(),
                artifact: record.key.artifact.clone(),
                commit: record.commit.clone(),
                digest: record.digest.clone(),
                targets: Vec::new(),
            });
        let k = &record.key;
        let target = if ejected.contains(&(k.target.clone(), k.source.clone(), k.artifact.clone()))
        {
            format!("{} (ejected)", k.target)
        } else {
            k.target.clone()
        };
        entry.targets.push(target);
    }

    Ok(groups
        .into_values()
        .map(|mut m| {
            m.targets.sort();
            m.targets.dedup();
            m
        })
        .collect())
}

/// Reports artifact-level and path-level allow decisions for `path` under `source`.
///
/// The offer is leaf-granular: `path_allowed` is whether the leaf `path` is selected by
/// the source offer; `artifact_allowed` is whether the top-level component is reachable —
/// some leaf at or under it is selectable.
#[must_use]
pub fn check_match_cmd(source: &ParsedSource, path: &str) -> CheckMatchReport {
    let Ok(selection) = OfferSelection::compile(source.includes(), source.excludes(), None) else {
        return CheckMatchReport {
            artifact_allowed: false,
            path_allowed: false,
        };
    };
    let component = path.split('/').next().unwrap_or(path);
    let probe = format!("{component}/.phora-probe");
    let artifact_allowed = !selection.select(&[component]).is_empty()
        || !selection.select(&[probe.as_str()]).is_empty();
    CheckMatchReport {
        artifact_allowed,
        path_allowed: !selection.select(&[path]).is_empty(),
    }
}

/// How the source offer treats one path: allowed (and by which include),
/// vetoed by an exclude, or outside the offer (with nearest-leaf suggestions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferAttribution {
    /// Selected by the offer; `include` names the matching include, or `None`
    /// for an implicit-full offer (no include declared).
    Allowed { include: Option<String> },
    /// Dropped by `exclude`, having otherwise been included.
    Vetoed { exclude: String },
    /// Neither offered nor excluded — not in the candidate set after include.
    Outside { suggestions: Vec<String> },
}

/// How `take` resolves an offered leaf into a deployed artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeAttribution {
    /// Deployed at its own root-relative path.
    Identity { dest: String },
    /// Mapped to a different destination by a rename take.
    Renamed { src: String, dest: String },
    /// Folded into a collapsed directory deployed as one artifact.
    Collapsed { dir: String },
    /// Offered, but a narrowing take dropped it.
    Dropped,
}

/// One offered leaf in a no-path binding summary and how `take` resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainLeaf {
    pub leaf: String,
    pub take: TakeAttribution,
}

/// Side-effect-free offer/take attribution for `phora explain`: either a single
/// path's verdict or, with no path, the binding's offered-leaf summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplainBody {
    Path {
        path: String,
        offer: OfferAttribution,
        /// `None` when the path is outside the offer (nothing to take).
        take: Option<TakeAttribution>,
    },
    Summary {
        leaves: Vec<ExplainLeaf>,
    },
}

/// The full `phora explain` report a renderer formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainReport {
    pub target: String,
    pub source: String,
    pub body: ExplainBody,
    pub warnings: Vec<String>,
}

/// Config-typed inputs the attribution maps onto the offer + resolved plan; the
/// same shape `BindingPlanInput` consumes, so `explain` reuses the take resolver.
pub(crate) struct ExplainInput<'a> {
    pub target: &'a str,
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

/// Attributes one `path` (or, with `None`, every offered leaf) under a binding: which
/// include offered it or which exclude vetoed it, then how `take` resolves the leaf.
///
/// # Errors
/// Errors if the offer fails to compile or the take resolver rejects the binding.
pub(crate) fn explain_path(input: &ExplainInput<'_>, path: Option<&str>) -> Result<ExplainReport> {
    let plan = resolve_binding_plan(&BindingPlanInput {
        identity: input.source,
        source: input.source,
        commit: input.commit,
        offer: input.offer,
        candidate_leaves: input.candidate_leaves,
        take: input.take,
        mode: input.mode,
        collapse: input.collapse,
        layout: input.layout,
        target_path: input.target_path,
        template_opt_in: input.template_opt_in,
    })?;

    let body = match path {
        Some(path) => {
            let offer = attribute_offer(&input.offer, input.candidate_leaves, path)?;
            let local = local_path(&input.offer, input.candidate_leaves, path);
            let take = match &offer {
                OfferAttribution::Allowed { .. } => Some(attribute_take(
                    &plan,
                    local.as_deref(),
                    input.mode,
                    input.template_opt_in,
                )),
                OfferAttribution::Vetoed { .. } | OfferAttribution::Outside { .. } => None,
            };
            ExplainBody::Path {
                path: path.to_owned(),
                offer,
                take,
            }
        }
        None => ExplainBody::Summary {
            leaves: offered_leaves(&input.offer, input.candidate_leaves)?
                .into_iter()
                .map(|leaf| ExplainLeaf {
                    take: attribute_take(&plan, Some(&leaf), input.mode, input.template_opt_in),
                    leaf,
                })
                .collect(),
        },
    };

    Ok(ExplainReport {
        target: input.target.to_owned(),
        source: input.source.to_owned(),
        body,
        warnings: plan_warning_phrases(&plan),
    })
}

fn source_path(root: Option<&Path>, leaf: &str) -> String {
    match root {
        Some(r) => format!("{}/{leaf}", r.display()),
        None => leaf.to_owned(),
    }
}

fn local_path(offer: &Offer<'_>, candidates: &[String], path: &str) -> Option<String> {
    let full = source_path(offer.root(), path);
    let probe = OfferSelection::compile(&[], &[], offer.root()).ok()?;
    probe
        .select(&[full.as_str()])
        .into_iter()
        .next()
        .or_else(|| {
            candidates
                .iter()
                .any(|c| c == &full)
                .then(|| path.to_owned())
        })
}

/// Probes each include/exclude pattern individually to name the one that decides `path`.
fn attribute_offer(
    offer: &Offer<'_>,
    candidates: &[String],
    path: &str,
) -> Result<OfferAttribution> {
    let root = offer.root();
    let full = source_path(root, path);
    let included = if offer.is_implicit_full() {
        OfferSelection::compile(&[], &[], root)?
            .select(&[full.as_str()])
            .iter()
            .any(|p| !p.is_empty())
    } else {
        offer
            .includes()
            .iter()
            .any(|inc| single_include_selects(inc, root, &full))
    };

    if !included {
        let offered = offered_leaves(offer, candidates)?;
        let refs: Vec<&str> = offered.iter().map(String::as_str).collect();
        return Ok(OfferAttribution::Outside {
            suggestions: did_you_mean(path, refs.iter().copied()).unwrap_or_default(),
        });
    }

    if let Some(exclude) = offer
        .excludes()
        .iter()
        .find(|exc| single_exclude_vetoes(exc, root, &full))
    {
        return Ok(OfferAttribution::Vetoed {
            exclude: exclude.clone(),
        });
    }

    let include = offer
        .includes()
        .iter()
        .find(|inc| single_include_selects(inc, root, &full))
        .cloned();
    Ok(OfferAttribution::Allowed { include })
}

fn single_include_selects(pattern: &str, root: Option<&Path>, path: &str) -> bool {
    OfferSelection::compile(std::slice::from_ref(&pattern.to_owned()), &[], root)
        .is_ok_and(|sel| !sel.select(&[path]).is_empty())
}

fn single_exclude_vetoes(pattern: &str, root: Option<&Path>, path: &str) -> bool {
    let Ok(without) = OfferSelection::compile(&[], &[], root) else {
        return false;
    };
    let Ok(with) = OfferSelection::compile(&[], std::slice::from_ref(&pattern.to_owned()), root)
    else {
        return false;
    };
    !without.select(&[path]).is_empty() && with.select(&[path]).is_empty()
}

fn attribute_take(
    plan: &ResolvedBindingPlan,
    local: Option<&str>,
    mode: DeployMode,
    template_opt_in: &TemplateOptIn,
) -> TakeAttribution {
    let Some(local) = local else {
        return TakeAttribution::Dropped;
    };
    for item in &plan.items {
        match &item.materialization {
            Materialization::Leaf(take) if take.source == local => {
                let kept_with_suffix_strip = mode == DeployMode::Copy
                    && template_opt_in.deployed_name(&take.source) == take.dest;
                return if take.source == take.dest || kept_with_suffix_strip {
                    TakeAttribution::Identity {
                        dest: take.dest.clone(),
                    }
                } else {
                    TakeAttribution::Renamed {
                        src: take.source.clone(),
                        dest: take.dest.clone(),
                    }
                };
            }
            Materialization::CollapsedDir { dir } => {
                let prefix = format!("{dir}/");
                if local.starts_with(&prefix) && item.kept_leaves.iter().any(|k| k.source == local)
                {
                    return TakeAttribution::Collapsed { dir: dir.clone() };
                }
            }
            Materialization::Leaf(_) => {}
        }
    }
    TakeAttribution::Dropped
}

fn plan_warning_phrases(plan: &ResolvedBindingPlan) -> Vec<String> {
    plan.warnings
        .iter()
        .map(|w| match w {
            PlanWarning::TakeNoMatchGlob(p) => format!("take glob `{p}` matched no offered leaf"),
            PlanWarning::LostCollapseToExclude(dir) => {
                format!("`{dir}` could not collapse: a within-dir exclude forced per-leaf links")
            }
        })
        .collect()
}

/// The lock-and-cache reads `explain` draws on without fetching, resolving, or writing.
pub(crate) struct OfflineCtx<'a> {
    pub config: &'a Config,
    pub parsed: &'a BTreeMap<String, ParsedSource>,
    pub remotes: &'a BTreeMap<String, String>,
    pub backend: &'a dyn SourceBackend,
    pub lock: Option<&'a Lock>,
}

/// Resolves the offline binding of `source` under `target` from the lock, then attributes
/// `path`. Reads the lock and store cache only — never fetches, resolves, or writes.
///
/// # Errors
/// Returns a structured selection diagnostic if the target/source is unknown, the binding
/// is unbound or unlocked, or the cached leaves are missing (cache miss / stale lock).
pub(crate) fn explain_cmd(
    ctx: &OfflineCtx<'_>,
    target: &str,
    source: &str,
    path: Option<&str>,
) -> Result<ExplainReport> {
    let (config, parsed, remotes, backend, lock) =
        (ctx.config, ctx.parsed, ctx.remotes, ctx.backend, ctx.lock);
    let target_cfg = config.targets.get(target).ok_or_else(|| {
        unbound_diagnostic(
            target,
            "the configured targets",
            "unknown target",
            "add it under `[targets]` or check the spelling",
            "phora preview",
        )
    })?;

    let binding = target_cfg
        .resolve_sources(parsed)
        .into_iter()
        .find(|b| b.source == source || b.identity == source)
        .ok_or_else(|| {
            unbound_diagnostic(
                source,
                &format!("the sources bound under `{target}`"),
                "not bound under this target",
                &format!("bind it with `phora bind {source} --to {target}`, then `phora sync`"),
                &format!("phora preview --target {target}"),
            )
        })?;

    let src = parsed.get(binding.source).ok_or_else(|| {
        Error::Config(format!(
            "target references undefined source: {}",
            binding.source
        ))
    })?;
    let remote = remotes
        .get(binding.source)
        .map(String::as_str)
        .ok_or_else(|| {
            Error::Config(format!(
                "no resolved remote for source `{}`",
                binding.source
            ))
        })?;

    let (commit, candidates) = match src.deploy_mode() {
        DeployMode::Link => {
            let leaves =
                crate::sync::discover::discover_working_tree_leaves(Path::new(remote), None)
                    .map_err(|_| {
                        cache_miss_diagnostic(
                            binding.source,
                            "the source working tree is unavailable",
                        )
                    })?;
            ("link".to_owned(), leaves)
        }
        DeployMode::Copy => {
            let disc = ref_discriminator(&binding.effective_ref, &src.refspec());
            let locked = lock
                .and_then(|l| l.find_entry(binding.source, disc.as_deref()))
                .ok_or_else(|| {
                    cache_miss_diagnostic(binding.source, "no locked commit for this binding")
                })?;
            let name = crate::kernel::SourceName::trusted(binding.source.to_owned());
            let leaves = backend
                .list_source_leaves(&name, remote, &locked.commit, None)
                .map_err(|_| {
                    cache_miss_diagnostic(binding.source, "its cached export is missing")
                })?;
            (locked.commit.clone(), leaves)
        }
    };

    let layout = target_cfg.layout();
    let input = ExplainInput {
        target,
        source: binding.source,
        commit: &commit,
        offer: src.offer(),
        candidate_leaves: &candidates,
        take: binding.take,
        mode: src.deploy_mode(),
        collapse: binding.collapse,
        layout: &layout,
        target_path: &target_cfg.expanded_path(),
        template_opt_in: &binding.template_opt_in,
    };
    explain_path(&input, path)
}

fn unbound_diagnostic(entry: &str, matched: &str, why: &str, remedy: &str, debug: &str) -> Error {
    SelectionDiagnostic {
        entry: entry.to_owned(),
        matched_against: matched.to_owned(),
        why: why.to_owned(),
        did_you_mean: None,
        remedy: remedy.to_owned(),
        debug_hint: Some(debug.to_owned()),
        details: Vec::new(),
    }
    .config()
}

fn cache_miss_diagnostic(source: &str, why: &str) -> Error {
    SelectionDiagnostic {
        entry: source.to_owned(),
        matched_against: "the lock and store cache".to_owned(),
        why: why.to_owned(),
        did_you_mean: None,
        remedy: format!("run `phora sync` to fetch and lock `{source}`"),
        debug_hint: Some(format!("phora preview --source {source}")),
        details: Vec::new(),
    }
    .sync()
}

/// A `phora list` row for one managed artifact under a target: its source, the
/// artifact name, and a human-readable state label (`✓`, `[modified]`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStatus {
    pub source: String,
    pub artifact: String,
    pub state: String,
}

/// `phora list` grouped by target: every managed artifact's status under one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetListing {
    pub target: String,
    pub artifacts: Vec<ArtifactStatus>,
}

/// One `phora source list` row: a source's name, its resolved remote, and refspec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub name: String,
    pub remote: String,
    pub refspec: String,
}

/// `phora source show`: one source's effective remote + refspec, plus every
/// target whose `sources` list deploys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSummary {
    pub name: String,
    pub remote: String,
    pub refspec: String,
    pub targets: Vec<String>,
}

/// A target's `sources = [...]` list; a no-key target resolves to the empty set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceResolution {
    Explicit(Vec<String>),
}

/// One `phora target list` row: a target's name, path, and source-resolution mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRow {
    pub name: String,
    pub path: String,
    pub resolution: SourceResolution,
}

/// `phora target show`: a target's effective config, the source names it binds,
/// and per-artifact deployment state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDetail {
    pub name: String,
    pub path: String,
    pub bound_sources: Vec<String>,
    pub artifacts: Vec<ArtifactStatus>,
}

/// `phora source list`: one row per source over the merged config.
///
/// # Errors
///
/// Returns an error if a source fails to resolve its remote or refspec.
pub fn source_listing(config: &Config) -> Result<Vec<SourceRow>> {
    config
        .parsed_sources()?
        .iter()
        .map(|(name, parsed)| {
            Ok(SourceRow {
                name: name.clone(),
                remote: resolved_remote(config, parsed)?,
                refspec: parsed.refspec().to_string(),
            })
        })
        .collect()
}

fn resolved_remote(config: &Config, parsed: &ParsedSource) -> Result<String> {
    if let Some(url) = parsed.source_url() {
        return Ok(url.to_owned());
    }
    let protocol = parsed
        .protocol()
        .or(config.protocol)
        .unwrap_or(Protocol::Https);
    parsed.resolved_remote(&config.hosts, protocol)
}

/// Every target with a binding whose underlying source is `name`.
#[must_use]
pub fn targets_receiving(config: &Config, name: &str) -> Vec<String> {
    let mut receiving: Vec<String> = config
        .targets
        .iter()
        .filter(|(_, target)| target.declared_sources().any(|source| source == name))
        .map(|(target_name, _)| target_name.clone())
        .collect();
    receiving.sort();
    receiving.dedup();
    receiving
}

fn binding_identities(target: &Target) -> Vec<String> {
    target
        .sources
        .iter()
        .flatten()
        .map(|(identity, _)| identity.clone())
        .collect()
}

/// `phora source show`: effective source config + targets that deploy it.
///
/// # Errors
///
/// Returns an error if `name` is not defined in the merged config.
pub fn source_summary(config: &Config, name: &str) -> Result<SourceSummary> {
    let source = config
        .sources
        .get(name)
        .ok_or_else(|| Error::Config(format!("source `{name}` is not defined")))?;
    let parsed = ParsedSource::parse(name, source)?;
    Ok(SourceSummary {
        name: name.to_owned(),
        remote: resolved_remote(config, &parsed)?,
        refspec: parsed.refspec().to_string(),
        targets: targets_receiving(config, name),
    })
}

/// `phora target list`: one row per target with its source-resolution mode.
#[must_use]
pub fn target_listing(config: &Config) -> Vec<TargetRow> {
    config
        .targets
        .iter()
        .map(|(name, target)| TargetRow {
            name: name.clone(),
            path: target.path.to_string_lossy().into_owned(),
            resolution: SourceResolution::Explicit(binding_identities(target)),
        })
        .collect()
}

/// `phora target show`: effective target config, resolved bound sources, and
/// per-artifact deployment state.
///
/// # Errors
///
/// Returns an error if `name` is not defined, or on-disk state cannot be read.
pub fn target_detail(config: &Config, registry: &dyn Registry, name: &str) -> Result<TargetDetail> {
    let target = config
        .targets
        .get(name)
        .ok_or_else(|| Error::Config(format!("target `{name}` is not defined")))?;
    let parsed = config.parsed_sources()?;
    let bound_sources = target
        .resolve_sources(&parsed)
        .into_iter()
        .map(|binding| {
            let default_ref = parsed
                .get(binding.source)
                .map(SourceFields::intrinsic_refspec);
            let differs = default_ref.is_none_or(|d| {
                crate::lock::encode_ref(&binding.effective_ref) != crate::lock::encode_ref(&d)
            });
            if differs {
                format!("{} @ {}", binding.identity, binding.effective_ref)
            } else {
                binding.identity.to_owned()
            }
        })
        .collect();
    Ok(TargetDetail {
        name: name.to_owned(),
        path: target.path.to_string_lossy().into_owned(),
        bound_sources,
        artifacts: target_artifact_statuses(name, target, registry)?,
    })
}

/// Whether the registry still holds deployed records for `target` — the warning
/// predicate for `phora target rm`.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn target_has_deployed_artifacts(registry: &dyn Registry, target: &str) -> Result<bool> {
    Ok(!registry.list_target(target)?.is_empty())
}

/// Registry-driven `phora list`: per target, the status of every managed artifact,
/// computed via [`check_artifact_state`](crate::deploy::check_artifact_state).
///
/// # Errors
///
/// Returns an error if the registry or on-disk targets cannot be read.
pub fn list_statuses(config: &Config, registry: &dyn Registry) -> Result<Vec<TargetListing>> {
    config
        .targets
        .iter()
        .map(|(target_name, target)| {
            Ok(TargetListing {
                target: target_name.clone(),
                artifacts: target_artifact_statuses(target_name, target, registry)?,
            })
        })
        .collect()
}

fn target_artifact_statuses(
    target_name: &str,
    target: &crate::config::Target,
    registry: &dyn Registry,
) -> Result<Vec<ArtifactStatus>> {
    let ejected = registry.load_ejected(target_name)?;
    let mut artifacts = Vec::new();
    for rec in registry.list_target(target_name)? {
        let artifact_dst = crate::sync::record_artifact_path(target, &rec);
        let state = check_artifact_state(
            &artifact_dst,
            &rec.key.source,
            &rec.commit,
            &ejected,
            &rec.key.artifact,
            registry,
            &rec.key,
            rec.vars_digest.as_deref(),
        )?;
        artifacts.push(ArtifactStatus {
            source: rec.key.source,
            artifact: rec.key.artifact,
            state: state_label(&state).to_owned(),
        });
    }
    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FileRegistry;
    use tempfile::TempDir;

    fn empty_registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    fn bound(detail: &TargetDetail) -> &str {
        detail
            .bound_sources
            .first()
            .expect("one bound source")
            .as_str()
    }

    #[test]
    fn target_show_renders_non_default_effective_ref() {
        let config = Config::parse(
            "version = 1\n\n[sources.fzf]\ngit = \"g\"\nbranch = \"main\"\n\n\
             [targets.t]\npath = \"~/x\"\n\n\
             [targets.t.sources]\ncanary = { source = \"fzf\", tag = \"v0.56.0\" }\n",
        )
        .expect("config with a ref-pinned binding parses");
        let (_dir, reg) = empty_registry();

        let detail = target_detail(&config, &reg, "t").expect("target detail");
        let entry = bound(&detail);
        assert!(
            entry.contains("canary"),
            "the bound source must carry its identity `canary`, got {entry:?}"
        );
        assert!(
            entry.contains("v0.56.0"),
            "a binding whose effective ref differs from the source default must surface that ref \
             (v0.56.0) in `target show`, got {entry:?}"
        );
    }

    #[test]
    fn target_show_omits_redundant_default_ref() {
        let config = Config::parse(
            "version = 1\n\n[sources.fzf]\ngit = \"g\"\nbranch = \"main\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"fzf\"]\n",
        )
        .expect("config with a bare binding parses");
        let (_dir, reg) = empty_registry();

        let detail = target_detail(&config, &reg, "t").expect("target detail");
        let entry = bound(&detail);
        assert!(
            !entry.contains("main"),
            "a bare binding whose effective ref equals the source default must show just the \
             identity, never appending the redundant default ref, got {entry:?}"
        );
    }
}

#[cfg(test)]
mod explain_tests {
    use std::path::PathBuf;

    use crate::config::{DeployMode, LayoutConfig, ParsedSource, Source, TakeEntry, TemplateOptIn};
    use crate::diagnostic::{MATCHED_AGAINST, REMEDY, SELECTION, TO_DEBUG};

    use super::{
        ExplainBody, ExplainInput, ExplainReport, OfferAttribution, OfflineCtx, TakeAttribution,
        explain_cmd, explain_path,
    };

    fn leaves(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn source_with(include: &[&str], exclude: &[&str], mode: DeployMode) -> ParsedSource {
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

    fn explain(
        source: &ParsedSource,
        candidates: &[&str],
        take: Option<&[TakeEntry]>,
        collapse: Option<bool>,
        path: Option<&str>,
    ) -> ExplainReport {
        let candidate_leaves = leaves(candidates);
        let layout = LayoutConfig::default();
        let target_path = PathBuf::from("/dst");
        let input = ExplainInput {
            target: "home",
            source: "s",
            commit: "c0ffee",
            offer: source.offer(),
            candidate_leaves: &candidate_leaves,
            take,
            mode: source.deploy_mode(),
            collapse,
            layout: &layout,
            target_path: &target_path,
            template_opt_in: &TemplateOptIn::SuffixOnly,
        };
        explain_path(&input, path).expect("attribution resolves")
    }

    fn path_body(report: &ExplainReport) -> (&OfferAttribution, &Option<TakeAttribution>) {
        match &report.body {
            ExplainBody::Path { offer, take, .. } => (offer, take),
            ExplainBody::Summary { .. } => panic!("expected a path body, got a summary"),
        }
    }

    #[test]
    fn path_offered_by_include_names_that_include_and_reports_allowed() {
        let source = source_with(&["*.lua"], &[], DeployMode::Copy);
        let report = explain(
            &source,
            &["init.lua", "README.md"],
            None,
            None,
            Some("init.lua"),
        );
        let (offer, take) = path_body(&report);
        assert_eq!(
            *offer,
            OfferAttribution::Allowed {
                include: Some("*.lua".to_string())
            },
            "an offered path must be attributed to the include that selected it; got {offer:?}"
        );
        assert_eq!(
            *take,
            Some(TakeAttribution::Identity {
                dest: "init.lua".to_string()
            }),
            "with no take the offered leaf is kept at identity; got {take:?}"
        );
    }

    #[test]
    fn path_under_source_root_is_attributed_in_offered_leaf_space() {
        let raw = toml::from_str::<Source>(
            "git = \"https://example.com/x.git\"\nroot = \"skills\"\ninclude = [\"skill-creator\"]\n",
        )
        .expect("source DTO deserializes");
        let source = ParsedSource::parse("s", &raw).expect("source parses");
        let report = explain(
            &source,
            &["skills/skill-creator/SKILL.md", "skills/internal/x.md"],
            None,
            None,
            Some("skill-creator/SKILL.md"),
        );
        let (offer, _take) = path_body(&report);
        assert_eq!(
            *offer,
            OfferAttribution::Allowed {
                include: Some("skill-creator".to_string())
            },
            "an offered leaf named relative to `root` must attribute to its include, not read as \
             outside the offer; got {offer:?}"
        );
    }

    #[test]
    fn path_vetoed_by_exclude_names_the_exclude() {
        let source = source_with(&["**"], &["secret.md"], DeployMode::Copy);
        let report = explain(
            &source,
            &["secret.md", "open.md"],
            None,
            None,
            Some("secret.md"),
        );
        let (offer, take) = path_body(&report);
        assert_eq!(
            *offer,
            OfferAttribution::Vetoed {
                exclude: "secret.md".to_string()
            },
            "a path matched by include but dropped by an exclude must name that exclude; got {offer:?}"
        );
        assert_eq!(
            *take, None,
            "a vetoed path is not offered, so there is no take outcome; got {take:?}"
        );
    }

    #[test]
    fn path_outside_offer_reports_not_offered_with_nearest_suggestion() {
        let source = source_with(&["*.lua"], &[], DeployMode::Copy);
        let report = explain(
            &source,
            &["init.lua", "keymaps.lua"],
            None,
            None,
            Some("init.lus"),
        );
        let (offer, take) = path_body(&report);
        match offer {
            OfferAttribution::Outside { suggestions } => assert!(
                suggestions.contains(&"init.lua".to_string()),
                "the nearest offered leaf `init.lua` must be suggested; got {suggestions:?}"
            ),
            other => panic!("a path no include matches must be outside the offer; got {other:?}"),
        }
        assert_eq!(
            *take, None,
            "an outside path has no take outcome; got {take:?}"
        );
    }

    #[test]
    fn take_rename_shows_src_to_dest() {
        let source = source_with(&["**"], &[], DeployMode::Copy);
        let take = [TakeEntry::Rename {
            src: "x.md".to_string(),
            dest: "renamed.md".to_string(),
        }];
        let report = explain(&source, &["x.md"], Some(&take), None, Some("x.md"));
        let (_, take) = path_body(&report);
        assert_eq!(
            *take,
            Some(TakeAttribution::Renamed {
                src: "x.md".to_string(),
                dest: "renamed.md".to_string()
            }),
            "a renamed leaf must surface its src -> dest mapping; got {take:?}"
        );
    }

    #[test]
    fn copy_mode_tmpl_suffix_strip_is_kept_at_identity_not_a_rename() {
        let source = source_with(&["**"], &[], DeployMode::Copy);
        let report = explain(
            &source,
            &["config.lua.tmpl"],
            None,
            None,
            Some("config.lua.tmpl"),
        );
        let (_, take) = path_body(&report);
        assert_eq!(
            *take,
            Some(TakeAttribution::Identity {
                dest: "config.lua".to_string()
            }),
            "a copy-mode `.tmpl` leaf is kept at identity — the suffix strip is the template \
             engine's deployed-name transform, not a `take` rename; got {take:?}"
        );
    }

    #[test]
    fn take_collapse_names_the_collapsed_directory() {
        let source = source_with(&["**"], &[], DeployMode::Link);
        let report = explain(&source, &["d/a.md", "d/b.md"], None, None, Some("d/a.md"));
        let (_, take) = path_body(&report);
        assert_eq!(
            *take,
            Some(TakeAttribution::Collapsed {
                dir: "d".to_string()
            }),
            "a leaf folded into a wholly-taken link dir must name the collapsed dir; got {take:?}"
        );
    }

    #[test]
    fn narrowing_take_reports_dropped_sibling_as_not_taken() {
        let source = source_with(&["**"], &[], DeployMode::Copy);
        let take = [TakeEntry::Leaf("keep.md".to_string())];
        let report = explain(
            &source,
            &["keep.md", "drop.md"],
            Some(&take),
            None,
            Some("drop.md"),
        );
        let (offer, take) = path_body(&report);
        assert!(
            matches!(offer, OfferAttribution::Allowed { .. }),
            "the dropped sibling is still offered; got {offer:?}"
        );
        assert_eq!(
            *take,
            Some(TakeAttribution::Dropped),
            "an offered leaf a narrowing take excludes must be reported as not taken; got {take:?}"
        );
    }

    #[test]
    fn no_path_summarizes_offered_leaves_with_take_outcomes() {
        let source = source_with(&["*.md"], &[], DeployMode::Copy);
        let report = explain(&source, &["a.md", "b.md", "skip.txt"], None, None, None);
        match &report.body {
            ExplainBody::Summary { leaves } => {
                let names: Vec<&str> = leaves.iter().map(|l| l.leaf.as_str()).collect();
                assert_eq!(
                    names,
                    vec!["a.md", "b.md"],
                    "the summary lists exactly the offered leaves (the unmatched `skip.txt` is \
                     out); got {names:?}"
                );
                assert!(
                    leaves
                        .iter()
                        .all(|l| matches!(l.take, TakeAttribution::Identity { .. })),
                    "with no take every offered leaf is kept at identity; got {leaves:?}"
                );
            }
            ExplainBody::Path { .. } => panic!("no path must produce a summary body"),
        }
    }

    fn config_with_binding(extra: &str) -> crate::config::Config {
        crate::config::Config::parse(&format!(
            "version = 1\n\n[sources.dots]\ngit = \"https://example.com/dots.git\"\n\n\
             [targets.home]\npath = \"~/x\"\n{extra}",
        ))
        .expect("config parses")
    }

    fn offline(
        config: &crate::config::Config,
        target: &str,
        source: &str,
        path: Option<&str>,
    ) -> crate::error::Result<ExplainReport> {
        let parsed = config.parsed_sources().expect("sources parse");
        let remotes = crate::sync::resolved_remotes(config, &parsed).expect("remotes resolve");
        let backend = crate::source::GitBackend::new(PathBuf::from("/nonexistent-cache"));
        let ctx = OfflineCtx {
            config,
            parsed: &parsed,
            remotes: &remotes,
            backend: &backend,
            lock: None,
        };
        explain_cmd(&ctx, target, source, path)
    }

    fn assert_named_diagnostic(rendered: &str, entry: &str) {
        for phrase in [SELECTION, MATCHED_AGAINST, REMEDY, TO_DEBUG] {
            assert!(
                rendered.contains(phrase),
                "the diagnostic must render `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains(entry),
            "the diagnostic must name `{entry}`; got:\n{rendered}"
        );
    }

    #[test]
    fn unknown_target_yields_a_structured_diagnostic_without_panicking() {
        let config = config_with_binding("sources = [\"dots\"]\n");
        let rendered = offline(&config, "ghost", "dots", Some("x"))
            .expect_err("an unknown target must be a structured diagnostic, not a panic")
            .to_string();
        assert_named_diagnostic(&rendered, "ghost");
        assert!(
            rendered.contains("to debug: phora preview"),
            "the unknown-target diagnostic must point at the preview command; got:\n{rendered}"
        );
    }

    #[test]
    fn unbound_source_yields_a_structured_diagnostic_pointing_at_sync() {
        let config = config_with_binding("");
        let rendered = offline(&config, "home", "dots", Some("x"))
            .expect_err("a source not bound under the target must be a structured diagnostic")
            .to_string();
        assert_named_diagnostic(&rendered, "dots");
        assert!(
            rendered.contains("to debug: phora preview --target home"),
            "the unbound-source diagnostic must point at the preview command scoped to the \
             target; got:\n{rendered}"
        );
    }

    #[test]
    fn cache_miss_yields_a_structured_diagnostic_and_never_fetches() {
        let config = config_with_binding("sources = [\"dots\"]\n");
        let rendered = offline(&config, "home", "dots", Some("x"))
            .expect_err("an unlocked copy binding must be a structured diagnostic, not a fetch")
            .to_string();
        assert_named_diagnostic(&rendered, "dots");
        assert!(
            rendered.contains("phora sync"),
            "the cache-miss remedy must point at `phora sync`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("to debug: phora preview --source dots"),
            "the cache-miss diagnostic must point at the preview command scoped to the source; \
             got:\n{rendered}"
        );
    }

    fn lock_for_dots() -> crate::lock::Lock {
        crate::lock::Lock {
            version: crate::lock::LOCK_SCHEMA_VERSION,
            sources: vec![crate::lock::LockedSource {
                name: "dots".to_string(),
                git: "https://example.com/dots.git".to_string(),
                resolved: "https://example.com/dots.git".to_string(),
                commit: "c0ffee".to_string(),
                digest: String::new(),
                config_digest: String::new(),
                r#ref: None,
                instance: None,
            }],
            trusted_hooks: Vec::new(),
            candidate_hooks: Vec::new(),
        }
    }

    #[test]
    fn locked_but_missing_cached_export_yields_a_structured_diagnostic_without_fetching() {
        let config = config_with_binding("sources = [\"dots\"]\n");
        let parsed = config.parsed_sources().expect("sources parse");
        let remotes = crate::sync::resolved_remotes(&config, &parsed).expect("remotes resolve");
        let backend = crate::source::GitBackend::new(PathBuf::from("/nonexistent-cache"));
        let lock = lock_for_dots();
        let ctx = OfflineCtx {
            config: &config,
            parsed: &parsed,
            remotes: &remotes,
            backend: &backend,
            lock: Some(&lock),
        };
        let rendered = explain_cmd(&ctx, "home", "dots", Some("x"))
            .expect_err(
                "a locked binding whose cached export is absent must be a structured diagnostic, \
                 not a fetch",
            )
            .to_string();
        assert_named_diagnostic(&rendered, "dots");
        assert!(
            rendered.contains("phora sync"),
            "the missing-export remedy must point at `phora sync`; got:\n{rendered}"
        );
    }
}
