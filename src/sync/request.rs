//! Public request and report values for a synchronization run.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use crate::config::Config;
use crate::lock::Lock;
use crate::projection::diagnostic::ProjectionWarning;

use super::ConflictResolver;
use super::hooks::HookOutcome;
use super::model::{ChangeSet, ConflictKind, ReconciliationPolicy, RemovalReason};
use super::progress::{self, ProgressSink};

/// The base and optional local lock carried into or returned from synchronization.
#[derive(Debug, Clone, Default)]
pub struct LockSet {
    pub base: Option<Lock>,
    pub local: Option<Lock>,
}

/// One unpinned transitive hook awaiting a trust decision.
#[derive(Debug, Clone)]
pub struct TrustRequest<'a> {
    pub dep_instance: &'a str,
    pub hook_id: &'a str,
    pub command: String,
}

/// Decides whether an unpinned transitive hook may run.
pub trait TrustPrompt: Sync {
    fn confirm(&self, request: &TrustRequest<'_>) -> bool;
}

/// Declines every unpinned transitive hook.
pub struct DeclineAll;

impl TrustPrompt for DeclineAll {
    fn confirm(&self, _request: &TrustRequest<'_>) -> bool {
        false
    }
}

/// All caller-controlled inputs to a synchronization run.
pub struct SyncRequest<'a> {
    pub base_config: &'a Config,
    pub local_config: Option<&'a Config>,
    pub locks: LockSet,
    pub options: SyncOptions,
    pub resolver: Option<&'a dyn ConflictResolver>,
    pub sink: &'a dyn ProgressSink,
    pub trust_prompt: Option<&'a dyn TrustPrompt>,
}

/// Explicit policies controlling synchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncOptions {
    pub source_policy: SourcePolicy,
    pub conflict_policy: ConflictPolicy,
    pub prune_policy: PrunePolicy,
    pub hook_policy: HookPolicy,
    pub moved_pin_policy: MovedPinPolicy,
    pub concurrency: Concurrency,
}

/// How source snapshots are selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePolicy {
    Locked,
    Refresh,
    Frozen,
}

/// How desired changes that collide with local content are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Refuse,
    ResolveInteractively,
    Overwrite,
}

/// Whether managed records absent from the projection are retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrunePolicy {
    KeepOrphans,
    RemoveOrphans,
}

/// Which hook families participate in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPolicy {
    All,
    NoTransitive,
    None,
}

/// How a committed source pin that dropped an offered artifact is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedPinPolicy {
    Seal,
    FastForward,
}

/// Optional fixed worker count; `None` derives the pool size from the work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Concurrency {
    pub jobs: Option<NonZeroUsize>,
}

impl From<&SyncOptions> for ReconciliationPolicy {
    fn from(options: &SyncOptions) -> Self {
        Self {
            force: matches!(options.conflict_policy, ConflictPolicy::Overwrite),
            prune: matches!(options.prune_policy, PrunePolicy::RemoveOrphans),
            follow_moved_pin: matches!(options.moved_pin_policy, MovedPinPolicy::FastForward),
        }
    }
}

/// One state mutation completed by the normal workspace apply pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedChange {
    Deployed {
        target: String,
        source: String,
        artifact: String,
    },
    Overwritten {
        target: String,
        source: String,
        artifact: String,
    },
    OverlayRewritten {
        target: String,
        source: String,
        artifact: String,
    },
    Ejected {
        target: String,
        source: String,
        artifact: String,
    },
    Removed {
        target: String,
        source: String,
        artifact: String,
        reason: RemovalReason,
    },
}

/// One reconciled change deliberately left unapplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkippedChange {
    Conflict {
        target: String,
        source: String,
        artifact: String,
        kind: ConflictKind,
    },
    Failed {
        target: String,
        source: String,
        artifact: String,
        message: String,
    },
    ReadonlyOverlayRewrite {
        target: String,
        source: String,
        artifact: String,
    },
}

/// A non-fatal diagnostic returned to the rendering boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncWarning {
    Projection(ProjectionWarning),
    MalformedTransitiveHooks {
        target: String,
        detail: String,
    },
    LinkPathNotPortable {
        source: String,
        path: PathBuf,
    },
    ReferenceMoved {
        source: String,
        target: String,
        from: String,
        to: String,
    },
    OrphanedRecords {
        count: usize,
    },
    PruneSkippedAfterFailures,
    PruneRefused {
        path: PathBuf,
        reason: String,
    },
    OrphanRecordPathUnknown {
        source: String,
        artifact: String,
        layout: String,
    },
    FastForwardKeptLive {
        source: String,
        artifact: String,
        path: PathBuf,
    },
    FastForwardDropped {
        source: String,
        artifact: String,
    },
    CrossDeviceFallback {
        destination: PathBuf,
    },
    ConflictModified {
        source: String,
        artifact: String,
        changed: Vec<PathBuf>,
    },
    ConflictForeign {
        path: PathBuf,
    },
    UntrustedTransitiveHooks {
        count: usize,
    },
    HistoryContentFilter {
        source: String,
        attributes: bool,
        autocrlf: bool,
    },
    /// A rebuild failed; the source keeps deploying its previous output.
    BuildFailed {
        source: String,
        detail: String,
    },
}

/// Overall synchronization outcome used by callers to choose an exit code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyncStatus {
    #[default]
    Success,
    Failed,
}

/// Complete synchronization result; rendering and exit mapping happen at the CLI edge.
#[derive(Debug)]
pub struct SyncReport {
    pub locks: LockSet,
    pub changes: ChangeSet,
    pub applied: Vec<AppliedChange>,
    pub skipped: Vec<SkippedChange>,
    pub warnings: Vec<SyncWarning>,
    pub hook_outcomes: Vec<HookOutcome>,
    pub status: SyncStatus,
}

/// Accumulates the run's report and forwards each entry to the observation port.
pub(super) struct SyncEvents<'a> {
    sink: &'a dyn ProgressSink,
    pub(super) applied: Vec<AppliedChange>,
    pub(super) skipped: Vec<SkippedChange>,
    pub(super) warnings: Vec<SyncWarning>,
    pub(super) unchanged: usize,
}

impl std::fmt::Debug for SyncEvents<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncEvents")
            .field("applied", &self.applied)
            .field("skipped", &self.skipped)
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl<'a> SyncEvents<'a> {
    pub(super) fn new(sink: &'a dyn ProgressSink) -> Self {
        Self {
            sink,
            applied: Vec::new(),
            skipped: Vec::new(),
            warnings: Vec::new(),
            unchanged: 0,
        }
    }

    /// For a walk whose report is thrown away; observers must not see it either.
    pub(super) fn discarding() -> SyncEvents<'static> {
        SyncEvents::new(progress::SILENT)
    }

    pub(super) fn sink(&self) -> &'a dyn ProgressSink {
        self.sink
    }

    pub(super) fn push_applied(&mut self, change: AppliedChange) {
        self.sink.artifact_applied(&change);
        self.applied.push(change);
    }

    pub(super) fn push_skipped(&mut self, change: SkippedChange) {
        self.sink.artifact_skipped(&change);
        self.skipped.push(change);
    }

    pub(super) fn push_warning(&mut self, warning: SyncWarning) {
        self.sink.warning(&warning);
        self.warnings.push(warning);
    }

    pub(super) fn push_unchanged(&mut self, artifact: &progress::ArtifactId) {
        self.sink.artifact_unchanged(artifact);
        self.unchanged += 1;
    }
}
