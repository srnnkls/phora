use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::TemplateOptIn;
use crate::kernel::{Materialization, SourceName};
use crate::source::{
    ExportLeaf, ExportPolicy, ExportRequest, SourceBackend, SourceEntryKind, SourceError,
};

use super::plan::ProjectedArtifact;
use super::target::leaf_basename;

type Result<T> = std::result::Result<T, SourceError>;

pub(super) struct SourceReadRequest<'a> {
    pub(super) source: &'a SourceName,
    pub(super) url: &'a str,
    pub(super) commit: &'a str,
    pub(super) root: Option<&'a Path>,
    pub(super) policy: &'a ExportPolicy,
    pub(super) scratch_dir: &'a Path,
    pub(super) commit_time: u64,
    pub(super) template_opt_in: &'a TemplateOptIn,
    pub(super) artifact: &'a ProjectedArtifact,
}

pub(super) struct SourceReads {
    by_source: BTreeMap<PathBuf, PathBuf>,
}

impl SourceReads {
    pub(super) fn read(&self, repo_relative: &Path) -> Result<(Vec<u8>, SourceEntryKind)> {
        let staged =
            self.by_source
                .get(repo_relative)
                .ok_or_else(|| SourceError::MappedKeyNotFound {
                    key: repo_relative.to_path_buf(),
                })?;
        let meta = std::fs::symlink_metadata(staged)?;
        if meta.file_type().is_symlink() {
            return Ok((link_target_bytes(staged)?, SourceEntryKind::Symlink));
        }
        let bytes = std::fs::read(staged)?;
        Ok((bytes, blob_kind(&meta)))
    }
}

pub(super) fn stage_source_reads(
    backend: &dyn SourceBackend,
    req: &SourceReadRequest<'_>,
) -> Result<SourceReads> {
    let plan = match &req.artifact.materialization {
        Materialization::CollapsedDir { dir } => {
            collapsed_dir_leaves(dir, &req.artifact.kept_leaves, req.template_opt_in)
        }
        Materialization::Leaf(take) => vec![ExportLeaf {
            source: PathBuf::from(&take.source),
            dest: PathBuf::from(leaf_basename(&take.dest)),
        }],
    };
    let raw_policy = ExportPolicy {
        preserve_executable: true,
        ..req.policy.clone()
    };
    backend.export_artifact(&ExportRequest {
        source: req.source,
        url: req.url,
        commit: req.commit,
        root: req.root,
        policy: &raw_policy,
        staging_dir: req.scratch_dir,
        commit_time: req.commit_time,
        template_opt_in: &TemplateOptIn::Disabled,
        vars: &BTreeMap::new(),
        leaves: &plan,
    })?;
    let by_source = plan
        .into_iter()
        .map(|leaf| {
            let repo_relative = req
                .root
                .map_or_else(|| leaf.source.clone(), |root| root.join(&leaf.source));
            (repo_relative, req.scratch_dir.join(&leaf.dest))
        })
        .collect();
    Ok(SourceReads { by_source })
}

/// The collapsed dir's leaf plan: every kept child staged at its dir-relative deployed
/// name (the child path under `dir/`, run through the template opt-in). The source is the
/// kept leaf's full source path; the dest is dir-relative so staging mirrors the dir.
pub(super) fn collapsed_dir_leaves(
    dir: &str,
    kept_leaves: &[crate::kernel::ResolvedTake],
    template_opt_in: &TemplateOptIn,
) -> Vec<ExportLeaf> {
    let prefix = format!("{dir}/");
    kept_leaves
        .iter()
        .filter_map(|kept| {
            let child = kept.dest.strip_prefix(&prefix)?;
            Some(ExportLeaf {
                source: PathBuf::from(&kept.source),
                dest: PathBuf::from(template_opt_in.deployed_name(child)),
            })
        })
        .collect()
}

#[cfg(unix)]
fn link_target_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::fs::read_link(path)?.into_os_string().into_vec())
}

#[cfg(windows)]
fn link_target_bytes(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read_link(path)?
        .to_string_lossy()
        .into_owned()
        .into_bytes())
}

#[cfg(unix)]
fn blob_kind(meta: &std::fs::Metadata) -> SourceEntryKind {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o111 != 0 {
        SourceEntryKind::Executable
    } else {
        SourceEntryKind::File
    }
}

#[cfg(not(unix))]
fn blob_kind(_meta: &std::fs::Metadata) -> SourceEntryKind {
    SourceEntryKind::File
}
