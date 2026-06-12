use std::collections::{BTreeMap, HashSet};

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::kernel::{Selection, SourceName};
use crate::lock::encode_ref;
use crate::source::SourceBackend;
use crate::store::{ArtifactKey, Registry};

use super::discover::discover_artifacts_for_source;
use super::{remote_for, remove_orphan_path};

pub(super) fn prune_orphans(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let mut expected: HashSet<ArtifactKey> = HashSet::new();
    for (target_name, target) in &config.targets {
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
            let git = remote_for(remotes, binding.source)?;
            let source_name = SourceName::trusted(binding.source);
            let selection = Selection::new(binding.include, binding.exclude)?;
            let discovered = discover_artifacts_for_source(
                source,
                git,
                &source_name,
                commit,
                backend,
                &selection,
                binding.root,
            )?;
            for artifact in discovered {
                expected.insert(ArtifactKey {
                    target: target_name.clone(),
                    source: binding.identity.to_owned(),
                    artifact: artifact.as_str().to_owned(),
                });
            }
        }
    }

    for record in registry.list_all()? {
        if expected.contains(&record.key) {
            continue;
        }
        if let Some(target) = config.targets.get(&record.key.target) {
            let dst = target.expanded_path().join(
                target
                    .layout()
                    .artifact_path(&record.key.source, &record.key.artifact),
            );
            if dst.exists() {
                eprintln!(
                    "phora: pruning orphaned {}:{}",
                    record.key.source, record.key.artifact
                );
                remove_orphan_path(&dst)
                    .map_err(|e| Error::Sync(format!("prune {}: {e}", dst.display())))?;
            }
        }
        registry.remove(&record.key)?;
    }
    Ok(())
}
