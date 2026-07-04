use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::source::SourceBackend;
use crate::store::Registry;

use super::confine::{ProtectedPathSet, confine_destination};
use super::plan::{expected_artifact_keys, plan_targets};
use super::remove_orphan_path;

type ExpectedByBinding = BTreeMap<(String, String), Vec<String>>;
type ExpectedPaths = BTreeMap<String, Vec<PathBuf>>;
type LivePathsBySource = BTreeMap<String, Vec<(String, PathBuf)>>;

fn is_still_expected(
    expected: &ExpectedByBinding,
    target: &str,
    source: &str,
    artifact: &str,
) -> bool {
    let Some(keys) = expected.get(&(target.to_owned(), source.to_owned())) else {
        return false;
    };
    keys.iter().any(|expected_key| expected_key == artifact)
}

pub(super) fn overlaps_live_dest(
    path: &Path,
    expected_paths: &ExpectedPaths,
    target: &str,
) -> bool {
    expected_paths
        .get(target)
        .is_some_and(|live| live.iter().any(|dest| touches(path, dest)))
}

fn touches(path: &Path, dest: &Path) -> bool {
    dest.starts_with(path) || path.starts_with(dest)
}

fn overlaps_any_live_dest(path: &Path, live: &LivePathsBySource, target: &str) -> bool {
    live.get(target)
        .is_some_and(|dests| dests.iter().any(|(_, dest)| touches(path, dest)))
}

fn overlaps_foreign_live_dest(
    path: &Path,
    live: &LivePathsBySource,
    target: &str,
    source: &str,
) -> bool {
    live.get(target).is_some_and(|dests| {
        dests
            .iter()
            .any(|(dest_source, dest)| dest_source != source && touches(path, dest))
    })
}

pub(super) fn expected_live_paths(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<ExpectedPaths> {
    let plans = plan_targets(config, parsed, remotes, backend, resolved_commits)?;
    let mut expected_paths: ExpectedPaths = BTreeMap::new();
    for plan in &plans {
        let Some(target) = config.targets.get(&plan.target) else {
            continue;
        };
        let paths = expected_paths.entry(plan.target.clone()).or_default();
        for binding in &plan.bindings {
            for key in &expected_artifact_keys(binding) {
                paths.push(
                    target
                        .expanded_path()
                        .join(target.layout().artifact_path(&binding.identity, key)),
                );
            }
        }
    }
    Ok(expected_paths)
}

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
    let mut expected: ExpectedByBinding = BTreeMap::new();
    let mut live_paths: LivePathsBySource = BTreeMap::new();
    for plan in &plans {
        let target = config.targets.get(&plan.target);
        for binding in &plan.bindings {
            let keys = expected_artifact_keys(binding);
            if let Some(target) = target {
                let dests = live_paths.entry(plan.target.clone()).or_default();
                for key in &keys {
                    dests.push((
                        binding.identity.clone(),
                        target
                            .expanded_path()
                            .join(target.layout().artifact_path(&binding.identity, key)),
                    ));
                }
            }
            expected
                .entry((plan.target.clone(), binding.identity.clone()))
                .or_default()
                .extend(keys);
        }
    }

    for record in registry.list_all()? {
        if is_still_expected(
            &expected,
            &record.key.target,
            &record.key.source,
            &record.key.artifact,
        ) {
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
                Ok(path)
                    if path.exists()
                        && !overlaps_any_live_dest(&path, &live_paths, &record.key.target) =>
                {
                    eprintln!(
                        "phora: pruning orphaned {}:{}",
                        record.key.source, record.key.artifact
                    );
                    remove_orphan_path(&path)
                        .map_err(|e| Error::Sync(format!("prune {}: {e}", path.display())))?;
                }
                Ok(path)
                    if path.exists()
                        && overlaps_foreign_live_dest(
                            &path,
                            &live_paths,
                            &record.key.target,
                            &record.key.source,
                        ) =>
                {
                    continue;
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
