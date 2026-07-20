//! Public request and report values for a synchronization run.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use crate::config::Config;
use crate::lock::Lock;
use crate::projection::diagnostic::ProjectionWarning;

use super::ConflictResolver;
use super::hooks::HookOutcome;
use super::model::{ChangeSet, ConflictKind, ReconciliationPolicy, RemovalReason};

/// The base and optional local lock carried into or returned from synchronization.
#[derive(Debug, Clone, Default)]
pub struct LockSet {
    pub base: Option<Lock>,
    pub local: Option<Lock>,
}

/// All caller-controlled inputs to a synchronization run.
pub struct SyncRequest<'a> {
    pub base_config: &'a Config,
    pub local_config: Option<&'a Config>,
    pub locks: LockSet,
    pub options: SyncOptions,
    pub resolver: Option<&'a dyn ConflictResolver>,
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

#[derive(Debug, Default)]
pub(super) struct SyncEvents {
    pub(super) applied: Vec<AppliedChange>,
    pub(super) skipped: Vec<SkippedChange>,
    pub(super) warnings: Vec<SyncWarning>,
}
