//! Transitive dependency graph: the dep-manifest DTO and the graph identity keys
//! (`FetchNode` for dedup, `Instance` for namespacing).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::source::NormalizedUrl;

use super::{BuildInputs, BuildSpec, BuildTool, DEFAULT_OFFER, DeployMode, Source, Target};

/// A transitive dep's `phora.toml`, parsed EXACTLY ONCE into its declarative graph
/// fields. Trust-control fields (`trust`/`trusted_hooks`/`allow_hooks`) are tolerated
/// and dropped — never stored, so no trust state rides into admission. Hooks are
/// retained out-of-band as an uninterpreted [`toml::Value`]; a transitive global
/// `[hooks]` block is stripped (consumer-owned only).
#[derive(Debug, Clone)]
pub struct TransitiveManifest {
    pub sources: BTreeMap<String, Source>,
    pub targets: BTreeMap<String, Target>,
    pub offers: BTreeMap<String, ManifestOffer>,
    hooks: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
struct ManifestGraph {
    #[serde(default)]
    sources: BTreeMap<String, Source>,
    #[serde(default)]
    targets: BTreeMap<String, Target>,
    #[serde(default)]
    offers: BTreeMap<String, ManifestOffer>,
}

/// A named slice of a repo: own files under `root` plus the targets it selects by key.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestOffer {
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub targets: Option<Vec<String>>,
}

/// Own files are `None` when a target covers the offer root; target paths are relative to it.
#[derive(Debug, Clone)]
pub struct ResolvedOffer {
    pub root: PathBuf,
    pub files: Option<Source>,
    pub targets: BTreeMap<String, Target>,
}

pub(crate) fn resolve_offer(
    name: &str,
    offers: &BTreeMap<String, ManifestOffer>,
    sources: &BTreeMap<String, Source>,
    targets: &BTreeMap<String, Target>,
) -> Result<ResolvedOffer> {
    Repo {
        offers,
        sources,
        targets,
    }
    .resolve(name, &mut Vec::new())
}

struct Repo<'a> {
    offers: &'a BTreeMap<String, ManifestOffer>,
    sources: &'a BTreeMap<String, Source>,
    targets: &'a BTreeMap<String, Target>,
}

impl Repo<'_> {
    fn resolve(&self, name: &str, resolving: &mut Vec<String>) -> Result<ResolvedOffer> {
        resolving.push(name.to_owned());
        let resolved = self.resolve_within(name, resolving);
        resolving.pop();
        resolved
    }

    fn resolve_within(&self, name: &str, resolving: &mut Vec<String>) -> Result<ResolvedOffer> {
        let implicit = ManifestOffer::default();
        let offer = match self.offers.get(name) {
            Some(offer) => offer,
            None if name == DEFAULT_OFFER => &implicit,
            None => {
                let declared: Vec<&str> = self.offers.keys().map(String::as_str).collect();
                return Err(Error::Config(format!(
                    "no offer `{name}`; declared offers: [{}]",
                    declared.join(", ")
                )));
            }
        };
        let fail = |detail: String| Error::Config(format!("offer `{name}`: {detail}"));
        let root = match offer.root.as_deref() {
            Some(root) if escapes(root) => {
                return Err(fail(format!(
                    "root `{}` must be a relative subpath of the repo",
                    root.display()
                )));
            }
            Some(root) => normalized(root),
            None => PathBuf::new(),
        };
        let selected: Vec<(&String, &Target)> = match &offer.targets {
            None => self
                .targets
                .iter()
                .filter(|(_, target)| {
                    placed(&target.path, &root).is_some()
                        && self.offerable(target, resolving).is_ok()
                })
                .collect(),
            Some(keys) => keys
                .iter()
                .map(|key| {
                    let (key, target) = self.targets.get_key_value(key).ok_or_else(|| {
                        let declared: Vec<&str> = self.targets.keys().map(String::as_str).collect();
                        fail(format!(
                            "selects undeclared target `{key}`; declared targets: [{}]",
                            declared.join(", ")
                        ))
                    })?;
                    self.offerable(target, resolving)
                        .map_err(|why| fail(format!("target `{key}` cannot be offered: {why}")))?;
                    if placed(&target.path, &root).is_none() {
                        return Err(fail(format!(
                            "target `{key}` at `{}` lies outside the offer root `{}`",
                            target.path.display(),
                            root.display()
                        )));
                    }
                    Ok((key, target))
                })
                .collect::<Result<_>>()?,
        };
        let placed_targets = selected
            .into_iter()
            .map(|(key, target)| {
                let mut target = target.clone();
                target.path = placed(&target.path, &root).unwrap_or_default();
                (key.clone(), target)
            })
            .collect();
        Ok(ResolvedOffer {
            files: own_files(offer, &root, self.targets)?,
            targets: placed_targets,
            root,
        })
    }

    fn offerable(
        &self,
        target: &Target,
        resolving: &mut Vec<String>,
    ) -> std::result::Result<(), String> {
        if escapes(&target.path) {
            return Err(format!(
                "its path `{}` is not a relative subpath of the repo",
                target.path.display()
            ));
        }
        for (identity, binding) in target.bindings() {
            let name = binding.effective_source(identity);
            let Some(source) = self.sources.get(name) else {
                return Err(format!("it binds undefined source `{name}`"));
            };
            if let Some(build) = &source.build {
                self.offerable_build(name, build, resolving)?;
                continue;
            }
            let kind = if source.path.is_some() {
                "a local path"
            } else if source.deploy == Some(DeployMode::Link) {
                "a linked"
            } else {
                continue;
            };
            return Err(format!("it binds `{name}`, {kind} source"));
        }
        Ok(())
    }

    fn offerable_build(
        &self,
        name: &str,
        build: &BuildSpec,
        resolving: &mut Vec<String>,
    ) -> std::result::Result<(), String> {
        let (BuildTool::Builder(_), BuildInputs::Repo) = (&build.tool, &build.inputs) else {
            return Err(format!(
                "it binds `{name}`, a build that runs its own command or reads named \
                 sources; an offered build names a `builder` and reads this repo's offer"
            ));
        };
        let offer = build.offer.as_deref().unwrap_or(DEFAULT_OFFER);
        if resolving.iter().any(|o| o == offer) {
            return Err(format!(
                "it binds `{name}`, a build that reads its own output through offer `{offer}`"
            ));
        }
        self.resolve(offer, resolving)
            .map(drop)
            .map_err(|e| format!("it binds `{name}`, whose input cannot resolve: {e}"))
    }
}

fn own_files(
    offer: &ManifestOffer,
    root: &Path,
    targets: &BTreeMap<String, Target>,
) -> Result<Option<Source>> {
    let mut exclude = offer.exclude.clone();
    if offer.include.is_empty() {
        exclude.extend(["/phora.toml".to_owned(), "/phora.lock".to_owned()]);
    }
    for target in targets.values().filter(|target| !escapes(&target.path)) {
        let path = normalized(&target.path);
        if path.as_os_str().is_empty() {
            continue;
        }
        match placed(&path, root) {
            Some(rest) if rest.as_os_str().is_empty() => return Ok(None),
            Some(rest) => exclude.push(format!("/{}/", rest.display())),
            None if root.starts_with(&path) => return Ok(None),
            None => {}
        }
    }
    let mut source = toml::Table::new();
    source.insert("path".into(), ".".into());
    if !root.as_os_str().is_empty() {
        source.insert("root".into(), root.to_string_lossy().into_owned().into());
    }
    if !offer.include.is_empty() {
        source.insert("include".into(), offer.include.clone().into());
    }
    if !exclude.is_empty() {
        source.insert("exclude".into(), exclude.into());
    }
    toml::Value::Table(source)
        .try_into()
        .map(Some)
        .map_err(|e: toml::de::Error| Error::Config(format!("own files: {e}")))
}

fn placed(path: &Path, root: &Path) -> Option<PathBuf> {
    normalized(path)
        .strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
}

pub const MEMBER_SEPARATOR: char = '%';

/// What a namespaced key names within one [`Instance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Files,
    /// The whole repo at the instance's snapshot, as a composed build reads it.
    Repo,
    Named(String),
}

impl Member {
    /// `None` for a key no [`Instance`] minted.
    #[must_use]
    pub fn of(key: &str) -> Option<Self> {
        let (_, member) = key.split_once(MEMBER_SEPARATOR)?;
        Some(match member {
            "" => Self::Files,
            "%" => Self::Repo,
            named => Self::Named(named.to_owned()),
        })
    }

    #[must_use]
    pub fn is_namespaced(key: &str) -> bool {
        key.contains(MEMBER_SEPARATOR)
    }
}

impl TransitiveManifest {
    /// Parses a transitive `phora.toml`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the document is not valid TOML or its
    /// declarative `[sources]`/`[targets]`/`[offers]` fields do not type.
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_toml(text).map_err(|error| Error::Config(error.to_string()))
    }

    /// Crate-private structured-error parsing path for callers that need the concrete
    /// [`toml::de::Error`].
    ///
    /// # Errors
    ///
    /// Returns an error when the document has invalid TOML syntax or its declarative
    /// fields cannot be deserialized.
    pub(crate) fn parse_toml(text: &str) -> std::result::Result<Self, toml::de::Error> {
        let document: toml::Value = toml::from_str(text)?;
        let hooks = collect_opaque_hooks(&document);
        let mut graph: ManifestGraph = document.try_into()?;
        if let Some(key) = unnamespaceable_key(graph.sources.keys().chain(graph.targets.keys())) {
            return Err(<toml::de::Error as serde::de::Error>::custom(format!(
                "key `{key}` cannot contain `{MEMBER_SEPARATOR}`"
            )));
        }
        let sources = &graph.sources;
        for target in graph.targets.values_mut() {
            target.lower_transitive(|name| sources.get(name).is_some_and(Source::is_transitive));
        }
        Ok(Self {
            sources: graph.sources,
            targets: graph.targets,
            offers: graph.offers,
            hooks,
        })
    }

    pub(crate) fn offer(&self, name: &str) -> Result<ResolvedOffer> {
        resolve_offer(name, &self.offers, &self.sources, &self.targets)
    }

    /// The declared offers plus the implicit `default`.
    pub fn offer_names(&self) -> impl Iterator<Item = &str> {
        (!self.offers.contains_key(DEFAULT_OFFER))
            .then_some(DEFAULT_OFFER)
            .into_iter()
            .chain(self.offers.keys().map(String::as_str))
    }

    /// The retained per-target hooks as an uninterpreted payload; never the global `[hooks]`.
    #[must_use]
    pub fn hooks(&self) -> Option<&toml::Value> {
        self.hooks.as_ref()
    }
}

pub(crate) fn unnamespaceable_key<'k>(
    mut keys: impl Iterator<Item = &'k String>,
) -> Option<&'k String> {
    keys.find(|key| key.contains(MEMBER_SEPARATOR))
}

fn escapes(path: &Path) -> bool {
    path.is_absolute()
        || path.starts_with("~")
        || path.components().any(|c| matches!(c, Component::ParentDir))
}

fn normalized(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// Per-target `hooks` keyed by target; the top-level `[hooks]` is consumer-owned.
fn collect_opaque_hooks(document: &toml::Value) -> Option<toml::Value> {
    let retained: toml::value::Table = document
        .get("targets")?
        .as_table()?
        .iter()
        .filter_map(|(name, target)| Some((name.clone(), target.get("hooks")?.clone())))
        .collect();
    (!retained.is_empty()).then_some(toml::Value::Table(retained))
}

/// Graph dedup key: a fetched node is `(normalized-url, ref, commit)`. A diamond
/// reaching the same triple collapses to one fetch; equivalent URL forms normalize
/// to the same node; a differing commit is a different node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FetchNode {
    url: String,
    r#ref: String,
    commit: String,
}

impl FetchNode {
    #[must_use]
    pub fn new(url: &str, r#ref: &str, commit: &str) -> Self {
        Self {
            url: NormalizedUrl::parse(url).as_str().to_owned(),
            r#ref: r#ref.to_owned(),
            commit: commit.to_owned(),
        }
    }

    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }
}

/// Namespacing key for one binding composing a fetched node under one anchor. It omits
/// the snapshot, so ownership survives commit and pin-to-link switches, and offer switches
/// that keep the offer root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Instance {
    parent: String,
    identity: String,
    anchor_target: String,
    fetch_node: FetchNode,
    root: PathBuf,
}

/// Hash input kept byte-identical so existing ownership keys stay valid.
const STABLE_KEY_SALT: &str = "package";

impl Instance {
    #[must_use]
    pub fn new(parent: &str, identity: &str, anchor_target: &str, fetch_node: FetchNode) -> Self {
        Self {
            parent: parent.to_owned(),
            identity: identity.to_owned(),
            anchor_target: anchor_target.to_owned(),
            fetch_node,
            root: PathBuf::new(),
        }
    }

    #[must_use]
    pub fn placed_at(mut self, root: &Path) -> Self {
        self.root = root.to_path_buf();
        self
    }

    pub(crate) fn key(&self, member: &Member) -> String {
        match member {
            Member::Files => format!("{}{MEMBER_SEPARATOR}", self.stable_key()),
            Member::Repo => format!("{0}{MEMBER_SEPARATOR}{MEMBER_SEPARATOR}", self.stable_key()),
            Member::Named(name) => format!("{}{MEMBER_SEPARATOR}{name}", self.stable_key()),
        }
    }

    #[must_use]
    pub fn fetch_node(&self) -> &FetchNode {
        &self.fetch_node
    }

    /// Length-prefixed field hash; stable across field reorders, unlike a `Debug` rendering.
    #[must_use]
    pub fn stable_key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        let root = self.root.to_string_lossy();
        let placement = (!root.is_empty()).then_some(root.as_ref());
        for field in [
            self.parent.as_str(),
            self.identity.as_str(),
            self.anchor_target.as_str(),
            STABLE_KEY_SALT,
        ]
        .into_iter()
        .chain(placement)
        {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.finalize().to_hex()[..16].to_owned()
    }
}
