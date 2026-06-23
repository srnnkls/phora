//! Top-level orchestration: the `sync` pipeline, eject/uneject, and shared helpers.

mod confine;
pub(crate) mod discover;
pub(crate) mod hooks;
mod plan;
mod preview;
mod prune;
mod rebuild;
mod resolve;
mod target;
mod transitive;
mod verify;

#[cfg(test)]
mod tests;

pub use hooks::{HookOutcome, HookScope, HookStatus};
pub use plan::{
    BindingPlanInput, PlanWarning, PlannedItem, ResolvedBindingPlan, TargetPlan,
    expected_artifact_keys, plan_target, plan_targets, resolve_binding_plan, resolve_target_plan,
};
pub use preview::{
    BindingWarnings, PreviewCollision, PreviewEntry, PreviewFile, PreviewTargetPlan,
    PreviewWarning, SyncState, preview_targets,
};
pub use rebuild::{RebuildReport, rebuild_registry};
pub use verify::{VerifyMismatch, VerifyReason, verify};

#[cfg(feature = "bench")]
pub use resolve::resolve_sources_for_bench;

use prune::prune_orphans;
use resolve::resolve_sources;
pub(crate) use target::record_artifact_path;
use target::{TargetRun, deploy_target};

#[cfg(test)]
use {
    crate::config::LayoutKind,
    crate::deploy::check_artifact_state,
    crate::lock::LockedSource,
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
    /// Suppress transitive (composed-dep) hooks only; the consumer's own hooks still run.
    pub no_transitive_hooks: bool,
    /// Refuse to fetch or re-resolve: a source absent from or drifted in the lock hard-errors.
    pub frozen: bool,
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
    /// Transitive hooks discovered but left unrun for lack of trust.
    pub stripped_transitive_hooks: usize,
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

/// Trust comes ONLY from `trusted_hooks`; a `candidate_hooks` record grants none (anti-TOFU).
fn trusted_preimages(effective_lock: Option<&Lock>) -> BTreeSet<String> {
    let Some(lock) = effective_lock else {
        return BTreeSet::new();
    };
    lock.trusted_hooks
        .iter()
        .map(|h| h.preimage.clone())
        .collect()
}

fn take_hook_candidates(
    graph: &mut transitive::ResolvedGraph,
) -> Vec<transitive::TransitiveHookCandidate> {
    for diagnostic in std::mem::take(&mut graph.hook_diagnostics) {
        eprintln!("phora: {diagnostic}");
    }
    std::mem::take(&mut graph.hook_candidates)
}

/// Surfaces every interpreted transitive hook in the lock with its commit-bound preimage so a
/// consumer can pin an approval; recording grants no trust on its own.
fn record_candidate_hooks(
    base_lock: &mut Lock,
    candidates: &[transitive::TransitiveHookCandidate],
) {
    base_lock.candidate_hooks = candidates
        .iter()
        .map(crate::lock::CandidateHookRecord::from)
        .collect();
}

struct TransitiveHookDecision {
    outcomes: Vec<hooks::HookOutcome>,
    stripped: usize,
}

/// Runs the trusted transitive hooks and appends any newly-approved (interactive) ones to the
/// consumer lock's `trusted_hooks`.
fn decide_transitive_hooks(
    base_lock: &mut Lock,
    candidates: &[transitive::TransitiveHookCandidate],
    effective_lock: Option<&Lock>,
    interactive: bool,
) -> Result<TransitiveHookDecision> {
    let trusted = trusted_preimages(effective_lock);
    let runs: Vec<hooks::TransitiveHookRun<'_>> = candidates
        .iter()
        .map(|c| hooks::TransitiveHookRun {
            dep_instance: &c.dep_instance,
            hook_id: &c.hook_id,
            command: &c.command,
            preimage: &c.preimage,
            target_path: &c.target_path,
            source: &c.source,
            commit: &c.commit,
        })
        .collect();
    let (outcomes, approvals) = if interactive {
        hooks::dispatch_transitive_hooks(&runs, &trusted, &hooks::TtyTrustPrompt)?
    } else {
        hooks::dispatch_transitive_hooks(&runs, &trusted, &hooks::DeclineAll)?
    };
    let now = chrono::Utc::now().to_rfc3339();
    for approval in approvals {
        base_lock.trusted_hooks.push(crate::lock::TrustedHook {
            dep_instance: approval.dep_instance,
            hook_id: approval.hook_id,
            preimage: approval.preimage,
            approved_at: now.clone(),
            source: approval.source,
            commit: approval.commit,
        });
    }
    let ran = outcomes.len();
    Ok(TransitiveHookDecision {
        outcomes,
        stripped: candidates.len() - ran,
    })
}

struct DeployAll<'a> {
    config: &'a Config,
    parsed: &'a BTreeMap<String, ParsedSource>,
    remotes: &'a BTreeMap<String, String>,
    resolved_commits: &'a BTreeMap<(String, String), String>,
    protected: &'a confine::ProtectedPathSet,
    input: &'a SyncInput<'a>,
    backend: &'a (dyn SourceBackend + Sync),
    registry: &'a dyn Registry,
    journal: &'a Journal,
}

fn deploy_all_targets(ctx: &DeployAll<'_>) -> Result<bool> {
    let mut had_failures = false;
    for (target_name, target) in &ctx.config.targets {
        had_failures |= deploy_target(
            TargetRun {
                parsed: ctx.parsed,
                target_name,
                target,
                commits: ctx.resolved_commits,
                remotes: ctx.remotes,
                force: ctx.input.force,
                interactive: ctx.input.interactive,
                resolver: ctx.input.resolver,
                vars: &ctx.config.vars,
                protected: ctx.protected,
            },
            ctx.backend,
            ctx.registry,
            ctx.journal,
        )?;
    }
    Ok(had_failures)
}

fn sweep_target_parents(config: &Config, journal: &Journal, registry: &dyn Registry) -> Result<()> {
    let mut swept_parents: BTreeSet<PathBuf> = BTreeSet::new();
    for target in config.targets.values() {
        let parent = match &target.confine {
            Some(anchor) => anchor.clone(),
            None => target_parent(&target.expanded_path()),
        };
        if swept_parents.insert(parent.clone()) {
            recovery_sweep(&parent, journal, registry)?;
        }
    }
    Ok(())
}

fn effective_lock(input: &SyncInput<'_>) -> Option<Lock> {
    match (&input.base_lock, &input.local_lock) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    }
}

pub fn sync(
    input: &SyncInput<'_>,
    backend: &(dyn SourceBackend + Sync),
    registry: &dyn Registry,
) -> Result<SyncOutput> {
    let mut effective_config =
        merge_configs(input.base_config.clone(), input.local_config.cloned());
    effective_config.validate()?;
    let mut parsed = effective_config.parsed_sources()?;
    let mut remotes = resolved_remotes(&effective_config, &parsed)?;
    let effective_lock = effective_lock(input);
    let mut graph = transitive::resolve_transitive_graph(
        &effective_config,
        &parsed,
        backend,
        input.frozen,
        effective_lock.as_ref(),
    )?;
    let hook_candidates = take_hook_candidates(&mut graph);
    let instances = graph.inject(&mut effective_config, &mut parsed, &mut remotes);
    for warning in validate_link_mode(input.base_config, &parsed, &remotes)? {
        eprintln!("phora: {warning}");
    }

    let local_names: BTreeSet<String> = input
        .local_config
        .map(|lc| lc.sources.keys().cloned().collect())
        .unwrap_or_default();

    let journal = Journal::open(&registry.locks_dir())?;
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Sync(format!("resolve current dir for confinement: {e}")))?;
    let protected = confine::ProtectedPathSet::resolve(&effective_config.paths, &cwd)?;

    sweep_target_parents(&effective_config, &journal, registry)?;

    let recorded_after_recovery: Vec<ArtifactKey> =
        registry.list_all()?.into_iter().map(|r| r.key).collect();

    let (routed, resolved_commits) = resolve_sources(
        &effective_config,
        &parsed,
        &remotes,
        &instances,
        effective_lock.as_ref(),
        backend,
        input.force,
        input.frozen,
        input.jobs,
    )?;
    let (mut base_lock, local_lock) = split_locks(routed, &local_names);
    base_lock.trusted_hooks = effective_lock
        .as_ref()
        .map(|lock| lock.trusted_hooks.clone())
        .unwrap_or_default();

    validate_sealed_offer(
        &effective_config,
        &parsed,
        &remotes,
        backend,
        &resolved_commits,
        &recorded_after_recovery,
    )?;

    let mut had_failures = deploy_all_targets(&DeployAll {
        config: &effective_config,
        parsed: &parsed,
        remotes: &remotes,
        resolved_commits: &resolved_commits,
        protected: &protected,
        input,
        backend,
        registry,
        journal: &journal,
    })?;

    if input.prune && !had_failures {
        prune_orphans(
            &effective_config,
            &parsed,
            &remotes,
            backend,
            registry,
            &resolved_commits,
            &protected,
        )?;
    } else if input.prune {
        eprintln!("phora: skipping --prune because some artifacts failed to deploy");
    }

    let (hook_results, stripped_transitive_hooks) = run_all_hooks(
        input,
        &effective_config,
        registry,
        &mut base_lock,
        &hook_candidates,
        effective_lock.as_ref(),
    )?;
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
        stripped_transitive_hooks,
    })
}

fn run_all_hooks(
    input: &SyncInput<'_>,
    config: &Config,
    registry: &dyn Registry,
    base_lock: &mut Lock,
    hook_candidates: &[transitive::TransitiveHookCandidate],
    effective_lock: Option<&Lock>,
) -> Result<(Vec<hooks::HookOutcome>, usize)> {
    let mut hook_results = if input.no_hooks {
        Vec::new()
    } else {
        hooks::dispatch_hooks(config, registry)?
    };
    record_candidate_hooks(base_lock, hook_candidates);
    let mut stripped = 0;
    if !input.no_hooks && !input.no_transitive_hooks {
        let mut decision = decide_transitive_hooks(
            base_lock,
            hook_candidates,
            effective_lock,
            input.interactive,
        )?;
        stripped = decision.stripped;
        hook_results.append(&mut decision.outcomes);
    }
    Ok((hook_results, stripped))
}

struct BindingOffer {
    offered: Vec<String>,
    selection: crate::kernel::OfferSelection,
}

/// Compares against the resolved OFFER set, not the take/kept set: a leaf dropped by
/// `take` while still offered stays allowed. A recorded key the source no longer provides
/// only hard-errors when the offer config still ADMITS its path (a silent source narrowing);
/// when the config itself narrowed past it, it is a pure orphan left to `--prune`.
fn validate_sealed_offer(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &(dyn SourceBackend + Sync),
    resolved_commits: &BTreeMap<(String, String), String>,
    recorded: &[ArtifactKey],
) -> Result<()> {
    use crate::kernel::{OfferSelection, SourceName};

    let mut offers: BTreeMap<(String, String), BindingOffer> = BTreeMap::new();
    for (target_name, target) in &config.targets {
        for binding in target.resolve_sources(parsed) {
            let Some(source) = parsed.get(binding.source) else {
                continue;
            };
            let commit_key = (
                binding.source.to_owned(),
                crate::lock::encode_ref(&binding.effective_ref),
            );
            let Some(commit) = resolved_commits.get(&commit_key) else {
                continue;
            };
            let name = SourceName::trusted(binding.source);
            let git = remote_for(remotes, name.as_str())?;
            let candidates = match source.deploy_mode() {
                DeployMode::Link => discover::discover_working_tree_leaves(Path::new(git), None)?,
                DeployMode::Copy => backend.list_source_leaves(&name, git, commit, None)?,
            };
            let offer = source.offer();
            let selection =
                OfferSelection::compile(offer.includes(), offer.excludes(), offer.root())?;
            let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
            let offered = selection.select(&refs);
            offers.insert(
                (target_name.clone(), binding.identity.to_owned()),
                BindingOffer { offered, selection },
            );
        }
    }

    for key in recorded {
        let Some(offer) = offers.get(&(key.target.clone(), key.source.clone())) else {
            continue;
        };
        let artifact = &key.artifact;
        let still_offered = offer
            .offered
            .iter()
            .any(|leaf| leaf == artifact || leaf.starts_with(&format!("{artifact}/")));
        if !still_offered && offer.selection.admits_published(artifact) {
            return Err(sealed_offer_diagnostic(&key.target, &key.source, artifact));
        }
    }
    Ok(())
}

fn sealed_offer_diagnostic(target: &str, source: &str, artifact: &str) -> Error {
    crate::diagnostic::SelectionDiagnostic {
        entry: format!("{target}:{source}:{artifact}"),
        matched_against: format!("the current offer of source `{source}` in target `{target}`"),
        why: "a recorded artifact is no longer in the source's offer".to_string(),
        did_you_mean: None,
        remedy: "restore the artifact to the source's offer, or eject it before removing it"
            .to_string(),
        debug_hint: Some(format!("phora explain {target} {source} {artifact}")),
    }
    .sync()
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
