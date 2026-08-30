use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{DeployMode, LayoutConfig, ParsedSource, Target, TemplateOptIn};
use crate::error::{Error, Result};
use crate::projection::model::{ContentTransform, Materialization};
use crate::source::{ResolvedSource, SourceIdentity, SourcePath, SourceStore, safe_relpath};
use crate::sync::state::{
    ArtifactKey, ArtifactRecord, Ejection, ManifestFile, NewArtifactRecord, RecordKind,
    ScannedFile, StateStore,
};

use super::apply::{apply_artifact_report, link_artifact};
use super::confine::{ProtectedPathSet, confine_destination};
use super::journal::Journal;
use super::resolve::ResolvedSourceMap;
use super::stage::{StageRequest, stage_artifact};
use super::{
    Conflict, ConflictResolver, Resolution, StagingGuard, nonce, remote_for, target_parent,
};
use crate::projection::diagnostic::ProjectionWarning;
use crate::projection::model::{ProjectedArtifact, TargetProjection};
use crate::sync::model::{ChangeSet, ConflictKind, ManagedCondition, ObservedArtifact, SyncChange};
use crate::sync::request::SyncEvents;
use crate::sync::{AppliedChange, SkippedChange, SyncWarning};

#[derive(Clone, Copy)]
pub(super) struct TargetRun<'a> {
    pub(super) parsed: &'a BTreeMap<String, ParsedSource>,
    pub(super) target_name: &'a str,
    pub(super) target: &'a Target,
    pub(super) remotes: &'a BTreeMap<String, String>,
    pub(super) resolved_sources: &'a ResolvedSourceMap,
    pub(super) vars: &'a BTreeMap<String, String>,
    pub(super) protected: &'a ProtectedPathSet,
}

impl TargetRun<'_> {
    fn confined(&self, dst: &Path) -> Result<PathBuf> {
        match &self.target.confine {
            Some(anchor) => confine_destination(anchor, dst, self.protected),
            None if is_composed_target(self.target_name) => Err(Error::Config(format!(
                "confinement: composed target `{}` reached deploy without a confine anchor; \
                 refusing an unconfined write to {}",
                self.target_name,
                dst.display()
            ))),
            None => Ok(dst.to_path_buf()),
        }
    }
}

/// `%` marks the namespaced key minted by `transitive::namespaced_key`.
pub(super) fn is_composed_target(target_name: &str) -> bool {
    target_name.contains('%')
}

#[derive(Clone)]
pub(super) struct ConflictOutcome {
    resolution: Resolution,
    kind: ConflictKind,
    warn: bool,
}

pub(super) type ConflictDecisions = BTreeMap<(String, String, String), ConflictOutcome>;

pub(super) fn decisions_abort(decisions: &ConflictDecisions) -> bool {
    decisions
        .values()
        .any(|outcome| matches!(outcome.resolution, Resolution::Abort))
}

pub(super) type ChangeIndex<'a> = BTreeMap<(String, String, String), &'a SyncChange>;
pub(super) type ObservationIndex<'a> =
    BTreeMap<(String, String, String), &'a ObservedArtifact<ArtifactRecord>>;

pub(super) struct Reconciliation<'a> {
    observed: ObservationIndex<'a>,
    changes: ChangeIndex<'a>,
    decisions: ConflictDecisions,
}

impl<'a> Reconciliation<'a> {
    pub(super) fn new(
        changeset: &'a ChangeSet,
        observed: &'a super::model::ObservedProjectState<ArtifactRecord>,
        decisions: ConflictDecisions,
    ) -> Self {
        Self {
            observed: observed
                .artifacts
                .iter()
                .map(|entry| {
                    (
                        (
                            entry.target.clone(),
                            entry.source.clone(),
                            entry.artifact.clone(),
                        ),
                        &entry.observation,
                    )
                })
                .collect(),
            changes: changeset
                .changes
                .iter()
                .map(|change| (change_key(change), change))
                .collect(),
            decisions,
        }
    }
}

fn change_key(change: &SyncChange) -> (String, String, String) {
    let (target, source, artifact) = match change {
        SyncChange::Deploy {
            target,
            source,
            artifact,
        }
        | SyncChange::Overwrite {
            target,
            source,
            artifact,
        }
        | SyncChange::Conflict {
            target,
            source,
            artifact,
            ..
        }
        | SyncChange::Remove {
            target,
            source,
            artifact,
            ..
        } => (target, source, artifact),
    };
    (target.clone(), source.clone(), artifact.clone())
}

pub(super) fn walk_projection_target(
    run: TargetRun<'_>,
    projection: &TargetProjection,
    registry: &dyn StateStore,
    surface_warnings: bool,
    mut visit: impl FnMut(&TargetRun<'_>, &ArtifactEntry<'_>) -> Result<bool>,
) -> Result<bool> {
    let layout = run.target.layout();
    let ejected = registry.ejections(run.target_name)?;
    let mut had_failures = false;

    let template_opt_ins: BTreeMap<String, TemplateOptIn> = run
        .target
        .resolve_sources(run.parsed)
        .into_iter()
        .map(|b| (b.identity.to_owned(), b.template_opt_in))
        .collect();

    for binding in &projection.bindings {
        if surface_warnings {
            let mut discarded = SyncEvents::default();
            collect_projection_warnings(&binding.warnings, &mut discarded);
        }
        let template_opt_in = template_opt_ins.get(&binding.identity).ok_or_else(|| {
            Error::Sync(format!(
                "binding `{}` planned without a resolved template opt-in",
                binding.identity
            ))
        })?;
        let source = run.parsed.get(&binding.source).ok_or_else(|| {
            Error::Config(format!(
                "target references undefined source: {}",
                binding.source
            ))
        })?;
        let git = remote_for(run.remotes, &binding.source)?;
        let resolved = run
            .resolved_sources
            .get(&(binding.source.clone(), binding.commit.clone()))
            .ok_or_else(|| {
                Error::Sync(format!(
                    "binding `{}` has no immutable source snapshot at {}",
                    binding.source, binding.commit
                ))
            })?;

        for item in &binding.artifacts {
            let key = item.materialization.published_key();
            safe_relpath(key).map_err(|_| unsafe_dest_diagnostic(key))?;
            let deploy_dst = run.target.expanded_path().join(item.destination.as_str());
            let artifact_dst = run.confined(&deploy_dst)?;
            let dst_is_symlink =
                std::fs::symlink_metadata(&artifact_dst).is_ok_and(|m| m.file_type().is_symlink());
            let mode_transition = match source.deploy_mode() {
                DeployMode::Link => artifact_dst.exists() && !dst_is_symlink,
                DeployMode::Copy => dst_is_symlink,
            };
            let entry = ArtifactEntry {
                source,
                resolved,
                git,
                identity: &binding.identity,
                underlying_source: &binding.source,
                commit: &binding.commit,
                item,
                artifact_dst: &artifact_dst,
                layout: layout.clone(),
                ejected: &ejected,
                mode_transition,
                template_opt_in,
            };
            had_failures |= visit(&run, &entry)?;
        }
    }

    Ok(had_failures)
}

pub(super) fn resolve_conflicts(
    changeset: &ChangeSet,
    resolver: Option<&dyn ConflictResolver>,
    interactive: bool,
) -> Result<ConflictDecisions> {
    resolve_conflicts_reusing(
        changeset,
        &ConflictDecisions::new(),
        &BTreeSet::new(),
        resolver,
        interactive,
    )
}

pub(super) fn resolve_conflicts_reusing(
    changeset: &ChangeSet,
    previous: &ConflictDecisions,
    excluded_targets: &BTreeSet<String>,
    resolver: Option<&dyn ConflictResolver>,
    interactive: bool,
) -> Result<ConflictDecisions> {
    let mut decisions = ConflictDecisions::new();
    for change in &changeset.changes {
        let SyncChange::Conflict {
            target,
            source,
            artifact,
            kind,
        } = change
        else {
            continue;
        };
        if excluded_targets.contains(target) {
            continue;
        }
        let key = (target.clone(), source.clone(), artifact.clone());
        if let Some(outcome) = previous.get(&key)
            && &outcome.kind == kind
        {
            decisions.insert(key, outcome.clone());
            continue;
        }
        let (resolution, warn) = match resolver {
            Some(resolver) if interactive => (
                resolver.resolve(&Conflict {
                    target: target.clone(),
                    source: source.clone(),
                    artifact: artifact.clone(),
                    kind: kind.clone(),
                }),
                false,
            ),
            _ => (Resolution::Skip, true),
        };
        decisions.insert(
            key,
            ConflictOutcome {
                resolution,
                kind: kind.clone(),
                warn,
            },
        );
    }
    if decisions_abort(&decisions) {
        return Err(Error::Aborted);
    }
    Ok(decisions)
}

#[cfg(test)]
pub(super) fn deploy_reconciled_target(
    run: TargetRun<'_>,
    projection: &TargetProjection,
    reconciliation: &Reconciliation<'_>,
    backend: &dyn SourceStore,
    registry: &dyn StateStore,
    journal: &Journal,
) -> Result<bool> {
    let mut events = SyncEvents::default();
    deploy_reconciled_target_report(
        run,
        projection,
        reconciliation,
        backend,
        registry,
        journal,
        &mut events,
    )
}

pub(super) fn deploy_reconciled_target_report(
    run: TargetRun<'_>,
    projection: &TargetProjection,
    reconciliation: &Reconciliation<'_>,
    backend: &dyn SourceStore,
    registry: &dyn StateStore,
    journal: &Journal,
    events: &mut SyncEvents,
) -> Result<bool> {
    for binding in &projection.bindings {
        collect_projection_warnings(&binding.warnings, events);
    }
    walk_projection_target(run, projection, registry, false, |run, entry| {
        apply_reconciled(
            run,
            entry,
            projection,
            reconciliation,
            backend,
            registry,
            journal,
            events,
        )
    })
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one artifact apply coordinates projection, reconciliation, source/state capabilities, journal, and report collection"
)]
fn apply_reconciled(
    run: &TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    projection: &TargetProjection,
    reconciliation: &Reconciliation<'_>,
    backend: &dyn SourceStore,
    registry: &dyn StateStore,
    journal: &Journal,
    events: &mut SyncEvents,
) -> Result<bool> {
    let published_key = entry.published_key().to_owned();
    let triplet = conflict_triplet(run, entry);
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.identity.to_owned(),
        artifact: published_key.clone(),
    };
    let change = reconciliation.changes.get(&triplet).copied();
    let writes = matches!(
        change,
        Some(
            SyncChange::Deploy { .. } | SyncChange::Overwrite { .. } | SyncChange::Conflict { .. }
        )
    );
    if journal.refuses_writes() && writes {
        return Err(journal.readonly_error());
    }

    let deploy_root = run.target.deploy_root();
    let deploy = |key: ArtifactKey, events: &mut SyncEvents| match entry.source.deploy_mode() {
        DeployMode::Link => deploy_link(registry, journal, entry, key, deploy_root.clone()),
        DeployMode::Copy => deploy_one(
            backend,
            registry,
            journal,
            DeployContext {
                deploy_root: deploy_root.clone(),
                layout: entry.layout.clone(),
                source: entry.source,
                underlying_source: entry.underlying_source,
                root: entry.source.offer().root(),
                commit: entry.commit,
                resolved: entry.resolved,
                artifact: entry.item,
                target: projection,
                kind: entry.record_kind(),
                artifact_dst: entry.artifact_dst,
                key,
                template_opt_in: entry.template_opt_in,
                vars: run.vars,
                confine_anchor: run.target.confine.as_deref(),
                events,
            },
        ),
    };

    match change {
        Some(SyncChange::Deploy { .. } | SyncChange::Overwrite { .. }) => {
            let applied = match change {
                Some(SyncChange::Overwrite { .. }) => AppliedChange::Overwritten {
                    target: run.target_name.to_owned(),
                    source: entry.identity.to_owned(),
                    artifact: published_key.clone(),
                },
                _ => AppliedChange::Deployed {
                    target: run.target_name.to_owned(),
                    source: entry.identity.to_owned(),
                    artifact: published_key.clone(),
                },
            };
            Ok(run_deploy(
                deploy,
                key,
                run.target_name,
                entry.identity,
                &published_key,
                applied,
                events,
            ))
        }
        Some(SyncChange::Conflict { .. }) => match reconciliation.decisions.get(&triplet) {
            Some(outcome) => {
                if outcome.warn {
                    collect_conflict_warning(
                        entry.identity,
                        &published_key,
                        &outcome.kind,
                        entry.artifact_dst,
                        events,
                    );
                }
                apply_resolution(
                    outcome.resolution,
                    deploy,
                    key,
                    run,
                    entry,
                    &published_key,
                    registry,
                    &outcome.kind,
                    events,
                )
            }
            None => Err(Error::Sync(format!(
                "unresolved conflict for {}:{published_key} in target {} reached apply without a \
                 preflight decision",
                entry.identity, run.target_name
            ))),
        },
        Some(SyncChange::Remove { .. }) | None => {
            persist_metadata_refresh(&reconciliation.observed, &triplet, registry, &key)?;
            Ok(false)
        }
    }
}

fn run_deploy(
    deploy: impl FnOnce(ArtifactKey, &mut SyncEvents) -> Result<()>,
    key: ArtifactKey,
    target: &str,
    identity: &str,
    published_key: &str,
    applied: AppliedChange,
    events: &mut SyncEvents,
) -> bool {
    match deploy(key, events) {
        Ok(()) => {
            events.applied.push(applied);
            false
        }
        Err(e) => {
            events.skipped.push(SkippedChange::Failed {
                target: target.to_owned(),
                source: identity.to_owned(),
                artifact: published_key.to_owned(),
                message: e.to_string(),
            });
            true
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "resolution applies one preflight decision with the full artifact identity and report collector"
)]
fn apply_resolution(
    resolution: Resolution,
    deploy: impl FnOnce(ArtifactKey, &mut SyncEvents) -> Result<()>,
    key: ArtifactKey,
    run: &TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    published_key: &str,
    registry: &dyn StateStore,
    kind: &ConflictKind,
    events: &mut SyncEvents,
) -> Result<bool> {
    match resolution {
        Resolution::Skip => {
            events.skipped.push(SkippedChange::Conflict {
                target: run.target_name.to_owned(),
                source: entry.identity.to_owned(),
                artifact: published_key.to_owned(),
                kind: kind.clone(),
            });
            Ok(false)
        }
        Resolution::Overwrite => {
            let applied = AppliedChange::Overwritten {
                target: run.target_name.to_owned(),
                source: entry.identity.to_owned(),
                artifact: published_key.to_owned(),
            };
            Ok(run_deploy(
                deploy,
                key,
                run.target_name,
                entry.identity,
                published_key,
                applied,
                events,
            ))
        }
        Resolution::Eject => {
            let mut ejected = registry.ejections(run.target_name)?;
            ejected.push(Ejection {
                source: entry.identity.to_owned(),
                artifact: published_key.to_owned(),
                ejected_at: chrono::Utc::now().to_rfc3339(),
            });
            registry.save_ejections(run.target_name, &ejected)?;
            events.applied.push(AppliedChange::Ejected {
                target: run.target_name.to_owned(),
                source: entry.identity.to_owned(),
                artifact: published_key.to_owned(),
            });
            Ok(false)
        }
        Resolution::Abort => Err(Error::Aborted),
    }
}

fn persist_metadata_refresh(
    observed: &ObservationIndex<'_>,
    triplet: &(String, String, String),
    registry: &dyn StateStore,
    key: &ArtifactKey,
) -> Result<()> {
    if let Some(ObservedArtifact::Managed(managed)) = observed.get(triplet).copied()
        && let ManagedCondition::MetadataChangedButContentClean { refreshed } = &managed.condition
    {
        persist_revalidated_refresh(registry, key, refreshed)?;
    }
    Ok(())
}

fn collect_projection_warnings(warnings: &[ProjectionWarning], events: &mut SyncEvents) {
    for warning in warnings {
        events
            .warnings
            .push(SyncWarning::Projection(warning.clone()));
    }
}

fn unsafe_dest_diagnostic(dest: &str) -> Error {
    crate::diagnostic::SelectionDiagnostic {
        entry: dest.to_owned(),
        matched_against: "the deploy root".to_owned(),
        why: "destination is not a portable relative path".to_owned(),
        did_you_mean: None,
        remedy: "use a forward-slashed relative path inside the deploy root".to_owned(),
        debug_hint: Some("phora preview --files".to_owned()),
        details: Vec::new(),
    }
    .sync()
}

pub(super) struct ArtifactEntry<'a> {
    pub(super) source: &'a ParsedSource,
    pub(super) resolved: &'a ResolvedSource,
    pub(super) git: &'a str,
    pub(super) identity: &'a str,
    pub(super) underlying_source: &'a str,
    pub(super) commit: &'a str,
    pub(super) item: &'a ProjectedArtifact,
    pub(super) artifact_dst: &'a Path,
    pub(super) layout: LayoutConfig,
    pub(super) ejected: &'a [Ejection],
    pub(super) mode_transition: bool,
    pub(super) template_opt_in: &'a TemplateOptIn,
}

impl ArtifactEntry<'_> {
    fn published_key(&self) -> &str {
        self.item.materialization.published_key()
    }

    fn record_kind(&self) -> RecordKind {
        match &self.item.materialization {
            Materialization::CollapsedDir { .. } | Materialization::WholeRoot { .. } => {
                RecordKind::Dir
            }
            Materialization::Leaf(_) => RecordKind::File,
        }
    }
}

pub(crate) fn record_relative_destination(
    layout: &LayoutConfig,
    record: &ArtifactRecord,
) -> PathBuf {
    if record.history {
        PathBuf::from(&record.key.artifact)
    } else {
        layout.artifact_path(&record.key.source, &record.key.artifact)
    }
}

pub(crate) fn record_artifact_path(target: &Target, record: &ArtifactRecord) -> PathBuf {
    target
        .expanded_path()
        .join(record_relative_destination(&target.layout(), record))
}

/// A `File` record's single manifest file IS the dest, so its base is the dest's parent;
/// a `Dir` record's base is the deployed directory itself.
pub(super) fn record_manifest_base(target: &Target, record: &ArtifactRecord) -> PathBuf {
    let artifact_path = record_artifact_path(target, record);
    match record.kind {
        RecordKind::File => artifact_path
            .parent()
            .map_or(artifact_path.clone(), Path::to_path_buf),
        RecordKind::Dir => artifact_path,
    }
}

fn conflict_triplet(run: &TargetRun<'_>, entry: &ArtifactEntry<'_>) -> (String, String, String) {
    (
        run.target_name.to_owned(),
        entry.identity.to_owned(),
        entry.published_key().to_owned(),
    )
}

/// `check_artifact_state` compares this only when `record.vars_digest.is_some()`; that lets a
/// non-templated record skip the git-tree walk here and still resolve Clean (INV-8).
pub(super) fn expected_vars_digest(
    entry: &ArtifactEntry<'_>,
    registry: &dyn StateStore,
    key: &ArtifactKey,
    vars: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    if !matches!(entry.source.deploy_mode(), DeployMode::Copy) {
        return Ok(None);
    }
    let Some(record) = registry.artifact(key)? else {
        return Ok(None);
    };
    if record.linked || record.vars_digest.is_none() {
        return Ok(None);
    }
    let templated = match &entry.item.materialization {
        Materialization::Leaf(take) => entry.template_opt_in.renders(&take.source),
        Materialization::CollapsedDir { .. } | Materialization::WholeRoot { .. } => entry
            .item
            .leaves
            .iter()
            .any(|leaf| leaf.transform == ContentTransform::Template),
    };
    Ok(templated.then(|| crate::source::vars_digest(vars)))
}

fn persist_revalidated_refresh(
    registry: &dyn StateStore,
    key: &ArtifactKey,
    fresh: &[ScannedFile],
) -> Result<()> {
    let Some(mut record) = registry.artifact(key)? else {
        return Ok(());
    };
    let refreshed: BTreeMap<&PathBuf, &ScannedFile> = fresh.iter().map(|f| (&f.path, f)).collect();
    for mf in &mut record.files {
        if let Some(scanned) = refreshed.get(&mf.path) {
            mf.size = scanned.size;
            mf.mtime = scanned.mtime;
        }
    }
    registry.put_artifact(&record)?;
    Ok(())
}

fn collect_conflict_warning(
    source: &str,
    artifact: &str,
    kind: &ConflictKind,
    dst: &Path,
    events: &mut SyncEvents,
) {
    match kind {
        ConflictKind::Modified { changed } => {
            events.warnings.push(SyncWarning::ConflictModified {
                source: source.to_owned(),
                artifact: artifact.to_owned(),
                changed: changed.clone(),
            });
        }
        ConflictKind::Foreign => {
            events.warnings.push(SyncWarning::ConflictForeign {
                path: dst.to_path_buf(),
            });
        }
    }
}

struct DeployContext<'a> {
    deploy_root: String,
    layout: LayoutConfig,
    source: &'a ParsedSource,
    underlying_source: &'a str,
    root: Option<&'a Path>,
    commit: &'a str,
    resolved: &'a ResolvedSource,
    artifact: &'a ProjectedArtifact,
    target: &'a TargetProjection,
    kind: RecordKind,
    artifact_dst: &'a Path,
    key: ArtifactKey,
    template_opt_in: &'a TemplateOptIn,
    vars: &'a BTreeMap<String, String>,
    confine_anchor: Option<&'a Path>,
    events: &'a mut SyncEvents,
}

fn deploy_one(
    backend: &dyn SourceStore,
    registry: &dyn StateStore,
    journal: &Journal,
    ctx: DeployContext<'_>,
) -> Result<()> {
    let staging_base = target_parent(ctx.artifact_dst).join(".phora-stage");
    let key_label = ctx.key.artifact.replace('/', "_");
    let staging = staging_base.join(format!("{key_label}-{}", nonce()));
    let mut staging_guard = StagingGuard::new(&staging_base, &staging);

    let policy = ctx.source.export_policy();

    let staging_payload = match &ctx.artifact.materialization {
        Materialization::CollapsedDir { .. } | Materialization::WholeRoot { .. } => staging.clone(),
        Materialization::Leaf(take) => staging.join(leaf_basename(&take.dest)),
    };

    let staged = stage_artifact(
        &StageRequest {
            artifact: ctx.artifact,
            target: ctx.target,
            variables: ctx.vars,
        },
        ctx.root,
        &policy,
        &staging,
        ctx.resolved.authored_at.unix_seconds(),
        ctx.template_opt_in,
        |repo_relative| {
            let path = SourcePath::new(&repo_relative.to_string_lossy().replace('\\', "/"))?;
            let entry = backend.read(&ctx.resolved.snapshot, &path)?;
            Ok((entry.bytes, entry.meta.kind))
        },
    )?;
    let files: Vec<ManifestFile> = staged
        .files
        .iter()
        .map(|f| ManifestFile {
            path: PathBuf::from(f.destination.as_str()),
            size: f.size,
            mtime: f.mtime,
            blake3: f.blake3.clone(),
        })
        .collect();

    if let Some(anchor) = ctx.confine_anchor {
        super::confine::reject_symlink_ancestor_at_write(anchor, ctx.artifact_dst)?;
    }

    if let Some(parent) = ctx.artifact_dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Sync(format!("create target dir {}: {e}", parent.display())))?;
    }

    let record = ArtifactRecord::projected(NewArtifactRecord {
        key: ctx.key,
        underlying_source: ctx.underlying_source,
        commit: ctx.commit,
        digest: staged.digest,
        layout: ctx.layout.kind.label().to_owned(),
        kind: ctx.kind,
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files,
        history: matches!(
            &ctx.artifact.materialization,
            Materialization::WholeRoot { .. }
        ),
        vars_digest: staged.vars_digest,
        deploy_root: Some(ctx.deploy_root),
        layout_separator: ctx.layout.persisted_separator(),
    });

    if matches!(ctx.kind, RecordKind::Dir) {
        staging_guard.disarm();
    }
    apply_artifact_report(
        &staging_base,
        &staging_payload,
        ctx.artifact_dst,
        record,
        journal,
        registry,
        ctx.events,
    )
}

fn deploy_link(
    registry: &dyn StateStore,
    journal: &Journal,
    entry: &ArtifactEntry<'_>,
    key: ArtifactKey,
    deploy_root: String,
) -> Result<()> {
    let policy = entry.source.export_policy();
    let record = ArtifactRecord {
        version: 1,
        key,
        source: entry.underlying_source.to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: entry.layout.kind.label().to_owned(),
        kind: entry.record_kind(),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
        history: matches!(
            &entry.item.materialization,
            Materialization::WholeRoot { .. }
        ),
        vars_digest: None,
        deploy_root: Some(deploy_root),
        layout_separator: entry.layout.persisted_separator(),
    };
    let staging_base = target_parent(entry.artifact_dst).join(".phora-stage");
    link_artifact(
        &staging_base,
        entry.artifact_dst,
        &link_target(entry),
        record,
        journal,
        registry,
    )
}

fn link_target(entry: &ArtifactEntry<'_>) -> PathBuf {
    let mut target = match &entry.resolved.normalized_location {
        SourceIdentity::Worktree(root) => root.as_path().to_path_buf(),
        SourceIdentity::Git(_) | SourceIdentity::Url(_) => {
            let base = Path::new(entry.git);
            if base.is_absolute() {
                base.to_path_buf()
            } else {
                base.canonicalize().unwrap_or_else(|_| {
                    std::env::current_dir()
                        .map_or_else(|_| base.to_path_buf(), |cwd| cwd.join(base))
                })
            }
        }
    };
    if let Some(root) = entry.source.offer().root() {
        target.push(root);
    }
    match &entry.item.materialization {
        Materialization::CollapsedDir { dir } => target.push(dir),
        Materialization::Leaf(take) => target.push(&take.source),
        Materialization::WholeRoot { .. } => {}
    }
    target
}

fn leaf_basename(dest: &str) -> String {
    dest.rsplit('/').next().unwrap_or(dest).to_owned()
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct ReconcileTestPolicy {
    mode_transition: bool,
    force: bool,
}

#[cfg(test)]
impl ReconcileTestPolicy {
    const STANDARD: Self = Self {
        mode_transition: false,
        force: false,
    };
    const MODE_TRANSITION: Self = Self {
        mode_transition: true,
        force: false,
    };
    const FORCED_MODE_TRANSITION: Self = Self {
        mode_transition: true,
        force: true,
    };
}

#[cfg(test)]
fn reconciled_test_change(
    observation: ObservedArtifact<()>,
    test_policy: ReconcileTestPolicy,
) -> Option<SyncChange> {
    let artifact = ProjectedArtifact {
        destination: crate::projection::model::TargetPath::new("a.txt").expect("valid dest"),
        source: crate::projection::model::ResolvedSourceRef::new("src", "0123"),
        materialization: Materialization::Leaf(crate::projection::take::ResolvedTake {
            source: "a.txt".to_owned(),
            dest: "a.txt".to_owned(),
        }),
        kept_leaves: Vec::new(),
        leaves: Vec::new(),
    };
    let binding = crate::projection::model::BindingProjection {
        identity: "src".to_owned(),
        source: "src".to_owned(),
        commit: "0123".to_owned(),
        attribution: crate::projection::model::BindingAttribution::default(),
        artifacts: vec![artifact.clone()],
        warnings: Vec::new(),
    };
    let projection = crate::projection::model::Projection {
        targets: vec![TargetProjection {
            target: "dest".to_owned(),
            bindings: vec![binding],
            artifacts: vec![artifact],
            warnings: Vec::new(),
        }],
        warnings: Vec::new(),
    };
    let observed = crate::sync::model::ObservedProjectState {
        artifacts: vec![crate::sync::model::ObservedEntry {
            target: "dest".to_owned(),
            source: "src".to_owned(),
            artifact: "a.txt".to_owned(),
            observation: super::observe::absorb_mode_transition(
                observation,
                test_policy.mode_transition,
            ),
        }],
    };
    let policy = crate::sync::model::ReconciliationPolicy {
        force: test_policy.force,
        prune: false,
        follow_moved_pin: false,
    };
    super::reconcile::reconcile(&projection, &observed, &policy)
        .expect("live reconcile accepts the fixture")
        .changes
        .into_iter()
        .next()
}

#[cfg(test)]
mod confine_fail_closed_tests {
    use super::*;
    use crate::config::Target;

    fn composed_target_without_anchor(dst: &Path) -> Target {
        Target {
            path: dst.to_path_buf(),
            sources: None,
            layout: None,
            hooks: None,
            imports: None,
            take: None,
            collapse: None,
            confine: None,
        }
    }

    fn run_for<'a>(
        target: &'a Target,
        target_name: &'a str,
        protected: &'a ProtectedPathSet,
        parsed: &'a BTreeMap<String, ParsedSource>,
        remotes: &'a BTreeMap<String, String>,
        resolved_sources: &'a ResolvedSourceMap,
        vars: &'a BTreeMap<String, String>,
    ) -> TargetRun<'a> {
        TargetRun {
            parsed,
            target_name,
            target,
            remotes,
            resolved_sources,
            vars,
            protected,
        }
    }

    #[test]
    fn composed_target_missing_its_confine_anchor_fails_closed() {
        let outside = Path::new("/home/u/.ssh/authorized_keys");
        let target = composed_target_without_anchor(outside);
        let protected =
            ProtectedPathSet::resolve(&crate::config::Paths::default(), Path::new("/home/u/proj"))
                .expect("protected");
        let parsed = BTreeMap::new();
        let remotes = BTreeMap::new();
        let resolved_sources = BTreeMap::new();
        let vars = BTreeMap::new();
        let run = run_for(
            &target,
            "root%1%nvim",
            &protected,
            &parsed,
            &remotes,
            &resolved_sources,
            &vars,
        );

        run.confined(outside).expect_err(
            "a composed/transitive target (namespaced name carries `%`) reaching deploy with \
             `confine == None` must fail closed; falling through to an unconfined write lets a dep \
             escape to any absolute path",
        );
    }

    #[test]
    fn unsafe_artifact_dest_diagnostic_points_at_the_preview_command() {
        let rendered = super::unsafe_dest_diagnostic("../escape").to_string();
        for phrase in [
            crate::diagnostic::SELECTION,
            crate::diagnostic::MATCHED_AGAINST,
            crate::diagnostic::REMEDY,
            crate::diagnostic::TO_DEBUG,
        ] {
            assert!(
                rendered.contains(phrase),
                "the unsafe-dest rejection must render `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains("../escape"),
            "the rejection must name the offending dest; got:\n{rendered}"
        );
        assert!(
            rendered.contains("to debug: phora preview --files"),
            "an unsafe artifact dest must point at the preview command; got:\n{rendered}"
        );
    }
}

#[cfg(test)]
mod kind_aware_layout_tests {
    use super::*;
    use crate::config::{LayoutConfig, LayoutKind, Target};
    use crate::sync::state::{ArtifactKey, ArtifactRecord, ManifestFile, RecordKind};

    fn target_with_layout(root: &Path, kind: LayoutKind) -> Target {
        Target {
            path: root.to_path_buf(),
            sources: None,
            layout: Some(LayoutConfig {
                kind,
                separator: match kind {
                    LayoutKind::Prefixed => "-".to_owned(),
                    LayoutKind::Flat | LayoutKind::BySource => String::new(),
                },
            }),
            hooks: None,
            imports: None,
            take: None,
            collapse: None,
            confine: None,
        }
    }

    fn record(identity: &str, artifact: &str, layout: &str, kind: RecordKind) -> ArtifactRecord {
        ArtifactRecord {
            version: 1,
            key: ArtifactKey {
                target: "dest".to_owned(),
                source: identity.to_owned(),
                artifact: artifact.to_owned(),
            },
            source: identity.to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: layout.to_owned(),
            kind,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from(artifact),
                size: 4,
                mtime: 0,
                blake3: "blake3:d4e5f6".to_owned(),
            }],
            linked: false,
            history: false,
            vars_digest: None,
            deploy_root: None,
            layout_separator: None,
        }
    }

    #[test]
    fn file_kind_deploys_at_flat_layout_path_not_root() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::Flat);
        let rec = record("agents-src", "CLAUDE.md", "flat", RecordKind::File);
        let dir_twin = record("agents-src", "CLAUDE.md", "flat", RecordKind::Dir);

        let path = record_artifact_path(&target, &rec);

        assert_eq!(
            path,
            root.join("CLAUDE.md"),
            "a flat-layout file artifact deploys at <target>/CLAUDE.md (the layout path)"
        );
        assert_eq!(
            path,
            record_artifact_path(&target, &dir_twin),
            "a file artifact's deploy path must track layout EXACTLY as its dir twin does — \
             the File-vs-Dir divergence lives only in record_manifest_base, never in the deploy path"
        );
    }

    #[test]
    fn file_kind_deploys_at_by_source_layout_path() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::BySource);
        let rec = record("agents-src", "CLAUDE.md", "by-source", RecordKind::File);
        let dir_twin = record("agents-src", "CLAUDE.md", "by-source", RecordKind::Dir);

        let path = record_artifact_path(&target, &rec);

        assert_eq!(
            path,
            root.join("agents-src").join("CLAUDE.md"),
            "a by-source file artifact deploys under its identity dir, honoring layout — \
             not flattened to the target root"
        );
        assert_ne!(
            path,
            root.join("CLAUDE.md"),
            "a kind=file record must NOT collapse to the target root; it honors layout"
        );
        assert_eq!(
            path,
            record_artifact_path(&target, &dir_twin),
            "a file artifact's deploy path must track layout EXACTLY as its dir twin does — \
             the File-vs-Dir divergence lives only in record_manifest_base, never in the deploy path"
        );
    }

    #[test]
    fn file_kind_deploys_at_prefixed_layout_path() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::Prefixed);
        let rec = record("agents-src", "CLAUDE.md", "prefixed", RecordKind::File);
        let dir_twin = record("agents-src", "CLAUDE.md", "prefixed", RecordKind::Dir);

        let path = record_artifact_path(&target, &rec);

        assert_eq!(
            path,
            root.join("agents-src-CLAUDE.md"),
            "a prefixed file artifact deploys at the separator-joined layout path"
        );
        assert_eq!(
            path,
            record_artifact_path(&target, &dir_twin),
            "a file artifact's deploy path must track layout EXACTLY as its dir twin does — \
             the File-vs-Dir divergence lives only in record_manifest_base, never in the deploy path"
        );
    }

    #[test]
    fn history_record_deploys_at_binding_identity_without_by_source_composition() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::BySource);
        let identity = "dotfiles";
        let rec = ArtifactRecord {
            history: true,
            ..record(identity, identity, "by-source", RecordKind::Dir)
        };

        assert_eq!(record_artifact_path(&target, &rec), root.join(identity));
    }

    #[test]
    fn dir_kind_deploys_at_by_source_layout_path_unchanged() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::BySource);
        let rec = record("dotfiles", "nvim", "by-source", RecordKind::Dir);

        let path = record_artifact_path(&target, &rec);

        assert_eq!(
            path,
            root.join("dotfiles").join("nvim"),
            "a dir artifact's deploy path is unchanged: the layout path for its identity"
        );
    }

    #[test]
    fn file_kind_manifest_base_is_the_parent_of_the_deployed_file() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::BySource);
        let rec = record("agents-src", "CLAUDE.md", "by-source", RecordKind::File);

        let base = record_manifest_base(&target, &rec);

        assert_eq!(
            base,
            root.join("agents-src"),
            "a file record's manifest base is the PARENT of the deployed file, so its single \
             manifest entry joins to the file itself"
        );
        assert_eq!(
            base.join(&rec.files[0].path),
            record_artifact_path(&target, &rec),
            "manifest_base joined with the manifest file path must reconstruct the deployed file"
        );
    }

    #[test]
    fn file_kind_manifest_base_reconstructs_prefixed_path() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::Prefixed);
        let rec = ArtifactRecord {
            version: 1,
            key: ArtifactKey {
                target: "dest".to_owned(),
                source: "agents-src".to_owned(),
                artifact: "CLAUDE.md".to_owned(),
            },
            source: "agents-src".to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "prefixed".to_owned(),
            kind: RecordKind::File,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("agents-src-CLAUDE.md"),
                size: 4,
                mtime: 0,
                blake3: "blake3:d4e5f6".to_owned(),
            }],
            linked: false,
            history: false,
            vars_digest: None,
            deploy_root: None,
            layout_separator: None,
        };

        let base = record_manifest_base(&target, &rec);

        assert_eq!(
            base,
            root.to_path_buf(),
            "a prefixed file record's manifest base is the PARENT (the target root), since the \
             deployed file's basename is the full prefixed name `agents-src-CLAUDE.md`"
        );
        assert_eq!(
            base.join(&rec.files[0].path),
            record_artifact_path(&target, &rec),
            "manifest_base joined with the FULL prefixed manifest path must reconstruct the deployed \
             file — leaving the manifest path as bare `CLAUDE.md` would join to the wrong location"
        );
    }

    #[test]
    fn dir_kind_manifest_base_is_the_deployed_directory() {
        let root = Path::new("/home/u/dest");
        let target = target_with_layout(root, LayoutKind::BySource);
        let rec = record("dotfiles", "nvim", "by-source", RecordKind::Dir);

        let base = record_manifest_base(&target, &rec);

        assert_eq!(
            base,
            record_artifact_path(&target, &rec),
            "a dir record's manifest base IS the deployed directory, so file paths join under it"
        );
    }
}

#[cfg(test)]
mod revalidated_treated_like_clean_tests {
    use super::*;

    fn managed(condition: ManagedCondition) -> ObservedArtifact<()> {
        ObservedArtifact::Managed(crate::sync::model::ManagedArtifact {
            record: (),
            condition,
        })
    }

    #[test]
    fn revalidated_artifact_is_not_a_conflict() {
        let revalidated = reconciled_test_change(
            managed(ManagedCondition::MetadataChangedButContentClean {
                refreshed: Vec::new(),
            }),
            ReconcileTestPolicy::STANDARD,
        );

        assert!(
            revalidated.is_none(),
            "a revalidated artifact only carries refreshed stats; sync must NOT treat it as a \
             conflict (got {revalidated:?})"
        );

        let modified = reconciled_test_change(
            managed(ManagedCondition::Modified {
                changed: vec![PathBuf::from("a.txt")],
            }),
            ReconcileTestPolicy::STANDARD,
        );
        assert!(
            matches!(
                modified,
                Some(SyncChange::Conflict {
                    kind: ConflictKind::Modified { .. },
                    ..
                })
            ),
            "positive control: the live reconcile seam classifies a non-forced Modified artifact \
             as a conflict (got {modified:?})"
        );

        let foreign = reconciled_test_change(
            ObservedArtifact::Foreign(PathBuf::from("/tmp/dst/a.txt")),
            ReconcileTestPolicy::STANDARD,
        );
        assert!(
            matches!(
                foreign,
                Some(SyncChange::Conflict {
                    kind: ConflictKind::Foreign,
                    ..
                })
            ),
            "positive control: a non-forced Foreign artifact IS a conflict (got {foreign:?})"
        );
    }

    #[test]
    fn revalidated_state_reconciles_like_clean() {
        assert!(
            reconciled_test_change(
                managed(ManagedCondition::MetadataChangedButContentClean {
                    refreshed: Vec::new(),
                }),
                ReconcileTestPolicy::STANDARD,
            )
            .is_none(),
            "a revalidated artifact returns to the caller WITHOUT writing, exactly like Clean; the \
             early-skip set must include it"
        );
        assert!(
            reconciled_test_change(
                managed(ManagedCondition::Clean),
                ReconcileTestPolicy::STANDARD,
            )
            .is_none(),
            "premise: Clean is a no-op redeploy"
        );
        assert!(
            matches!(
                reconciled_test_change(
                    ObservedArtifact::Foreign(PathBuf::from("/tmp/dst/a.txt")),
                    ReconcileTestPolicy::STANDARD,
                ),
                Some(SyncChange::Conflict { .. })
            ),
            "a foreign artifact is not a silent no-op skip"
        );
    }
}

#[cfg(test)]
mod mode_transition_conflict_tests {
    use super::*;

    fn managed(condition: ManagedCondition) -> ObservedArtifact<()> {
        ObservedArtifact::Managed(crate::sync::model::ManagedArtifact {
            record: (),
            condition,
        })
    }

    fn modified_state() -> ObservedArtifact<()> {
        managed(ManagedCondition::Modified {
            changed: vec![PathBuf::from("a.txt")],
        })
    }

    #[test]
    fn mode_transition_onto_modified_is_a_conflict() {
        let kind = reconciled_test_change(modified_state(), ReconcileTestPolicy::MODE_TRANSITION);
        assert!(
            matches!(
                kind,
                Some(SyncChange::Conflict {
                    kind: ConflictKind::Modified { .. },
                    ..
                })
            ),
            "a mode transition onto a locally-modified managed artifact must still classify a \
             Modified conflict (skip unless --force); the mode_transition arm must not swallow it \
             (got {kind:?})"
        );
    }

    #[test]
    fn mode_transition_onto_foreign_is_a_conflict() {
        let kind = reconciled_test_change(
            ObservedArtifact::Foreign(PathBuf::from("/tmp/dst/a.txt")),
            ReconcileTestPolicy::MODE_TRANSITION,
        );
        assert!(
            matches!(
                kind,
                Some(SyncChange::Conflict {
                    kind: ConflictKind::Foreign,
                    ..
                })
            ),
            "a mode transition onto unmanaged (Foreign) content must still classify a Foreign \
             conflict, not silently replace it with a symlink (got {kind:?})"
        );
    }

    #[test]
    fn force_overrides_a_transition_conflict() {
        assert!(
            matches!(
                reconciled_test_change(
                    modified_state(),
                    ReconcileTestPolicy::FORCED_MODE_TRANSITION,
                ),
                Some(SyncChange::Overwrite { .. })
            ),
            "--force must still apply a mode transition over Modified content (no conflict)"
        );
        assert!(
            matches!(
                reconciled_test_change(
                    ObservedArtifact::Foreign(PathBuf::from("/tmp/dst/a.txt")),
                    ReconcileTestPolicy::FORCED_MODE_TRANSITION,
                ),
                Some(SyncChange::Overwrite { .. })
            ),
            "--force must still apply a mode transition over Foreign content (no conflict)"
        );
    }

    #[test]
    fn clean_like_transitions_stay_silent() {
        for state in [
            managed(ManagedCondition::Clean),
            managed(ManagedCondition::Linked),
            managed(ManagedCondition::MetadataChangedButContentClean {
                refreshed: Vec::new(),
            }),
        ] {
            let change = reconciled_test_change(state, ReconcileTestPolicy::MODE_TRANSITION);
            assert!(
                matches!(
                    change,
                    Some(SyncChange::Deploy { .. } | SyncChange::Overwrite { .. })
                ),
                "a clean-like mode transition must proceed as non-conflicting deploy work; only \
                 Modified/Foreign destinations conflict (got {change:?})"
            );
        }
    }
}
