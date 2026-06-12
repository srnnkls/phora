//! Offline preview: a per-target plan built from the lock alone, annotating each
//! binding's sync state without fetching, resolving, or writing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, DeployMode, LayoutConfig, LayoutKind, ParsedSource, Target};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Selection, SourceName};
use crate::lock::{Lock, ref_discriminator};
use crate::source::SourceBackend;

use super::plan::discover_binding;
use super::remote_for;

/// Whether a binding is renderable now or needs action before it can deploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SyncState {
    Synced,
    NotLocked,
    NeedsSync,
    LinkWorkingTreeGone,
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
    /// Artifact-relative file paths (empty until `--files` enrichment).
    pub files: Vec<PathBuf>,
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

    let collisions = detect_flat_collisions(&layout, &entries);
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

fn preview_link(ctx: &BindingCtx, entries: &mut Vec<PreviewEntry>) -> Result<()> {
    match discover_binding(
        ctx.source,
        ctx.name,
        "link",
        ctx.binding.include,
        ctx.binding.exclude,
        ctx.binding.root,
        ctx.remotes,
        ctx.backend,
    ) {
        Ok(discovered) => {
            for artifact in discovered {
                let mut entry = synced_entry(ctx, artifact.as_str(), "link");
                if ctx.files {
                    entry.files = link_files(ctx, artifact.as_str())?;
                }
                entries.push(entry);
            }
        }
        Err(err @ Error::Config(_)) => return Err(err),
        Err(_) => entries.push(annotation(ctx, "link", SyncState::LinkWorkingTreeGone)),
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

    match discover_binding(
        ctx.source,
        ctx.name,
        &locked.commit,
        ctx.binding.include,
        ctx.binding.exclude,
        ctx.binding.root,
        ctx.remotes,
        ctx.backend,
    ) {
        Ok(discovered) => {
            for artifact in discovered {
                let mut entry = synced_entry(ctx, artifact.as_str(), &locked.commit);
                if ctx.files {
                    entry.files = copy_files(ctx, artifact.as_str(), &locked.commit)?;
                }
                entries.push(entry);
            }
        }
        Err(err @ Error::Config(_)) => return Err(err),
        Err(_) => entries.push(annotation(ctx, &locked.commit, SyncState::NeedsSync)),
    }
    Ok(())
}

fn copy_files(ctx: &BindingCtx, artifact: &str, commit: &str) -> Result<Vec<PathBuf>> {
    let git = remote_for(ctx.remotes, ctx.binding.source)?;
    let selection = Selection::new(ctx.binding.include, ctx.binding.exclude)?;
    Ok(ctx.backend.list_artifact_files(
        ctx.name,
        git,
        commit,
        ctx.binding.root,
        &ArtifactName::trusted(artifact),
        &selection,
    )?)
}

fn link_files(ctx: &BindingCtx, artifact: &str) -> Result<Vec<PathBuf>> {
    let git = remote_for(ctx.remotes, ctx.binding.source)?;
    let selection = Selection::new(ctx.binding.include, ctx.binding.exclude)?;
    let base = ctx
        .binding
        .root
        .map_or_else(|| PathBuf::from(git), |r| Path::new(git).join(r))
        .join(artifact);
    let mut files = Vec::new();
    collect_working_tree_files(&base, Path::new(""), &selection, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_working_tree_files(
    base: &Path,
    rel: &Path,
    selection: &Selection,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    let dir = base.join(rel);
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| Error::Sync(format!("scan working tree {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read entry in {}: {e}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let entry_rel = rel.join(&name);
        let ft = entry
            .file_type()
            .map_err(|e| Error::Sync(format!("stat {}: {e}", entry.path().display())))?;
        if ft.is_symlink() {
            continue;
        }
        let is_dir = ft.is_dir();
        if !selection.selects_path(&entry_rel, is_dir) {
            continue;
        }
        if is_dir {
            collect_working_tree_files(base, &entry_rel, selection, files)?;
        } else {
            files.push(entry_rel);
        }
    }
    Ok(())
}

fn synced_entry(ctx: &BindingCtx, artifact: &str, commit: &str) -> PreviewEntry {
    PreviewEntry {
        identity: ctx.binding.identity.to_owned(),
        source: ctx.binding.source.to_owned(),
        artifact: artifact.to_owned(),
        commit: commit.to_owned(),
        destination: ctx
            .path
            .join(ctx.layout.artifact_path(ctx.binding.identity, artifact)),
        state: SyncState::Synced,
        files: Vec::new(),
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

fn detect_flat_collisions(
    layout: &LayoutConfig,
    entries: &[PreviewEntry],
) -> Vec<PreviewCollision> {
    if layout.kind != LayoutKind::Flat {
        return Vec::new();
    }

    let mut by_artifact: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.state == SyncState::Synced) {
        by_artifact
            .entry(entry.artifact.as_str())
            .or_default()
            .push(entry.identity.clone());
    }

    by_artifact
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(artifact, sources)| PreviewCollision {
            artifact: artifact.to_owned(),
            sources,
        })
        .collect()
}
