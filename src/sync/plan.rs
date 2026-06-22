//! Registry-free, network-free plan builder shared by sync, prune, and preview.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, ParsedSource, Target};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Selection, SourceName};
use crate::lock::encode_ref;
use crate::source::SourceBackend;

use super::discover::discover_artifacts_for_source;
use super::remote_for;

/// Discover one binding's artifacts at `commit` under `root`, using the binding's
/// effective `include`/`exclude` so plan/preview/prune discover the same set the
/// deploy path (`deploy_target`) would deploy.
///
/// # Errors
/// Errors if the source has no resolved remote, the selection is invalid, or discovery fails.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the deploy path's discovery inputs (source + binding selection + remote)"
)]
pub(super) fn discover_binding(
    source: &ParsedSource,
    source_name: &SourceName,
    commit: &str,
    include: &[String],
    exclude: &[String],
    root: Option<&Path>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
) -> Result<Vec<ArtifactName>> {
    let git = remote_for(remotes, source_name.as_str())?;
    let selection = Selection::new(include, exclude)?;
    discover_artifacts_for_source(source, git, source_name, commit, backend, &selection, root)
}

/// One target's deployment plan: every artifact destined for it under the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPlan {
    pub target: String,
    pub entries: Vec<PlanEntry>,
}

/// A single artifact's deployment: its binding identity and underlying source,
/// commit, and computed destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub identity: String,
    pub source: String,
    pub artifact: String,
    pub commit: String,
    /// Layout-computed path under the target where the artifact deploys.
    pub destination: PathBuf,
    /// True when projected from a binding's `map` (explicit key→dest, no discovery or layout).
    pub mapped: bool,
}

/// Plan one target's deployments: registry-free and network-free, reading discovered
/// artifacts and computing destinations via the layout, taking resolved commits as a
/// precondition; it never fetches or writes.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
#[must_use = "a plan describes deployments but performs none; consume the returned TargetPlan"]
pub fn plan_target(
    target_name: &str,
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<TargetPlan> {
    let path = target.expanded_path();
    let layout = target.layout();
    let mut entries = Vec::new();

    for binding in target.resolve_sources(parsed) {
        let source = parsed.get(binding.source).ok_or_else(|| {
            Error::Config(format!(
                "target references undefined source: {}",
                binding.source
            ))
        })?;
        let commit_key = (
            binding.source.to_owned(),
            encode_ref(&binding.effective_ref),
        );
        let commit = resolved_commits.get(&commit_key).ok_or_else(|| {
            Error::Sync(format!(
                "no resolved commit for {} at {}",
                binding.source, binding.effective_ref
            ))
        })?;
        let name = SourceName::trusted(binding.source);
        let discovered = discover_binding(
            source,
            &name,
            commit,
            binding.include,
            binding.exclude,
            binding.root,
            remotes,
            backend,
        )?;

        for artifact in discovered {
            entries.push(PlanEntry {
                identity: binding.identity.to_owned(),
                source: binding.source.to_owned(),
                artifact: artifact.as_str().to_owned(),
                commit: commit.clone(),
                destination: path.join(layout.artifact_path(binding.identity, artifact.as_str())),
                mapped: false,
            });
        }
    }

    Ok(TargetPlan {
        target: target_name.to_owned(),
        entries,
    })
}

/// Plan every target in `config`, forwarding to `plan_target` for each.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
#[must_use = "a plan describes deployments but performs none; consume the returned TargetPlans"]
pub fn plan_targets(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<Vec<TargetPlan>> {
    config
        .targets
        .iter()
        .map(|(name, target)| plan_target(name, target, parsed, remotes, backend, resolved_commits))
        .collect()
}
