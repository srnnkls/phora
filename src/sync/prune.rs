use std::collections::{BTreeMap, HashSet};

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::kernel::Selection;
use crate::registry::{ArtifactKey, Registry};
use crate::source::SourceBackend;

use super::discover::discover_artifacts_for_source;
use super::{remote_for, remove_orphan_path};

pub(super) fn prune_orphans(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    resolved_commits: &BTreeMap<String, String>,
) -> Result<()> {
    let mut expected: HashSet<ArtifactKey> = HashSet::new();
    for (target_name, target) in &config.targets {
        for source_name in target.resolve_sources(parsed) {
            let source = parsed.get(source_name).ok_or_else(|| {
                Error::Config(format!("target references undefined source: {source_name}"))
            })?;
            let commit = &resolved_commits[source_name];
            let git = remote_for(remotes, source_name)?;
            let selection = Selection::new(source.includes(), source.excludes())?;
            let discovered = discover_artifacts_for_source(
                source,
                git,
                source_name,
                commit,
                backend,
                &selection,
            )?;
            for artifact in discovered {
                expected.insert(ArtifactKey {
                    target: target_name.clone(),
                    source: source_name.to_owned(),
                    artifact,
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
