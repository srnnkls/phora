use std::collections::BTreeMap;

use crate::projection::model::{ArtifactRelativePath, ProjectedArtifact, TargetProjection};

pub struct StageRequest<'a> {
    pub artifact: &'a ProjectedArtifact,
    pub target: &'a TargetProjection,
    pub variables: &'a BTreeMap<String, String>,
}

pub struct StagedArtifact {
    pub files: Vec<StagedFile>,
    pub digest: String,
    pub vars_digest: Option<String>,
}

pub struct StagedFile {
    pub destination: ArtifactRelativePath,
    pub size: u64,
    pub mtime: u64,
    pub blake3: String,
}
