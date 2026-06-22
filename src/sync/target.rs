use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{DeployMode, LayoutKind, ParsedSource, Target, TemplateOptIn};
use crate::deploy::{ArtifactState, Journal, check_artifact_state, deploy_artifact, link_artifact};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Materialization, Selection, SourceName, safe_relpath};
use crate::source::{ExportRequest, SourceBackend};
use crate::store::{
    ArtifactKey, EjectedEntry, MAP_LAYOUT, ProjectedRecord, RecordKind, Registry, RegistryRecord,
};

use super::confine::{ProtectedPathSet, confine_destination};
use super::plan::{PlanWarning, PlannedItem, plan_target};
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
    let layout_kind = run.target.layout().kind;
    let ejected = registry.load_ejected(run.target_name)?;
    let mut had_failures = false;

    let plan = plan_target(
        run.target_name,
        run.target,
        run.parsed,
        run.remotes,
        backend,
        run.commits,
    )?;

    let template_opt_ins: BTreeMap<String, TemplateOptIn> = run
        .target
        .resolve_sources(run.parsed)
        .into_iter()
        .map(|b| (b.identity.to_owned(), b.template_opt_in))
        .collect();

    for binding in &plan.bindings {
        surface_plan_warnings(&binding.warnings);
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
        let source_name = SourceName::trusted(&binding.source);

        for item in &binding.items {
            let key = published_key(&item.materialization);
            safe_relpath(key).map_err(|_| unsafe_dest_diagnostic(key))?;
            let artifact_dst = run.confined(&item.destination)?;
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
                identity: &binding.identity,
                underlying_source: &binding.source,
                commit: &binding.commit,
                item,
                artifact_dst: &artifact_dst,
                layout_kind,
                ejected: &ejected,
                mode_transition,
                template_opt_in,
            };
            had_failures |= deploy_artifact_entry(run, &entry, backend, registry, journal)?;
        }
    }

    Ok(had_failures)
}

fn surface_plan_warnings(warnings: &[PlanWarning]) {
    for warning in warnings {
        match warning {
            PlanWarning::TakeNoMatchGlob(pattern) => {
                eprintln!("phora: take pattern matched no offered leaf: {pattern}");
            }
            PlanWarning::LostCollapseToExclude(dir) => {
                eprintln!(
                    "phora: dir `{dir}` cannot collapse to one symlink under a within-dir exclude; \
                     falling back to per-leaf links"
                );
            }
        }
    }
}

/// The published artifact key — the collapsed-dir or the renamed/identity leaf dest.
fn published_key(materialization: &Materialization) -> &str {
    match materialization {
        Materialization::CollapsedDir { dir } => dir,
        Materialization::Leaf(take) => &take.dest,
    }
}

fn unsafe_dest_diagnostic(dest: &str) -> Error {
    crate::diagnostic::SelectionDiagnostic {
        entry: dest.to_owned(),
        matched_against: "the deploy root".to_owned(),
        why: "destination is not a portable relative path".to_owned(),
        did_you_mean: None,
        remedy: "use a forward-slashed relative path inside the deploy root".to_owned(),
        debug_hint: None,
    }
    .sync()
}

pub(super) struct ArtifactEntry<'a> {
    pub(super) source: &'a ParsedSource,
    pub(super) git: &'a str,
    pub(super) source_name: &'a SourceName,
    pub(super) identity: &'a str,
    pub(super) underlying_source: &'a str,
    pub(super) commit: &'a str,
    pub(super) item: &'a PlannedItem,
    pub(super) artifact_dst: &'a Path,
    pub(super) layout_kind: LayoutKind,
    pub(super) ejected: &'a [EjectedEntry],
    pub(super) mode_transition: bool,
    pub(super) template_opt_in: &'a TemplateOptIn,
}

impl ArtifactEntry<'_> {
    fn published_key(&self) -> &str {
        published_key(&self.item.materialization)
    }

    fn record_kind(&self) -> RecordKind {
        match &self.item.materialization {
            Materialization::CollapsedDir { .. } => RecordKind::Dir,
            Materialization::Leaf(_) => RecordKind::File,
        }
    }
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
        return target.expanded_path();
    }
    let artifact_path = record_artifact_path(target, record);
    match record.kind {
        RecordKind::File => artifact_path
            .parent()
            .map_or(artifact_path.clone(), Path::to_path_buf),
        RecordKind::Dir => artifact_path,
    }
}

pub(super) fn deploy_artifact_entry(
    run: TargetRun<'_>,
    entry: &ArtifactEntry<'_>,
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
) -> Result<bool> {
    let artifact_dst = entry.artifact_dst;
    let published_key = entry.published_key().to_owned();
    let key = ArtifactKey {
        target: run.target_name.to_owned(),
        source: entry.identity.to_owned(),
        artifact: published_key.clone(),
    };

    let expected_vars_digest = expected_vars_digest(entry, backend, registry, &key, run.vars)?;
    let state = check_artifact_state(
        artifact_dst,
        entry.identity,
        entry.commit,
        entry.ejected,
        &published_key,
        registry,
        &key,
        expected_vars_digest.as_deref(),
    )?;

    let conflict_kind = conflict_kind_for(&state, entry, run.force);

    let deploy = |key: ArtifactKey| match entry.source.deploy_mode() {
        DeployMode::Link => deploy_link(registry, journal, entry, key),
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
                root: entry.source.offer().root(),
                commit: entry.commit,
                materialization: &entry.item.materialization,
                kept_leaves: &entry.item.kept_leaves,
                kind: entry.record_kind(),
                artifact_dst,
                key,
                template_opt_in: entry.template_opt_in,
                vars: run.vars,
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
                artifact: published_key.clone(),
                kind,
            }),
            _ => {
                warn_skip(entry.identity, &published_key, &kind, artifact_dst);
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
                    "phora: failed to deploy {}:{published_key}: {e}",
                    entry.identity
                );
                Ok(true)
            }
        },
        Resolution::Eject => {
            let mut ejected = registry.load_ejected(run.target_name)?;
            ejected.push(EjectedEntry {
                source: entry.identity.to_owned(),
                artifact: published_key.clone(),
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
    let offer_root = entry.source.offer().root();
    let templated = match &entry.item.materialization {
        Materialization::Leaf(take) => entry.template_opt_in.renders(&take.source),
        Materialization::CollapsedDir { dir } => {
            let subtree = offer_root.map_or_else(|| PathBuf::from(dir), |r| r.join(dir));
            let leaves = backend.list_source_leaves(
                entry.source_name,
                entry.git,
                entry.commit,
                Some(&subtree),
            )?;
            leaves
                .iter()
                .any(|leaf| entry.template_opt_in.renders(&format!("{dir}/{leaf}")))
        }
    };
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
    materialization: &'a Materialization,
    kept_leaves: &'a [crate::kernel::ResolvedTake],
    kind: RecordKind,
    artifact_dst: &'a Path,
    key: ArtifactKey,
    template_opt_in: &'a TemplateOptIn,
    vars: &'a BTreeMap<String, String>,
    confine_anchor: Option<&'a Path>,
}

fn deploy_one(
    backend: &dyn SourceBackend,
    registry: &dyn Registry,
    journal: &Journal,
    ctx: DeployContext<'_>,
) -> Result<()> {
    let staging_base = target_parent(ctx.artifact_dst).join(".phora-stage");
    let key_label = ctx.key.artifact.replace('/', "_");
    let staging = staging_base.join(format!("{key_label}-{}", nonce()));
    let mut staging_guard = StagingGuard::new(&staging_base, &staging);

    let git = ctx.git;
    let commit_time = backend.commit_time(ctx.source_name, git, ctx.commit)?;
    let policy = ctx.source.export_policy();
    let empty_selection = Selection::new(&[], &[])?;

    let (export, staging_payload, files) = match ctx.materialization {
        Materialization::CollapsedDir { dir } => {
            let artifact = ArtifactName::trusted(dir.clone());
            let kept_selection = collapsed_dir_selection(dir, ctx.kept_leaves)?;
            let req = ExportRequest {
                source: ctx.source_name,
                url: git,
                commit: ctx.commit,
                root: ctx.root,
                artifact: &artifact,
                selection: &kept_selection,
                policy: &policy,
                staging_dir: &staging,
                commit_time,
                template_opt_in: ctx.template_opt_in,
                vars: ctx.vars,
                path_map: None,
            };
            let export = backend.export_artifact(&req)?;
            let files = export.files.clone();
            (export, staging.clone(), files)
        }
        Materialization::Leaf(take) => {
            let dest_leaf = leaf_basename(&take.dest);
            let path_map =
                BTreeMap::from([(PathBuf::from(&take.source), PathBuf::from(&dest_leaf))]);
            let req = ExportRequest {
                source: ctx.source_name,
                url: git,
                commit: ctx.commit,
                root: ctx.root,
                artifact: &ArtifactName::trusted(take.dest.clone()),
                selection: &empty_selection,
                policy: &policy,
                staging_dir: &staging,
                commit_time,
                template_opt_in: ctx.template_opt_in,
                vars: ctx.vars,
                path_map: Some(&path_map),
            };
            let export = backend.export_artifact(&req)?;
            let files = export.files.clone();
            (export, staging.join(&dest_leaf), files)
        }
    };

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
        layout: format!("{:?}", ctx.layout_kind).to_lowercase(),
        kind: ctx.kind,
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files,
        vars_digest: export.vars_digest,
    });

    if matches!(ctx.kind, RecordKind::Dir) {
        staging_guard.disarm();
    }
    deploy_artifact(
        &staging_base,
        &staging_payload,
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
        layout: format!("{:?}", entry.layout_kind).to_lowercase(),
        kind: entry.record_kind(),
        allow_symlinks: policy.allow_symlinks,
        preserve_executable: policy.preserve_executable,
        files: vec![],
        linked: true,
        vars_digest: None,
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
    let base = Path::new(entry.git);
    let mut target = if base.is_absolute() {
        base.to_path_buf()
    } else {
        base.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir().map_or_else(|_| base.to_path_buf(), |c| c.join(base))
        })
    };
    if let Some(root) = entry.source.offer().root() {
        target.push(root);
    }
    match &entry.item.materialization {
        Materialization::CollapsedDir { dir } => target.push(dir),
        Materialization::Leaf(take) => target.push(&take.source),
    }
    target
}

fn leaf_basename(dest: &str) -> String {
    dest.rsplit('/').next().unwrap_or(dest).to_owned()
}

fn collapsed_dir_selection(
    dir: &str,
    kept_leaves: &[crate::kernel::ResolvedTake],
) -> Result<Selection> {
    let prefix = format!("{dir}/");
    let include: Vec<String> = kept_leaves
        .iter()
        .filter_map(|kept| kept.source.strip_prefix(&prefix).map(anchor_to_dir_root))
        .collect();
    Selection::new(&include, &[])
}

fn anchor_to_dir_root(under_dir: &str) -> String {
    format!("/{under_dir}")
}

#[cfg(test)]
mod collapsed_dir_selection_tests {
    use super::*;
    use crate::kernel::ResolvedTake;

    fn kept(source: &str) -> ResolvedTake {
        ResolvedTake {
            source: source.to_owned(),
            dest: source.to_owned(),
        }
    }

    #[test]
    fn kept_leaf_selection_admits_only_the_kept_child_not_a_deeper_namesake() {
        let kept_leaves = [kept("editor/a.md")];
        let sel =
            collapsed_dir_selection("editor", &kept_leaves).expect("kept-leaf selection compiles");

        assert!(
            sel.selects_path(Path::new("a.md"), false),
            "the kept leaf must be admitted at the dir root"
        );
        assert!(
            !sel.selects_path(Path::new("sub/a.md"), false),
            "a same-named child under a deeper, unkept sibling dir must NOT be re-admitted; the \
             include is root-anchored, not a `**/` depth match"
        );
        assert!(
            !sel.selects_path(Path::new("secret"), false),
            "an offer-excluded sibling absent from the kept set must NOT be admitted"
        );
    }
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
            take: BTreeMap::new(),
            collapse: BTreeMap::new(),
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
        let protected =
            ProtectedPathSet::resolve(&crate::config::Paths::default(), Path::new("/home/u/proj"))
                .expect("protected");
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

#[cfg(test)]
mod kind_aware_layout_tests {
    use super::*;
    use crate::config::{LayoutConfig, LayoutKind, Target};
    use crate::store::{ArtifactKey, ManifestFile, RecordKind, RegistryRecord};

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
            take: BTreeMap::new(),
            collapse: BTreeMap::new(),
            confine: None,
        }
    }

    fn record(identity: &str, artifact: &str, layout: &str, kind: RecordKind) -> RegistryRecord {
        RegistryRecord {
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
            vars_digest: None,
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
            "a kind=file record must NOT collapse to the target root the way MAP_LAYOUT did"
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
        let rec = RegistryRecord {
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
            vars_digest: None,
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
