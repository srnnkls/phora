use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{Config, LayoutConfig, LayoutKind};
use crate::error::{Error, Result};
use crate::sync::state::{ArtifactRecord, StateStore};

use super::confine::{ProtectedPathSet, confine_destination};
use super::{persisted_manifest_relative_path, remove_orphan_path};
use crate::projection::build::projected_artifacts;
use crate::projection::model::Projection;
use crate::sync::model::{ObservedArtifact, ObservedProjectState, SyncChange};
use crate::sync::request::SyncEvents;
use crate::sync::{AppliedChange, SyncWarning};

#[cfg(test)]
type ExpectedByBinding = BTreeMap<(String, String), Vec<String>>;
type ExpectedPaths = BTreeMap<String, Vec<PathBuf>>;
type LivePathsBySource = BTreeMap<String, Vec<(String, PathBuf)>>;

#[cfg(test)]
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

fn reconstruct_layout(record: &ArtifactRecord) -> Option<LayoutConfig> {
    let kind = LayoutKind::from_record_label(&record.layout)?;
    let separator = match kind {
        LayoutKind::Prefixed => record.layout_separator.clone()?,
        LayoutKind::Flat | LayoutKind::BySource => String::new(),
    };
    Some(LayoutConfig { kind, separator })
}

#[must_use]
pub(crate) fn is_orphan(config: &Config, record: &ArtifactRecord) -> bool {
    !config.targets.contains_key(&record.key.target)
}

pub(crate) fn orphan_records(
    config: &Config,
    registry: &dyn StateStore,
) -> Result<Vec<ArtifactRecord>> {
    let mut orphans: Vec<ArtifactRecord> = registry
        .all_artifacts()?
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
pub(crate) fn orphan_artifact_path(record: &ArtifactRecord) -> Option<PathBuf> {
    let root = record.deploy_root.as_deref()?;
    let layout = reconstruct_layout(record)?;
    Some(Path::new(root).join(super::target::record_relative_destination(&layout, record)))
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

pub(super) fn expected_live_paths(projection: &Projection, config: &Config) -> ExpectedPaths {
    let mut expected_paths: ExpectedPaths = BTreeMap::new();
    for plan in &projection.targets {
        let Some(target) = config.targets.get(&plan.target) else {
            continue;
        };
        let paths = expected_paths.entry(plan.target.clone()).or_default();
        for binding in &plan.bindings {
            for item in projected_artifacts(binding) {
                paths.push(target.expanded_path().join(item.destination.as_str()));
            }
        }
    }
    expected_paths
}

#[cfg(test)]
pub(super) fn prune_projected(
    projection: &Projection,
    config: &Config,
    registry: &dyn StateStore,
    protected: &ProtectedPathSet,
) -> Result<()> {
    let mut expected: ExpectedByBinding = BTreeMap::new();
    let mut live_paths: LivePathsBySource = BTreeMap::new();
    for plan in &projection.targets {
        let target = config.targets.get(&plan.target);
        for binding in &plan.bindings {
            let keys: Vec<String> = projected_artifacts(binding)
                .map(|item| item.materialization.published_key().to_owned())
                .collect();
            if let Some(target) = target {
                let dests = live_paths.entry(plan.target.clone()).or_default();
                for item in projected_artifacts(binding) {
                    dests.push((
                        binding.identity.clone(),
                        target.expanded_path().join(item.destination.as_str()),
                    ));
                }
            }
            expected
                .entry((plan.target.clone(), binding.identity.clone()))
                .or_default()
                .extend(keys);
        }
    }

    let records = registry.all_artifacts()?;

    for record in records {
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
                Ok(path) if path.exists() => {
                    prune_stale_manifest_children(&path, &record, &live_paths, &record.key.target)?;
                }
                Ok(_) => {}
                Err(_e) => {
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
        registry.remove_artifact(&record.key)?;
    }
    Ok(())
}

pub(super) fn apply_reconciled_removals(
    changes: &[SyncChange],
    observed: &ObservedProjectState<ArtifactRecord>,
    projection: &Projection,
    config: &Config,
    registry: &dyn StateStore,
    protected: &ProtectedPathSet,
    events: &mut SyncEvents,
) -> Result<()> {
    let removals: Vec<(&str, &str, &str, &crate::sync::model::RemovalReason)> = changes
        .iter()
        .filter_map(|change| match change {
            SyncChange::Remove {
                target,
                source,
                artifact,
                reason,
            } => Some((target.as_str(), source.as_str(), artifact.as_str(), reason)),
            _ => None,
        })
        .collect();
    let records: BTreeMap<(&str, &str, &str), &ArtifactRecord> = observed
        .artifacts
        .iter()
        .filter_map(|entry| match &entry.observation {
            ObservedArtifact::Managed(managed) => Some((
                (
                    entry.target.as_str(),
                    entry.source.as_str(),
                    entry.artifact.as_str(),
                ),
                &managed.record,
            )),
            _ => None,
        })
        .collect();
    let live_paths = live_paths_by_source(projection, config);
    for (target, source, artifact, reason) in removals {
        let Some(record) = records.get(&(target, source, artifact)).copied() else {
            return Err(Error::Sync(format!(
                "reconciled removal {source}:{artifact} in target {target} has no managed record"
            )));
        };
        if !remove_reconciled_record(record, config, registry, protected, &live_paths, events)? {
            continue;
        }
        events.applied.push(AppliedChange::Removed {
            target: target.to_owned(),
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            reason: reason.clone(),
        });
    }
    Ok(())
}

fn live_paths_by_source(projection: &Projection, config: &Config) -> LivePathsBySource {
    let mut live_paths = LivePathsBySource::new();
    for target_projection in &projection.targets {
        let Some(target) = config.targets.get(&target_projection.target) else {
            continue;
        };
        let paths = live_paths
            .entry(target_projection.target.clone())
            .or_default();
        for binding in &target_projection.bindings {
            for item in projected_artifacts(binding) {
                paths.push((
                    binding.identity.clone(),
                    target.expanded_path().join(item.destination.as_str()),
                ));
            }
        }
    }
    live_paths
}

fn remove_reconciled_record(
    record: &ArtifactRecord,
    config: &Config,
    registry: &dyn StateStore,
    protected: &ProtectedPathSet,
    live_paths: &LivePathsBySource,
    events: &mut SyncEvents,
) -> Result<bool> {
    if let Some(target) = config.targets.get(&record.key.target) {
        let dst = super::target::record_artifact_path(target, record);
        let confined = match &target.confine {
            Some(anchor) => confine_destination(anchor, &dst, protected),
            None if super::target::is_composed_target(&record.key.target) => {
                Err(Error::Config(format!(
                    "confinement: composed target `{}` reached prune without a confine anchor; \
                     refusing an unconfined delete",
                    record.key.target
                )))
            }
            None => Ok(dst.clone()),
        };
        match confined {
            Ok(path)
                if path.exists()
                    && !overlaps_any_live_dest(&path, live_paths, &record.key.target) =>
            {
                remove_orphan_path(&path)
                    .map_err(|error| Error::Sync(format!("prune {}: {error}", path.display())))?;
            }
            Ok(path)
                if path.exists()
                    && overlaps_foreign_live_dest(
                        &path,
                        live_paths,
                        &record.key.target,
                        &record.key.source,
                    ) =>
            {
                return Ok(false);
            }
            Ok(path) if path.exists() => {
                prune_stale_manifest_children(&path, record, live_paths, &record.key.target)?;
            }
            Ok(_) => {}
            Err(error) => {
                events.warnings.push(SyncWarning::PruneRefused {
                    path: dst.clone(),
                    reason: error.to_string(),
                });
                return Ok(false);
            }
        }
    } else {
        let Some(path) = orphan_artifact_path(record) else {
            if record.deploy_root.is_some() {
                events.warnings.push(SyncWarning::OrphanRecordPathUnknown {
                    source: record.key.source.clone(),
                    artifact: record.key.artifact.clone(),
                    layout: record.layout.clone(),
                });
            }
            registry.remove_artifact(&record.key)?;
            return Ok(true);
        };
        if super::target::is_composed_target(&record.key.target) {
            events.warnings.push(SyncWarning::PruneRefused {
                path: path.clone(),
                reason: format!(
                    "composed target `{}` has no confine anchor",
                    record.key.target
                ),
            });
            return Ok(false);
        }
        if overlaps_any_live_path(&path, live_paths) {
            return Ok(false);
        }
        if path.exists() {
            remove_orphan_path(&path)
                .map_err(|error| Error::Sync(format!("prune {}: {error}", path.display())))?;
        }
    }
    registry.remove_artifact(&record.key)?;
    Ok(true)
}

fn prune_stale_manifest_children(
    artifact_root: &Path,
    record: &ArtifactRecord,
    live_paths: &LivePathsBySource,
    target: &str,
) -> Result<()> {
    if record.linked || record.kind != crate::sync::state::RecordKind::Dir {
        return Ok(());
    }
    let root_metadata = match std::fs::symlink_metadata(artifact_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::Sync(format!(
                "inspect stale artifact root {}: {error}",
                artifact_root.display()
            )));
        }
    };
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Ok(());
    }
    let manifest_children = record
        .files
        .iter()
        .map(|file| persisted_manifest_relative_path(&file.path))
        .collect::<Result<Vec<_>>>()?;
    let mut empty_dir_candidates = BTreeSet::new();
    for relative in manifest_children {
        let stale_path = artifact_root.join(relative.as_str());
        if overlaps_any_live_dest(&stale_path, live_paths, target) {
            continue;
        }
        if has_symlink_ancestor(artifact_root, &stale_path)? {
            continue;
        }
        match std::fs::symlink_metadata(&stale_path) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(_) => remove_orphan_path(&stale_path)
                .map_err(|error| Error::Sync(format!("prune {}: {error}", stale_path.display())))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::Sync(format!(
                    "inspect stale manifest child {}: {error}",
                    stale_path.display()
                )));
            }
        }
        let mut parent = stale_path.parent();
        while let Some(dir) = parent {
            if dir == artifact_root || !dir.starts_with(artifact_root) {
                break;
            }
            empty_dir_candidates.insert(dir.to_path_buf());
            parent = dir.parent();
        }
    }
    let mut empty_dir_candidates: Vec<PathBuf> = empty_dir_candidates.into_iter().collect();
    empty_dir_candidates.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for dir in empty_dir_candidates {
        match std::fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => {
                return Err(Error::Sync(format!(
                    "remove empty stale directory {}: {error}",
                    dir.display()
                )));
            }
        }
    }
    Ok(())
}

fn has_symlink_ancestor(artifact_root: &Path, path: &Path) -> Result<bool> {
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    let Ok(relative_parent) = parent.strip_prefix(artifact_root) else {
        return Ok(true);
    };
    let mut ancestor = artifact_root.to_path_buf();
    for component in relative_parent.components() {
        ancestor.push(component);
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Ok(true);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(Error::Sync(format!(
                    "inspect stale manifest ancestor {}: {error}",
                    ancestor.display()
                )));
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
fn keep_orphan(
    record: &ArtifactRecord,
    path: &Path,
    live_paths: &LivePathsBySource,
) -> Result<bool> {
    if super::target::is_composed_target(&record.key.target) {
        return Ok(true);
    }
    if overlaps_any_live_path(path, live_paths) {
        return Ok(true);
    }
    if path.exists() {
        remove_orphan_path(path)
            .map_err(|e| Error::Sync(format!("prune {}: {e}", path.display())))?;
    }
    Ok(false)
}

#[cfg(test)]
fn diagnose_unreconstructable_orphan(record: &ArtifactRecord) {
    let _ = record;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::state::{ArtifactKey, RecordKind};

    fn orphan_record(layout: &str, separator: Option<&str>) -> ArtifactRecord {
        ArtifactRecord {
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
            history: false,
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
