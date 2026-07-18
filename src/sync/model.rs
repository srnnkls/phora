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
