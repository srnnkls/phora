use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, LayoutConfig, LayoutKind, ParsedSource};
use crate::error::{Error, Result};
use crate::source::SourceBackend;
use crate::store::{Registry, RegistryRecord};

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

fn overlaps_any_live_path(path: &Path, live: &LivePathsBySource) -> bool {
    live.values().flatten().any(|(_, dest)| touches(path, dest))
}

fn reconstruct_layout(record: &RegistryRecord) -> Option<LayoutConfig> {
    let kind = LayoutKind::from_record_label(&record.layout)?;
    let separator = match kind {
        LayoutKind::Prefixed => record.layout_separator.clone()?,
        LayoutKind::Flat | LayoutKind::BySource => String::new(),
    };
    Some(LayoutConfig { kind, separator })
}

#[must_use]
pub(crate) fn is_orphan(config: &Config, record: &RegistryRecord) -> bool {
    !config.targets.contains_key(&record.key.target)
}

pub(crate) fn orphan_records(
    config: &Config,
    registry: &dyn Registry,
) -> Result<Vec<RegistryRecord>> {
    let mut orphans: Vec<RegistryRecord> = registry
        .list_all()?
        .into_iter()
        .filter(|record| is_orphan(config, record))
        .collect();
    orphans.sort_by(|a, b| {
        (&a.key.target, &a.key.source, &a.key.artifact).cmp(&(
            &b.key.target,
            &b.key.source,
            &b.key.artifact,
        ))
    });
    Ok(orphans)
}

/// `None` rather than a guessed path whenever the path cannot be reconstructed exactly: legacy record without `deploy_root`, unrecognized layout label, or `Prefixed` without its persisted separator.
#[must_use]
pub(crate) fn orphan_artifact_path(record: &RegistryRecord) -> Option<PathBuf> {
    let root = record.deploy_root.as_deref()?;
    let layout = reconstruct_layout(record)?;
    Some(Path::new(root).join(layout.artifact_path(&record.key.source, &record.key.artifact)))
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
        } else {
            match orphan_artifact_path(&record) {
                Some(path) if keep_orphan(&record, &path, &live_paths)? => continue,
                Some(_) => {}
                None => diagnose_unreconstructable_orphan(&record),
            }
        }
        registry.remove(&record.key)?;
    }
    Ok(())
}

fn keep_orphan(
    record: &RegistryRecord,
    path: &Path,
    live_paths: &LivePathsBySource,
) -> Result<bool> {
    if super::target::is_composed_target(&record.key.target) {
        eprintln!(
            "phora: refusing to prune out-of-anchor {}: composed target `{}` has no confine anchor",
            path.display(),
            record.key.target
        );
        return Ok(true);
    }
    if overlaps_any_live_path(path, live_paths) {
        return Ok(true);
    }
    if path.exists() {
        eprintln!(
            "phora: pruning orphaned {}:{}",
            record.key.source, record.key.artifact
        );
        remove_orphan_path(path)
            .map_err(|e| Error::Sync(format!("prune {}: {e}", path.display())))?;
    }
    Ok(false)
}

fn diagnose_unreconstructable_orphan(record: &RegistryRecord) {
    if record.deploy_root.is_some() {
        eprintln!(
            "phora: dropping the record for orphaned {}:{} only — its on-disk path cannot be \
             reconstructed (layout `{}` unrecognized or missing its separator); any file is left \
             in place rather than deleting a guessed path",
            record.key.source, record.key.artifact, record.layout
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ArtifactKey, RecordKind};

    fn orphan_record(layout: &str, separator: Option<&str>) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: "gone".to_owned(),
                source: "my-src".to_owned(),
                artifact: "conf".to_owned(),
            },
            source: "my-src".to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: layout.to_owned(),
            kind: RecordKind::File,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: false,
            vars_digest: None,
            deploy_root: Some("/deploy".to_owned()),
            layout_separator: separator.map(str::to_owned),
        }
    }

    #[test]
    fn prefixed_orphan_reconstructs_with_the_persisted_custom_separator() {
        let record = orphan_record("prefixed", Some("_"));
        assert_eq!(
            orphan_artifact_path(&record),
            Some(PathBuf::from("/deploy/my-src_conf")),
            "a prefixed orphan must join <source><persisted-sep><artifact>, honoring the stored \
             `_` separator — never a hardcoded dash that would strand the real file and delete a \
             guessed one"
        );
    }

    #[test]
    fn prefixed_orphan_without_persisted_separator_refuses_to_guess() {
        let record = orphan_record("prefixed", None);
        assert_eq!(
            orphan_artifact_path(&record),
            None,
            "a prefixed record missing its persisted separator cannot be reconstructed exactly, so \
             the path must be None rather than a dash-guessed path that could feed a delete"
        );
    }

    #[test]
    fn unrecognized_layout_label_refuses_to_guess() {
        let record = orphan_record("nonsense", None);
        assert_eq!(
            orphan_artifact_path(&record),
            None,
            "an unparseable layout label must yield None, never silently coerced to Flat and fed \
             to a delete"
        );
    }

    #[test]
    fn legacy_bysource_label_still_reconstructs() {
        let record = orphan_record("bysource", None);
        assert_eq!(
            orphan_artifact_path(&record),
            Some(PathBuf::from("/deploy/my-src/conf")),
            "a record written by a pre-hardening build (Debug-lowercased `bysource`) must still \
             reconstruct as by-source so its orphan stays prunable"
        );
    }
}
