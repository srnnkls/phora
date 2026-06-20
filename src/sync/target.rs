use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{DeployMode, LayoutConfig, LayoutKind, ParsedSource, Target, TemplateOptIn};
use crate::deploy::{ArtifactState, Journal, check_artifact_state, deploy_artifact, link_artifact};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Selection, SourceName};
use crate::lock::encode_ref;
use crate::source::{ExportRequest, SourceBackend};
use crate::store::{
    ArtifactKey, EjectedEntry, MAP_LAYOUT, ProjectedRecord, Registry, RegistryRecord,
};

use super::confine::{ProtectedPathSet, confine_destination};
use super::discover::discover_artifacts_for_source;
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

pub(super) fn deploy_target(
    run: TargetRun<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<bool> {
    let target_path = run.target.expanded_path();
    let layout = run.target.layout();
    let ejected = registry.load_ejected(run.target_name)?;
    let mut seen_dest: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut had_failures = false;

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

        let mut deploy_one_entry = |artifact_name: &ArtifactName,
                                    mapped_source_key: Option<&str>|
         -> Result<()> {
            let artifact_dst = artifact_dst_for(
                &target_path,
                &layout,
                binding.identity,
                artifact_name,
                mapped_source_key,
            );
            if let Some(other) = seen_dest.insert(artifact_dst.clone(), binding.identity.to_owned())
            {
                return Err(Error::Collision {
                    artifact: artifact_name.as_str().to_owned(),
                    sources: vec![other, binding.identity.to_owned()],
                    target: run.target_name.to_owned(),
                });
            }
            let dst_is_symlink =
                std::fs::symlink_metadata(&artifact_dst).is_ok_and(|m| m.file_type().is_symlink());
            let mode_transition = match source.deploy_mode() {
                DeployMode::Link => artifact_dst.exists() && !dst_is_symlink,
                DeployMode::Copy => dst_is_symlink,
            };
            let entry = ArtifactEntry {
                source,
                git,
                source_name: &source_name,
                identity: binding.identity,
                underlying_source: binding.source,
                root: binding.root,
                commit,
                selection: &selection,
                artifact_name,
                target_path: &target_path,
                layout_kind: layout.kind,
                ejected: &ejected,
                mode_transition,
                template_opt_in: &binding.template_opt_in,
                mapped_source_key,
            };
            had_failures |= deploy_artifact_entry(run, &entry, backend, registry, journal)?;
            Ok(())
        };

        if let Some(map) = binding.map {
            for (key, dest) in map {
                deploy_one_entry(&ArtifactName::trusted(dest.as_str()), Some(key.as_str()))?;
            }
            continue;
        }

        let discovered = discover_artifacts_for_source(
            source,
            git,
            &source_name,
            commit,
            backend,
            &selection,
            binding.root,
        )?;

        for artifact_name in discovered {
            deploy_one_entry(&artifact_name, None)?;
        }
    }

    Ok(had_failures)
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
}

/// A `map`-layout dest is a single component at the target root; layout helpers must not be applied.
pub(crate) fn record_artifact_path(target: &Target, record: &RegistryRecord) -> PathBuf {
    if record.layout == MAP_LAYOUT {
        target.expanded_path().join(&record.key.artifact)
    } else {
        target.expanded_path().join(
            target
                .layout()
                .artifact_path(&record.key.source, &record.key.artifact),
        )
    }
}

/// A `map` record's single manifest file IS the dest, so its base is the target root.
pub(super) fn record_manifest_base(target: &Target, record: &RegistryRecord) -> PathBuf {
    if record.layout == MAP_LAYOUT {
        target.expanded_path()
    } else {
        record_artifact_path(target, record)
    }
}

/// Mapped leaves land as a renamed FILE at the target root, ignoring layout.
fn artifact_dst_for(
    target_path: &Path,
    layout: &LayoutConfig,
    identity: &str,
    artifact_name: &ArtifactName,
    mapped_source_key: Option<&str>,
) -> PathBuf {
    if mapped_source_key.is_some() {
        target_path.join(artifact_name.as_str())
    } else {
        target_path.join(layout.artifact_path(identity, artifact_name.as_str()))
    }
}

fn artifact_dst_for_entry(run: TargetRun<'_>, entry: &ArtifactEntry<'_>) -> PathBuf {
    artifact_dst_for(
        entry.target_path,
        &run.target.layout(),
        entry.identity,
        entry.artifact_name,
        entry.mapped_source_key,
    )
}

pub(super) fn deploy_artifact_entry(
    run: TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<bool> {
    let artifact_dst = run.confined(&artifact_dst_for_entry(run, entry))?;
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
                confine_anchor: run.target.confine.as_deref(),
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
            return Ok(false);
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

    match resolution {
        Resolution::Skip => Ok(false),
        Resolution::Overwrite => match deploy(key) {
            Ok(()) => Ok(false),
            Err(e) => {
                eprintln!(
                    "phora: failed to deploy {}:{}: {e}",
                    entry.identity, entry.artifact_name
                );
                Ok(true)
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
            Ok(false)
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
    confine_anchor: Option<&'a Path>,
}

fn deploy_one(
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
    ctx: DeployContext<'_>,
) -> Result<()> {
    let staging_base = target_parent(ctx.artifact_dst).join(".phora-stage");
    let staging = staging_base.join(format!("{}-{}", ctx.artifact_name, nonce()));
    let mut staging_guard = StagingGuard::new(&staging_base, &staging);

    let git = ctx.git;
    let commit_time = backend.commit_time(ctx.source_name, git, ctx.commit)?;
    let policy = ctx.source.export_policy();
    let path_map = ctx.mapped_source_key.map(|key| {
        BTreeMap::from([(
            PathBuf::from(key),
            PathBuf::from(ctx.artifact_name.as_str()),
        )])
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

    if let Some(anchor) = ctx.confine_anchor {
        super::confine::reject_symlink_ancestor_at_write(anchor, ctx.artifact_dst)?;
    }

    if let Some(parent) = ctx.artifact_dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Sync(format!("create target dir {}: {e}", parent.display())))?;
    }

    let record = RegistryRecord::projected(ProjectedRecord {
        key: ctx.key,
        underlying_source: ctx.underlying_source,
        commit: ctx.commit,
        digest: export.digest,
        layout: if ctx.mapped_source_key.is_some() {
            MAP_LAYOUT.to_owned()
        } else {
            format!("{:?}", ctx.layout_kind).to_lowercase()
        },
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: export.files,
        vars_digest: export.vars_digest,
    });

    // Guard stays armed: deploy_artifact only moves the file, so the guard reaps the empty staging dir.
    if ctx.mapped_source_key.is_some() {
        return deploy_artifact(
            &staging_base,
            &staging.join(ctx.artifact_name.as_str()),
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
    let record = RegistryRecord {
        version: 1,
        key,
        source: entry.underlying_source.to_owned(),
        commit: "link".to_owned(),
        digest: "link:".to_owned(),
        projected_at: chrono::Utc::now().to_rfc3339(),
        layout: if entry.mapped_source_key.is_some() {
            MAP_LAYOUT.to_owned()
        } else {
            format!("{:?}", entry.layout_kind).to_lowercase()
        },
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
        vars_digest: None,
    };
    let staging_base = target_parent(artifact_dst).join(".phora-stage");
    link_artifact(
        &staging_base,
        artifact_dst,
        &link_target(entry),
        record,
        journal,
        registry,
    )
}

/// Absolute working-tree path the symlink points at: `<remote>/<root>/<leaf>`, where
/// `<leaf>` is the mapped source key for a mapped binding, else the artifact name.
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
            .unwrap_or_else(|| entry.artifact_name.as_str()),
    );
    target
}

#[cfg(test)]
mod confine_fail_closed_tests {
    #![allow(clippy::too_many_arguments)]
    use super::*;
    use crate::config::Target;
    use crate::sync::ConflictResolver;

    struct NeverResolve;
    impl ConflictResolver for NeverResolve {
        fn resolve(&self, _conflict: &Conflict) -> Resolution {
            Resolution::Skip
        }
    }

    fn composed_target_without_anchor(dst: &Path) -> Target {
        Target {
            path: dst.to_path_buf(),
            sources: None,
            layout: None,
            hooks: None,
            imports: None,
            confine: None,
        }
    }

    fn run_for<'a>(
        target: &'a Target,
        target_name: &'a str,
        protected: &'a ProtectedPathSet,
        parsed: &'a BTreeMap<String, ParsedSource>,
        commits: &'a BTreeMap<(String, String), String>,
        remotes: &'a BTreeMap<String, String>,
        vars: &'a BTreeMap<String, String>,
        resolver: &'a dyn ConflictResolver,
    ) -> TargetRun<'a> {
        TargetRun {
            parsed,
            target_name,
            target,
            commits,
            remotes,
            force: false,
            interactive: false,
            resolver: Some(resolver),
            vars,
            protected,
        }
    }

    #[test]
    fn composed_target_missing_its_confine_anchor_fails_closed() {
        let outside = Path::new("/home/u/.ssh/authorized_keys");
        let target = composed_target_without_anchor(outside);
        let protected = ProtectedPathSet::resolve(Path::new("/home/u/proj")).expect("protected");
        let parsed = BTreeMap::new();
        let commits = BTreeMap::new();
        let remotes = BTreeMap::new();
        let vars = BTreeMap::new();
        let resolver = NeverResolve;
        let run = run_for(
            &target,
            "root%1%nvim",
            &protected,
            &parsed,
            &commits,
            &remotes,
            &vars,
            &resolver,
        );

        run.confined(outside).expect_err(
            "a composed/transitive target (namespaced name carries `%`) reaching deploy with \
             `confine == None` must fail closed; falling through to an unconfined write lets a dep \
             escape to any absolute path",
        );
    }
}
