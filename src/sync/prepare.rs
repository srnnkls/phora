//! A preparation boundary inside one synchronization, sharing its registry and journal.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{Config, Target, TargetPhase};
use crate::error::{Error, Result};
use crate::lock::{Lock, merge_locks};
use crate::source::SourceStore;

use super::progress::{
    ArtifactId, FetchId, FetchOutcome, Phase, ProgressSink, Severity, SyncSummary,
};
use super::request::SyncEvents;
use super::state::StateStore;
use super::{
    AppliedChange, HookOutcome, HookStatus, LockSet, RunOptions, SkippedChange, SourcePolicy,
    SyncExecution, SyncRunInput, SyncStatus, SyncWarning, SyncWorkspace, confine, hooks, summarize,
    sync_workspace,
};

/// Select records for reconciliation while retaining the single persistent state owner.
#[derive(Clone, Default)]
pub(super) enum TargetScope {
    #[default]
    All,
    Prepare(BTreeSet<String>),
    Deploy(BTreeSet<String>),
}

impl TargetScope {
    pub fn contains(&self, target: &str) -> bool {
        match self {
            Self::All => true,
            Self::Prepare(targets) => targets.contains(target),
            Self::Deploy(targets) => !targets.contains(target),
        }
    }

    pub fn select(&self, records: Vec<super::ArtifactRecord>) -> Vec<super::ArtifactRecord> {
        records
            .into_iter()
            .filter(|r| self.contains(&r.key.target))
            .collect()
    }
}

pub(super) struct Preparation {
    roots: Vec<PathBuf>,
    pub sources: BTreeSet<String>,
    deploy_sources: BTreeSet<String>,
}

impl Preparation {
    pub fn new(config: &Config) -> Result<Self> {
        if !config
            .targets
            .values()
            .any(|t| t.phase() == TargetPhase::Prepare)
        {
            return Ok(Self {
                roots: Vec::new(),
                sources: BTreeSet::new(),
                deploy_sources: BTreeSet::new(),
            });
        }
        let cwd = std::env::current_dir()?;
        let mut roots = Vec::new();
        let mut sources = BTreeSet::new();
        let mut deploy_sources = BTreeSet::new();
        let mut destinations = Vec::new();
        for (name, target) in &config.targets {
            let names = target
                .declared_sources()
                .chain(target.imports.iter().flatten().map(|i| i.source.as_str()));
            let physical = confine::fold_path(&confine::normalize_physical(
                &cwd.join(target.expanded_path()),
            )?);
            if target.phase() == TargetPhase::Prepare {
                sources.extend(names.map(str::to_owned));
                roots.push(physical.clone());
            } else {
                deploy_sources.extend(names.map(str::to_owned));
            }
            destinations.push((name, target.phase(), physical));
        }
        for (name, phase, path) in &destinations {
            for (other, other_phase, other_path) in &destinations {
                if phase != other_phase
                    && (path.starts_with(other_path) || other_path.starts_with(path))
                {
                    return Err(Error::Config(format!(
                        "prepare and deploy targets `{name}` and `{other}` overlap; use separate destination trees"
                    )));
                }
            }
        }
        Ok(Self {
            roots,
            sources,
            deploy_sources,
        })
    }

    pub fn enabled(&self, config: &Config) -> bool {
        !self.roots.is_empty()
            || config
                .hooks
                .as_ref()
                .is_some_and(|h| h.post_prepare.is_some())
    }

    fn contains(&self, path: &std::path::Path) -> Result<bool> {
        let path = confine::fold_path(&confine::normalize_physical(
            &std::env::current_dir()?.join(path),
        )?);
        Ok(self.roots.iter().any(|root| path.starts_with(root)))
    }

    fn targets(
        &self,
        workspace: &SyncWorkspace,
        registry: &dyn StateStore,
    ) -> Result<BTreeSet<String>> {
        let mut targets: BTreeSet<_> = workspace
            .config
            .targets
            .iter()
            .filter(|(_, t)| t.phase() == TargetPhase::Prepare)
            .map(|(n, _)| n.clone())
            .collect();
        // Include old composed identities under the same input anchors, so pruning a
        // removed dependency stays in preparation and cannot touch deployment records.
        for record in registry.all_artifacts()? {
            if let Some(root) = record.deploy_root.as_deref()
                && self.contains(std::path::Path::new(root))?
            {
                targets.insert(record.key.target);
            }
        }
        Ok(targets)
    }

    fn partition(&self, workspace: &SyncWorkspace, prepare: bool) -> Result<SyncWorkspace> {
        let mut part = workspace.clone();
        part.config
            .targets
            .retain(|_, t| (t.phase() == TargetPhase::Prepare) == prepare);
        let mut names: BTreeSet<_> = part
            .config
            .targets
            .values()
            .flat_map(Target::declared_sources)
            .map(str::to_owned)
            .collect();
        if prepare {
            names.extend(self.sources.iter().cloned());
            part.config.hooks = None;
        } else {
            names.extend(self.deploy_sources.iter().cloned());
            names.extend(
                workspace
                    .parsed
                    .keys()
                    .filter(|n| !self.sources.contains(*n))
                    .cloned(),
            );
        }
        part.parsed.retain(|n, _| names.contains(n));
        part.import_refs
            .retain(|i| names.contains(&i.source) && (i.phase == TargetPhase::Prepare) == prepare);
        let mut candidates = Vec::new();
        for candidate in part.hook_candidates {
            if self.contains(&candidate.target_path)? == prepare {
                candidates.push(candidate);
            }
        }
        part.hook_candidates = candidates;
        Ok(part)
    }
}

/// Preserve unvisited pins after a preparation failure; replace only entries that resolved.
pub(crate) fn merge_lock_sets(older: &LockSet, newer: &LockSet) -> LockSet {
    fn merge(older: Option<&Lock>, newer: Option<&Lock>) -> Option<Lock> {
        match (older, newer) {
            (Some(old), Some(new)) => {
                let mut merged = merge_locks(old, Some(new));
                for candidate in &new.candidate_hooks {
                    merged.candidate_hooks.retain(|c| {
                        c.dep_instance != candidate.dep_instance || c.hook_id != candidate.hook_id
                    });
                    merged.candidate_hooks.push(candidate.clone());
                }
                Some(merged)
            }
            (Some(lock), None) | (None, Some(lock)) => Some(lock.clone()),
            (None, None) => None,
        }
    }
    LockSet {
        base: merge(older.base.as_ref(), newer.base.as_ref()),
        local: merge(older.local.as_ref(), newer.local.as_ref()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "preparation shares the enclosing run, state, and report accumulator"
)]
pub(super) fn sync<R: StateStore>(
    input: &SyncRunInput<'_>,
    workspace: &SyncWorkspace,
    preparation: &Preparation,
    backend: &dyn SourceStore,
    registry: &R,
    early_hooks: Vec<HookOutcome>,
    events: SyncEvents<'_>,
    started: Instant,
) -> Result<SyncExecution> {
    // Recovery can restore records for old imported identities. Classify those too.
    let journal = super::open_sync_journal(input.lockless, registry)?;
    super::sweep_target_parents(&workspace.config, &journal, registry)?;
    let targets = preparation.targets(workspace, registry)?;
    let mut prepared = preparation.partition(workspace, true)?;
    prepared.target_scope = TargetScope::Prepare(targets.clone());
    let mut deployed = preparation.partition(workspace, false)?;
    deployed.target_scope = TargetScope::Deploy(targets);
    let sink = PhaseSink {
        inner: input.sink(),
        summaries: Mutex::new(Vec::new()),
    };
    let mut phase_input = SyncRunInput {
        sink: &sink,
        locks: input.locks.clone(),
        ..*input
    };
    let mut result = sync_workspace(
        &phase_input,
        prepared,
        backend,
        registry,
        early_hooks,
        events,
        started,
    )?;
    // Ordinary sync can skip conflicts without failing. Preparation must be complete
    // before a generator can consume its inputs.
    if !result.report.skipped.is_empty()
        || result
            .report
            .applied
            .iter()
            .any(|a| matches!(a, AppliedChange::Ejected { .. }))
    {
        result.report.status = SyncStatus::Failed;
        result.deploy_failures = true;
    }
    if result.report.status == SyncStatus::Success && input.hooks_enabled() {
        let outcomes = hooks::dispatch_post_prepare(&super::merged_config(input))?;
        for outcome in &outcomes {
            input.sink().hook_finished(outcome);
        }
        if outcomes.iter().any(|o| o.status == HookStatus::Failure) {
            result.report.status = SyncStatus::Failed;
        }
        result.report.hook_outcomes.extend(outcomes);
    }
    if result.report.status == SyncStatus::Failed {
        result.report.locks = merge_lock_sets(&input.locks, &result.report.locks);
        sink.finish(&result.report, started);
        return Ok(result);
    }
    // A generator may create symlinks. Recheck physical target separation before applying outputs.
    Preparation::new(&super::merged_config(input))?;
    phase_input.locks = if input.refresh_sources() {
        result.report.locks.clone()
    } else {
        merge_lock_sets(&input.locks, &result.report.locks)
    };
    if input.refresh_sources() {
        phase_input.options.source_policy = SourcePolicy::Locked;
    }
    let normal = sync_workspace(
        &phase_input,
        deployed,
        backend,
        registry,
        Vec::new(),
        SyncEvents::new(&sink),
        started,
    )?;
    append_execution(&mut result, normal);
    sink.finish(&result.report, started);
    Ok(result)
}

fn append_execution(result: &mut SyncExecution, normal: SyncExecution) {
    result.report.locks = merge_lock_sets(&result.report.locks, &normal.report.locks);
    result
        .report
        .changes
        .changes
        .extend(normal.report.changes.changes);
    result.report.applied.extend(normal.report.applied);
    result.report.skipped.extend(normal.report.skipped);
    result.report.warnings.extend(normal.report.warnings);
    result
        .report
        .hook_outcomes
        .extend(normal.report.hook_outcomes);
    result.report.status = normal.report.status;
    result.deploy_failures |= normal.deploy_failures;
    result.stripped_transitive_hooks += normal.stripped_transitive_hooks;
    result
        .report
        .warnings
        .retain(|w| !matches!(w, SyncWarning::UntrustedTransitiveHooks { .. }));
    if result.stripped_transitive_hooks > 0 {
        result
            .report
            .warnings
            .push(SyncWarning::UntrustedTransitiveHooks {
                count: result.stripped_transitive_hooks,
            });
    }
}

/// Forward live events, but emit one final summary for the enclosing sync.
struct PhaseSink<'a> {
    inner: &'a dyn ProgressSink,
    summaries: Mutex<Vec<SyncSummary>>,
}

impl PhaseSink<'_> {
    fn finish(&self, report: &super::SyncReport, started: Instant) {
        let summaries = self
            .summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let summary = summarize(
            summaries.iter().map(|s| s.targets).sum(),
            summaries.iter().map(|s| s.unchanged).sum(),
            &report.applied,
            &report.skipped,
            &report.hook_outcomes,
            started.elapsed(),
        );
        self.inner.finished(&summary);
    }
}

impl ProgressSink for PhaseSink<'_> {
    fn phase_started(&self, phase: Phase) {
        self.inner.phase_started(phase);
    }
    fn phase_finished(&self, phase: Phase) {
        self.inner.phase_finished(phase);
    }
    fn resolve_planned(&self, groups: usize) {
        self.inner.resolve_planned(groups);
    }
    fn fetch_started(&self, fetch: &FetchId) {
        self.inner.fetch_started(fetch);
    }
    fn fetch_finished(&self, fetch: &FetchId, outcome: FetchOutcome) {
        self.inner.fetch_finished(fetch, outcome);
    }
    fn artifacts_planned(&self, artifacts: usize) {
        self.inner.artifacts_planned(artifacts);
    }
    fn artifact_applied(&self, change: &AppliedChange) {
        self.inner.artifact_applied(change);
    }
    fn artifact_skipped(&self, change: &SkippedChange) {
        self.inner.artifact_skipped(change);
    }
    fn artifact_unchanged(&self, artifact: &ArtifactId) {
        self.inner.artifact_unchanged(artifact);
    }
    fn hook_started(&self, id: &str) {
        self.inner.hook_started(id);
    }
    fn hook_finished(&self, outcome: &HookOutcome) {
        self.inner.hook_finished(outcome);
    }
    fn warning(&self, warning: &SyncWarning) {
        self.inner.warning(warning);
    }
    fn diagnostic(&self, severity: Severity, message: &str) {
        self.inner.diagnostic(severity, message);
    }
    fn finished(&self, summary: &SyncSummary) {
        self.summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(summary.clone());
    }
    fn aborted(&self, error: &str) {
        self.inner.aborted(error);
    }
}
