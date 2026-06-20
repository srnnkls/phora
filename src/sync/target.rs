use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{DeployMode, LayoutConfig, LayoutKind, ParsedSource, Target, TemplateOptIn};
use crate::deploy::{ArtifactState, Journal, check_artifact_state, deploy_artifact, link_artifact};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Selection, SourceName, locator_basename};
use crate::lock::encode_ref;
use crate::source::{ExportRequest, SourceBackend, SourceError};
use crate::store::{
    ArtifactKey, EjectedEntry, ProjectedRecord, RecordKind, Registry, RegistryRecord,
};

use super::discover::{LinkArtifact, discover_link_artifacts};
use super::{
    Conflict, ConflictKind, ConflictResolver, Resolution, StagingGuard, nonce, remote_for,
    target_parent,
};

#[derive(Clone, Copy)]
pub(super) struct TargetRun<'a> {
    pub(super) parsed: &'a BTreeMap<String, ParsedSource>,
    pub(super) target_name: &'a str,
    pub(super) target: &'a Target,
    pub(super) commits: &'a BTreeMap<(String, String), String>,
    pub(super) remotes: &'a BTreeMap<String, String>,
    pub(super) force: bool,
    pub(super) interactive: bool,
    pub(super) resolver: Option<&'a dyn ConflictResolver>,
    pub(super) vars: &'a BTreeMap<String, String>,
}

pub(super) struct TargetDeploy {
    pub(super) had_failures: bool,
    pub(super) warnings: Vec<String>,
}

pub(super) fn deploy_target(
    run: TargetRun<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<TargetDeploy> {
    let target_path = run.target.expanded_path();
    let layout = run.target.layout();
    let ejected = registry.load_ejected(run.target_name)?;

    let PlannedBindings {
        bindings,
        mut warnings,
    } = plan_bindings(&run, backend)?;

    let mut seen_dest: BTreeMap<PathBuf, String> = BTreeMap::new();
    for binding in &bindings {
        for artifact in &binding.artifacts {
            let dst = artifact_dst_for(
                &target_path,
                &layout,
                &binding.identity,
                &artifact.name,
                artifact.deploy_rel.as_deref(),
            );
            if let Some(other) = seen_dest.insert(dst, binding.identity.clone()) {
                return Err(Error::Collision {
                    artifact: artifact.name.as_str().to_owned(),
                    sources: vec![other, binding.identity.clone()],
                    target: run.target_name.to_owned(),
                });
            }
        }
    }

    let mut had_failures = false;
    for binding in &bindings {
        let source_name = SourceName::trusted(&binding.underlying_source);
        for artifact in &binding.artifacts {
            let artifact_dst = artifact_dst_for(
                &target_path,
                &layout,
                &binding.identity,
                &artifact.name,
                artifact.deploy_rel.as_deref(),
            );
            let dst_is_symlink =
                std::fs::symlink_metadata(&artifact_dst).is_ok_and(|m| m.file_type().is_symlink());
            let mode_transition = match binding.source.deploy_mode() {
                DeployMode::Link => artifact_dst.exists() && !dst_is_symlink,
                DeployMode::Copy => dst_is_symlink,
            };
            let entry = ArtifactEntry {
                source: binding.source,
                git: &binding.git,
                source_name: &source_name,
                identity: &binding.identity,
                underlying_source: &binding.underlying_source,
                root: binding.root,
                commit: &binding.commit,
                selection: &binding.selection,
                artifact_name: &artifact.name,
                target_path: &target_path,
                layout_kind: layout.kind,
                ejected: &ejected,
                mode_transition,
                template_opt_in: &binding.template_opt_in,
                mapped_source_key: artifact.mapped_source_key.as_deref(),
                deploy_rel: artifact.deploy_rel.as_deref(),
                link_kind: artifact.link_kind,
                source_locator: artifact.source_locator.as_deref(),
            };
            let outcome = deploy_artifact_entry(run, &entry, backend, registry, journal)?;
            had_failures |= outcome.had_failure;
            warnings.extend(outcome.warning);
        }
    }

    Ok(TargetDeploy {
        had_failures,
        warnings,
    })
}

struct PlannedArtifact {
    name: ArtifactName,
    mapped_source_key: Option<String>,
    /// Nested map dest under the layout dir; the record still keys on `name` (its basename).
    deploy_rel: Option<PathBuf>,
    link_kind: RecordKind,
    /// Source relpath under the effective root that discovery matched; the link-target leaf.
    source_locator: Option<String>,
}

struct PlannedBinding<'a> {
    source: &'a ParsedSource,
    identity: String,
    underlying_source: String,
    git: String,
    commit: String,
    root: Option<&'a Path>,
    selection: Selection,
    template_opt_in: TemplateOptIn,
    artifacts: Vec<PlannedArtifact>,
}

struct PlannedBindings<'a> {
    bindings: Vec<PlannedBinding<'a>>,
    warnings: Vec<String>,
}

fn plan_bindings<'a>(
    run: &TargetRun<'a>,
    backend: &dyn SourceBackend,
) -> Result<PlannedBindings<'a>> {
    let mut planned = Vec::new();
    let mut warnings = Vec::new();
    for binding in run.target.resolve_sources(run.parsed) {
        let source = run.parsed.get(binding.source).ok_or_else(|| {
            Error::Config(format!(
                "target references undefined source: {}",
                binding.source
            ))
        })?;
        let commit_key = (
            binding.source.to_owned(),
            encode_ref(&binding.effective_ref),
        );
        let commit = run.commits.get(&commit_key).ok_or_else(|| {
            Error::Sync(format!(
                "no resolved commit for {} at {}",
                binding.source, binding.effective_ref
            ))
        })?;
        let git = remote_for(run.remotes, binding.source)?;
        let source_name = SourceName::trusted(binding.source);
        let selection = Selection::new(binding.include, binding.exclude)?;

        let artifacts = if let Some(map) = binding.map {
            map.iter()
                .map(|(key, dest)| PlannedArtifact {
                    name: ArtifactName::trusted(locator_basename(dest)),
                    mapped_source_key: Some(key.as_str().to_owned()),
                    deploy_rel: Some(PathBuf::from(dest.as_str())),
                    link_kind: RecordKind::File,
                    source_locator: None,
                })
                .collect()
        } else {
            let discovered = discover_link_artifacts(
                source,
                git,
                &source_name,
                commit,
                backend,
                &selection,
                binding.root,
            )?;
            for entry in binding.include {
                if !entry_matched_any(entry, &discovered)? {
                    warnings.push(format!(
                        "target `{}` binding `{}`: include `{entry}` matched nothing in the source tree",
                        run.target_name, binding.identity,
                    ));
                }
            }
            discovered
                .into_iter()
                .map(|a| PlannedArtifact {
                    name: a.name,
                    mapped_source_key: None,
                    deploy_rel: None,
                    link_kind: a.kind,
                    source_locator: Some(a.locator),
                })
                .collect()
        };

        planned.push(PlannedBinding {
            source,
            identity: binding.identity.to_owned(),
            underlying_source: binding.source.to_owned(),
            git: git.to_owned(),
            commit: commit.clone(),
            root: binding.root,
            selection,
            template_opt_in: binding.template_opt_in,
            artifacts,
        });
    }
    Ok(PlannedBindings {
        bindings: planned,
        warnings,
    })
}

pub(super) struct ArtifactEntry<'a> {
    pub(super) source: &'a ParsedSource,
    pub(super) git: &'a str,
    pub(super) source_name: &'a SourceName,
    pub(super) identity: &'a str,
    pub(super) underlying_source: &'a str,
    pub(super) root: Option<&'a Path>,
    pub(super) commit: &'a str,
    pub(super) selection: &'a Selection,
    pub(super) artifact_name: &'a ArtifactName,
    pub(super) target_path: &'a Path,
    pub(super) layout_kind: LayoutKind,
    pub(super) ejected: &'a [EjectedEntry],
    pub(super) mode_transition: bool,
    pub(super) template_opt_in: &'a TemplateOptIn,
    /// Source-side key for a mapped leaf; `None` for layout-routed artifacts.
    pub(super) mapped_source_key: Option<&'a str>,
    /// Nested map dest under the layout dir; deploy honors it, the record keys on the basename.
    pub(super) deploy_rel: Option<&'a Path>,
    /// Link-mode `RecordKind` from discovery's working-tree scan, not a live re-stat.
    pub(super) link_kind: RecordKind,
    /// Source relpath under the effective root that discovery matched; the link-target leaf.
    pub(super) source_locator: Option<&'a str>,
}

fn entry_matched_any(entry: &str, discovered: &[LinkArtifact]) -> Result<bool> {
    let probe = Selection::new(std::slice::from_ref(&entry.to_owned()), &[])?;
    let nested: Vec<&str> = probe
        .nested_locators()
        .iter()
        .map(|l| locator_basename(l))
        .collect();
    Ok(discovered.iter().any(|a| {
        let name = a.name.as_str();
        probe.selects_artifact(name) || nested.contains(&name)
    }))
}

/// The artifact's deployed location at its layout path (a file for `kind=file`, a dir for `kind=dir`).
pub(crate) fn record_artifact_path(target: &Target, record: &RegistryRecord) -> PathBuf {
    let suffix = record
        .deploy_rel
        .as_deref()
        .map_or_else(|| PathBuf::from(&record.key.artifact), Path::to_path_buf);
    target.expanded_path().join(
        target
            .layout()
            .artifact_path(&record.key.source, &suffix.to_string_lossy()),
    )
}

/// Base to join manifest file paths against: a file record's base is the deployed file's parent; a dir record's is the deployed dir.
pub(super) fn record_manifest_base(target: &Target, record: &RegistryRecord) -> PathBuf {
    let path = record_artifact_path(target, record);
    match record.kind {
        RecordKind::File => path.parent().map_or(path.clone(), Path::to_path_buf),
        RecordKind::Dir => path,
    }
}

/// Every artifact (file/map or dir) deploys at its layout path for `identity`.
fn artifact_dst_for(
    target_path: &Path,
    layout: &LayoutConfig,
    identity: &str,
    artifact_name: &ArtifactName,
    deploy_rel: Option<&Path>,
) -> PathBuf {
    let suffix =
        deploy_rel.map_or_else(|| PathBuf::from(artifact_name.as_str()), Path::to_path_buf);
    target_path.join(layout.artifact_path(identity, &suffix.to_string_lossy()))
}

fn artifact_dst_for_entry(run: TargetRun<'_>, entry: &ArtifactEntry<'_>) -> PathBuf {
    artifact_dst_for(
        entry.target_path,
        &run.target.layout(),
        entry.identity,
        entry.artifact_name,
        entry.deploy_rel,
    )
}

pub(super) struct DeployOutcome {
    pub(super) had_failure: bool,
    pub(super) warning: Option<String>,
}

impl DeployOutcome {
    fn clean() -> Self {
        Self {
            had_failure: false,
            warning: None,
        }
    }

    fn failed() -> Self {
        Self {
            had_failure: true,
            warning: None,
        }
    }

    fn warned(message: String) -> Self {
        Self {
            had_failure: false,
            warning: Some(message),
        }
    }
}

pub(super) fn deploy_artifact_entry(
    run: TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<DeployOutcome> {
    let artifact_dst = artifact_dst_for_entry(run, entry);
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.identity.to_owned(),
        artifact: entry.artifact_name.as_str().to_owned(),
    };

    let expected_vars_digest = expected_vars_digest(entry, backend, registry, &key, run.vars)?;
    let state = check_artifact_state(
        &artifact_dst,
        entry.identity,
        entry.commit,
        entry.ejected,
        entry.artifact_name.as_str(),
        registry,
        &key,
        expected_vars_digest.as_deref(),
    )?;

    let conflict_kind = conflict_kind_for(&state, entry, run.force);

    let deploy = |key: ArtifactKey| match entry.source.deploy_mode() {
        DeployMode::Link => deploy_link(registry, journal, entry, &artifact_dst, key),
        DeployMode::Copy => deploy_one(
            backend,
            registry,
            journal,
            DeployContext {
                target_path: entry.target_path,
                layout_kind: entry.layout_kind,
                source: entry.source,
                git: entry.git,
                source_name: entry.source_name,
                underlying_source: entry.underlying_source,
                root: entry.root,
                commit: entry.commit,
                selection: entry.selection,
                artifact_name: entry.artifact_name,
                artifact_dst: &artifact_dst,
                key,
                template_opt_in: entry.template_opt_in,
                vars: run.vars,
                mapped_source_key: entry.mapped_source_key,
                deploy_rel: entry.deploy_rel,
            },
        ),
    };

    let resolution = match conflict_kind {
        None if !entry.mode_transition
            && matches!(
                state,
                ArtifactState::Ejected | ArtifactState::Clean | ArtifactState::Linked
            ) =>
        {
            return Ok(DeployOutcome::clean());
        }
        None => Resolution::Overwrite,
        Some(kind) => match run.resolver {
            Some(resolver) if run.interactive => resolver.resolve(&Conflict {
                target: run.target_name.to_owned(),
                source: entry.identity.to_owned(),
                artifact: entry.artifact_name.as_str().to_owned(),
                kind,
            }),
            _ => {
                warn_skip(
                    entry.identity,
                    entry.artifact_name.as_str(),
                    &kind,
                    &artifact_dst,
                );
                Resolution::Skip
            }
        },
    };

    apply_resolution(run, entry, registry, resolution, key, deploy)
}

fn apply_resolution(
    run: TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    registry: &dyn Registry,
    resolution: Resolution,
    key: ArtifactKey,
    deploy: impl FnOnce(ArtifactKey) -> Result<()>,
) -> Result<DeployOutcome> {
    match resolution {
        Resolution::Skip => Ok(DeployOutcome::clean()),
        Resolution::Overwrite => match deploy(key) {
            Ok(()) => Ok(DeployOutcome::clean()),
            Err(Error::SourceCtx(SourceError::MappedKeyNotFound { key })) => {
                Ok(DeployOutcome::warned(format!(
                    "target `{}` binding `{}`: map key `{}` matched nothing in the source tree",
                    run.target_name,
                    entry.identity,
                    key.display(),
                )))
            }
            Err(e) => {
                eprintln!(
                    "phora: failed to deploy {}:{}: {e}",
                    entry.identity, entry.artifact_name
                );
                Ok(DeployOutcome::failed())
            }
        },
        Resolution::Eject => {
            let mut ejected = registry.load_ejected(run.target_name)?;
            ejected.push(EjectedEntry {
                source: entry.identity.to_owned(),
                artifact: entry.artifact_name.as_str().to_owned(),
                ejected_at: chrono::Utc::now().to_rfc3339(),
            });
            registry.save_ejected(run.target_name, &ejected)?;
            Ok(DeployOutcome::clean())
        }
        Resolution::Abort => Err(Error::Aborted),
    }
}

/// `check_artifact_state` compares this only when `record.vars_digest.is_some()`; that lets a
/// non-templated record skip the git-tree walk here and still resolve Clean (INV-8).
fn expected_vars_digest(
    entry: &ArtifactEntry<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    key: &ArtifactKey,
    vars: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    if !matches!(entry.source.deploy_mode(), DeployMode::Copy) {
        return Ok(None);
    }
    let Some(record) = registry.get(key)? else {
        return Ok(None);
    };
    if record.linked || record.vars_digest.is_none() {
        return Ok(None);
    }
    let files = backend.list_artifact_files(
        entry.source_name,
        entry.git,
        entry.commit,
        entry.root,
        entry.artifact_name,
        entry.selection,
    )?;
    let templated = files.iter().any(|p| {
        entry
            .template_opt_in
            .renders(&p.to_string_lossy().replace('\\', "/"))
    });
    Ok(templated.then(|| crate::source::vars_digest(vars)))
}

fn conflict_kind_for(
    state: &ArtifactState,
    entry: &ArtifactEntry<'_>,
    force: bool,
) -> Option<ConflictKind> {
    match state {
        _ if entry.mode_transition => None,
        ArtifactState::Modified { changed } if !force => Some(ConflictKind::Modified {
            changed: changed.clone(),
        }),
        ArtifactState::Foreign if !force => Some(ConflictKind::Foreign),
        _ => None,
    }
}

fn warn_skip(source: &str, artifact: &str, kind: &ConflictKind, dst: &Path) {
    match kind {
        ConflictKind::Modified { changed } => {
            eprintln!("phora: skipping locally modified {source}:{artifact}");
            for path in changed {
                eprintln!("    {}", path.display());
            }
            eprintln!("  use --force to overwrite");
        }
        ConflictKind::Foreign => {
            eprintln!(
                "phora: skipping foreign content at {}; use --force to overwrite",
                dst.display()
            );
        }
    }
}

struct DeployContext<'a> {
    target_path: &'a Path,
    layout_kind: LayoutKind,
    source: &'a ParsedSource,
    git: &'a str,
    source_name: &'a SourceName,
    underlying_source: &'a str,
    root: Option<&'a Path>,
    commit: &'a str,
    selection: &'a Selection,
    artifact_name: &'a ArtifactName,
    artifact_dst: &'a Path,
    key: ArtifactKey,
    template_opt_in: &'a TemplateOptIn,
    vars: &'a BTreeMap<String, String>,
    mapped_source_key: Option<&'a str>,
    deploy_rel: Option<&'a Path>,
}

fn deploy_one(
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
    ctx: DeployContext<'_>,
) -> Result<()> {
    let staging_base = target_parent(ctx.target_path).join(".phora-stage");
    let staging = staging_base.join(format!("{}-{}", ctx.artifact_name, nonce()));
    let mut staging_guard = StagingGuard::new(&staging_base, &staging);

    let git = ctx.git;
    let commit_time = backend.commit_time(ctx.source_name, git, ctx.commit)?;
    let policy = ctx.source.export_policy();
    let path_map = ctx.mapped_source_key.map(|key| {
        let dest = ctx.deploy_rel.map_or_else(
            || PathBuf::from(ctx.artifact_name.as_str()),
            Path::to_path_buf,
        );
        BTreeMap::from([(PathBuf::from(key), dest)])
    });
    let req = ExportRequest {
        source: ctx.source_name,
        url: git,
        commit: ctx.commit,
        root: ctx.root,
        artifact: ctx.artifact_name,
        selection: ctx.selection,
        policy: &policy,
        staging_dir: &staging,
        commit_time,
        template_opt_in: ctx.template_opt_in,
        vars: ctx.vars,
        path_map: path_map.as_ref(),
    };
    let export = backend.export_artifact(&req)?;

    if let Some(parent) = ctx.artifact_dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Sync(format!("create target dir {}: {e}", parent.display())))?;
    }

    let staged_file = if export.kind == RecordKind::File {
        let mf = export.files.first().ok_or_else(|| {
            Error::Sync("kind=file export must have exactly one staged file".into())
        })?;
        Some(staging.join(&mf.path))
    } else {
        None
    };

    let record = RegistryRecord::projected(ProjectedRecord {
        key: ctx.key,
        underlying_source: ctx.underlying_source,
        commit: ctx.commit,
        digest: export.digest,
        layout: format!("{:?}", ctx.layout_kind).to_lowercase(),
        kind: export.kind,
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: export.files,
        vars_digest: export.vars_digest,
        deploy_rel: ctx.deploy_rel.map(Path::to_path_buf),
    });

    // Guard stays armed for a single-file move: deploy_artifact only takes the file, leaving the staging dir for the guard to reap.
    if let Some(staged_file) = staged_file {
        return deploy_artifact(
            &staging_base,
            &staged_file,
            ctx.artifact_dst,
            record,
            journal,
            registry,
        );
    }

    staging_guard.disarm();
    deploy_artifact(
        &staging_base,
        &staging,
        ctx.artifact_dst,
        record,
        journal,
        registry,
    )
}

fn deploy_link(
    registry: &dyn Registry,
    journal: &Journal,
    entry: &ArtifactEntry<'_>,
    artifact_dst: &Path,
    key: ArtifactKey,
) -> Result<()> {
    let policy = entry.source.export_policy();
    let link_target = link_target(entry);
    let kind = entry.link_kind;
    let record = RegistryRecord {
        version: 1,
        key,
        source: entry.underlying_source.to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: format!("{:?}", entry.layout_kind).to_lowercase(),
        kind,
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
        vars_digest: None,
        deploy_rel: entry.deploy_rel.map(Path::to_path_buf),
    };
    let staging_base = target_parent(entry.target_path).join(".phora-stage");
    link_artifact(
        &staging_base,
        artifact_dst,
        &link_target,
        record,
        journal,
        registry,
    )
}

/// Absolute working-tree path the symlink points at: `<remote>/<root>/<leaf>`, where
/// `<leaf>` is the mapped source key, else the matched source locator, else the artifact name.
fn link_target(entry: &ArtifactEntry<'_>) -> PathBuf {
    let base = Path::new(entry.git);
    let mut target = if base.is_absolute() {
        base.to_path_buf()
    } else {
        base.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir().map_or_else(|_| base.to_path_buf(), |c| c.join(base))
        })
    };
    if let Some(root) = entry.root {
        target.push(root);
    }
    target.push(
        entry
            .mapped_source_key
            .or(entry.source_locator)
            .unwrap_or_else(|| entry.artifact_name.as_str()),
    );
    target
}
