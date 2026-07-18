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
    pub artifacts: Vec<ObservedArtifact<R>>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedCondition {
    Clean,
    MetadataChangedButContentClean { refreshed: Vec<ScannedFile> },
    Outdated,
    Modified { changed: Vec<PathBuf> },
    Linked,
}

/// What kind of conflict surfaced at an artifact destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    Modified { changed: Vec<PathBuf> },
    Foreign,
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
    Conflict {
        target: String,
        source: String,
        artifact: String,
        kind: ConflictKind,
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
    MovedPin,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub changes: Vec<SyncChange>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconciliationPolicy {
    pub force: bool,
    pub prune: bool,
    pub follow_moved_pin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    Unmatched {
        target: String,
        source: String,
        artifact: String,
    },
}
