//! Registry-free, network-free projection builder shared by sync, prune, and preview.
//!
//! The pure core (`project_binding`/`project_target`) computes the desired target
//! structure from projection specs and a source inventory. The sync-owned
//! orchestrators (`plan_target`/`project_workspace`) discover leaves through the
//! source seam, convert config into specs, and drive the pure core.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::GlobSet;

use crate::config::{Config, DeployMode, ParsedSource, TakeEntry, Target};
use crate::diagnostic::SelectionDiagnostic;
use crate::error::{Error, Result};
use crate::kernel::KernelError;
use crate::kernel::{
    CollapseChoice, CollapseMode, CollapseWarning, Materialization, OfferSelection, ResolvedTake,
    SourceName, Take, TakeWarning, fold_dest, is_take_glob, plan_collapse, resolve_take,
    safe_relpath,
};
use crate::lock::encode_ref;
use crate::source::{SourceBackend, SourceInventory, SourcePath};

use super::discover::discover_working_tree_leaves;
use super::remote_for;

// ---- projection input specs: one-way conversions from config DTOs ----

/// A source's offer compiled into an owned, config-free spec.
#[derive(Debug, Clone)]
pub struct OfferSpec {
    includes: Vec<String>,
    excludes: Vec<String>,
    root: Option<PathBuf>,
}

impl OfferSpec {
    #[must_use]
    pub fn new(includes: Vec<String>, excludes: Vec<String>, root: Option<PathBuf>) -> Self {
        Self {
            includes,
            excludes,
            root,
        }
    }

    /// The implicit full offer: no include patterns, anchored at the source root.
    #[must_use]
    pub fn implicit_full() -> Self {
        Self {
            includes: Vec::new(),
            excludes: Vec::new(),
            root: None,
        }
    }

    #[must_use]
    pub fn includes(&self) -> &[String] {
        &self.includes
    }

    #[must_use]
    pub fn excludes(&self) -> &[String] {
        &self.excludes
    }

    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// True when no include was declared — the implicit full offer.
    #[must_use]
    pub fn is_implicit_full(&self) -> bool {
        self.includes.is_empty()
    }
}

/// A binding's `take` directive classified into a config-free spec.
#[derive(Debug, Clone)]
pub enum TakeSpec {
    /// An omitted `take`: project every offered leaf at identity.
    ProjectAll,
    /// An explicit `take`; an all-empty spec projects nothing.
    Explicit {
        literals: Vec<String>,
        globs: Vec<String>,
        renames: Vec<(String, String)>,
    },
}

impl TakeSpec {
    #[must_use]
    pub fn from_entries(entries: Option<&[TakeEntry]>) -> Self {
        let Some(entries) = entries else {
            return Self::ProjectAll;
        };
        let mut literals = Vec::new();
        let mut globs = Vec::new();
        let mut renames = Vec::new();
        for entry in entries {
            match entry {
                TakeEntry::Leaf(leaf) if is_take_glob(leaf) => globs.push(leaf.clone()),
                TakeEntry::Leaf(leaf) => literals.push(leaf.clone()),
                TakeEntry::Rename { src, dest } => renames.push((src.clone(), dest.clone())),
            }
        }
        Self::Explicit {
            literals,
            globs,
            renames,
        }
    }

    #[must_use]
    pub fn is_project_all(&self) -> bool {
        matches!(self, Self::ProjectAll)
    }

    #[must_use]
    pub fn is_project_none(&self) -> bool {
        matches!(
            self,
            Self::Explicit { literals, globs, renames }
                if literals.is_empty() && globs.is_empty() && renames.is_empty()
        )
    }

    #[must_use]
    pub fn literals(&self) -> Vec<String> {
        match self {
            Self::Explicit { literals, .. } => literals.clone(),
            Self::ProjectAll => Vec::new(),
        }
    }

    #[must_use]
    pub fn globs(&self) -> Vec<String> {
        let mut globs = match self {
            Self::Explicit { globs, .. } => globs.clone(),
            Self::ProjectAll => Vec::new(),
        };
        globs.sort();
        globs
    }

    #[must_use]
    pub fn renames(&self) -> Vec<(String, String)> {
        match self {
            Self::Explicit { renames, .. } => renames.clone(),
            Self::ProjectAll => Vec::new(),
        }
    }

    fn directives(&self) -> Option<Vec<Take<'_>>> {
        match self {
            Self::ProjectAll => None,
            Self::Explicit {
                literals,
                globs,
                renames,
            } => {
                let mut directives = Vec::new();
                for literal in literals {
                    directives.push(Take::Literal(literal));
                }
                for glob in globs {
                    directives.push(Take::Glob(glob));
                }
                for (src, dest) in renames {
                    directives.push(Take::Rename { src, dest });
                }
                Some(directives)
            }
        }
    }
}

/// How a target composes each binding's identity with its artifact key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutStyle {
    Flat,
    BySource,
    Prefixed,
}

/// The target layout compiled into an owned spec.
#[derive(Debug, Clone)]
pub struct LayoutSpec {
    style: LayoutStyle,
    separator: String,
}

impl LayoutSpec {
    #[must_use]
    pub fn new(style: LayoutStyle, separator: String) -> Self {
        Self { style, separator }
    }

    #[must_use]
    pub fn artifact_path(&self, identity: &str, key: &str) -> PathBuf {
        match self.style {
            LayoutStyle::Flat => PathBuf::from(key),
            LayoutStyle::BySource => PathBuf::from(identity).join(key),
            LayoutStyle::Prefixed => PathBuf::from(format!("{identity}{}{key}", self.separator)),
        }
    }
}

/// A binding's template opt-in compiled into a render policy.
#[derive(Debug, Clone)]
pub struct TemplatePolicy {
    rule: TemplateRule,
}

#[derive(Debug, Clone)]
enum TemplateRule {
    SuffixOnly,
    Globs(GlobSet),
    Disabled,
}

const TMPL_SUFFIX: &str = ".tmpl";

impl TemplatePolicy {
    #[must_use]
    pub fn suffix_only() -> Self {
        Self {
            rule: TemplateRule::SuffixOnly,
        }
    }

    #[must_use]
    pub fn globs(set: GlobSet) -> Self {
        Self {
            rule: TemplateRule::Globs(set),
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self {
            rule: TemplateRule::Disabled,
        }
    }

    #[must_use]
    pub fn renders(&self, path: &str) -> bool {
        let suffix_opts_in = path.ends_with(TMPL_SUFFIX) && path != TMPL_SUFFIX;
        match &self.rule {
            TemplateRule::SuffixOnly => suffix_opts_in,
            TemplateRule::Globs(set) => set.is_match(path) || suffix_opts_in,
            TemplateRule::Disabled => false,
        }
    }

    #[must_use]
    pub fn deployed_name(&self, path: &str) -> String {
        if self.renders(path)
            && let Some(stripped) = path.strip_suffix(TMPL_SUFFIX)
            && !stripped.is_empty()
        {
            return stripped.to_owned();
        }
        path.to_owned()
    }
}

/// How a binding materializes its artifacts: a link symlink or a subtree copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationPolicy {
    Copy,
    Link,
}

impl MaterializationPolicy {
    fn collapse_mode(self) -> CollapseMode {
        match self {
            Self::Link => CollapseMode::Link,
            Self::Copy => CollapseMode::Copy,
        }
    }

    fn is_copy(self) -> bool {
        matches!(self, Self::Copy)
    }
}

/// A binding's `collapse` override compiled into a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollapsePreference {
    #[default]
    Default,
    ForcePerLeaf,
    ForceCollapse,
}

impl CollapsePreference {
    fn choice(self) -> CollapseChoice {
        match self {
            Self::Default => CollapseChoice::Default,
            Self::ForcePerLeaf => CollapseChoice::ForcePerLeaf,
            Self::ForceCollapse => CollapseChoice::ForceCollapse,
        }
    }

    fn as_bool(self) -> Option<bool> {
        match self {
            Self::Default => None,
            Self::ForcePerLeaf => Some(false),
            Self::ForceCollapse => Some(true),
        }
    }
}

/// The content transform a projected leaf carries: verbatim, or template-rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentTransform {
    Identity,
    Template,
}

/// A resolved source identity: the source name and its resolved commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSourceRef {
    name: String,
    commit: String,
}

impl ResolvedSourceRef {
    pub fn new(name: impl Into<String>, commit: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            commit: commit.into(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }
}

// ---- lexical target-relative path newtypes ----

/// A target-relative destination path validated by the lexical `safe_relpath` rule,
/// preserved verbatim (no case fold, no NFC), ordered by UTF-8 bytes. Sync joins the
/// target root; the projection never absolutizes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetPath(String);

impl TargetPath {
    /// # Errors
    /// Returns [`KernelError`] when `path` is not a safe forward-slashed relative path.
    pub fn new(path: &str) -> std::result::Result<Self, KernelError> {
        safe_relpath(path)?;
        Ok(Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for TargetPath {
    type Err = KernelError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl std::fmt::Display for TargetPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An artifact-relative leaf destination validated by the lexical `safe_relpath` rule,
/// preserved verbatim, ordered by UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactRelativePath(String);

impl ArtifactRelativePath {
    /// # Errors
    /// Returns [`KernelError`] when `path` is not a safe forward-slashed relative path.
    pub fn new(path: &str) -> std::result::Result<Self, KernelError> {
        safe_relpath(path)?;
        Ok(Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for ArtifactRelativePath {
    type Err = KernelError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl std::fmt::Display for ArtifactRelativePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---- projection output ----

/// A whole workspace's desired structure: one projection per target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    pub targets: Vec<TargetProjection>,
    pub warnings: Vec<ProjectionWarning>,
}

/// One target's desired structure: its bindings, their union of artifacts, and warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetProjection {
    pub target: String,
    pub bindings: Vec<BindingProjection>,
    pub artifacts: Vec<ProjectedArtifact>,
    pub warnings: Vec<ProjectionWarning>,
}

/// One binding's projected artifacts under its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingProjection {
    pub identity: String,
    pub source: String,
    pub commit: String,
    pub artifacts: Vec<ProjectedArtifact>,
    pub warnings: Vec<ProjectionWarning>,
}

/// A single projected deployment unit and its target-relative destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedArtifact {
    pub destination: TargetPath,
    pub source: ResolvedSourceRef,
    pub materialization: Materialization,
    /// Transitional kernel-typed field still read by target.rs/preview.rs/rebuild.rs;
    /// retires with T015 once they consume `leaves`.
    pub kept_leaves: Vec<ResolvedTake>,
    pub leaves: Vec<ProjectedLeaf>,
}

/// One projected leaf under an artifact: its source path, artifact-relative
/// destination, and content transform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedLeaf {
    pub source: SourcePath,
    pub destination: ArtifactRelativePath,
    pub transform: ContentTransform,
}

/// A non-fatal take/collapse outcome carried up from the projection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectionWarning {
    TakeNoMatchGlob(String),
    LostCollapseToExclude(String),
}

/// A structured projection failure. Each variant preserves the rendered diagnostic
/// so the CLI boundary shows the same text; `From<ProjectionError>` unwraps it.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("{rendered}")]
    LeafNotOffered { rendered: Box<Error> },
    #[error("{rendered}")]
    DuplicateDestination { rendered: Box<Error> },
    #[error("{rendered}")]
    CollapseBlocked { rendered: Box<Error> },
    #[error("{rendered}")]
    Other { rendered: Box<Error> },
}

impl From<ProjectionError> for Error {
    fn from(error: ProjectionError) -> Self {
        match error {
            ProjectionError::LeafNotOffered { rendered }
            | ProjectionError::DuplicateDestination { rendered }
            | ProjectionError::CollapseBlocked { rendered }
            | ProjectionError::Other { rendered } => *rendered,
        }
    }
}

/// Config-free inputs the projection maps onto the kernel for one binding.
pub struct BindingProjectionInput<'a> {
    pub identity: &'a str,
    pub source: &'a ResolvedSourceRef,
    pub offer: &'a OfferSpec,
    pub inventory: &'a SourceInventory,
    pub take: &'a TakeSpec,
    pub collapse: CollapsePreference,
    pub materialization: MaterializationPolicy,
    pub layout: &'a LayoutSpec,
    pub templates: &'a TemplatePolicy,
}

/// Projects one binding: compile the offer over the inventory, seal `take` over it,
/// fold collapse, then compose each materialization's target-relative destination.
///
/// # Errors
/// Errors if the offer fails to compile, `take` references a non-offered leaf, two
/// kept leaves collide, or a demanded collapse is blocked.
pub fn project_binding(
    input: &BindingProjectionInput<'_>,
) -> std::result::Result<BindingProjection, ProjectionError> {
    let candidates: Vec<&str> = input
        .inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    let selection = OfferSelection::compile(
        input.offer.includes(),
        input.offer.excludes(),
        input.offer.root(),
    )
    .map_err(other)?;
    let offer = selection.select(&candidates);
    let physical_tree = OfferSelection::compile(&[], &[], input.offer.root())
        .map_err(other)?
        .select(&candidates);

    let directives = input.take.directives();
    let resolution = resolve_take(&offer, directives.as_deref())
        .map_err(|error| classify_take_error(error, &offer, input.take))?;

    let mode = input.materialization.collapse_mode();
    let choice = input.collapse.choice();
    let plan =
        plan_collapse(&resolution.kept, &physical_tree, mode, choice).map_err(collapse_blocked)?;
    let materializations = reject_partial_take_collapse(
        plan.items,
        &resolution.kept,
        &offer,
        input.collapse.as_bool(),
    )
    .map_err(collapse_blocked)?
    .into_iter()
    .map(|materialization| {
        apply_deployed_name(materialization, input.materialization, input.templates)
    })
    .collect::<Vec<_>>();

    let mut artifacts = Vec::with_capacity(materializations.len());
    for materialization in materializations {
        let key = materialization.published_key().to_owned();
        let layout_path = input.layout.artifact_path(input.identity, &key);
        let dest = layout_path.to_string_lossy();
        let destination = TargetPath::new(&dest).map_err(|_| ProjectionError::Other {
            rendered: Box::new(unsafe_target_path(&dest)),
        })?;
        let kept_leaves = kept_leaves_under(&materialization, &resolution.kept);
        let leaves = build_leaves(
            &materialization,
            &kept_leaves,
            input.materialization,
            input.templates,
            offer_root_prefix(input.offer).as_deref(),
        )?;
        artifacts.push(ProjectedArtifact {
            destination,
            source: input.source.clone(),
            materialization,
            kept_leaves,
            leaves,
        });
    }

    let mut warnings: Vec<ProjectionWarning> = resolution
        .warnings
        .into_iter()
        .map(|warning| {
            let TakeWarning::NoMatchGlob(pattern) = warning;
            ProjectionWarning::TakeNoMatchGlob(pattern)
        })
        .chain(plan.warnings.into_iter().map(|warning| {
            let CollapseWarning::LostCollapseToExclude { dir } = warning;
            ProjectionWarning::LostCollapseToExclude(dir)
        }))
        .collect();
    warnings.sort();

    Ok(BindingProjection {
        identity: input.identity.to_owned(),
        source: input.source.name().to_owned(),
        commit: input.source.commit().to_owned(),
        artifacts,
        warnings,
    })
}

fn other(error: Error) -> ProjectionError {
    ProjectionError::Other {
        rendered: Box::new(error),
    }
}

fn collapse_blocked(error: Error) -> ProjectionError {
    ProjectionError::CollapseBlocked {
        rendered: Box::new(error),
    }
}

/// A take failure is `LeafNotOffered` when a literal or rename source is not in the
/// offer set; the offer seal rejects it first, so it dominates any other take fault.
fn classify_take_error(error: Error, offer: &[String], take: &TakeSpec) -> ProjectionError {
    if let TakeSpec::Explicit {
        literals, renames, ..
    } = take
    {
        let offered: BTreeSet<&str> = offer.iter().map(String::as_str).collect();
        let unoffered = literals.iter().any(|leaf| !offered.contains(leaf.as_str()))
            || renames
                .iter()
                .any(|(src, _)| !offered.contains(src.as_str()));
        if unoffered {
            return ProjectionError::LeafNotOffered {
                rendered: Box::new(error),
            };
        }
    }
    ProjectionError::Other {
        rendered: Box::new(error),
    }
}

fn offer_root_prefix(offer: &OfferSpec) -> Option<String> {
    offer.root().and_then(|root| {
        let trimmed = root.to_string_lossy().trim_end_matches('/').to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn build_leaves(
    materialization: &Materialization,
    kept_leaves: &[ResolvedTake],
    policy: MaterializationPolicy,
    templates: &TemplatePolicy,
    root: Option<&str>,
) -> std::result::Result<Vec<ProjectedLeaf>, ProjectionError> {
    let transform_of = |source: &str| {
        if policy.is_copy() && templates.renders(source) {
            ContentTransform::Template
        } else {
            ContentTransform::Identity
        }
    };
    let inventory_source = |source: &str| match root {
        Some(root) => format!("{root}/{source}"),
        None => source.to_owned(),
    };
    match materialization {
        Materialization::Leaf(take) => {
            let dest = take.dest.rsplit('/').next().unwrap_or(&take.dest);
            Ok(vec![ProjectedLeaf {
                source: SourcePath::new(&inventory_source(&take.source)).map_err(unsafe_leaf)?,
                destination: ArtifactRelativePath::new(dest).map_err(unsafe_leaf)?,
                transform: transform_of(&take.source),
            }])
        }
        Materialization::CollapsedDir { dir } => {
            let prefix = format!("{dir}/");
            let mut leaves = Vec::new();
            for kept in kept_leaves {
                let Some(child) = kept.dest.strip_prefix(&prefix) else {
                    continue;
                };
                let deployed = if policy.is_copy() {
                    templates.deployed_name(child)
                } else {
                    child.to_owned()
                };
                leaves.push(ProjectedLeaf {
                    source: SourcePath::new(&inventory_source(&kept.source))
                        .map_err(unsafe_leaf)?,
                    destination: ArtifactRelativePath::new(&deployed).map_err(unsafe_leaf)?,
                    transform: transform_of(&kept.source),
                });
            }
            Ok(leaves)
        }
    }
}

fn unsafe_leaf(error: KernelError) -> ProjectionError {
    let KernelError::UnsafeComponent(entry) = error;
    ProjectionError::Other {
        rendered: Box::new(unsafe_target_path(&entry)),
    }
}

fn unsafe_target_path(dest: &str) -> Error {
    SelectionDiagnostic {
        entry: dest.to_owned(),
        matched_against: "the target root".to_owned(),
        why: "destination is not a portable relative path".to_owned(),
        did_you_mean: None,
        remedy: "use a forward-slashed relative path inside the target".to_owned(),
        debug_hint: Some("phora preview --files".to_owned()),
        details: Vec::new(),
    }
    .sync()
}

/// A `CollapsedDir` is sound only when every *offered* leaf under it is kept at
/// identity; a `take` that drops an offered sibling makes the dir partial. The
/// partial dir is re-collapsed against only its offered-and-kept leaves, so a
/// wholly-taken sub-dir still collapses while the dropped siblings stay out.
fn reject_partial_take_collapse(
    items: Vec<Materialization>,
    kept: &[ResolvedTake],
    offer: &[String],
    collapse: Option<bool>,
) -> Result<Vec<Materialization>> {
    let kept_at_identity: BTreeSet<&str> = kept
        .iter()
        .filter(|r| r.source == r.dest)
        .map(|r| r.source.as_str())
        .collect();
    let mode = CollapseMode::Link;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Materialization::CollapsedDir { dir } = &item else {
            out.push(item);
            continue;
        };
        let prefix = format!("{dir}/");
        let offered_orphan = offer
            .iter()
            .any(|leaf| leaf.starts_with(&prefix) && !kept_at_identity.contains(leaf.as_str()));
        if !offered_orphan {
            out.push(item);
            continue;
        }
        if collapse == Some(true) {
            return Err(partial_take_collapse_diagnostic(dir));
        }
        let kept_under: Vec<ResolvedTake> = kept
            .iter()
            .filter(|r| r.source.starts_with(&prefix))
            .cloned()
            .collect();
        let offered_under: Vec<String> = offer
            .iter()
            .filter(|leaf| leaf.starts_with(&prefix))
            .cloned()
            .collect();
        let sub = plan_collapse(&kept_under, &offered_under, mode, CollapseChoice::Default)?;
        out.extend(sub.items);
    }
    out.sort_by(|a, b| a.published_key().cmp(b.published_key()));
    Ok(out)
}

fn partial_take_collapse_diagnostic(dir: &str) -> Error {
    SelectionDiagnostic {
        entry: dir.to_owned(),
        matched_against: "the offered leaves under the collapsed directory".to_owned(),
        why: "`collapse = true` was demanded but `take` keeps only part of the offered directory"
            .to_owned(),
        did_you_mean: None,
        remedy: "take the whole directory, or omit `collapse`".to_owned(),
        debug_hint: Some("phora preview --files".to_owned()),
        details: Vec::new(),
    }
    .sync()
}

pub(crate) fn map_take_entries(entries: &[TakeEntry]) -> Vec<Take<'_>> {
    entries
        .iter()
        .map(|entry| match entry {
            TakeEntry::Leaf(leaf) if is_take_glob(leaf) => Take::Glob(leaf),
            TakeEntry::Leaf(leaf) => Take::Literal(leaf),
            TakeEntry::Rename { src, dest } => Take::Rename { src, dest },
        })
        .collect()
}

fn apply_deployed_name(
    materialization: Materialization,
    policy: MaterializationPolicy,
    templates: &TemplatePolicy,
) -> Materialization {
    let Materialization::Leaf(mut take) = materialization else {
        return materialization;
    };
    if policy.is_copy() && take.source == take.dest {
        take.dest = templates.deployed_name(&take.source);
    }
    Materialization::Leaf(take)
}

fn kept_leaves_under(
    materialization: &Materialization,
    kept: &[ResolvedTake],
) -> Vec<ResolvedTake> {
    let Materialization::CollapsedDir { dir } = materialization else {
        return Vec::new();
    };
    let prefix = format!("{dir}/");
    kept.iter()
        .filter(|r| r.dest.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Projects every binding of one target, then rejects any destination two bindings
/// land on, folded the way `take` folds within a binding (NFC + simple-lowercase).
///
/// # Errors
/// Errors if any binding fails to resolve or two bindings collide on a destination.
pub fn project_target(
    target_name: &str,
    inputs: &[BindingProjectionInput<'_>],
) -> std::result::Result<TargetProjection, ProjectionError> {
    let bindings = inputs
        .iter()
        .map(project_binding)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    reject_cross_binding_dups(target_name, &bindings)?;
    let artifacts: Vec<ProjectedArtifact> = bindings
        .iter()
        .flat_map(|binding| binding.artifacts.iter().cloned())
        .collect();
    let warnings: Vec<ProjectionWarning> = bindings
        .iter()
        .flat_map(|binding| binding.warnings.iter().cloned())
        .collect();
    Ok(TargetProjection {
        target: target_name.to_owned(),
        bindings,
        artifacts,
        warnings,
    })
}

fn reject_cross_binding_dups(
    target_name: &str,
    bindings: &[BindingProjection],
) -> std::result::Result<(), ProjectionError> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for binding in bindings {
        for artifact in &binding.artifacts {
            let dest = artifact.destination.as_str().to_owned();
            if let Some(first) = seen.insert(fold_dest(&dest), dest.clone()) {
                return Err(duplicate_destination(target_name, &first, &dest));
            }
        }
    }
    for (folded, dest) in &seen {
        for ancestor in ancestor_prefixes(folded) {
            if let Some(ancestor_dest) = seen.get(ancestor) {
                return Err(duplicate_destination(target_name, ancestor_dest, dest));
            }
        }
    }
    Ok(())
}

fn duplicate_destination(target_name: &str, first: &str, second: &str) -> ProjectionError {
    ProjectionError::DuplicateDestination {
        rendered: Box::new(cross_binding_dup_diagnostic(target_name, first, second)),
    }
}

fn ancestor_prefixes(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(i, _)| &path[..i])
}

fn cross_binding_dup_diagnostic(target_name: &str, first: &str, second: &str) -> Error {
    let entry = if first <= second {
        format!("{first} / {second}")
    } else {
        format!("{second} / {first}")
    };
    SelectionDiagnostic {
        entry,
        matched_against: "the target's destinations across all bindings".to_string(),
        why: "two bindings resolve to the same destination".to_string(),
        did_you_mean: None,
        remedy: "rename one source's leaf, or separate the bindings under the layout".to_string(),
        debug_hint: Some(format!("phora preview --target {target_name}")),
        details: Vec::new(),
    }
    .sync()
}

/// The published artifact keys prune republishes for one binding: each artifact's
/// collapsed-dir or leaf destination key, in native projected order.
#[must_use]
pub fn projected_artifact_keys(binding: &BindingProjection) -> Vec<String> {
    binding
        .artifacts
        .iter()
        .map(|artifact| artifact.materialization.published_key().to_owned())
        .collect()
}

/// Projects one target's deployments: registry-free and network-free, discovering each
/// binding's candidate leaves via the source seam and projecting the leaf-granular
/// structure, taking resolved commits as a precondition; it never fetches or writes.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
pub fn plan_target(
    target_name: &str,
    target: &Target,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<TargetProjection> {
    let layout = LayoutSpec::from(&target.layout());

    let mut discovered = Vec::new();
    for binding in target.resolve_sources(parsed) {
        let source = parsed.get(binding.source).ok_or_else(|| {
            Error::Config(format!(
                "target references undefined source: {}",
                binding.source
            ))
        })?;
        let commit_key = (
            binding.source.to_owned(),
            encode_ref(&binding.effective_ref),
        );
        let commit = resolved_commits
            .get(&commit_key)
            .ok_or_else(|| {
                Error::Sync(format!(
                    "no resolved commit for {} at {}",
                    binding.source, binding.effective_ref
                ))
            })?
            .clone();
        let name = SourceName::trusted(binding.source);
        let leaves = discover_binding_leaves(source, &name, &commit, remotes, backend)?;
        discovered.push(DiscoveredBinding {
            identity: binding.identity.to_owned(),
            source: ResolvedSourceRef::new(binding.source, commit),
            inventory: SourceInventory::from_paths(leaves)?,
            offer: OfferSpec::from(source.offer()),
            take: TakeSpec::from_entries(binding.take),
            materialization: MaterializationPolicy::from(&source.deploy_mode()),
            collapse: CollapsePreference::from(binding.collapse),
            templates: TemplatePolicy::from(&binding.template_opt_in),
        });
    }

    let inputs: Vec<BindingProjectionInput<'_>> = discovered
        .iter()
        .map(|d| BindingProjectionInput {
            identity: &d.identity,
            source: &d.source,
            offer: &d.offer,
            inventory: &d.inventory,
            take: &d.take,
            collapse: d.collapse,
            materialization: d.materialization,
            layout: &layout,
            templates: &d.templates,
        })
        .collect();

    Ok(project_target(target_name, &inputs)?)
}

struct DiscoveredBinding {
    identity: String,
    source: ResolvedSourceRef,
    inventory: SourceInventory,
    offer: OfferSpec,
    take: TakeSpec,
    materialization: MaterializationPolicy,
    collapse: CollapsePreference,
    templates: TemplatePolicy,
}

/// Every candidate leaf one binding offers at `commit`, unfiltered: the offer's
/// own include/exclude is applied downstream by `OfferSelection`.
fn discover_binding_leaves(
    source: &ParsedSource,
    source_name: &SourceName,
    commit: &str,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
) -> Result<Vec<String>> {
    let git = remote_for(remotes, source_name.as_str())?;
    match source.deploy_mode() {
        DeployMode::Link => Ok(discover_working_tree_leaves(Path::new(git), None)?),
        DeployMode::Copy => Ok(backend.list_source_leaves(source_name, git, commit, None)?),
    }
}

/// Projects every target in `config`, forwarding to `plan_target` for each.
///
/// # Errors
/// Errors if a referenced source is undefined, has no resolved commit, or discovery fails.
#[must_use = "a projection describes deployments but performs none; consume the returned Projection"]
pub fn project_workspace(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    backend: &dyn SourceBackend,
    resolved_commits: &BTreeMap<(String, String), String>,
) -> Result<Projection> {
    let targets = config
        .targets
        .iter()
        .map(|(name, target)| plan_target(name, target, parsed, remotes, backend, resolved_commits))
        .collect::<Result<Vec<_>>>()?;
    let warnings = targets
        .iter()
        .flat_map(|target| target.warnings.iter().cloned())
        .collect();
    Ok(Projection { targets, warnings })
}

#[cfg(test)]
mod projection_builder_tests {
    use std::path::Path;

    use crate::config::{DeployMode, LayoutConfig, ParsedSource, Source, TakeEntry, TemplateOptIn};
    use crate::diagnostic::{MATCHED_AGAINST, REMEDY, SELECTION, TO_DEBUG};
    use crate::kernel::Materialization;
    use crate::source::SourceInventory;

    use super::{
        BindingProjection, BindingProjectionInput, CollapsePreference, LayoutSpec,
        MaterializationPolicy, OfferSpec, ProjectionError, ResolvedSourceRef, TakeSpec,
        TemplatePolicy, project_binding, project_target, projected_artifact_keys,
    };

    const COMMIT: &str = "c0ffee";

    fn source_with(
        root: Option<&str>,
        include: &[&str],
        exclude: &[&str],
        mode: DeployMode,
    ) -> ParsedSource {
        use std::fmt::Write as _;
        let mut toml = String::from("git = \"https://example.com/x.git\"\n");
        if let Some(r) = root {
            let _ = writeln!(toml, "root = \"{r}\"");
        }
        if !include.is_empty() {
            let list = include
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(toml, "include = [{list}]");
        }
        if !exclude.is_empty() {
            let list = exclude
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(toml, "exclude = [{list}]");
        }
        match mode {
            DeployMode::Link => toml.push_str("deploy = \"link\"\n"),
            DeployMode::Copy => toml.push_str("deploy = \"copy\"\n"),
        }
        let raw = toml::from_str::<Source>(&toml).expect("source DTO deserializes");
        ParsedSource::parse("s", &raw).expect("source parses")
    }

    fn named_layout(kind: &str) -> LayoutConfig {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            layout: LayoutConfig,
        }
        toml::from_str::<Wrapper>(&format!("layout = \"{kind}\""))
            .map(|w| w.layout)
            .expect("layout parses")
    }

    struct Case<'a> {
        source: &'a ParsedSource,
        leaves: Vec<String>,
        take: Option<Vec<TakeEntry>>,
        collapse: Option<bool>,
        layout: LayoutConfig,
        identity: &'a str,
    }

    impl<'a> Case<'a> {
        fn flat(source: &'a ParsedSource, leaves: &[&str]) -> Self {
            Self {
                source,
                leaves: leaves.iter().map(|s| (*s).to_string()).collect(),
                take: None,
                collapse: None,
                layout: LayoutConfig::default(),
                identity: "s",
            }
        }

        fn project(&self) -> BindingProjection {
            self.try_project().expect("binding projects")
        }

        fn try_project(&self) -> std::result::Result<BindingProjection, ProjectionError> {
            let inventory = SourceInventory::from_paths(self.leaves.iter().map(String::as_str))
                .expect("valid paths");
            let offer = OfferSpec::from(self.source.offer());
            let take = TakeSpec::from_entries(self.take.as_deref());
            let templates = TemplatePolicy::from(&TemplateOptIn::SuffixOnly);
            let layout = LayoutSpec::from(&self.layout);
            let source = ResolvedSourceRef::new("s", COMMIT);
            project_binding(&BindingProjectionInput {
                identity: self.identity,
                source: &source,
                offer: &offer,
                inventory: &inventory,
                take: &take,
                collapse: CollapsePreference::from(self.collapse),
                materialization: MaterializationPolicy::from(&self.source.deploy_mode()),
                layout: &layout,
                templates: &templates,
            })
        }
    }

    fn dests(binding: &BindingProjection) -> Vec<String> {
        binding
            .artifacts
            .iter()
            .map(|a| a.destination.as_str().to_owned())
            .collect()
    }

    fn materializations(binding: &BindingProjection) -> Vec<Materialization> {
        binding
            .artifacts
            .iter()
            .map(|a| a.materialization.clone())
            .collect()
    }

    fn leaf(source: &str, dest: &str) -> Materialization {
        Materialization::Leaf(crate::kernel::ResolvedTake {
            source: source.to_string(),
            dest: dest.to_string(),
        })
    }

    fn collapsed(dir: &str) -> Materialization {
        Materialization::CollapsedDir {
            dir: dir.to_string(),
        }
    }

    fn assert_named_diagnostic(rendered: &str, entry: &str) {
        for phrase in [SELECTION, MATCHED_AGAINST, REMEDY, TO_DEBUG] {
            assert!(
                rendered.contains(phrase),
                "the rejection must render `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains(entry),
            "the rejection must name `{entry}`; got:\n{rendered}"
        );
    }

    fn rendered(error: ProjectionError) -> String {
        crate::error::Error::from(error).to_string()
    }

    #[test]
    fn flat_bind_projects_source_offer_to_root_relative_leaf_set() {
        let source = source_with(Some("editor"), &["*.lua"], &[], DeployMode::Copy);
        let case = Case::flat(
            &source,
            &["editor/init.lua", "editor/README.md", "other/x.lua"],
        );
        let binding = case.project();
        assert_eq!(
            materializations(&binding),
            vec![leaf("init.lua", "init.lua")],
            "the offer re-anchors at root `editor`, drops the unmatched sibling, and publishes \
             the root-relative `init.lua`"
        );
    }

    #[test]
    fn link_bind_discovers_working_tree_leaves_with_dotfiles_matching() {
        let source = source_with(None, &[], &[], DeployMode::Link);
        let case = Case::flat(&source, &[".zshrc", ".config/nvim/init.lua", "plain.txt"]);
        let binding = case.project();
        assert_eq!(
            dests(&binding),
            vec![
                ".config".to_string(),
                ".zshrc".to_string(),
                "plain.txt".to_string()
            ],
            "an implicit-full offer keeps offered dotfiles with no opt-in, and a wholly-taken \
             dot-dir collapses"
        );
    }

    #[test]
    fn omitted_take_keeps_every_offered_leaf_at_identity() {
        let source = source_with(None, &["*.md"], &[], DeployMode::Copy);
        let case = Case::flat(&source, &["a.md", "b.md", "skip.txt"]);
        let binding = case.project();
        assert_eq!(
            dests(&binding),
            vec!["a.md".to_string(), "b.md".to_string()],
            "an omitted take projects every offered leaf at identity"
        );
    }

    #[test]
    fn take_literal_outside_offer_is_a_hard_error() {
        let source = source_with(None, &["*.md"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["present.md"]);
        case.take = Some(vec![TakeEntry::Leaf("absent.md".to_string())]);
        let err = case
            .try_project()
            .expect_err("a take literal outside the offer must hard-error");
        assert!(
            matches!(err, ProjectionError::LeafNotOffered { .. }),
            "an unoffered take literal is a structured LeafNotOffered; got {err:?}"
        );
        assert_named_diagnostic(&rendered(err), "absent.md");
    }

    #[test]
    fn take_rename_maps_leaf_to_dest_destructively() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["x.md", "untouched.md"]);
        case.take = Some(vec![TakeEntry::Rename {
            src: "x.md".to_string(),
            dest: "renamed.md".to_string(),
        }]);
        let binding = case.project();
        assert_eq!(
            materializations(&binding),
            vec![leaf("x.md", "renamed.md")],
            "a rename emits the leaf only at its destination and consumes the original"
        );
        assert_eq!(dests(&binding), vec!["renamed.md".to_string()]);
    }

    #[test]
    fn collapse_link_default_wholly_taken_dir_becomes_one_collapsed_dir() {
        let source = source_with(None, &["**"], &[], DeployMode::Link);
        let case = Case::flat(&source, &["d/a.md", "d/b.md"]);
        let binding = case.project();
        assert_eq!(materializations(&binding), vec![collapsed("d")]);
        assert_eq!(dests(&binding), vec!["d".to_string()]);
    }

    #[test]
    fn deploy_mode_copy_maps_to_collapse_mode_copy() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Copy);
        let case = Case::flat(&source, &["d/a.md", "d/secret.md"]);
        assert_eq!(
            materializations(&case.project()),
            vec![collapsed("d")],
            "DeployMode::Copy maps to CollapseMode::Copy: a within-dir exclude does NOT block collapse"
        );
    }

    #[test]
    fn deploy_mode_link_maps_to_collapse_mode_link() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Link);
        let case = Case::flat(&source, &["d/a.md", "d/secret.md"]);
        assert_eq!(
            materializations(&case.project()),
            vec![leaf("d/a.md", "d/a.md")],
            "DeployMode::Link maps to CollapseMode::Link: the within-dir exclude blocks collapse"
        );
    }

    #[test]
    fn collapse_force_collapse_blocked_under_link_is_hard_error() {
        let source = source_with(None, &["d/a.md"], &[], DeployMode::Link);
        let mut case = Case::flat(&source, &["d/a.md", "d/secret.md"]);
        case.collapse = Some(true);
        let err = case.try_project().expect_err(
            "`collapse = true` blocked by a within-dir exclude under link is a hard error",
        );
        assert!(matches!(err, ProjectionError::CollapseBlocked { .. }));
        let text = rendered(err);
        assert_named_diagnostic(&text, "d");
        assert!(
            text.contains("to debug: phora preview --files"),
            "a partial-take collapse block must point at the preview command; got:\n{text}"
        );
    }

    #[test]
    fn layout_flat_destination_is_published_key() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let case = Case::flat(&source, &["top.md"]);
        assert_eq!(dests(&case.project()), vec!["top.md".to_string()]);
    }

    #[test]
    fn layout_by_source_prefixes_identity() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["top.md"]);
        case.identity = "mysrc";
        case.layout = named_layout("by-source");
        assert_eq!(dests(&case.project()), vec!["mysrc/top.md".to_string()]);
    }

    #[test]
    fn layout_prefixed_joins_with_separator() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["a/deep.md"]);
        case.identity = "mysrc";
        case.layout = named_layout("prefixed");
        assert_eq!(dests(&case.project()), vec!["mysrc-a".to_string()]);
    }

    fn binding_input<'a>(
        identity: &'a str,
        source: &'a ResolvedSourceRef,
        inventory: &'a SourceInventory,
        offer: &'a OfferSpec,
        take: &'a TakeSpec,
        templates: &'a TemplatePolicy,
        layout: &'a LayoutSpec,
    ) -> BindingProjectionInput<'a> {
        BindingProjectionInput {
            identity,
            source,
            offer,
            inventory,
            take,
            collapse: CollapsePreference::default(),
            materialization: MaterializationPolicy::Copy,
            layout,
            templates,
        }
    }

    #[test]
    fn cross_binding_duplicate_dest_across_all_bindings_is_rejected() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let offer = OfferSpec::from(source.offer());
        let take = TakeSpec::from_entries(None);
        let templates = TemplatePolicy::from(&TemplateOptIn::SuffixOnly);
        let layout = LayoutSpec::from(&LayoutConfig::default());
        let inv = SourceInventory::from_paths(["shared.md"]).expect("valid paths");
        let one = ResolvedSourceRef::new("s", COMMIT);
        let two = ResolvedSourceRef::new("s", COMMIT);
        let inputs = [
            binding_input("one", &one, &inv, &offer, &take, &templates, &layout),
            binding_input("two", &two, &inv, &offer, &take, &templates, &layout),
        ];
        let err = project_target("home", &inputs)
            .expect_err("two bindings landing the same shared.md must be rejected target-globally");
        assert!(matches!(err, ProjectionError::DuplicateDestination { .. }));
        let text = rendered(err);
        assert_named_diagnostic(&text, "shared.md");
        assert!(
            text.contains("to debug: phora preview --target home"),
            "a cross-binding dup must point at the preview command scoped to the target; got:\n{text}"
        );
    }

    #[test]
    fn prune_expected_set_is_derived_from_projected_keys() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["d/a.md", "d/b.md", "top.md"]);
        case.take = Some(vec![TakeEntry::Leaf("top.md".to_string())]);
        let binding = case.project();
        assert_eq!(
            projected_artifact_keys(&binding),
            vec!["top.md".to_string()],
            "prune's expected set derives from the projected published keys"
        );
    }

    #[test]
    fn destinations_stay_target_relative() {
        let source = source_with(None, &["**"], &[], DeployMode::Copy);
        let mut case = Case::flat(&source, &["a.md"]);
        case.identity = "editor";
        case.layout = named_layout("by-source");
        let binding = case.project();
        let dest = binding.artifacts[0].destination.as_str();
        assert!(
            !dest.starts_with('/'),
            "the projection keeps the destination target-relative, joining the root is sync's job; got `{dest}`"
        );
        assert_eq!(
            Path::new("/home/u/deploy").join(dest),
            Path::new("/home/u/deploy/editor/a.md"),
            "a consumer reconstructs the absolute deploy path by joining the target root"
        );
    }
}
