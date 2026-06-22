//! Offline preview: a per-target plan built from the lock alone, annotating each
//! binding's sync state without fetching, resolving, or writing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, DeployMode, LayoutConfig, ParsedSource, Target};
use crate::error::{Error, Result};
use crate::kernel::{Materialization, SourceName};
use crate::lock::{Lock, ref_discriminator};
use crate::source::SourceBackend;

use super::discover::discover_working_tree_leaves;
use super::plan::{BindingPlanInput, PlannedItem, ResolvedBindingPlan, resolve_binding_plan};
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
    pub entries: Vec<PreviewEntry>,
    pub collisions: Vec<PreviewCollision>,
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
    config
        .targets
        .iter()
        .map(|(name, target)| preview_target(name, target, parsed, remotes, backend, lock, files))
        .collect()
}

fn preview_target(
    target_name: &str,
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    lock: Option<&Lock>,
    files: bool,
) -> Result<PreviewTargetPlan> {
    let path = target.expanded_path();
    let layout = target.layout();
    let mut entries = Vec::new();

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
            DeployMode::Link => preview_link(&ctx, &mut entries)?,
            DeployMode::Copy => preview_copy(&ctx, lock, &mut entries)?,
        }
    }

    let collisions = detect_dest_collisions(&entries);
    Ok(PreviewTargetPlan {
        target: target_name.to_owned(),
        entries,
        collisions,
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

/// The published artifact key — the collapsed-dir or the renamed/identity leaf dest.
fn published_key(materialization: &Materialization) -> &str {
    match materialization {
        Materialization::CollapsedDir { dir } => dir,
        Materialization::Leaf(take) => &take.dest,
    }
}

fn preview_link(ctx: &BindingCtx, entries: &mut Vec<PreviewEntry>) -> Result<()> {
    let git = remote_for(ctx.remotes, ctx.binding.source)?;
    let Ok(candidates) = discover_working_tree_leaves(Path::new(git), None) else {
        entries.push(annotation(ctx, "link", SyncState::LinkWorkingTreeGone));
        return Ok(());
    };
    let plan = resolve_plan(ctx, "link", &candidates)?;
    for item in &plan.items {
        push_item(ctx, item, "link", entries);
    }
    Ok(())
}

fn preview_copy(
    ctx: &BindingCtx,
    lock: Option<&Lock>,
    entries: &mut Vec<PreviewEntry>,
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
    Ok(())
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
    let key = published_key(&item.materialization).to_owned();
    let mut entry = PreviewEntry {
        identity: ctx.binding.identity.to_owned(),
        source: ctx.binding.source.to_owned(),
        artifact: key,
        commit: commit.to_owned(),
        destination: item.destination.clone(),
        state: SyncState::Synced,
        files: Vec::new(),
    };
    if ctx.files {
        entry.files = item_files(ctx, item);
    }
    entries.push(entry);
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
