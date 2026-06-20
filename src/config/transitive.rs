//! Transitive dependency graph: the dep-manifest DTO and the graph identity keys
//! (`FetchNode` for dedup, `Instance` for namespacing).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::source::NormalizedUrl;

use super::{Source, Target};

/// A transitive dep's `phora.toml`, parsed EXACTLY ONCE into its declarative graph
/// fields. Trust-control fields (`trust`/`trusted_hooks`/`allow_hooks`) are tolerated
/// and dropped — never stored, so no trust state rides into admission. Hooks are
/// retained out-of-band as an uninterpreted [`toml::Value`]; a transitive global
/// `[hooks]` block is stripped (consumer-owned only).
#[derive(Debug, Clone)]
pub struct TransitiveManifest {
    pub sources: BTreeMap<String, Source>,
    pub targets: BTreeMap<String, Target>,
    hooks: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
struct ManifestGraph {
    #[serde(default)]
    sources: BTreeMap<String, Source>,
    #[serde(default)]
    targets: BTreeMap<String, Target>,
}

impl TransitiveManifest {
    /// Parses a transitive `phora.toml`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the document is not valid TOML or its
    /// declarative `[sources]`/`[targets]` fields do not type.
    pub fn parse(text: &str) -> Result<Self> {
        let document: toml::Value =
            toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        let hooks = collect_opaque_hooks(&document);
        let graph: ManifestGraph = document
            .try_into()
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            sources: graph.sources,
            targets: graph.targets,
            hooks,
        })
    }

    /// The retained per-target hooks as an uninterpreted payload, or `None` when the
    /// dep declares no per-target hooks. The transitive global `[hooks]` is never here.
    #[must_use]
    pub fn hooks(&self) -> Option<&toml::Value> {
        self.hooks.as_ref()
    }
}

/// Per-target `hooks` sub-tables keyed by target name; the top-level `[hooks]` is
/// consumer-owned and excluded.
fn collect_opaque_hooks(document: &toml::Value) -> Option<toml::Value> {
    let targets = document.get("targets")?.as_table()?;
    let mut retained = toml::value::Table::new();
    for (name, target) in targets {
        if let Some(hooks) = target.get("hooks") {
            retained.insert(name.clone(), hooks.clone());
        }
    }
    if retained.is_empty() {
        None
    } else {
        Some(toml::Value::Table(retained))
    }
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

/// Namespacing key: the SAME fetched node mounted at two anchors is two distinct
/// instances. `(parent, source_name, anchor_target, fetch_node)` keys hooks/paths
/// while still referencing its shared [`FetchNode`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Instance {
    parent: String,
    source_name: String,
    anchor_target: String,
    fetch_node: FetchNode,
}

impl Instance {
    #[must_use]
    pub fn new(
        parent: &str,
        source_name: &str,
        anchor_target: &str,
        fetch_node: FetchNode,
    ) -> Self {
        Self {
            parent: parent.to_owned(),
            source_name: source_name.to_owned(),
            anchor_target: anchor_target.to_owned(),
            fetch_node,
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
        for field in [
            self.parent.as_str(),
            self.source_name.as_str(),
            self.anchor_target.as_str(),
            self.fetch_node.url.as_str(),
            self.fetch_node.r#ref.as_str(),
            self.fetch_node.commit.as_str(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.finalize().to_hex()[..16].to_owned()
    }
}
