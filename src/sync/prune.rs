use std::collections::{BTreeMap, HashSet};

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::source::SourceBackend;
use crate::store::{ArtifactKey, Registry};

use super::plan::plan_targets;
use super::remove_orphan_path;

pub(super) fn prune_orphans(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let plans = plan_targets(config, parsed, remotes, backend, resolved_commits)?;
    let mut expected: HashSet<ArtifactKey> = HashSet::new();
    for plan in &plans {
        for entry in &plan.entries {
            expected.insert(ArtifactKey {
                target: plan.target.clone(),
                source: entry.identity.clone(),
                artifact: entry.artifact.clone(),
            });
        }
    }

    for record in registry.list_all()? {
        if expected.contains(&record.key) {
            continue;
        }
        if let Some(target) = config.targets.get(&record.key.target) {
            let dst = super::target::record_artifact_path(target, &record);
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
