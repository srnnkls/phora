//! Registry-free, network-free projection orchestration shared by sync, prune, and preview.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Config, DeployMode, ParsedSource, Target};
use crate::error::{Error, Result};
use crate::kernel::SourceName;
use crate::lock::encode_ref;
use crate::source::{SourceBackend, SourceInventory};

use super::discover::discover_working_tree_leaves;
use super::remote_for;

use crate::projection::build::{build_workspace, project_target};
use crate::projection::model::{
    BindingProjectionInput, CollapsePreference, LayoutSpec, MaterializationPolicy, OfferSpec,
    Projection, ResolvedSourceRef, TakeSpec, TargetProjection, TemplatePolicy,
    WorkspaceTargetInput,
};

/// Projects one target's deployments: registry-free and network-free, discovering each
/// binding's candidate leaves via the source seam and projecting the leaf-granular
/// structure, taking resolved commits as a precondition; it never fetches or writes.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
pub fn plan_target(
    target_name: &str,
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<TargetProjection> {
    let discovery = discover_target(target, parsed, remotes, backend, resolved_commits)?;
    Ok(project_target(target_name, &binding_inputs(&discovery))?)
}

struct TargetDiscovery {
    layout: LayoutSpec,
    discovered: Vec<DiscoveredBinding>,
}

fn discover_target(
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<TargetDiscovery> {
    let layout = LayoutSpec::from(&target.layout());
    let mut discovered = Vec::new();
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
        let commit = resolved_commits
            .get(&commit_key)
            .ok_or_else(|| {
                Error::Sync(format!(
                    "no resolved commit for {} at {}",
                    binding.source, binding.effective_ref
                ))
            })?
            .clone();
        let name = SourceName::trusted(binding.source);
        let leaves = discover_binding_leaves(source, &name, &commit, remotes, backend)?;
        discovered.push(DiscoveredBinding {
            identity: binding.identity.to_owned(),
            source: ResolvedSourceRef::new(binding.source, commit),
            inventory: SourceInventory::from_paths(leaves)?,
            offer: OfferSpec::from(source.offer()),
            take: TakeSpec::from_entries(binding.take),
            materialization: MaterializationPolicy::from(&source.deploy_mode()),
            collapse: CollapsePreference::from(binding.collapse),
            templates: TemplatePolicy::from(&binding.template_opt_in),
        });
    }
    Ok(TargetDiscovery { layout, discovered })
}

fn binding_inputs(discovery: &TargetDiscovery) -> Vec<BindingProjectionInput<'_>> {
    discovery
        .discovered
        .iter()
        .map(|d| BindingProjectionInput {
            identity: &d.identity,
            source: &d.source,
            offer: &d.offer,
            inventory: &d.inventory,
            take: &d.take,
            collapse: d.collapse,
            materialization: d.materialization,
            layout: &discovery.layout,
            templates: &d.templates,
        })
        .collect()
}

struct DiscoveredBinding {
    identity: String,
    source: ResolvedSourceRef,
    inventory: SourceInventory,
    offer: OfferSpec,
    take: TakeSpec,
    materialization: MaterializationPolicy,
    collapse: CollapsePreference,
    templates: TemplatePolicy,
}

/// Every candidate leaf one binding offers at `commit`, unfiltered: the offer's
/// own include/exclude is applied downstream by `OfferSelection`.
fn discover_binding_leaves(
    source: &ParsedSource,
    source_name: &SourceName,
    commit: &str,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
) -> Result<Vec<String>> {
    let git = remote_for(remotes, source_name.as_str())?;
    match source.deploy_mode() {
        DeployMode::Link => Ok(discover_working_tree_leaves(Path::new(git), None)?),
        DeployMode::Copy => Ok(backend.list_source_leaves(source_name, git, commit, None)?),
    }
}

/// Projects every target in `config`, forwarding to `plan_target` for each.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
#[must_use = "a projection describes deployments but performs none; consume the returned Projection"]
pub fn project_workspace(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<Projection> {
    let discoveries = config
        .targets
        .iter()
        .map(|(name, target)| {
            Ok((
                name.as_str(),
                discover_target(target, parsed, remotes, backend, resolved_commits)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let workspace: Vec<WorkspaceTargetInput<'_>> = discoveries
        .iter()
        .map(|(name, discovery)| WorkspaceTargetInput {
            target: name,
            bindings: binding_inputs(discovery),
        })
        .collect();
    Ok(build_workspace(&workspace)?)
}
