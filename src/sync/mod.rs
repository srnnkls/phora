//! Top-level orchestration: the `sync` pipeline, eject/uneject, and shared helpers.

pub mod apply;
pub(crate) mod confine;
pub(crate) mod discover;
pub(crate) mod hooks;
pub mod inspect;
pub mod journal;
pub mod model;
mod observe;
mod plan;
mod preview;
mod prune;
mod rebuild;
pub mod reconcile;
pub mod recovery;
mod resolve;
pub(crate) mod scan;
pub(crate) mod stage;
pub mod state;
mod target;
pub(crate) mod transitive;
mod verify;

#[cfg(test)]
mod tests;

pub use hooks::{HookOutcome, HookScope, HookStatus};
pub use plan::{plan_target, project_workspace};

use crate::projection::build::projected_artifact_keys;
use crate::projection::model::{ArtifactRelativePath, Projection};
use crate::sync::model::ReconciliationPolicy;

pub(crate) use preview::offered_leaves;
pub use preview::{
    BindingWarnings, PreviewCollision, PreviewEntry, PreviewFile, PreviewTargetPlan,
    PreviewWarning, SyncState, preview_targets,
};
pub use rebuild::{RebuildReport, rebuild_registry, rebuild_registry_with};
pub use verify::{UntrustedHookFinding, VerifyMismatch, VerifyReason, VerifyReport, verify};

#[cfg(feature = "bench")]
pub use resolve::resolve_sources_for_bench;

use prune::prune_projected;
pub(crate) use prune::{orphan_artifact_path, orphan_records};
use resolve::resolve_sources;
pub use stage::{StageRequest, StagedArtifact, StagedFile, stage_artifact};
pub(crate) use target::record_artifact_path;
use target::{Reconciliation, TargetRun, deploy_reconciled_target, resolve_conflicts};

#[cfg(test)]
use {
    crate::config::LayoutKind, crate::lock::LockedSource,
    crate::projection::diagnostic::ProjectionWarning, crate::sync::inspect::check_artifact_state,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{
    Config, DeployMode, ParsedSource, PreDeployOnFail, Protocol, SourceMode, merge_configs,
};
use crate::error::{Error, Result};
use crate::lock::{Lock, merge_locks, split_locks};
use crate::source::{SourceBackend, SourceStore, is_local_path};
use crate::store::{ArtifactKey, EjectedEntry, Registry, RegistryRecord};

use journal::Journal;
use recovery::recovery_sweep;

pub trait StageSource: SourceBackend + SourceStore {}

impl<T: SourceBackend + SourceStore> StageSource for T {}

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
    /// Proceed without the project lock because the state root is read-only (frozen only).
    /// Legal only when the sync is write-free; any pending write is refused, naming the root.
    pub lockless: bool,
    /// Follow a moved pin: delete an artifact the new commit dropped rather than sealing; a same-commit narrowing still hard-errors.
    pub fast_forward: bool,
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

pub use model::ConflictKind;

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
pub(super) fn trusted_preimages(effective_lock: Option<&Lock>) -> BTreeSet<String> {
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
    projection: &'a Projection,
    protected: &'a confine::ProtectedPathSet,
    input: &'a SyncInput<'a>,
    backend: &'a (dyn StageSource + Sync),
    registry: &'a dyn Registry,
    journal: &'a Journal,
}

/// Outcome of the per-target deploy loop. `aborted` means a `pre_deploy` gate with the default
/// `abort` fired and short-circuited the loop (later targets unprocessed); `had_failures`
/// folds in skip-induced failures so it can suppress `--prune`.
struct DeployRun {
    had_failures: bool,
    pre_deploy: Vec<hooks::HookOutcome>,
    aborted: bool,
}

fn target_run<'a>(
    ctx: &DeployAll<'a>,
    target_name: &'a str,
    target: &'a crate::config::Target,
) -> TargetRun<'a> {
    TargetRun {
        parsed: ctx.parsed,
        target_name,
        target,
        remotes: ctx.remotes,
        vars: &ctx.config.vars,
        protected: ctx.protected,
    }
}

fn deploy_all_targets(ctx: &DeployAll<'_>) -> Result<DeployRun> {
    let observed = observe::observe_workspace(ctx, ctx.projection)?;
    let policy = ReconciliationPolicy {
        force: ctx.input.force,
        prune: false,
        follow_moved_pin: false,
    };
    let changeset = reconcile::reconcile(ctx.projection, &observed, &policy)
        .map_err(|e| Error::Sync(e.to_string()))?;
    let decisions = resolve_conflicts(&changeset, ctx.input.resolver, ctx.input.interactive)?;
    let reconciliation = Reconciliation::new(&changeset, &observed, decisions);

    let mut run = DeployRun {
        had_failures: false,
        pre_deploy: Vec::new(),
        aborted: false,
    };
    for (target_name, target) in &ctx.config.targets {
        if !ctx.input.no_hooks
            && let Some(hooks) = &target.hooks
            && hooks.pre_deploy.is_some()
        {
            let outcomes = hooks::dispatch_pre_deploy(hooks, target_name, &target.expanded_path())?;
            let failed = outcomes
                .iter()
                .any(|o| o.status == hooks::HookStatus::Failure);
            run.pre_deploy.extend(outcomes);
            if failed {
                match hooks.pre_deploy_on_fail {
                    // abort halts the whole sync: break before this target deploys.
                    PreDeployOnFail::Abort => {
                        run.aborted = true;
                        break;
                    }
                    // skip drops only this target's deploy but marks had_failures (suppresses prune).
                    PreDeployOnFail::Skip => {
                        run.had_failures = true;
                        continue;
                    }
                }
            }
        }
        let Some(target_projection) = ctx
            .projection
            .targets
            .iter()
            .find(|tp| &tp.target == target_name)
        else {
            continue;
        };
        run.had_failures |= deploy_reconciled_target(
            target_run(ctx, target_name, target),
            target_projection,
            &reconciliation,
            ctx.backend,
            ctx.registry,
            ctx.journal,
        )?;
    }
    Ok(run)
}

fn reject_cross_target_overlap(projection: &Projection, config: &Config) -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Sync(format!("resolve current dir for overlap check: {e}")))?;
    let mut placements: Vec<(&str, PathBuf)> = Vec::new();
    for target_projection in &projection.targets {
        let Some(target) = config.targets.get(&target_projection.target) else {
            continue;
        };
        let root = cwd.join(target.expanded_path());
        let layout = target.layout();
        for binding in &target_projection.bindings {
            for key in projected_artifact_keys(binding) {
                placements.push((
                    &target_projection.target,
                    root.join(layout.artifact_path(&binding.identity, &key)),
                ));
            }
        }
    }
    for (i, (first_target, first_path)) in placements.iter().enumerate() {
        for (second_target, second_path) in &placements[i + 1..] {
            if first_target != second_target
                && (first_path.starts_with(second_path) || second_path.starts_with(first_path))
            {
                return Err(cross_target_overlap_diagnostic(
                    first_target,
                    second_target,
                    first_path,
                    second_path,
                ));
            }
        }
    }
    Ok(())
}

fn cross_target_overlap_diagnostic(
    first_target: &str,
    second_target: &str,
    first_path: &Path,
    second_path: &Path,
) -> Error {
    crate::diagnostic::SelectionDiagnostic {
        entry: format!("{first_target} / {second_target}"),
        matched_against: "the physical deploy destinations across all targets".to_owned(),
        why: "two targets project artifacts onto overlapping physical paths".to_owned(),
        did_you_mean: None,
        remedy: "give each target a disjoint deploy path, or narrow their sources".to_owned(),
        debug_hint: Some("phora preview".to_owned()),
        details: vec![
            format!("target `{first_target}`: {}", first_path.display()),
            format!("target `{second_target}`: {}", second_path.display()),
        ],
    }
    .sync()
}

fn notify_orphans(config: &Config, registry: &dyn Registry) -> Result<()> {
    let count = orphan_records(config, registry)?.len();
    if count > 0 {
        eprintln!(
            "phora: {count} orphaned record(s) with no config target — \
             run `phora list --orphans` to inspect, `phora sync --prune` to remove"
        );
    }
    Ok(())
}

fn maybe_prune(ctx: &DeployAll<'_>, had_failures: bool) -> Result<()> {
    if !ctx.input.prune {
        return Ok(());
    }
    if had_failures {
        eprintln!("phora: skipping --prune because some artifacts failed to deploy");
        return Ok(());
    }
    prune_projected(ctx.projection, ctx.config, ctx.registry, ctx.protected)
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

fn local_source_names(input: &SyncInput<'_>) -> BTreeSet<String> {
    input
        .local_config
        .map(|config| config.sources.keys().cloned().collect())
        .unwrap_or_default()
}

fn merged_config(input: &SyncInput<'_>) -> Config {
    merge_configs(input.base_config.clone(), input.local_config.cloned())
}

fn open_sync_journal(lockless: bool, registry: &dyn Registry) -> Result<Journal> {
    if lockless {
        Ok(Journal::open_readonly(&registry.locks_dir()))
    } else {
        Journal::open(&registry.locks_dir())
    }
}

fn apply_fast_forward_drops(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    projection: &Projection,
    recorded: &[RegistryRecord],
    registry: &dyn Registry,
    protected: &confine::ProtectedPathSet,
    fast_forward: bool,
) -> Result<()> {
    let drops = validate_sealed_offer(config, parsed, projection, recorded, fast_forward)?;
    prune_fast_forward_drops(projection, config, registry, protected, &drops)
}

fn project_sync_workspace(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<Projection> {
    let projection = project_workspace(config, parsed, remotes, backend, resolved_commits)?;
    reject_cross_target_overlap(&projection, config)?;
    Ok(projection)
}

pub fn sync(
    input: &SyncInput<'_>,
    backend: &(dyn StageSource + Sync),
    registry: &dyn Registry,
) -> Result<SyncOutput> {
    let mut effective_config = merged_config(input);
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

    let local_names = local_source_names(input);

    let readonly_registry;
    let registry: &dyn Registry = if input.lockless {
        readonly_registry = crate::store::FrozenReadOnlyRegistry::new(registry);
        &readonly_registry
    } else {
        registry
    };
    let journal = open_sync_journal(input.lockless, registry)?;
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Sync(format!("resolve current dir for confinement: {e}")))?;
    let protected = confine::ProtectedPathSet::resolve(&effective_config.paths, &cwd)?;

    sweep_target_parents(&effective_config, &journal, registry)?;

    let recorded_after_recovery = live_recorded_artifacts(registry)?;

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
    record_candidate_hooks(&mut base_lock, &hook_candidates);

    report_ref_transitions(
        &effective_config,
        &parsed,
        effective_lock.as_ref(),
        &resolved_commits,
    );
    let projection = project_sync_workspace(
        &effective_config,
        &parsed,
        &remotes,
        backend,
        &resolved_commits,
    )?;
    apply_fast_forward_drops(
        &effective_config,
        &parsed,
        &projection,
        &recorded_after_recovery,
        registry,
        &protected,
        input.fast_forward,
    )?;

    // pre_sync gates the run: a failure aborts before deploy, leaving zero files deployed.
    let pre_sync_outcomes = run_pre_sync(input, &effective_config)?;
    if pre_sync_outcomes
        .iter()
        .any(|o| o.status == hooks::HookStatus::Failure)
    {
        return Ok(aborted_before_deploy_phase(
            base_lock,
            local_lock,
            pre_sync_outcomes,
        ));
    }

    let deploy = DeployAll {
        config: &effective_config,
        parsed: &parsed,
        remotes: &remotes,
        projection: &projection,
        protected: &protected,
        input,
        backend,
        registry,
        journal: &journal,
    };
    deploy_and_run_hooks(
        &deploy,
        base_lock,
        local_lock,
        &hook_candidates,
        effective_lock.as_ref(),
        pre_sync_outcomes,
    )
}

fn deploy_and_run_hooks(
    deploy: &DeployAll<'_>,
    mut base_lock: Lock,
    local_lock: Option<Lock>,
    hook_candidates: &[transitive::TransitiveHookCandidate],
    effective_lock: Option<&Lock>,
    pre_sync_outcomes: Vec<hooks::HookOutcome>,
) -> Result<SyncOutput> {
    let run = deploy_all_targets(deploy)?;
    // pre_deploy renders after pre_sync, before post_sync/on_change.
    let mut early_hooks = pre_sync_outcomes;
    early_hooks.extend(run.pre_deploy);
    if run.aborted {
        // An abort gate short-circuits like pre_sync: no prune, no further hook phases.
        return Ok(aborted_before_deploy_phase(
            base_lock,
            local_lock,
            early_hooks,
        ));
    }
    let mut had_failures = run.had_failures;
    maybe_prune(deploy, had_failures)?;
    if !deploy.input.prune {
        notify_orphans(deploy.config, deploy.registry)?;
    }

    let (hook_results, stripped_transitive_hooks) = run_all_hooks(
        deploy.input,
        deploy.config,
        deploy.registry,
        &mut base_lock,
        hook_candidates,
        effective_lock,
        early_hooks,
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

/// Short-circuit shared by a failed `pre_sync` gate and a `pre_deploy` abort: files already
/// deployed stay, but no prune and no post-deploy hook phase run. `deploy_failures` is false
/// because a gate failure is a hook failure, not a per-artifact deploy failure.
fn aborted_before_deploy_phase(
    base_lock: Lock,
    local_lock: Option<Lock>,
    hook_results: Vec<hooks::HookOutcome>,
) -> SyncOutput {
    SyncOutput {
        base_lock,
        local_lock,
        had_failures: true,
        deploy_failures: false,
        hook_results,
        stripped_transitive_hooks: 0,
    }
}

fn run_pre_sync(input: &SyncInput<'_>, config: &Config) -> Result<Vec<hooks::HookOutcome>> {
    if input.no_hooks {
        return Ok(Vec::new());
    }
    let target_names = config
        .targets
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    hooks::dispatch_pre_sync(config, &target_names)
}

fn run_all_hooks(
    input: &SyncInput<'_>,
    config: &Config,
    registry: &dyn Registry,
    base_lock: &mut Lock,
    hook_candidates: &[transitive::TransitiveHookCandidate],
    effective_lock: Option<&Lock>,
    early_hooks: Vec<hooks::HookOutcome>,
) -> Result<(Vec<hooks::HookOutcome>, usize)> {
    // pre_sync + pre_deploy render before post_sync/on_change, so they seed the result vec.
    let mut hook_results = early_hooks;
    if !input.no_hooks {
        hook_results.append(&mut hooks::dispatch_hooks(config, registry)?);
    }
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

struct BindingOffer<'projection> {
    selection: crate::kernel::OfferSelection,
    projected: &'projection crate::projection::model::BindingProjection,
    deploy_mode: DeployMode,
    ref_label: String,
}

#[derive(Clone, Copy)]
struct SealedRecordPolicy {
    immutable_copy: bool,
    commit_differs: bool,
}

impl SealedRecordPolicy {
    fn classify(record: &RegistryRecord, offer: &BindingOffer<'_>) -> Self {
        Self {
            immutable_copy: !record.linked && offer.deploy_mode == DeployMode::Copy,
            commit_differs: record.commit != offer.projected.commit,
        }
    }

    fn same_snapshot_transition(self) -> bool {
        self.immutable_copy && !self.commit_differs
    }

    fn fast_forward_actionable(self) -> bool {
        self.immutable_copy && self.commit_differs
    }
}

/// Compares stale records against the resolved OFFER set, not the take/kept set. Positive current
/// attribution clears a record in every deploy mode, so a leaf dropped only by `take` stays
/// allowed. Only immutable-copy records may treat exclusion of every candidate by the current
/// offer as intentional config narrowing, or exact current-commit equality as a projection-shape
/// transition; both are left to reconciliation/prune. When commits differ, only immutable-copy
/// records are actionable `--fast-forward` drops. A historical linked record or current Link
/// binding without positive attribution remains ambiguous and seals, even when the current offer
/// excludes every available candidate.
fn validate_sealed_offer(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    projection: &Projection,
    recorded: &[RegistryRecord],
    fast_forward: bool,
) -> Result<Vec<RegistryRecord>> {
    use crate::kernel::OfferSelection;

    let mut offers: BTreeMap<(String, String), BindingOffer<'_>> = BTreeMap::new();
    for (target_name, target) in &config.targets {
        let Some(target_projection) = projection
            .targets
            .iter()
            .find(|projected| projected.target == *target_name)
        else {
            continue;
        };
        for binding in target.resolve_sources(parsed) {
            let Some(source) = parsed.get(binding.source) else {
                continue;
            };
            let Some(projected_binding) = target_projection
                .bindings
                .iter()
                .find(|projected| projected.identity == binding.identity)
            else {
                continue;
            };
            let offer = source.offer();
            let selection =
                OfferSelection::compile(offer.includes(), offer.excludes(), offer.root())?;
            offers.insert(
                (target_name.clone(), binding.identity.to_owned()),
                BindingOffer {
                    selection,
                    projected: projected_binding,
                    deploy_mode: source.deploy_mode(),
                    ref_label: binding.effective_ref.to_string(),
                },
            );
        }
    }

    let mut dropped = Vec::new();
    for record in recorded {
        let key = &record.key;
        let Some(offer) = offers.get(&(key.target.clone(), key.source.clone())) else {
            continue;
        };
        let artifact = &key.artifact;
        if offer
            .projected
            .artifacts
            .iter()
            .any(|projected| projected.materialization.published_key() == artifact)
        {
            continue;
        }
        let policy = SealedRecordPolicy::classify(record, offer);
        if policy.same_snapshot_transition() {
            continue;
        }
        let source_paths = record_source_paths(offer.projected, record)?;
        let has_current_attribution = source_paths.iter().any(|source_path| {
            offer
                .projected
                .attribution
                .offered_leaves
                .iter()
                .any(|offered| paths_overlap(offered, source_path))
        });
        let selection_admits_candidate = source_paths
            .iter()
            .any(|source_path| offer.selection.admits_published(source_path));
        if has_current_attribution || (policy.immutable_copy && !selection_admits_candidate) {
            continue;
        }
        if fast_forward && policy.fast_forward_actionable() {
            dropped.push(record.clone());
            continue;
        }
        let deploy_path = config.targets.get(&key.target).map(|target| {
            target::record_artifact_path(target, record)
                .display()
                .to_string()
        });
        return Err(sealed_offer_diagnostic(&SealedOffer {
            target: &key.target,
            source: &key.source,
            artifact,
            deploy_path: deploy_path.as_deref(),
            recorded_commit: &record.commit,
            resolved_commit: &offer.projected.commit,
            resolved_ref: &offer.ref_label,
            fast_forward_actionable: policy.fast_forward_actionable(),
        }));
    }
    Ok(dropped)
}

fn record_source_paths(
    binding: &crate::projection::model::BindingProjection,
    record: &RegistryRecord,
) -> Result<BTreeSet<String>> {
    let artifact = record.key.artifact.as_str();
    let mut paths = BTreeSet::from([artifact.to_owned()]);
    if !record.linked && record.kind == crate::store::RecordKind::Dir {
        for file in &record.files {
            let relative = persisted_manifest_relative_path(&file.path)?;
            paths.insert(format!("{artifact}/{relative}"));
        }
    }
    for resolved in &binding.attribution.resolved_takes {
        if paths.iter().any(|path| paths_overlap(path, &resolved.dest)) {
            paths.insert(resolved.source.clone());
        }
    }
    if binding.attribution.copy_template_suffix && !record.linked && record.vars_digest.is_some() {
        paths.extend(
            paths
                .iter()
                .filter(|path| path.strip_suffix(".tmpl").is_none())
                .map(|path| format!("{path}.tmpl"))
                .collect::<Vec<_>>(),
        );
    }
    Ok(paths)
}

fn persisted_manifest_relative_path(path: &Path) -> Result<ArtifactRelativePath> {
    let relative = path.to_str().ok_or_else(|| {
        Error::Sync(format!(
            "invalid persisted manifest data path {}: path is not UTF-8",
            path.display()
        ))
    })?;
    ArtifactRelativePath::new(relative).map_err(|error| {
        Error::Sync(format!(
            "invalid persisted manifest data path {}: {error}",
            path.display()
        ))
    })
}

fn paths_overlap(first: &str, second: &str) -> bool {
    first == second
        || first
            .strip_prefix(second)
            .is_some_and(|rest| rest.starts_with('/'))
        || second
            .strip_prefix(first)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn prune_fast_forward_drops(
    projection: &Projection,
    config: &Config,
    registry: &dyn Registry,
    protected: &confine::ProtectedPathSet,
    drops: &[RegistryRecord],
) -> Result<()> {
    if drops.is_empty() {
        return Ok(());
    }
    if registry.refuses_writes() {
        return Err(registry.readonly_error().into());
    }
    let expected_paths = prune::expected_live_paths(projection, config);
    for record in drops {
        let Some(target) = config.targets.get(&record.key.target) else {
            continue;
        };
        let dst = target::record_artifact_path(target, record);
        let confined = match &target.confine {
            Some(anchor) => confine::confine_destination(anchor, &dst, protected),
            None if target::is_composed_target(&record.key.target) => Err(Error::Config(format!(
                "confinement: composed target `{}` reached fast-forward prune without a confine \
                 anchor; refusing an unconfined delete",
                record.key.target
            ))),
            None => Ok(dst.clone()),
        };
        let path = confined.map_err(|e| {
            Error::Sync(format!(
                "fast-forward refuses out-of-anchor {}: {e}; eject it instead",
                dst.display()
            ))
        })?;
        if prune::overlaps_live_dest(&path, &expected_paths, &record.key.target) {
            eprintln!(
                "phora: fast-forward unrecorded {}:{} but kept {} (a live artifact sits there)",
                record.key.source,
                record.key.artifact,
                path.display()
            );
        } else {
            eprintln!(
                "phora: fast-forward dropped {}:{} (removed upstream)",
                record.key.source, record.key.artifact
            );
            remove_orphan_path(&path)
                .map_err(|e| Error::Sync(format!("fast-forward prune {}: {e}", path.display())))?;
        }
        registry.remove(&record.key)?;
    }
    Ok(())
}

struct SealedOffer<'a> {
    target: &'a str,
    source: &'a str,
    artifact: &'a str,
    deploy_path: Option<&'a str>,
    recorded_commit: &'a str,
    resolved_commit: &'a str,
    resolved_ref: &'a str,
    fast_forward_actionable: bool,
}

fn sealed_offer_diagnostic(ctx: &SealedOffer<'_>) -> Error {
    let &SealedOffer {
        target,
        source,
        artifact,
        deploy_path,
        recorded_commit,
        resolved_commit,
        resolved_ref,
        fast_forward_actionable,
    } = ctx;
    let now = pin_label(resolved_ref, resolved_commit);
    let mut details = vec![
        format!("binding: source `{source}` → target `{target}`"),
        format!(
            "pin: recorded {} → now {now}",
            short_commit(recorded_commit)
        ),
    ];
    if let Some(path) = deploy_path {
        details.push(format!("path: {path}"));
    }
    let remedy = if fast_forward_actionable {
        "re-sync with `--fast-forward` to drop it, or eject it before removing it".to_string()
    } else {
        "restore the artifact to the source's offer, or eject it before removing it".to_string()
    };
    crate::diagnostic::SelectionDiagnostic {
        entry: format!("{target}:{source}:{artifact}"),
        matched_against: format!("the current offer of source `{source}` in target `{target}`"),
        why: "a recorded artifact is no longer in the source's offer".to_string(),
        did_you_mean: None,
        remedy,
        debug_hint: Some(format!("phora explain {target} {source} {artifact}")),
        details,
    }
    .sync()
}

fn short_commit(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn live_recorded_artifacts(registry: &dyn Registry) -> Result<Vec<RegistryRecord>> {
    let recorded = registry.list_all()?;
    let ejected = crate::store::ejected_index(registry, &recorded)?;
    Ok(recorded
        .into_iter()
        .filter(|r| {
            !ejected.contains(&(
                r.key.target.clone(),
                r.key.source.clone(),
                r.key.artifact.clone(),
            ))
        })
        .collect())
}

fn pin_label(ref_label: &str, commit: &str) -> String {
    if ref_label.is_empty() {
        short_commit(commit).to_string()
    } else {
        format!("{ref_label} ({})", short_commit(commit))
    }
}

fn report_ref_transitions(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    effective_lock: Option<&Lock>,
    resolved_commits: &BTreeMap<(String, String), String>,
) {
    for line in ref_transition_lines(config, parsed, effective_lock, resolved_commits) {
        eprintln!("{line}");
    }
}

fn ref_transition_lines(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    effective_lock: Option<&Lock>,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (target_name, target) in &config.targets {
        for binding in target.resolve_sources(parsed) {
            let Some(source) = parsed.get(binding.source) else {
                continue;
            };
            let encoded_ref = crate::lock::encode_ref(&binding.effective_ref);
            let Some(new_commit) = resolved_commits.get(&(binding.source.to_owned(), encoded_ref))
            else {
                continue;
            };
            let discriminator =
                crate::lock::ref_discriminator(&binding.effective_ref, &source.refspec());
            let Some(old) = effective_lock
                .and_then(|lock| lock.find_entry(binding.source, discriminator.as_deref()))
            else {
                continue;
            };
            if &old.commit == new_commit {
                continue;
            }
            let from = pin_label(&old.resolved, &old.commit);
            let to = pin_label(&binding.effective_ref.to_string(), new_commit);
            lines.push(format!(
                "phora: {} → {target_name}: {from} → {to}",
                binding.source
            ));
        }
    }
    lines
}

/// Resolves the composed transitive graph OFFLINE (frozen reads of the pinned dep manifests
/// already in the git mirror) and injects its synthetic confined targets and namespaced sources
/// so a read-only observer sees them. Degrades silently — if a dep is unpinned, the mirror is
/// absent, or the graph is otherwise unresolvable, config/parsed/remotes are left un-injected
/// rather than failing the observer.
pub(crate) fn inject_composed_graph(
    config: &mut Config,
    parsed: &mut BTreeMap<String, ParsedSource>,
    remotes: &mut BTreeMap<String, String>,
    backend: &(dyn SourceBackend + Sync),
    lock: Option<&Lock>,
) {
    if let Ok(graph) = transitive::resolve_transitive_graph(config, parsed, backend, true, lock) {
        graph.inject(config, parsed, remotes);
    }
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
