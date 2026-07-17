use std::path::{Path, PathBuf};

use globset::GlobSet;

use crate::kernel::safe_relpath;
use crate::projection::collapse::{CollapseChoice, CollapseMode};
use crate::projection::diagnostic::{ProjectionError, ProjectionWarning, unsafe_leaf};
use crate::projection::take::{ResolvedTake, Take};
use crate::source::{SourceInventory, SourcePath};

/// One planned deployment unit: a collapsed directory or a single kept leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Materialization {
    /// A whole directory deployed as one artifact, rooted at `dir`.
    CollapsedDir { dir: String },
    /// A single kept leaf deployed on its own.
    Leaf(ResolvedTake),
}

impl Materialization {
    /// The published artifact key: the collapsed dir, or the leaf's destination.
    #[must_use]
    pub fn published_key(&self) -> &str {
        match self {
            Materialization::CollapsedDir { dir } => dir,
            Materialization::Leaf(take) => &take.dest,
        }
    }
}

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

    pub(super) fn directives(&self) -> Option<Vec<Take<'_>>> {
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
    pub(super) fn collapse_mode(self) -> CollapseMode {
        match self {
            Self::Link => CollapseMode::Link,
            Self::Copy => CollapseMode::Copy,
        }
    }

    pub(super) fn is_copy(self) -> bool {
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
    pub(super) fn choice(self) -> CollapseChoice {
        match self {
            Self::Default => CollapseChoice::Default,
            Self::ForcePerLeaf => CollapseChoice::ForcePerLeaf,
            Self::ForceCollapse => CollapseChoice::ForceCollapse,
        }
    }

    pub(super) fn as_bool(self) -> Option<bool> {
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
    /// Returns [`ProjectionError`] when `path` is not a safe forward-slashed relative path.
    pub fn new(path: &str) -> std::result::Result<Self, ProjectionError> {
        safe_relpath(path).map_err(|_| unsafe_leaf(path))?;
        Ok(Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for TargetPath {
    type Err = ProjectionError;

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
    /// Returns [`ProjectionError`] when `path` is not a safe forward-slashed relative path.
    pub fn new(path: &str) -> std::result::Result<Self, ProjectionError> {
        safe_relpath(path).map_err(|_| unsafe_leaf(path))?;
        Ok(Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for ArtifactRelativePath {
    type Err = ProjectionError;

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
