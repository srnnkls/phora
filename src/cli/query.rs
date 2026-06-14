//! Read-only commands over config and registry: `list`, `where`, `check-match`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Config, ParsedSource, Protocol, SourceFields, Target, merge_configs};
use crate::deploy::check_artifact_state;
use crate::error::{Error, Result};
use crate::kernel::Selection;
use crate::lock::{Lock, merge_locks};
use crate::paths::cache_root;
use crate::source::SourceBackend;
use crate::store::Registry;
use crate::sync::{PreviewTargetPlan, preview_targets, resolved_remotes};

use super::render::{print_listings, render_preview_json, render_preview_tree, state_label};
use super::{build_router, load_config, load_local_config, load_locks, open_project_registry};

pub(super) fn run_list(plan: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = merge_configs(load_config()?, load_local_config(&cwd)?);
    let registry = open_project_registry()?;
    if plan {
        println!("plan: run `phora sync` to apply pending changes");
        return Ok(());
    }
    let listings = list_statuses(&config, &registry)?;
    print_listings(&listings);
    Ok(())
}

pub(super) fn run_preview(sel: &PreviewSelectors, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let config = merge_configs(base, local);

    let parsed = config.parsed_sources()?;
    let remotes = resolved_remotes(&config, &parsed)?;
    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = base_lock.map_or_else(
        || local_lock.clone(),
        |base| Some(merge_locks(&base, local_lock.as_ref())),
    );

    let backend = build_router(&config, cache_root()?.join("git"))?;
    let plan = preview_plan(&config, &parsed, &remotes, &backend, lock.as_ref(), sel)?;

    print!(
        "{}",
        if json {
            render_preview_json(&plan)?
        } else {
            render_preview_tree(&plan)
        }
    );
    Ok(())
}

/// Which slice of the preview to render: optional source/target filters and the
/// `--files` enrichment toggle.
#[derive(Debug, Default, Clone)]
pub(crate) struct PreviewSelectors {
    pub source: Option<String>,
    pub target: Option<String>,
    pub files: bool,
}

/// A filtered, optionally file-enriched offline preview across targets.
#[derive(Debug, Clone)]
pub(crate) struct PreviewPlan {
    pub targets: Vec<PreviewTargetPlan>,
}

/// Returns the offline preview filtered by the selectors, with synced entries
/// optionally enriched with their file lists.
///
/// # Errors
/// Errors if a selector names an unknown source/target or the preview build fails.
pub(crate) fn preview_plan(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    lock: Option<&Lock>,
    sel: &PreviewSelectors,
) -> Result<PreviewPlan> {
    if let Some(target) = &sel.target
        && !config.targets.contains_key(target)
    {
        return Err(Error::Config(format!("unknown target: {target}")));
    }
    if let Some(source) = &sel.source
        && !config.sources.contains_key(source)
    {
        return Err(Error::Config(format!("unknown source: {source}")));
    }

    let mut targets = preview_targets(config, parsed, remotes, backend, lock, sel.files)?;
    if let Some(target) = &sel.target {
        targets.retain(|t| &t.target == target);
    }
    if let Some(source) = &sel.source {
        for tp in &mut targets {
            tp.entries.retain(|e| &e.source == source);
        }
    }

    Ok(PreviewPlan { targets })
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
    let records = registry.list_all()?;
    let ejected = crate::store::ejected_index(registry, &records)?;
    let mut groups: BTreeMap<(String, String), WhereMatch> = BTreeMap::new();

    for record in records {
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
        let k = &record.key;
        let target = if ejected.contains(&(k.target.clone(), k.source.clone(), k.artifact.clone()))
        {
            format!("{} (ejected)", k.target)
        } else {
            k.target.clone()
        };
        entry.targets.push(target);
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
    let parsed = config.parsed_sources()?;
    let bound_sources = target
        .resolve_sources(&parsed)
        .into_iter()
        .map(|binding| {
            let default_ref = parsed
                .get(binding.source)
                .map(SourceFields::intrinsic_refspec);
            let differs = default_ref.is_none_or(|d| {
                crate::lock::encode_ref(&binding.effective_ref) != crate::lock::encode_ref(&d)
            });
            if differs {
                format!("{} @ {}", binding.identity, binding.effective_ref)
            } else {
                binding.identity.to_owned()
            }
        })
        .collect();
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
        let artifact_dst = crate::sync::record_artifact_path(target, &rec);
        let state = check_artifact_state(
            &artifact_dst,
            &rec.key.source,
            &rec.commit,
            &ejected,
            &rec.key.artifact,
            registry,
            &rec.key,
            rec.vars_digest.as_deref(),
        )?;
        artifacts.push(ArtifactStatus {
            source: rec.key.source,
            artifact: rec.key.artifact,
            state: state_label(&state).to_owned(),
        });
    }
    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FileRegistry;
    use tempfile::TempDir;

    fn empty_registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    fn bound(detail: &TargetDetail) -> &str {
        detail
            .bound_sources
            .first()
            .expect("one bound source")
            .as_str()
    }

    #[test]
    fn target_show_renders_non_default_effective_ref() {
        let config = Config::parse(
            "version = 1\n\n[sources.fzf]\ngit = \"g\"\nbranch = \"main\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"fzf\", as = \"canary\", tag = \"v0.56.0\" }]\n",
        )
        .expect("config with a ref-pinned binding parses");
        let (_dir, reg) = empty_registry();

        let detail = target_detail(&config, &reg, "t").expect("target detail");
        let entry = bound(&detail);
        assert!(
            entry.contains("canary"),
            "the bound source must carry its identity `canary`, got {entry:?}"
        );
        assert!(
            entry.contains("v0.56.0"),
            "a binding whose effective ref differs from the source default must surface that ref \
             (v0.56.0) in `target show`, got {entry:?}"
        );
    }

    #[test]
    fn target_show_omits_redundant_default_ref() {
        let config = Config::parse(
            "version = 1\n\n[sources.fzf]\ngit = \"g\"\nbranch = \"main\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"fzf\"]\n",
        )
        .expect("config with a bare binding parses");
        let (_dir, reg) = empty_registry();

        let detail = target_detail(&config, &reg, "t").expect("target detail");
        let entry = bound(&detail);
        assert!(
            !entry.contains("main"),
            "a bare binding whose effective ref equals the source default must show just the \
             identity, never appending the redundant default ref, got {entry:?}"
        );
    }
}
