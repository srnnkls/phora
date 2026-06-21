use std::collections::{BTreeMap, HashSet};

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::source::SourceBackend;
use crate::store::{ArtifactKey, Registry};

use super::confine::{ProtectedPathSet, confine_destination};
use super::plan::plan_targets;
use super::remove_orphan_path;

pub(super) fn prune_orphans(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    resolved_commits: &BTreeMap<(String, String), String>,
    protected: &ProtectedPathSet,
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
            let confined = match &target.confine {
                Some(anchor) => confine_destination(anchor, &dst, protected),
                None if super::target::is_composed_target(&record.key.target) => {
                    Err(Error::Config(format!(
                        "confinement: composed target `{}` reached prune without a confine \
                         anchor; refusing an unconfined delete",
                        record.key.target
                    )))
                }
                None => Ok(dst.clone()),
            };
            match confined {
                Ok(path) if path.exists() => {
                    eprintln!(
                        "phora: pruning orphaned {}:{}",
                        record.key.source, record.key.artifact
                    );
                    remove_orphan_path(&path)
                        .map_err(|e| Error::Sync(format!("prune {}: {e}", path.display())))?;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!(
                        "phora: refusing to prune out-of-anchor {}: {e}",
                        dst.display()
                    );
                    // Keep the record: the file is still on disk, so it must stay tracked.
                    continue;
                }
            }
        }
        registry.remove(&record.key)?;
    }
    Ok(())
}
