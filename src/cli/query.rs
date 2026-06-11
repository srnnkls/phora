//! Read-only commands over config and registry: `list`, `where`, `check-match`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Config, ParsedSource, Protocol, Target};
use crate::deploy::check_artifact_state;
use crate::error::{Error, Result};
use crate::kernel::Selection;
use crate::store::Registry;

use super::render::{print_listings, state_label};
use super::{load_config, open_project_registry};

pub(super) fn run_list(plan: bool) -> Result<()> {
    let config = load_config()?;
    let registry = open_project_registry()?;
    if plan {
        println!("plan: run `phora sync` to apply pending changes");
        return Ok(());
    }
    let listings = list_statuses(&config, &registry)?;
    print_listings(&listings);
    Ok(())
}

/// Reverse-lookup filter over the registry: every `Some` field is an AND constraint.
#[derive(Debug, Default, Clone)]
pub struct WhereFilter {
    pub digest: Option<String>,
    pub source: Option<String>,
    pub artifact: Option<String>,
    pub commit: Option<String>,
}

impl WhereFilter {
    fn matches(&self, record: &crate::store::RegistryRecord) -> bool {
        let eq = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        eq(&self.digest, &record.digest)
            && eq(&self.source, &record.key.source)
            && eq(&self.artifact, &record.key.artifact)
            && eq(&self.commit, &record.commit)
    }
}

/// One (source, artifact) deployment grouped across the targets it lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhereMatch {
    pub source: String,
    pub artifact: String,
    pub commit: String,
    pub digest: String,
    pub targets: Vec<String>,
}

/// Outcome of debugging include/exclude matching for a path under a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckMatchReport {
    pub artifact_allowed: bool,
    pub path_allowed: bool,
}

/// Filters the registry by the constraints in `filter`, grouping survivors by
/// (source, artifact) and listing the targets each is deployed to.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn where_cmd(registry: &dyn Registry, filter: &WhereFilter) -> Result<Vec<WhereMatch>> {
    let mut groups: BTreeMap<(String, String), WhereMatch> = BTreeMap::new();

    for record in registry.list_all()? {
        if !filter.matches(&record) {
            continue;
        }
        let entry = groups
            .entry((record.key.source.clone(), record.key.artifact.clone()))
            .or_insert_with(|| WhereMatch {
                source: record.key.source.clone(),
                artifact: record.key.artifact.clone(),
                commit: record.commit.clone(),
                digest: record.digest.clone(),
                targets: Vec::new(),
            });
        entry.targets.push(record.key.target.clone());
    }

    Ok(groups
        .into_values()
        .map(|mut m| {
            m.targets.sort();
            m.targets.dedup();
            m
        })
        .collect())
}

/// Reports artifact-level and path-level allow decisions for `path` under `source`.
#[must_use]
pub fn check_match_cmd(source: &ParsedSource, path: &str) -> CheckMatchReport {
    let Ok(selection) = Selection::new(source.includes(), source.excludes()) else {
        return CheckMatchReport {
            artifact_allowed: false,
            path_allowed: false,
        };
    };
    let artifact = path.split('/').next().unwrap_or(path);
    CheckMatchReport {
        artifact_allowed: selection.selects_artifact(artifact),
        path_allowed: selection.selects_path(Path::new(path), false),
    }
}

/// A `phora list` row for one managed artifact under a target: its source, the
/// artifact name, and a human-readable state label (`✓`, `[modified]`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStatus {
    pub source: String,
    pub artifact: String,
    pub state: String,
}

/// `phora list` grouped by target: every managed artifact's status under one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetListing {
    pub target: String,
    pub artifacts: Vec<ArtifactStatus>,
}

/// One `phora source list` row: a source's name, its resolved remote, and refspec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub name: String,
    pub remote: String,
    pub refspec: String,
}

/// `phora source show`: one source's effective remote + refspec, plus every
/// target whose `sources` list deploys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSummary {
    pub name: String,
    pub remote: String,
    pub refspec: String,
    pub targets: Vec<String>,
}

/// A target's `sources = [...]` list; a no-key target resolves to the empty set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceResolution {
    Explicit(Vec<String>),
}

/// One `phora target list` row: a target's name, path, and source-resolution mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRow {
    pub name: String,
    pub path: String,
    pub resolution: SourceResolution,
}

/// `phora target show`: a target's effective config, the source names it binds,
/// and per-artifact deployment state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDetail {
    pub name: String,
    pub path: String,
    pub bound_sources: Vec<String>,
    pub artifacts: Vec<ArtifactStatus>,
}

/// `phora source list`: one row per source over the merged config.
///
/// # Errors
///
/// Returns an error if a source fails to resolve its remote or refspec.
pub fn source_listing(config: &Config) -> Result<Vec<SourceRow>> {
    config
        .parsed_sources()?
        .iter()
        .map(|(name, parsed)| {
            Ok(SourceRow {
                name: name.clone(),
                remote: resolved_remote(config, parsed)?,
                refspec: parsed.refspec().to_string(),
            })
        })
        .collect()
}

fn resolved_remote(config: &Config, parsed: &ParsedSource) -> Result<String> {
    if let Some(url) = parsed.source_url() {
        return Ok(url.to_owned());
    }
    let protocol = parsed
        .protocol()
        .or(config.protocol)
        .unwrap_or(Protocol::Https);
    parsed.resolved_remote(&config.hosts, protocol)
}

/// Every target with a binding whose underlying source is `name`.
#[must_use]
pub fn targets_receiving(config: &Config, name: &str) -> Vec<String> {
    let mut receiving: Vec<String> = config
        .targets
        .iter()
        .filter(|(_, target)| target.declared_sources().any(|source| source == name))
        .map(|(target_name, _)| target_name.clone())
        .collect();
    receiving.sort();
    receiving.dedup();
    receiving
}

fn binding_identities(target: &Target) -> Vec<String> {
    target
        .sources
        .iter()
        .flatten()
        .map(|binding| binding.identity().to_owned())
        .collect()
}

/// `phora source show`: effective source config + targets that deploy it.
///
/// # Errors
///
/// Returns an error if `name` is not defined in the merged config.
pub fn source_summary(config: &Config, name: &str) -> Result<SourceSummary> {
    let source = config
        .sources
        .get(name)
        .ok_or_else(|| Error::Config(format!("source `{name}` is not defined")))?;
    let parsed = ParsedSource::parse(name, source)?;
    Ok(SourceSummary {
        name: name.to_owned(),
        remote: resolved_remote(config, &parsed)?,
        refspec: parsed.refspec().to_string(),
        targets: targets_receiving(config, name),
    })
}

/// `phora target list`: one row per target with its source-resolution mode.
#[must_use]
pub fn target_listing(config: &Config) -> Vec<TargetRow> {
    config
        .targets
        .iter()
        .map(|(name, target)| TargetRow {
            name: name.clone(),
            path: target.path.to_string_lossy().into_owned(),
            resolution: SourceResolution::Explicit(binding_identities(target)),
        })
        .collect()
}

/// `phora target show`: effective target config, resolved bound sources, and
/// per-artifact deployment state.
///
/// # Errors
///
/// Returns an error if `name` is not defined, or on-disk state cannot be read.
pub fn target_detail(config: &Config, registry: &dyn Registry, name: &str) -> Result<TargetDetail> {
    let target = config
        .targets
        .get(name)
        .ok_or_else(|| Error::Config(format!("target `{name}` is not defined")))?;
    let bound_sources = binding_identities(target);
    Ok(TargetDetail {
        name: name.to_owned(),
        path: target.path.to_string_lossy().into_owned(),
        bound_sources,
        artifacts: target_artifact_statuses(name, target, registry)?,
    })
}

/// Whether the registry still holds deployed records for `target` — the warning
/// predicate for `phora target rm`.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn target_has_deployed_artifacts(registry: &dyn Registry, target: &str) -> Result<bool> {
    Ok(!registry.list_target(target)?.is_empty())
}

/// Registry-driven `phora list`: per target, the status of every managed artifact,
/// computed via [`check_artifact_state`](crate::deploy::check_artifact_state).
///
/// # Errors
///
/// Returns an error if the registry or on-disk targets cannot be read.
pub fn list_statuses(config: &Config, registry: &dyn Registry) -> Result<Vec<TargetListing>> {
    config
        .targets
        .iter()
        .map(|(target_name, target)| {
            Ok(TargetListing {
                target: target_name.clone(),
                artifacts: target_artifact_statuses(target_name, target, registry)?,
            })
        })
        .collect()
}

fn target_artifact_statuses(
    target_name: &str,
    target: &crate::config::Target,
    registry: &dyn Registry,
) -> Result<Vec<ArtifactStatus>> {
    let ejected = registry.load_ejected(target_name)?;
    let mut artifacts = Vec::new();
    for rec in registry.list_target(target_name)? {
        let artifact_dst = target.expanded_path().join(
            target
                .layout()
                .artifact_path(&rec.key.source, &rec.key.artifact),
        );
        let state = check_artifact_state(
            &artifact_dst,
            &rec.key.source,
            &rec.commit,
            &ejected,
            &rec.key.artifact,
            registry,
            &rec.key,
        )?;
        artifacts.push(ArtifactStatus {
            source: rec.key.source,
            artifact: rec.key.artifact,
            state: state_label(&state).to_owned(),
        });
    }
    Ok(artifacts)
}
