use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{Config, LayoutConfig, LayoutKind};
use crate::error::{Error, Result};
use crate::source::SourceStore;
use crate::sync::state::{ArtifactRecord, StateStore};

use super::confine::ProtectedPathSet;
use super::{DeployAll, persisted_manifest_relative_path, remove_orphan_path};
use crate::projection::build::projected_artifacts;
use crate::projection::model::Projection;
use crate::sync::model::{
    LocalChange, ManagedArtifact, ManagedCondition, ObservedArtifact, ObservedProjectState,
    RemovalReason, SyncChange,
};
use crate::sync::request::SyncEvents;
use crate::sync::target::{ConflictDecisions, ConflictOutcome};
use crate::sync::{AppliedChange, Resolution, SkippedChange, SyncWarning};

#[cfg(test)]
type ExpectedByBinding = BTreeMap<(String, String), Vec<String>>;
pub(super) type LivePathsBySource = BTreeMap<String, Vec<(String, PathBuf)>>;

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

pub(super) fn contains_live_destination(path: &Path, live: &[PathBuf]) -> bool {
    live.iter().any(|destination| destination.starts_with(path))
}

fn touches(path: &Path, dest: &Path) -> bool {
    dest.starts_with(path) || path.starts_with(dest)
}

fn overlaps_any_live_dest(path: &Path, live: &LivePathsBySource, target: &str) -> bool {
    live.get(target)
        .is_some_and(|dests| dests.iter().any(|(_, dest)| touches(path, dest)))
}

pub(super) fn overlaps_any_live_path(path: &Path, live: &LivePathsBySource) -> bool {
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

/// A record config no longer deploys: its target is gone, or a configured target no longer binds
/// its source. Composed targets are judged by the transitive graph, so only their presence counts.
#[must_use]
pub(crate) fn is_orphan(config: &Config, record: &ArtifactRecord) -> bool {
    let Some(target) = config.targets.get(&record.key.target) else {
        return true;
    };
    if super::target::is_composed_target(&record.key.target) {
        return false;
    }
    let source = record.source_name();
    !target.declared_sources().any(|bound| bound == source)
        && !target
            .offer_bindings
            .iter()
            .flatten()
            .any(|offer| offer.source() == source)
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
    Some(
        Path::new(root)
            .join(super::target::record_relative_destination(&layout, record))
            .components()
            .collect(),
    )
}

pub(crate) fn removal_destination(
    target: &crate::config::Target,
    record: &ArtifactRecord,
) -> PathBuf {
    orphan_artifact_path(record)
        .unwrap_or_else(|| super::target::record_artifact_path(target, record))
}

pub(crate) fn removal_path(
    target: &crate::config::Target,
    record: &ArtifactRecord,
    protected: &ProtectedPathSet,
) -> Result<PathBuf> {
    let dst = removal_destination(target, record);
    super::confine::confine_removal_destination(
        target.confine.as_deref(),
        record.deploy_root.as_deref().map(Path::new),
        &dst,
        super::target::is_composed_target(&record.key.target),
        protected,
    )
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
                Some(anchor) => super::confine::confine_entry_destination(anchor, &dst, protected),
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
                Ok(path) if !overlaps_any_live_dest(&path, &live_paths, &record.key.target) => {
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

type ArtifactTriplet<'a> = (&'a str, &'a str, &'a str);

/// One planned removal: a plain one, or one waiting on a conflict decision.
struct RemovalRow<'a> {
    key: ArtifactTriplet<'a>,
    reason: RemovalReason,
    change: Option<&'a LocalChange>,
}

fn collect_removal_rows(changes: &[SyncChange]) -> Vec<RemovalRow<'_>> {
    changes
        .iter()
        .filter_map(|change| match change {
            SyncChange::Remove {
                target,
                source,
                artifact,
                reason,
            } => Some(RemovalRow {
                key: (target, source, artifact),
                reason: *reason,
                change: None,
            }),
            SyncChange::RemoveConflict {
                target,
                source,
                artifact,
                change,
                reason,
            } => Some(RemovalRow {
                key: (target, source, artifact),
                reason: *reason,
                change: Some(change),
            }),
            _ => None,
        })
        .collect()
}

/// Managed records by observed key, and the keys whose record was adopted from another binding.
fn index_observed_records(
    observed: &ObservedProjectState<ArtifactRecord>,
) -> (
    BTreeMap<ArtifactTriplet<'_>, &ArtifactRecord>,
    BTreeSet<ArtifactTriplet<'_>>,
) {
    let records: BTreeMap<ArtifactTriplet<'_>, &ArtifactRecord> = observed
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
    let adopted = records
        .iter()
        .filter(|((target, source, _), record)| {
            (record.key.target.as_str(), record.key.source.as_str()) != (*target, *source)
        })
        .map(|(_, record)| {
            (
                record.key.target.as_str(),
                record.key.source.as_str(),
                record.key.artifact.as_str(),
            )
        })
        .collect();
    (records, adopted)
}

pub(super) fn apply_reconciled_removals<R>(
    changes: &[SyncChange],
    observed: &ObservedProjectState<ArtifactRecord>,
    projection: &Projection,
    decisions: &ConflictDecisions,
    ctx: &DeployAll<'_, R>,
    events: &mut SyncEvents,
) -> Result<()>
where
    R: StateStore,
{
    let (records, adopted) = index_observed_records(observed);
    let live_paths = live_paths_by_source(projection, ctx.config);
    let mut emptied = EmptiedDirectories::default();
    for row in collect_removal_rows(changes) {
        let (target, source, artifact) = row.key;
        let Some(record) = records.get(&row.key).copied() else {
            return Err(Error::Sync(format!(
                "reconciled removal {source}:{artifact} in target {target} has no managed record"
            )));
        };
        if adopted.contains(&row.key) {
            continue;
        }
        apply_removal(
            &row,
            record,
            decisions,
            ctx,
            &live_paths,
            &mut emptied,
            events,
        )?;
    }
    emptied.sweep(ctx.config, ctx.registry, events)
}

fn apply_removal<R>(
    row: &RemovalRow<'_>,
    record: &ArtifactRecord,
    decisions: &ConflictDecisions,
    ctx: &DeployAll<'_, R>,
    live_paths: &LivePathsBySource,
    emptied: &mut EmptiedDirectories,
    events: &mut SyncEvents,
) -> Result<()>
where
    R: StateStore,
{
    let (target, source, artifact) = row.key;
    let located = locate_removal(ctx.config, record, ctx.protected);
    if let Some(change) = row.change {
        let outcome = decisions
            .get(&(target.to_owned(), source.to_owned(), artifact.to_owned()))
            .ok_or_else(|| {
                Error::Sync(format!(
                    "removal conflict {source}:{artifact} in target {target} has no decision"
                ))
            })?;
        let shown = match &located {
            Ok(Some(path)) => path.clone(),
            Ok(None) | Err(_) => PathBuf::from(artifact),
        };
        if !settle_removal_conflict(
            record,
            shown,
            change,
            row.reason,
            outcome,
            ctx.registry,
            events,
        )? {
            return Ok(());
        }
    }
    match remove_reconciled_record(
        record,
        located,
        ctx.config,
        ctx.backend,
        ctx.registry,
        live_paths,
        events,
    )? {
        Removal::Kept => {}
        Removal::Released(path) if row.reason != RemovalReason::Pruned => {
            events.push_warning(SyncWarning::ReleasedUnderLive {
                source: source.to_owned(),
                artifact: artifact.to_owned(),
                path,
                reason: row.reason,
            });
        }
        Removal::Deleted | Removal::Released(_) => {
            emptied.record(ctx.config, record);
            events.push_applied(AppliedChange::Removed {
                target: target.to_owned(),
                source: source.to_owned(),
                artifact: artifact.to_owned(),
                reason: row.reason,
            });
        }
    }
    Ok(())
}

pub(super) fn live_paths_by_source(projection: &Projection, config: &Config) -> LivePathsBySource {
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

/// How the artifact at `path` differs from what phora deployed there; `None` while it is
/// unchanged or already gone. A link is unchanged while it is a symlink reading the recorded
/// target (any symlink for a legacy record without one); a copy while its files match the record.
pub(super) fn detect_local_change(
    record: &ArtifactRecord,
    path: &Path,
    registry: &dyn StateStore,
) -> Result<Option<LocalChange>> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if record.linked {
        if !metadata.file_type().is_symlink() {
            return Ok(Some(LocalChange::Replaced));
        }
        let repointed = record.link_target.as_deref().is_some_and(|expected| {
            std::fs::read_link(path).ok().as_deref() != Some(Path::new(expected))
        });
        return Ok(repointed.then_some(LocalChange::Repointed));
    }
    if record.history {
        return Ok(None);
    }
    let ejected = registry.ejections(&record.key.target)?;
    let observation = super::inspect::inspect(
        path,
        &record.key.source,
        &record.commit,
        &ejected,
        registry,
        &record.key,
        record.vars_digest.as_deref(),
    )?;
    Ok(match observation {
        ObservedArtifact::Managed(ManagedArtifact {
            condition: ManagedCondition::Modified { changed },
            ..
        }) => Some(LocalChange::Modified { changed }),
        ObservedArtifact::Foreign(_) | ObservedArtifact::Ejected => Some(LocalChange::Replaced),
        ObservedArtifact::Missing | ObservedArtifact::Managed(_) => None,
    })
}

/// Reports an artifact a removal leaves in place; its record stays, so the next sync asks again.
fn report_kept(
    record: &ArtifactRecord,
    path: PathBuf,
    change: &LocalChange,
    reason: RemovalReason,
    warn: bool,
    events: &mut SyncEvents,
) {
    if warn {
        events.push_warning(SyncWarning::KeptLocalChange {
            source: record.key.source.clone(),
            artifact: record.key.artifact.clone(),
            path,
            change: change.clone(),
            reason,
        });
    }
    events.push_skipped(SkippedChange::Conflict {
        target: record.key.target.clone(),
        source: record.key.source.clone(),
        artifact: record.key.artifact.clone(),
        kind: change.to_conflict_kind(),
    });
}

/// Applies the decision on a removal conflict; returns whether the caller deletes the artifact.
/// Skip keeps it, overwrite deletes it, eject forgets its record and keeps its files.
fn settle_removal_conflict(
    record: &ArtifactRecord,
    path: PathBuf,
    change: &LocalChange,
    reason: RemovalReason,
    outcome: &ConflictOutcome,
    registry: &dyn StateStore,
    events: &mut SyncEvents,
) -> Result<bool> {
    match outcome.resolution() {
        Resolution::Skip => {
            report_kept(record, path, change, reason, outcome.warns(), events);
            Ok(false)
        }
        Resolution::Eject => {
            registry.remove_artifact(&record.key)?;
            events.push_applied(AppliedChange::Ejected {
                target: record.key.target.clone(),
                source: record.key.source.clone(),
                artifact: record.key.artifact.clone(),
            });
            Ok(false)
        }
        Resolution::Overwrite => Ok(true),
        Resolution::Abort => Err(Error::Aborted),
    }
}

/// Where removing `record` deletes: its confined recorded destination. `Ok(None)` for an orphan
/// of a removed target whose path cannot be reconstructed; `Err` when the path escapes its anchor.
pub(super) fn locate_removal(
    config: &Config,
    record: &ArtifactRecord,
    protected: &ProtectedPathSet,
) -> Result<Option<PathBuf>> {
    if let Some(target) = config.targets.get(&record.key.target) {
        return removal_path(target, record, protected).map(Some);
    }
    let Some(path) = orphan_artifact_path(record) else {
        return Ok(None);
    };
    if super::target::is_composed_target(&record.key.target) {
        return confine_orphan(config, record, &path, protected).map(Some);
    }
    Ok(Some(path))
}

/// What a removal did with an artifact.
enum Removal {
    Deleted,
    /// The record was forgotten; its path belongs to a live destination and stays.
    Released(PathBuf),
    Kept,
}

fn remove_reconciled_record(
    record: &ArtifactRecord,
    located: Result<Option<PathBuf>>,
    config: &Config,
    backend: &dyn SourceStore,
    registry: &dyn StateStore,
    live_paths: &LivePathsBySource,
    events: &mut SyncEvents,
) -> Result<Removal> {
    let path = match located {
        Ok(Some(path)) => path,
        Ok(None) => {
            if record.deploy_root.is_some() {
                events.push_warning(SyncWarning::OrphanRecordPathUnknown {
                    source: record.key.source.clone(),
                    artifact: record.key.artifact.clone(),
                    layout: record.layout.clone(),
                });
            }
            registry.remove_artifact(&record.key)?;
            return Ok(Removal::Deleted);
        }
        Err(error) => {
            events.push_warning(SyncWarning::PruneRefused {
                path: recorded_destination(config, record).unwrap_or_default(),
                reason: error.to_string(),
            });
            return Ok(Removal::Kept);
        }
    };
    let target = record.key.target.as_str();
    if config.targets.contains_key(target) {
        if overlaps_any_live_dest(&path, live_paths, target) {
            if path.exists()
                && overlaps_foreign_live_dest(&path, live_paths, target, &record.key.source)
            {
                return Ok(Removal::Kept);
            }
            if path.exists() {
                prune_stale_manifest_children(&path, record, live_paths, target)?;
            }
            super::detach_history_overlay(record, &path, backend)?;
            registry.remove_artifact(&record.key)?;
            return Ok(Removal::Deleted);
        }
    } else if registry.artifact(&record.key)?.is_none() {
        return Ok(Removal::Kept);
    }
    if overlaps_any_live_path(&path, live_paths) {
        return Ok(
            if release_under_live_destination(record, &path, live_paths, config, registry)? {
                Removal::Released(path)
            } else {
                Removal::Kept
            },
        );
    }
    remove_orphan_path(&path)
        .map_err(|error| Error::Sync(format!("prune {}: {error}", path.display())))?;
    super::detach_history_overlay(record, &path, backend)?;
    registry.remove_artifact(&record.key)?;
    Ok(Removal::Deleted)
}

fn release_under_live_destination(
    record: &ArtifactRecord,
    path: &Path,
    live_paths: &LivePathsBySource,
    config: &Config,
    registry: &dyn StateStore,
) -> Result<bool> {
    if !tracked_by_live_record(path, live_paths, config, registry)? {
        return Ok(false);
    }
    registry.remove_artifact(&record.key)?;
    Ok(true)
}

fn tracked_by_live_record(
    path: &Path,
    live_paths: &LivePathsBySource,
    config: &Config,
    registry: &dyn StateStore,
) -> Result<bool> {
    for (name, destinations) in live_paths {
        let Some(target) = config.targets.get(name) else {
            continue;
        };
        if !destinations.iter().any(|(_, dest)| touches(path, dest)) {
            continue;
        }
        if registry
            .target_artifacts(name)?
            .iter()
            .any(|record| touches(path, &removal_destination(target, record)))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A composed target dropped from the graph prunes inside the consumer anchor still holding
/// it, or inside its recorded deploy root once the importing target is gone too.
fn confine_orphan(
    config: &Config,
    record: &ArtifactRecord,
    path: &Path,
    protected: &ProtectedPathSet,
) -> Result<PathBuf> {
    let anchor = config
        .targets
        .values()
        .filter_map(|target| target.confine.as_deref())
        .find(|anchor| super::confine::within_anchor(anchor, path));
    super::confine::confine_removal_destination(
        anchor,
        record.deploy_root.as_deref().map(Path::new),
        path,
        true,
        protected,
    )
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
    let mut empty_dir_candidates = BTreeSet::new();
    for file in &record.files {
        let relative = persisted_manifest_relative_path(&file.path)?;
        let stale_path = artifact_root.join(relative.as_str());
        if overlaps_any_live_dest(&stale_path, live_paths, target) {
            continue;
        }
        if has_symlink_ancestor(artifact_root, &stale_path)? {
            continue;
        }
        match std::fs::symlink_metadata(&stale_path) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(metadata) if !super::inspect::matches_manifest(&stale_path, &metadata, file)? => {
                continue;
            }
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

pub(super) fn recorded_destination(config: &Config, record: &ArtifactRecord) -> Option<PathBuf> {
    match config.targets.get(&record.key.target) {
        Some(target) => Some(removal_destination(target, record)),
        None => orphan_artifact_path(record),
    }
}

#[derive(Default)]
pub(super) struct EmptiedDirectories {
    candidates: BTreeSet<PathBuf>,
}

impl EmptiedDirectories {
    pub(super) fn record(&mut self, config: &Config, record: &ArtifactRecord) {
        let Some(removed) = recorded_destination(config, record) else {
            return;
        };
        let root = match record.deploy_root.as_deref() {
            Some(root) => PathBuf::from(root),
            None => match config.targets.get(&record.key.target) {
                Some(target) => target.expanded_path(),
                None => return,
            },
        };
        let mut parent = removed.parent();
        while let Some(dir) = parent {
            if dir == root || !dir.starts_with(&root) {
                break;
            }
            self.candidates.insert(dir.to_path_buf());
            parent = dir.parent();
        }
    }

    pub(super) fn sweep(
        self,
        config: &Config,
        registry: &dyn StateStore,
        events: &mut SyncEvents,
    ) -> Result<()> {
        if self.candidates.is_empty() {
            return Ok(());
        }
        let tracked: Vec<PathBuf> = registry
            .all_artifacts()?
            .iter()
            .filter_map(|record| recorded_destination(config, record))
            .collect();
        let mut candidates: Vec<PathBuf> = self.candidates.into_iter().collect();
        candidates.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        let mut kept: Vec<PathBuf> = Vec::new();
        for dir in candidates {
            if tracked.iter().any(|path| path.starts_with(&dir)) {
                kept.push(dir);
                continue;
            }
            match std::fs::symlink_metadata(&dir) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(Error::Sync(format!(
                        "inspect emptied directory {}: {error}",
                        dir.display()
                    )));
                }
            }
            match std::fs::remove_dir(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                    if !kept.iter().any(|path| path.starts_with(&dir)) {
                        events.push_warning(SyncWarning::PruneKeptDirectory { path: dir.clone() });
                    }
                    kept.push(dir);
                }
                Err(error) => {
                    return Err(Error::Sync(format!(
                        "remove emptied directory {}: {error}",
                        dir.display()
                    )));
                }
            }
        }
        Ok(())
    }
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
    remove_orphan_path(path).map_err(|e| Error::Sync(format!("prune {}: {e}", path.display())))?;
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
            directories: None,
            linked: false,
            history: false,
            worktree_admin_id: None,
            mirror_key: None,
            cache_git_root: None,
            vars_digest: None,
            deploy_root: Some("/deploy".to_owned()),
            layout_separator: separator.map(str::to_owned),
            link_target: None,
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
