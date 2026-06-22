use std::collections::BTreeMap;

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::source::SourceBackend;
use crate::store::Registry;

use super::confine::{ProtectedPathSet, confine_destination};
use super::plan::{expected_artifact_keys, plan_targets};
use super::remove_orphan_path;

/// `(target, source) → expected leaf artifacts`. `deploy_target` records directory-granular
/// keys (`editor`) while the plan here is leaf-granular (`editor/init.lua`); either can be the
/// ancestor of the other, so an orphan is a record with no bidirectional containment match.
type ExpectedByBinding = BTreeMap<(String, String), Vec<String>>;

fn is_still_expected(
    expected: &ExpectedByBinding,
    target: &str,
    source: &str,
    artifact: &str,
) -> bool {
    let Some(leaves) = expected.get(&(target.to_owned(), source.to_owned())) else {
        return false;
    };
    leaves.iter().any(|expected_key| {
        expected_key == artifact
            || expected_key.starts_with(&format!("{artifact}/"))
            || artifact.starts_with(&format!("{expected_key}/"))
    })
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
    for plan in &plans {
        for binding in &plan.bindings {
            let bucket = expected
                .entry((plan.target.clone(), binding.identity.clone()))
                .or_default();
            bucket.extend(expected_artifact_keys(binding));
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
