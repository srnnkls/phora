//! Top-level orchestration: the `sync` pipeline, eject/uneject, and shared helpers.

mod discover;
mod hooks;
mod plan;
mod preview;
mod prune;
mod rebuild;
mod resolve;
mod target;
mod verify;

#[cfg(test)]
mod tests;

pub use hooks::{HookOutcome, HookScope, HookStatus};
pub use plan::{PlanEntry, TargetPlan, plan_target, plan_targets};
pub use preview::{
    PreviewCollision, PreviewEntry, PreviewFile, PreviewTargetPlan, SyncState, preview_targets,
};
pub use rebuild::{RebuildReport, rebuild_registry};
pub use verify::{VerifyMismatch, VerifyReason, verify};

use prune::prune_orphans;
use resolve::resolve_sources;
pub(crate) use target::record_artifact_path;
use target::{TargetRun, deploy_target};

#[cfg(test)]
use {
    crate::config::LayoutKind,
    crate::deploy::check_artifact_state,
    crate::kernel::Selection,
    crate::lock::LockedSource,
    discover::discover_working_tree,
    target::{ArtifactEntry, deploy_artifact_entry},
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{Config, DeployMode, ParsedSource, Protocol, SourceMode, merge_configs};
use crate::deploy::{Journal, recovery_sweep};
use crate::error::{Error, Result};
use crate::lock::{Lock, merge_locks, split_locks};
use crate::source::{SourceBackend, is_local_path};
use crate::store::{ArtifactKey, EjectedEntry, Registry};

/// Borrowed inputs to [`sync`]: the configs and locks plus run flags. Bundled so
/// the orchestration entry point stays stable as later phases add fields.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent CLI run flags, not a state machine"
)]
pub struct SyncInput<'a> {
    pub base_config: &'a Config,
    pub local_config: Option<&'a Config>,
    pub base_lock: Option<Lock>,
    pub local_lock: Option<Lock>,
    pub force: bool,
    pub interactive: bool,
    pub prune: bool,
    pub no_hooks: bool,
    pub resolver: Option<&'a dyn ConflictResolver>,
    /// Worker-pool size for parallel fetch/resolve/digest. `None` derives a
    /// default of `min(resolution_units, 8)`; `Some(n)` pins the pool to `n`.
    pub jobs: Option<usize>,
}

/// How the user wants a single Modified/Foreign conflict handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Skip,
    Overwrite,
    Eject,
    Abort,
}

/// What kind of conflict surfaced at an artifact destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    Modified { changed: Vec<std::path::PathBuf> },
    Foreign,
}

/// A single conflict presented to a [`ConflictResolver`] during interactive sync.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub target: String,
    pub source: String,
    pub artifact: String,
    pub kind: ConflictKind,
}

/// Decides how to resolve each Modified/Foreign conflict in interactive sync.
pub trait ConflictResolver {
    fn resolve(&self, conflict: &Conflict) -> Resolution;
}

/// Result of a sync run: the recomputed base and local locks, plus whether any
/// per-artifact export/deploy step failed (the CLI maps this to its exit code).
pub struct SyncOutput {
    pub base_lock: Lock,
    pub local_lock: Option<Lock>,
    pub had_failures: bool,
    pub deploy_failures: bool,
    pub hook_results: Vec<hooks::HookOutcome>,
}

/// A relative target path yields an empty (`""`) or absent parent; both normalize
/// to `.` so `recovery_sweep` scans exactly the dir deploy stages into.
pub(super) fn target_parent(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The protocol a source resolves under: its own, else the global default, else https.
pub(super) fn effective_protocol(source: &ParsedSource, config: &Config) -> Protocol {
    source
        .protocol()
        .or(config.protocol)
        .unwrap_or(Protocol::Https)
}

/// Resolves every source's concrete remote once, keyed by source name. A resolution
/// failure (unknown host, missing protocol template) surfaces named by source.
pub(crate) fn resolved_remotes(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
) -> Result<BTreeMap<String, String>> {
    let mut remotes = BTreeMap::new();
    for (name, source) in parsed {
        let remote = if source.mode() == SourceMode::Url {
            source
                .source_url()
                .ok_or_else(|| Error::Config(format!("source `{name}`: missing url")))?
                .to_owned()
        } else {
            let protocol = effective_protocol(source, config);
            source
                .resolved_remote(&config.hosts, protocol)
                .map_err(|e| Error::Config(format!("source `{name}`: {e}")))?
        };
        remotes.insert(name.clone(), remote);
    }
    Ok(remotes)
}

pub(super) fn remote_for<'a>(remotes: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    remotes
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| Error::Config(format!("no resolved remote for source `{name}`")))
}

/// Distinct suffix per call so sibling staging dirs in a shared base never collide.
pub(super) fn nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub fn sync(
    input: &SyncInput<'_>,
    backend: &(dyn SourceBackend + Sync),
    registry: &dyn Registry,
) -> Result<SyncOutput> {
    let effective_config = merge_configs(input.base_config.clone(), input.local_config.cloned());
    effective_config.validate()?;
    let parsed = effective_config.parsed_sources()?;
    let remotes = resolved_remotes(&effective_config, &parsed)?;
    for warning in validate_link_mode(input.base_config, &parsed, &remotes)? {
        eprintln!("phora: {warning}");
    }
    let effective_lock = match (&input.base_lock, &input.local_lock) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    };

    let local_names: BTreeSet<String> = input
        .local_config
        .map(|lc| lc.sources.keys().cloned().collect())
        .unwrap_or_default();

    let journal = Journal::open(&registry.locks_dir())?;

    let mut swept_parents: BTreeSet<PathBuf> = BTreeSet::new();
    for target in effective_config.targets.values() {
        let parent = target_parent(&target.expanded_path());
        if swept_parents.insert(parent.clone()) {
            recovery_sweep(&parent, &journal, registry)?;
        }
    }

    let (routed, resolved_commits) = resolve_sources(
        &effective_config,
        &parsed,
        &remotes,
        effective_lock.as_ref(),
        backend,
        input.force,
        input.jobs,
    )?;
    let (base_lock, local_lock) = split_locks(routed, &local_names);

    let mut had_failures = false;

    for (target_name, target) in &effective_config.targets {
        had_failures |= deploy_target(
            TargetRun {
                parsed: &parsed,
                target_name,
                target,
                commits: &resolved_commits,
                remotes: &remotes,
                force: input.force,
                interactive: input.interactive,
                resolver: input.resolver,
                vars: &effective_config.vars,
            },
            backend,
            registry,
            &journal,
        )?;
    }

    if input.prune {
        if had_failures {
            eprintln!("phora: skipping --prune because some artifacts failed to deploy");
        } else {
            prune_orphans(
                &effective_config,
                &parsed,
                &remotes,
                backend,
                registry,
                &resolved_commits,
            )?;
        }
    }

    let hook_results = if input.no_hooks {
        Vec::new()
    } else {
        hooks::dispatch_hooks(&effective_config, registry)?
    };
    let deploy_failures = had_failures;
    had_failures |= hook_results
        .iter()
        .any(|o| o.status == hooks::HookStatus::Failure);

    Ok(SyncOutput {
        base_lock,
        local_lock,
        had_failures,
        deploy_failures,
        hook_results,
    })
}

fn validate_link_mode(
    base: &Config,
    effective: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for (name, source) in effective {
        if source.deploy_mode() != DeployMode::Link {
            continue;
        }
        let git = remote_for(remotes, name)?;
        if !is_local_path(git) {
            return Err(Error::Config(format!(
                "source `{name}`: deploy = \"link\" requires a local filesystem path, \
                 not a remote URL `{git}`"
            )));
        }
        if base.sources.contains_key(name) && Path::new(git).is_absolute() {
            warnings.push(format!(
                "source `{name}`: deploy = \"link\" uses the absolute path `{git}`, \
                 which is not portable across machines"
            ));
        }
    }
    Ok(warnings)
}

/// Removes a half-exported `staging` dir on drop unless [`disarm`](StagingGuard::disarm)
/// hands cleanup to [`deploy_artifact`] on the success path.
pub(super) struct StagingGuard<'a> {
    staging_base: &'a Path,
    staging: &'a Path,
    armed: bool,
}

impl<'a> StagingGuard<'a> {
    pub(super) fn new(staging_base: &'a Path, staging: &'a Path) -> Self {
        Self {
            staging_base,
            staging,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = remove_orphan_path(self.staging);
        let _ = std::fs::remove_dir(self.staging_base);
    }
}

pub(super) fn remove_orphan_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn eject(
    config: &Config,
    registry: &dyn Registry,
    artifact: &str,
    source: &str,
    target: &str,
) -> Result<()> {
    if !config.targets.contains_key(target) {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    let key = ArtifactKey {
        target: target.to_owned(),
        source: source.to_owned(),
        artifact: artifact.to_owned(),
    };
    if registry.get(&key)?.is_none() {
        return Err(Error::Registry(format!(
            "{source}/{artifact} is not managed in target {target}"
        )));
    }

    let mut ejected = registry.load_ejected(target)?;
    let already = ejected
        .iter()
        .any(|e| e.source == source && e.artifact == artifact);
    if !already {
        ejected.push(EjectedEntry {
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            ejected_at: chrono::Utc::now().to_rfc3339(),
        });
        registry.save_ejected(target, &ejected)?;
    }
    // Record kept (not removed): list/where render `ejected` from it, and uneject restores by clearing the entry alone.
    Ok(())
}

pub fn uneject(
    config: &Config,
    registry: &dyn Registry,
    artifact: &str,
    source: &str,
    target: &str,
) -> Result<()> {
    if !config.targets.contains_key(target) {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    let mut ejected = registry.load_ejected(target)?;
    ejected.retain(|e| !(e.source == source && e.artifact == artifact));
    Ok(registry.save_ejected(target, &ejected)?)
}
