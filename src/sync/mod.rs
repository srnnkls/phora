//! Top-level orchestration: the `sync` pipeline, eject/uneject, and shared helpers.

pub mod apply;
pub(crate) mod confine;
pub(crate) mod discover;
pub(crate) mod hooks;
pub mod inspect;
pub mod journal;
pub mod model;
pub(crate) mod observe;
mod plan;
mod preview;
mod prune;
mod rebuild;
pub mod reconcile;
pub mod recovery;
mod request;
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

#[cfg(test)]
use crate::projection::build::projected_artifact_keys;
use crate::projection::build::projected_artifacts;
use crate::projection::model::{ArtifactRelativePath, Projection};
#[cfg(test)]
use crate::sync::model::ReconciliationPolicy;

pub(crate) use preview::offered_leaves;
pub use preview::{
    BindingWarnings, PreviewCollision, PreviewEntry, PreviewFile, PreviewTargetPlan,
    PreviewWarning, SyncState, preview_targets,
};
pub use rebuild::{RebuildReport, rebuild_registry, rebuild_registry_with};
pub use request::{
    AppliedChange, Concurrency, ConflictPolicy, HookPolicy, LockSet, MovedPinPolicy, PrunePolicy,
    SkippedChange, SourcePolicy, SyncOptions, SyncReport, SyncRequest, SyncStatus, SyncWarning,
};
pub use verify::{
    OverlayFinding, UntrustedHookFinding, VerifyMismatch, VerifyReason, VerifyReport, verify,
};

#[cfg(feature = "bench")]
pub use resolve::resolve_sources_for_bench;

#[cfg(test)]
use prune::prune_projected;
pub(crate) use prune::{orphan_artifact_path, orphan_records};
use request::SyncEvents;
use resolve::{ResolvedSourceMap, resolve_sources};
pub use stage::{StageRequest, StagedArtifact, StagedFile, stage_artifact};
#[cfg(test)]
use target::deploy_reconciled_target;
pub(crate) use target::record_artifact_path;
use target::{
    Reconciliation, TargetRun, deploy_reconciled_target_report, resolve_conflicts,
    resolve_conflicts_reusing,
};

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
use crate::source::{SourceStore, is_local_path};
use crate::sync::state::{ArtifactKey, ArtifactRecord, Ejection, StateStore};

use journal::Journal;
use recovery::recovery_sweep;

/// Test-only compatibility input for the pre-T027 unit fixtures.
#[cfg(test)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "legacy test fixtures exercise the old boolean mapping into SyncOptions"
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

/// Test-only compatibility output for the pre-T027 unit fixtures.
#[cfg(test)]
pub struct SyncOutput {
    pub base_lock: Lock,
    pub local_lock: Option<Lock>,
    pub had_failures: bool,
    pub deploy_failures: bool,
    pub hook_results: Vec<hooks::HookOutcome>,
    /// Transitive hooks discovered but left unrun for lack of trust.
    pub stripped_transitive_hooks: usize,
}

struct SyncRunInput<'a> {
    base_config: &'a Config,
    local_config: Option<&'a Config>,
    locks: LockSet,
    options: SyncOptions,
    resolver: Option<&'a dyn ConflictResolver>,
    lockless: bool,
}

impl SyncRunInput<'_> {
    fn refresh_sources(&self) -> bool {
        matches!(self.options.source_policy, SourcePolicy::Refresh)
    }

    fn frozen(&self) -> bool {
        matches!(self.options.source_policy, SourcePolicy::Frozen)
    }

    fn fast_forward(&self) -> bool {
        matches!(self.options.moved_pin_policy, MovedPinPolicy::FastForward)
    }

    fn jobs(&self) -> Option<usize> {
        self.options.concurrency.jobs.map(std::num::NonZero::get)
    }
}

trait RunOptions {
    fn options(&self) -> SyncOptions;
    fn resolver(&self) -> Option<&dyn ConflictResolver>;
    fn lockless(&self) -> bool;

    fn interactive(&self) -> bool {
        matches!(
            self.options().conflict_policy,
            ConflictPolicy::ResolveInteractively
        )
    }

    fn prune(&self) -> bool {
        matches!(self.options().prune_policy, PrunePolicy::RemoveOrphans)
    }

    fn hooks_enabled(&self) -> bool {
        !matches!(self.options().hook_policy, HookPolicy::None)
    }

    fn transitive_hooks_enabled(&self) -> bool {
        matches!(self.options().hook_policy, HookPolicy::All)
    }
}

impl RunOptions for SyncRunInput<'_> {
    fn options(&self) -> SyncOptions {
        self.options
    }

    fn resolver(&self) -> Option<&dyn ConflictResolver> {
        self.resolver
    }

    fn lockless(&self) -> bool {
        self.lockless
    }
}

#[cfg(test)]
impl RunOptions for SyncInput<'_> {
    fn options(&self) -> SyncOptions {
        self.run_input().options
    }

    fn resolver(&self) -> Option<&dyn ConflictResolver> {
        self.resolver
    }

    fn lockless(&self) -> bool {
        self.lockless
    }
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "legacy outcome details remain only for the cfg(test) compatibility adapter"
    )
)]
struct SyncExecution {
    report: SyncReport,
    deploy_failures: bool,
    stripped_transitive_hooks: usize,
}

#[cfg(test)]
trait TestSyncInvocation {
    type Output;

    fn run_input(&self) -> SyncRunInput<'_>;
    fn finish(execution: SyncExecution) -> Self::Output;
}

#[cfg(test)]
impl TestSyncInvocation for SyncRequest<'_> {
    type Output = SyncReport;

    fn run_input(&self) -> SyncRunInput<'_> {
        SyncRunInput {
            base_config: self.base_config,
            local_config: self.local_config,
            locks: self.locks.clone(),
            options: self.options,
            resolver: self.resolver,
            lockless: false,
        }
    }

    fn finish(execution: SyncExecution) -> Self::Output {
        execution.report
    }
}

#[cfg(test)]
impl TestSyncInvocation for SyncInput<'_> {
    type Output = SyncOutput;

    fn run_input(&self) -> SyncRunInput<'_> {
        let source_policy = if self.frozen {
            SourcePolicy::Frozen
        } else if self.force {
            SourcePolicy::Refresh
        } else {
            SourcePolicy::Locked
        };
        let conflict_policy = if self.force {
            ConflictPolicy::Overwrite
        } else if self.interactive {
            ConflictPolicy::ResolveInteractively
        } else {
            ConflictPolicy::Refuse
        };
        let hook_policy = if self.no_hooks {
            HookPolicy::None
        } else if self.no_transitive_hooks {
            HookPolicy::NoTransitive
        } else {
            HookPolicy::All
        };
        SyncRunInput {
            base_config: self.base_config,
            local_config: self.local_config,
            locks: LockSet {
                base: self.base_lock.clone(),
                local: self.local_lock.clone(),
            },
            options: SyncOptions {
                source_policy,
                conflict_policy,
                prune_policy: if self.prune {
                    PrunePolicy::RemoveOrphans
                } else {
                    PrunePolicy::KeepOrphans
                },
                hook_policy,
                moved_pin_policy: if self.fast_forward {
                    MovedPinPolicy::FastForward
                } else {
                    MovedPinPolicy::Seal
                },
                concurrency: Concurrency {
                    jobs: self.jobs.and_then(std::num::NonZeroUsize::new),
                },
            },
            resolver: self.resolver,
            lockless: self.lockless,
        }
    }

    fn finish(execution: SyncExecution) -> Self::Output {
        compatibility_output(execution)
    }
}

#[cfg(test)]
fn compatibility_output(execution: SyncExecution) -> SyncOutput {
    let SyncExecution {
        report,
        deploy_failures,
        stripped_transitive_hooks,
    } = execution;
    let LockSet { base, local } = report.locks;
    SyncOutput {
        base_lock: base.expect("sync execution always returns a base lock"),
        local_lock: local,
        had_failures: report.status == SyncStatus::Failed,
        deploy_failures,
        hook_results: report.hook_outcomes,
        stripped_transitive_hooks,
    }
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
    events: &mut SyncEvents,
) -> Vec<transitive::TransitiveHookCandidate> {
    for diagnostic in std::mem::take(&mut graph.hook_diagnostics) {
        events.warnings.push(SyncWarning::MalformedTransitiveHooks {
            target: diagnostic.target,
            detail: diagnostic.detail,
        });
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

struct DeployAll<'a, R> {
    config: &'a Config,
    parsed: &'a BTreeMap<String, ParsedSource>,
    remotes: &'a BTreeMap<String, String>,
    projection: &'a Projection,
    protected: &'a confine::ProtectedPathSet,
    input: &'a dyn RunOptions,
    resolved_sources: &'a ResolvedSourceMap,
    backend: &'a dyn SourceStore,
    registry: &'a R,
    journal: &'a Journal,
}

struct DeployProtocol<'a, R> {
    workspace: DeployAll<'a, R>,
    fast_forward_drops: &'a [FastForwardDrop],
}

/// Outcome of the all-gates-before-mutation deploy protocol. `aborted` means a `pre_deploy` gate
/// stopped hook processing before planned drops or target changes were applied; `had_failures`
/// folds in skip-induced failures so it can suppress `--prune`.
struct ApplyRun {
    had_failures: bool,
    pre_deploy: Vec<hooks::HookOutcome>,
    aborted: bool,
    changes: model::ChangeSet,
    events: SyncEvents,
}

fn target_run<'a, R>(
    ctx: &DeployAll<'a, R>,
    target_name: &'a str,
    target: &'a crate::config::Target,
) -> TargetRun<'a> {
    TargetRun {
        parsed: ctx.parsed,
        target_name,
        target,
        remotes: ctx.remotes,
        resolved_sources: ctx.resolved_sources,
        vars: &ctx.config.vars,
        protected: ctx.protected,
    }
}

#[derive(Default)]
struct PreDeployRun {
    outcomes: Vec<hooks::HookOutcome>,
    skipped_targets: BTreeSet<String>,
    aborted: bool,
    ran: bool,
}

fn run_pre_deploy<R>(ctx: &DeployAll<'_, R>) -> Result<PreDeployRun> {
    let mut run = PreDeployRun::default();
    if !ctx.input.hooks_enabled() {
        return Ok(run);
    }
    for (target_name, target) in &ctx.config.targets {
        let Some(hooks) = &target.hooks else {
            continue;
        };
        if hooks.pre_deploy.is_none() {
            continue;
        }
        let outcomes = hooks::dispatch_pre_deploy(hooks, target_name, &target.expanded_path())?;
        run.ran |= !outcomes.is_empty();
        let failed = outcomes
            .iter()
            .any(|outcome| outcome.status == hooks::HookStatus::Failure);
        run.outcomes.extend(outcomes);
        if !failed {
            continue;
        }
        match hooks.pre_deploy_on_fail {
            PreDeployOnFail::Abort => {
                run.aborted = true;
                break;
            }
            PreDeployOnFail::Skip => {
                run.skipped_targets.insert(target_name.clone());
            }
        }
    }
    Ok(run)
}

fn refuse_lockless_mutation(
    input: &dyn RunOptions,
    registry: &dyn StateStore,
    observed: &model::ObservedProjectState<ArtifactRecord>,
    changeset: &model::ChangeSet,
) -> Result<()> {
    let refresh_pending = observed.artifacts.iter().any(|entry| {
        matches!(
            entry.observation,
            model::ObservedArtifact::Managed(model::ManagedArtifact {
                condition: model::ManagedCondition::MetadataChangedButContentClean { .. },
                ..
            })
        ) && !changeset.changes.iter().any(|change| {
            matches!(
                change,
                model::SyncChange::RewriteOverlay {
                    target,
                    source,
                    artifact,
                } if target == &entry.target && source == &entry.source && artifact == &entry.artifact
            )
        })
    });
    let other_mutation = changeset
        .changes
        .iter()
        .any(|change| !matches!(change, model::SyncChange::RewriteOverlay { .. }));
    if input.lockless() && (other_mutation || refresh_pending) {
        return Err(readonly_state_error(registry));
    }
    Ok(())
}

fn apply_target_changes<R>(
    ctx: &DeployAll<'_, R>,
    fast_forward_drops: &[FastForwardDrop],
) -> Result<ApplyRun>
where
    R: StateStore,
{
    let initial_observed = observe::observe_workspace(ctx, ctx.projection)?;
    let options = ctx.input.options();
    let policy = (&options).into();
    let initial_changeset = reconcile::reconcile(ctx.projection, &initial_observed, &policy)
        .map_err(|e| Error::Sync(e.to_string()))?;
    let registry: &dyn StateStore = ctx.registry;
    refuse_lockless_mutation(ctx.input, registry, &initial_observed, &initial_changeset)?;
    let initial_decisions = resolve_conflicts(
        &initial_changeset,
        ctx.input.resolver(),
        ctx.input.interactive(),
    )?;
    let PreDeployRun {
        outcomes,
        skipped_targets,
        aborted,
        ran,
    } = run_pre_deploy(ctx)?;
    if aborted {
        return Ok(ApplyRun {
            had_failures: false,
            pre_deploy: outcomes,
            aborted: true,
            changes: initial_changeset,
            events: SyncEvents::default(),
        });
    }

    let (observed, changeset, decisions) = if ran {
        let observed = observe::observe_workspace(ctx, ctx.projection)?;
        let changeset = reconcile::reconcile(ctx.projection, &observed, &policy)
            .map_err(|error| Error::Sync(error.to_string()))?;
        refuse_lockless_mutation(ctx.input, registry, &observed, &changeset)?;
        let decisions = resolve_conflicts_reusing(
            &changeset,
            &initial_decisions,
            &skipped_targets,
            ctx.input.resolver(),
            ctx.input.interactive(),
        )?;
        (observed, changeset, decisions)
    } else {
        (initial_observed, initial_changeset, initial_decisions)
    };
    let reconciliation = Reconciliation::new(&changeset, &observed, decisions);
    let mut run = ApplyRun {
        had_failures: !skipped_targets.is_empty(),
        pre_deploy: outcomes,
        aborted: false,
        changes: changeset.clone(),
        events: SyncEvents::default(),
    };
    apply_fast_forward_drops(
        fast_forward_drops,
        registry,
        &skipped_targets,
        &mut run.events,
    )?;
    for (target_name, target) in &ctx.config.targets {
        if skipped_targets.contains(target_name) {
            continue;
        }
        let Some(target_projection) = ctx
            .projection
            .targets
            .iter()
            .find(|tp| &tp.target == target_name)
        else {
            continue;
        };
        run.had_failures |= deploy_reconciled_target_report(
            target_run(ctx, target_name, target),
            target_projection,
            &reconciliation,
            ctx.backend,
            registry,
            ctx.journal,
            &mut run.events,
        )?;
    }
    if !run.had_failures {
        prune::apply_reconciled_removals(
            &changeset.changes,
            &observed,
            ctx.projection,
            ctx.config,
            registry,
            ctx.protected,
            &mut run.events,
        )?;
    } else if ctx.input.prune() && run.had_failures {
        run.events
            .warnings
            .push(SyncWarning::PruneSkippedAfterFailures);
    }
    Ok(run)
}

fn reject_cross_target_overlap(projection: &Projection, config: &Config) -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Sync(format!("resolve current dir for overlap check: {e}")))?;
    let mut placements: Vec<(&str, PathBuf, PathBuf)> = Vec::new();
    for target_projection in &projection.targets {
        let Some(target) = config.targets.get(&target_projection.target) else {
            continue;
        };
        let root = cwd.join(target.expanded_path());
        for binding in &target_projection.bindings {
            for item in projected_artifacts(binding) {
                let path = root.join(item.destination.as_str());
                let physical = confine::normalize_physical(&path)?;
                let identity = confine::fold_path(&physical);
                placements.push((&target_projection.target, physical, identity));
            }
        }
    }
    for (i, (first_target, first_path, first_identity)) in placements.iter().enumerate() {
        for (second_target, second_path, second_identity) in &placements[i + 1..] {
            if first_target != second_target
                && (first_identity.starts_with(second_identity)
                    || second_identity.starts_with(first_identity))
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

fn notify_orphans(
    config: &Config,
    registry: &dyn StateStore,
    events: &mut SyncEvents,
) -> Result<()> {
    let count = orphan_records(config, registry)?.len();
    if count > 0 {
        events.warnings.push(SyncWarning::OrphanedRecords { count });
    }
    Ok(())
}

fn sweep_target_parents(
    config: &Config,
    journal: &Journal,
    registry: &dyn StateStore,
) -> Result<()> {
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

fn effective_lock(input: &SyncRunInput<'_>) -> Option<Lock> {
    match (&input.locks.base, &input.locks.local) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    }
}

fn local_source_names(input: &SyncRunInput<'_>) -> BTreeSet<String> {
    input
        .local_config
        .map(|config| config.sources.keys().cloned().collect())
        .unwrap_or_default()
}

fn merged_config(input: &SyncRunInput<'_>) -> Config {
    merge_configs(input.base_config.clone(), input.local_config.cloned())
}

fn open_sync_journal(lockless: bool, registry: &dyn StateStore) -> Result<Journal> {
    if lockless {
        Ok(Journal::open_readonly(&registry.journal_root()))
    } else {
        Journal::open(&registry.journal_root())
    }
}

fn readonly_state_error(registry: &dyn StateStore) -> Error {
    let journal_root = registry.journal_root();
    let state_root = journal_root.parent().unwrap_or(&journal_root);
    state::readonly_root_error(state_root).into()
}

enum FastForwardDrop {
    Remove {
        record: ArtifactRecord,
        path: PathBuf,
    },
    KeepLive {
        record: ArtifactRecord,
        path: PathBuf,
    },
}

impl FastForwardDrop {
    fn record(&self) -> &ArtifactRecord {
        match self {
            Self::Remove { record, .. } | Self::KeepLive { record, .. } => record,
        }
    }
}

fn plan_fast_forward_drops(
    projection: &Projection,
    config: &Config,
    registry: &dyn StateStore,
    protected: &confine::ProtectedPathSet,
    drops: Vec<ArtifactRecord>,
    lockless: bool,
) -> Result<Vec<FastForwardDrop>> {
    if drops.is_empty() {
        return Ok(Vec::new());
    }
    if lockless {
        return Err(readonly_state_error(registry));
    }
    let expected_paths = prune::expected_live_paths(projection, config);
    let mut plan = Vec::new();
    for record in drops {
        let Some(target) = config.targets.get(&record.key.target) else {
            continue;
        };
        let dst = target::record_artifact_path(target, &record);
        let confined = match &target.confine {
            Some(anchor) => confine::confine_destination(anchor, &dst, protected),
            None if target::is_composed_target(&record.key.target) => Err(Error::Config(format!(
                "confinement: composed target `{}` reached fast-forward prune without a confine \
                 anchor; refusing an unconfined delete",
                record.key.target
            ))),
            None => Ok(dst.clone()),
        };
        let path = confined.map_err(|error| {
            Error::Sync(format!(
                "fast-forward refuses out-of-anchor {}: {error}; eject it instead",
                dst.display()
            ))
        })?;
        if prune::overlaps_live_dest(&path, &expected_paths, &record.key.target) {
            plan.push(FastForwardDrop::KeepLive { record, path });
        } else {
            plan.push(FastForwardDrop::Remove { record, path });
        }
    }
    Ok(plan)
}

fn project_sync_workspace(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceStore,
    resolved_commits: &BTreeMap<(String, String), String>,
    resolved_sources: &ResolvedSourceMap,
) -> Result<Projection> {
    let projection = project_workspace(
        config,
        parsed,
        remotes,
        backend,
        resolved_commits,
        resolved_sources,
    )?;
    reject_cross_target_overlap(&projection, config)?;
    Ok(projection)
}

/// Synchronizes a workspace from an explicit request and returns structured outcomes.
///
/// # Errors
///
/// Returns an error when configuration, source resolution, state inspection, reconciliation,
/// confinement, staging, apply, or hook dispatch fails.
#[cfg(not(test))]
pub fn sync<R>(
    request: &SyncRequest<'_>,
    backend: &dyn SourceStore,
    registry: &R,
) -> Result<SyncReport>
where
    R: StateStore,
{
    let input = request_run_input(request, false);
    sync_core(&input, backend, registry).map(|execution| execution.report)
}

#[cfg(test)]
fn sync<I, R>(input: &I, backend: &dyn SourceStore, registry: &R) -> Result<I::Output>
where
    I: TestSyncInvocation,
    R: StateStore,
{
    let run_input = input.run_input();
    sync_core(&run_input, backend, registry).map(I::finish)
}

pub(crate) fn sync_opened<R>(
    request: &SyncRequest<'_>,
    backend: &dyn SourceStore,
    registry: &R,
    lockless: bool,
) -> Result<SyncReport>
where
    R: StateStore,
{
    let input = request_run_input(request, lockless);
    sync_core(&input, backend, registry).map(|execution| execution.report)
}

fn request_run_input<'a>(request: &'a SyncRequest<'a>, lockless: bool) -> SyncRunInput<'a> {
    SyncRunInput {
        base_config: request.base_config,
        local_config: request.local_config,
        locks: request.locks.clone(),
        options: request.options,
        resolver: request.resolver,
        lockless,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "top-level synchronization intentionally makes the ordered whole-run phases visible"
)]
fn sync_core<R>(
    input: &SyncRunInput<'_>,
    backend: &dyn SourceStore,
    registry: &R,
) -> Result<SyncExecution>
where
    R: StateStore,
{
    let mut events = SyncEvents::default();
    let mut effective_config = merged_config(input);
    effective_config.validate()?;
    let mut parsed = effective_config.parsed_sources()?;
    let mut remotes = resolved_remotes(&effective_config, &parsed)?;
    let effective_lock = effective_lock(input);
    let mut graph = transitive::resolve_transitive_graph(
        &effective_config,
        &parsed,
        backend,
        input.frozen(),
        effective_lock.as_ref(),
    )?;
    let hook_candidates = take_hook_candidates(&mut graph, &mut events);
    let instances = graph.inject(&mut effective_config, &mut parsed, &mut remotes);
    for warning in validate_link_mode(input.base_config, &parsed, &remotes)? {
        events.warnings.push(SyncWarning::LinkPathNotPortable {
            source: warning.source,
            path: warning.path,
        });
    }

    let local_names = local_source_names(input);

    let compat_registry: &dyn StateStore = registry;
    let journal = open_sync_journal(input.lockless, compat_registry)?;
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Sync(format!("resolve current dir for confinement: {e}")))?;
    let protected = confine::ProtectedPathSet::resolve(&effective_config.paths, &cwd)?;

    sweep_target_parents(&effective_config, &journal, compat_registry)?;

    let recorded_after_recovery = live_recorded_artifacts(compat_registry)?;

    let routed = resolve_sources(
        &effective_config,
        &parsed,
        &remotes,
        &instances,
        effective_lock.as_ref(),
        backend,
        input.refresh_sources(),
        input.frozen(),
        input.jobs(),
    )?;
    let (mut base_lock, local_lock) = split_locks(routed.locks, &local_names);
    base_lock.trusted_hooks = effective_lock
        .as_ref()
        .map(|lock| lock.trusted_hooks.clone())
        .unwrap_or_default();
    record_candidate_hooks(&mut base_lock, &hook_candidates);

    collect_ref_transitions(
        &effective_config,
        &parsed,
        effective_lock.as_ref(),
        &routed.commits,
        &mut events,
    );
    let projection = project_sync_workspace(
        &effective_config,
        &parsed,
        &remotes,
        backend,
        &routed.commits,
        &routed.resolved,
    )?;
    let pending_fast_forward_drops = validate_sealed_offer(
        &effective_config,
        &parsed,
        &projection,
        &recorded_after_recovery,
        input.fast_forward(),
    )?;
    let fast_forward_drops = plan_fast_forward_drops(
        &projection,
        &effective_config,
        compat_registry,
        &protected,
        pending_fast_forward_drops,
        input.lockless,
    )?;

    // pre_sync gates the run before planned drops or ordinary target changes are applied.
    let pre_sync_outcomes = run_pre_sync(input, &effective_config)?;
    if pre_sync_outcomes
        .iter()
        .any(|o| o.status == hooks::HookStatus::Failure)
    {
        return Ok(aborted_before_deploy_phase(
            base_lock,
            local_lock,
            pre_sync_outcomes,
            events,
        ));
    }

    let deploy = DeployProtocol {
        workspace: DeployAll {
            config: &effective_config,
            parsed: &parsed,
            remotes: &remotes,
            projection: &projection,
            resolved_sources: &routed.resolved,
            protected: &protected,
            input,
            backend,
            registry,
            journal: &journal,
        },
        fast_forward_drops: &fast_forward_drops,
    };
    deploy_and_run_hooks(
        &deploy,
        base_lock,
        local_lock,
        &hook_candidates,
        effective_lock.as_ref(),
        pre_sync_outcomes,
        events,
    )
}

fn deploy_and_run_hooks<R>(
    protocol: &DeployProtocol<'_, R>,
    mut base_lock: Lock,
    local_lock: Option<Lock>,
    hook_candidates: &[transitive::TransitiveHookCandidate],
    effective_lock: Option<&Lock>,
    pre_sync_outcomes: Vec<hooks::HookOutcome>,
    events: SyncEvents,
) -> Result<SyncExecution>
where
    R: StateStore,
{
    let DeployProtocol {
        workspace: deploy,
        fast_forward_drops,
    } = protocol;
    let mut run = apply_target_changes(deploy, fast_forward_drops)?;
    run.events.applied.splice(0..0, events.applied);
    run.events.skipped.splice(0..0, events.skipped);
    run.events.warnings.splice(0..0, events.warnings);
    // pre_deploy renders after pre_sync, before post_sync/on_change.
    let mut early_hooks = pre_sync_outcomes;
    early_hooks.extend(run.pre_deploy);
    if run.aborted {
        // An abort gate short-circuits like pre_sync: no prune, no further hook phases.
        return Ok(aborted_before_deploy_phase(
            base_lock,
            local_lock,
            early_hooks,
            run.events,
        ));
    }
    let mut had_failures = run.had_failures;
    let registry: &dyn StateStore = deploy.registry;
    if !deploy.input.prune() {
        notify_orphans(deploy.config, registry, &mut run.events)?;
    }

    let (hook_results, stripped_transitive_hooks) = run_all_hooks(
        deploy.input,
        deploy.config,
        registry,
        &mut base_lock,
        hook_candidates,
        effective_lock,
        early_hooks,
    )?;
    if stripped_transitive_hooks > 0 {
        run.events
            .warnings
            .push(SyncWarning::UntrustedTransitiveHooks {
                count: stripped_transitive_hooks,
            });
    }
    let deploy_failures = had_failures;
    had_failures |= hook_results
        .iter()
        .any(|o| o.status == hooks::HookStatus::Failure);

    Ok(SyncExecution {
        report: SyncReport {
            locks: LockSet {
                base: Some(base_lock),
                local: local_lock,
            },
            changes: run.changes,
            applied: run.events.applied,
            skipped: run.events.skipped,
            warnings: run.events.warnings,
            hook_outcomes: hook_results,
            status: if had_failures {
                SyncStatus::Failed
            } else {
                SyncStatus::Success
            },
        },
        deploy_failures,
        stripped_transitive_hooks,
    })
}

/// Short-circuit shared by a failed `pre_sync` gate and a `pre_deploy` abort. Both return before
/// planned drops or ordinary target changes are applied; hook side effects remain external.
/// `deploy_failures` is false because a gate failure is a hook failure, not a deploy failure.
fn aborted_before_deploy_phase(
    base_lock: Lock,
    local_lock: Option<Lock>,
    hook_results: Vec<hooks::HookOutcome>,
    events: SyncEvents,
) -> SyncExecution {
    SyncExecution {
        report: SyncReport {
            locks: LockSet {
                base: Some(base_lock),
                local: local_lock,
            },
            changes: model::ChangeSet::default(),
            applied: events.applied,
            skipped: events.skipped,
            warnings: events.warnings,
            hook_outcomes: hook_results,
            status: SyncStatus::Failed,
        },
        deploy_failures: false,
        stripped_transitive_hooks: 0,
    }
}

fn run_pre_sync(input: &SyncRunInput<'_>, config: &Config) -> Result<Vec<hooks::HookOutcome>> {
    if !input.hooks_enabled() {
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
    input: &dyn RunOptions,
    config: &Config,
    registry: &dyn StateStore,
    base_lock: &mut Lock,
    hook_candidates: &[transitive::TransitiveHookCandidate],
    effective_lock: Option<&Lock>,
    early_hooks: Vec<hooks::HookOutcome>,
) -> Result<(Vec<hooks::HookOutcome>, usize)> {
    // pre_sync + pre_deploy render before post_sync/on_change, so they seed the result vec.
    let mut hook_results = early_hooks;
    if input.hooks_enabled() {
        hook_results.append(&mut hooks::dispatch_hooks(
            config,
            registry,
            input.lockless(),
        )?);
    }
    let mut stripped = 0;
    if input.transitive_hooks_enabled() {
        let mut decision = decide_transitive_hooks(
            base_lock,
            hook_candidates,
            effective_lock,
            input.interactive(),
        )?;
        stripped = decision.stripped;
        hook_results.append(&mut decision.outcomes);
    }
    Ok((hook_results, stripped))
}

struct BindingOffer<'projection> {
    selection: crate::projection::offer::OfferSelection,
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
    fn classify(record: &ArtifactRecord, offer: &BindingOffer<'_>) -> Self {
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
    recorded: &[ArtifactRecord],
    fast_forward: bool,
) -> Result<Vec<ArtifactRecord>> {
    use crate::projection::offer::OfferSelection;

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
    record: &ArtifactRecord,
) -> Result<BTreeSet<String>> {
    let artifact = record.key.artifact.as_str();
    let mut paths = BTreeSet::from([artifact.to_owned()]);
    if !record.linked && record.kind == crate::sync::state::RecordKind::Dir {
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

fn apply_fast_forward_drops(
    drops: &[FastForwardDrop],
    registry: &dyn StateStore,
    skipped_targets: &BTreeSet<String>,
    events: &mut SyncEvents,
) -> Result<()> {
    for drop in drops {
        let record = drop.record();
        if skipped_targets.contains(&record.key.target) {
            continue;
        }
        match drop {
            FastForwardDrop::KeepLive { path, .. } => {
                events.warnings.push(SyncWarning::FastForwardKeptLive {
                    source: record.key.source.clone(),
                    artifact: record.key.artifact.clone(),
                    path: path.clone(),
                });
            }
            FastForwardDrop::Remove { path, .. } => {
                events.warnings.push(SyncWarning::FastForwardDropped {
                    source: record.key.source.clone(),
                    artifact: record.key.artifact.clone(),
                });
                remove_orphan_path(path).map_err(|error| {
                    Error::Sync(format!("fast-forward prune {}: {error}", path.display()))
                })?;
            }
        }
        registry.remove_artifact(&record.key)?;
    }
    Ok(())
}

#[cfg(test)]
fn prune_fast_forward_drops_report(
    projection: &Projection,
    config: &Config,
    registry: &dyn StateStore,
    protected: &confine::ProtectedPathSet,
    drops: &[ArtifactRecord],
    lockless: bool,
    events: &mut SyncEvents,
) -> Result<()> {
    let plan = plan_fast_forward_drops(
        projection,
        config,
        registry,
        protected,
        drops.to_vec(),
        lockless,
    )?;
    apply_fast_forward_drops(&plan, registry, &BTreeSet::new(), events)
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

fn live_recorded_artifacts(registry: &dyn StateStore) -> Result<Vec<ArtifactRecord>> {
    let recorded = registry.all_artifacts()?;
    let ejected = crate::sync::state::ejected_index(registry, &recorded)?;
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

fn collect_ref_transitions(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    effective_lock: Option<&Lock>,
    resolved_commits: &BTreeMap<(String, String), String>,
    events: &mut SyncEvents,
) {
    for transition in ref_transitions(config, parsed, effective_lock, resolved_commits) {
        events.warnings.push(SyncWarning::ReferenceMoved {
            source: transition.source,
            target: transition.target,
            from: transition.from,
            to: transition.to,
        });
    }
}

#[derive(Debug)]
struct RefTransition {
    source: String,
    target: String,
    from: String,
    to: String,
}

#[cfg(test)]
fn ref_transition_lines(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    effective_lock: Option<&Lock>,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Vec<String> {
    ref_transitions(config, parsed, effective_lock, resolved_commits)
        .into_iter()
        .map(|transition| {
            format!(
                "phora: {} → {}: {} → {}",
                transition.source, transition.target, transition.from, transition.to
            )
        })
        .collect()
}

fn ref_transitions(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    effective_lock: Option<&Lock>,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Vec<RefTransition> {
    let mut transitions = Vec::new();
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
            transitions.push(RefTransition {
                source: binding.source.to_owned(),
                target: target_name.clone(),
                from,
                to,
            });
        }
    }
    transitions
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
    backend: &dyn SourceStore,
    lock: Option<&Lock>,
) {
    if let Ok(graph) = transitive::resolve_transitive_graph(config, parsed, backend, true, lock) {
        graph.inject(config, parsed, remotes);
    }
}

#[derive(Debug)]
struct LinkModeWarning {
    source: String,
    path: PathBuf,
}

impl LinkModeWarning {
    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.source.contains(needle) || self.path.to_string_lossy().contains(needle)
    }
}

fn validate_link_mode(
    base: &Config,
    effective: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
) -> Result<Vec<LinkModeWarning>> {
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
            warnings.push(LinkModeWarning {
                source: name.clone(),
                path: PathBuf::from(git),
            });
        }
    }
    Ok(warnings)
}

/// Removes a half-exported `staging` dir on drop unless [`disarm`](StagingGuard::disarm)
/// hands cleanup to [`apply::apply_artifact`] on the success path.
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
    registry: &dyn StateStore,
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
    if registry.artifact(&key)?.is_none() {
        return Err(Error::StateStore(format!(
            "{source}/{artifact} is not managed in target {target}"
        )));
    }

    let mut ejected = registry.ejections(target)?;
    let already = ejected
        .iter()
        .any(|e| e.source == source && e.artifact == artifact);
    if !already {
        ejected.push(Ejection {
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            ejected_at: chrono::Utc::now().to_rfc3339(),
        });
        registry.save_ejections(target, &ejected)?;
    }
    // Record kept (not removed): list/where render `ejected` from it, and uneject restores by clearing the entry alone.
    Ok(())
}

pub fn uneject(
    config: &Config,
    registry: &dyn StateStore,
    artifact: &str,
    source: &str,
    target: &str,
) -> Result<()> {
    if !config.targets.contains_key(target) {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    let mut ejected = registry.ejections(target)?;
    ejected.retain(|e| !(e.source == source && e.artifact == artifact));
    Ok(registry.save_ejections(target, &ejected)?)
}
