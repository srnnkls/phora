use std::path::PathBuf;

/// Filesystem scan entry: stat metadata only, no content hash.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedProjectState {
    pub artifacts: Vec<ObservedArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedArtifact {
    pub condition: ManagedCondition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedCondition {
    Clean,
    MetadataChangedButContentClean,
    Outdated,
    Modified,
    Linked,
}
