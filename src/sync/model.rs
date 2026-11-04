use std::path::PathBuf;

/// Filesystem scan entry: stat metadata only, no content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedProjectState<R = ()> {
    pub artifacts: Vec<ObservedEntry<R>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedEntry<R = ()> {
    pub target: String,
    pub source: String,
    pub artifact: String,
    pub observation: ObservedArtifact<R>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedArtifact<R = ()> {
    Missing,
    Managed(ManagedArtifact<R>),
    Foreign(PathBuf),
    Ejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedArtifact<R = ()> {
    pub record: R,
    pub condition: ManagedCondition,
    pub overlay_stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedCondition {
    Clean,
    MetadataChangedButContentClean {
        refreshed: Vec<ScannedFile>,
    },
    Outdated,
    Modified {
        changed: Vec<PathBuf>,
    },
    Linked,
    /// A recorded link whose path now holds something phora did not deploy.
    Replaced,
    /// A recorded link that now points somewhere other than its recorded target.
    Repointed,
}

/// What kind of conflict surfaced at an artifact destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    Modified { changed: Vec<PathBuf> },
    Foreign,
}

/// How a deployed artifact phora was about to remove differs from what phora deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalChange {
    Modified { changed: Vec<PathBuf> },
    Replaced,
    Repointed,
}

impl ManagedCondition {
    /// The local change an orphan observation recorded, if any.
    #[must_use]
    pub fn to_local_change(&self) -> Option<LocalChange> {
        match self {
            Self::Modified { changed } => Some(LocalChange::Modified {
                changed: changed.clone(),
            }),
            Self::Replaced => Some(LocalChange::Replaced),
            Self::Repointed => Some(LocalChange::Repointed),
            Self::Clean
            | Self::MetadataChangedButContentClean { .. }
            | Self::Outdated
            | Self::Linked => None,
        }
    }
}

impl From<LocalChange> for ManagedCondition {
    fn from(change: LocalChange) -> Self {
        match change {
            LocalChange::Modified { changed } => Self::Modified { changed },
            LocalChange::Replaced => Self::Replaced,
            LocalChange::Repointed => Self::Repointed,
        }
    }
}

impl LocalChange {
    #[must_use]
    pub fn to_conflict_kind(&self) -> ConflictKind {
        match self {
            Self::Modified { changed } => ConflictKind::Modified {
                changed: changed.clone(),
            },
            Self::Replaced | Self::Repointed => ConflictKind::Foreign,
        }
    }
}

/// Why phora was removing an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalCause {
    UpstreamDrop,
    Prune,
}

impl RemovalCause {
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::UpstreamDrop => "removed upstream",
            Self::Prune => "no longer selected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncChange {
    Deploy {
        target: String,
        source: String,
        artifact: String,
    },
    Overwrite {
        target: String,
        source: String,
        artifact: String,
    },
    RewriteOverlay {
        target: String,
        source: String,
        artifact: String,
    },
    Conflict {
        target: String,
        source: String,
        artifact: String,
        kind: ConflictKind,
    },
    /// A removal of an artifact that changed locally; it waits on a conflict decision.
    RemoveConflict {
        target: String,
        source: String,
        artifact: String,
        change: LocalChange,
        cause: RemovalCause,
    },
    Remove {
        target: String,
        source: String,
        artifact: String,
        reason: RemovalReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalReason {
    Pruned,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub changes: Vec<SyncChange>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconciliationPolicy {
    pub force: bool,
    pub prune: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    UnmatchedArtifact {
        target: String,
        source: String,
        artifact: String,
    },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::UnmatchedArtifact {
                target,
                source,
                artifact,
            } => write!(
                f,
                "no observed state for desired artifact {source}/{artifact} in target {target}"
            ),
        }
    }
}

impl std::error::Error for SyncError {}
