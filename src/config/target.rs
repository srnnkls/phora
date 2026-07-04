//! Target DTOs and deploy layout resolution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use super::TargetHooks;
use super::source::{ParsedSource, Refspec, Source};

const TMPL_SUFFIX: &str = ".tmpl";

/// Per-binding template opt-in. The `.tmpl` suffix convention is on by
/// default; a glob list opts additional files in; `template = false` disables
/// everything, including the suffix.
#[derive(Debug, Clone)]
pub enum TemplateOptIn {
    /// No `template` key: only the `.tmpl` suffix convention renders.
    SuffixOnly,
    /// A `template` glob list: matching files render, plus any `.tmpl` file.
    Globs(GlobSet),
    /// `template = false`: nothing renders, no suffix is stripped.
    Disabled,
}

impl TemplateOptIn {
    /// True when `path` renders: it matches a template glob, or ends in `.tmpl` —
    /// unless rendering is disabled.
    #[must_use]
    pub fn renders(&self, path: &str) -> bool {
        let suffix_opts_in = path.ends_with(TMPL_SUFFIX) && path != TMPL_SUFFIX;
        match self {
            Self::SuffixOnly => suffix_opts_in,
            Self::Globs(set) => set.is_match(path) || suffix_opts_in,
            Self::Disabled => false,
        }
    }

    /// The deployed name: strips a trailing `.tmpl` only when the file renders;
    /// otherwise the name is unchanged.
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

impl<'de> Deserialize<'de> for TemplateOptIn {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TemplateVisitor;

        impl<'de> serde::de::Visitor<'de> for TemplateVisitor {
            type Value = TemplateOptIn;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("`false` to disable templating, or a list of glob strings")
            }

            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                if v {
                    return Err(E::custom(
                        "`template = true` is not valid; omit the key for the default .tmpl opt-in, or give a glob list",
                    ));
                }
                Ok(TemplateOptIn::Disabled)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut builder = GlobSetBuilder::new();
                let mut count = 0usize;
                while let Some(pattern) = seq.next_element::<String>()? {
                    let glob = Glob::new(&pattern).map_err(serde::de::Error::custom)?;
                    builder.add(glob);
                    count += 1;
                }
                if count == 0 {
                    return Err(serde::de::Error::custom(
                        "template glob list must not be empty; omit the key for the default .tmpl opt-in, or use `template = false` to disable",
                    ));
                }
                let set = builder.build().map_err(serde::de::Error::custom)?;
                Ok(TemplateOptIn::Globs(set))
            }
        }

        deserializer.deserialize_any(TemplateVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: PathBuf,
    #[serde(default, deserialize_with = "deserialize_bindings")]
    pub sources: Option<BTreeMap<String, Binding>>,
    pub layout: Option<LayoutConfig>,
    #[serde(default)]
    pub hooks: Option<TargetHooks>,
    #[serde(default)]
    pub imports: Option<Vec<String>>,
    #[serde(default)]
    pub take: Option<BTreeMap<String, Vec<TakeEntry>>>,
    #[serde(default)]
    pub collapse: Option<BTreeMap<String, bool>>,
    /// Composition-only anchor every destination must stay under; `Some` iff this is a composed dep target.
    #[serde(skip)]
    pub confine: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum TakeEntry {
    Leaf(String),
    Rename { src: String, dest: String },
}

impl<'de> Deserialize<'de> for TakeEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TakeEntryVisitor;

        impl<'de> serde::de::Visitor<'de> for TakeEntryVisitor {
            type Value = TakeEntry;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a leaf path string or a single-pair rename table")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(TakeEntry::Leaf(v.to_owned()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let Some((src, dest)) = map.next_entry::<String, String>()? else {
                    return Err(serde::de::Error::custom(
                        "a `take` rename table must carry one source-to-destination pair",
                    ));
                };
                if let Some((extra_src, _)) = map.next_entry::<String, String>()? {
                    return Err(serde::de::Error::custom(format!(
                        "a `take` rename entry must carry exactly one pair; \
                         drop the extra pair `{extra_src}` into its own entry"
                    )));
                }
                Ok(TakeEntry::Rename { src, dest })
            }
        }

        deserializer.deserialize_any(TakeEntryVisitor)
    }
}

/// A per-target binding value. The map key is the binding identity; `source`
/// overrides the effective source only when the alias diverges from the key.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub source: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    #[serde(default)]
    pub template: Option<TemplateOptIn>,
    #[serde(default)]
    pub take: Option<Vec<TakeEntry>>,
    #[serde(default)]
    pub collapse: Option<bool>,
}

impl Binding {
    /// The effective source name: the explicit `source` field, else the
    /// binding's identity (its map key).
    #[must_use]
    pub fn effective_source<'a>(&'a self, identity: &'a str) -> &'a str {
        self.source.as_deref().unwrap_or(identity)
    }

    #[must_use]
    pub fn template_opt_in(&self) -> TemplateOptIn {
        self.template.clone().unwrap_or(TemplateOptIn::SuffixOnly)
    }
}

fn deserialize_bindings<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<BTreeMap<String, Binding>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BindingsVisitor;

    impl<'de> serde::de::Visitor<'de> for BindingsVisitor {
        type Value = Option<BTreeMap<String, Binding>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a keyed binding table or a flat list of source names")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut map = BTreeMap::new();
            while let Some(name) = seq.next_element::<String>()? {
                if map.insert(name.clone(), Binding::default()).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate source `{name}` in the `sources` list"
                    )));
                }
            }
            Ok(Some(map))
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((key, binding)) = map.next_entry::<String, Binding>()? {
                out.insert(key, binding);
            }
            Ok(Some(out))
        }
    }

    deserializer.deserialize_any(BindingsVisitor)
}

#[derive(Debug)]
pub struct ResolvedBinding<'a> {
    pub identity: &'a str,
    pub source: &'a str,
    pub root: Option<&'a Path>,
    pub include: &'a [String],
    pub exclude: &'a [String],
    pub effective_ref: Refspec,
    pub template_opt_in: TemplateOptIn,
    pub take: Option<&'a [TakeEntry]>,
    pub collapse: Option<bool>,
}

impl ResolvedBinding<'_> {
    pub fn renames(&self) -> impl Iterator<Item = (&str, &str)> {
        self.take
            .into_iter()
            .flatten()
            .filter_map(|entry| match entry {
                TakeEntry::Rename { src, dest } => Some((src.as_str(), dest.as_str())),
                TakeEntry::Leaf(_) => None,
            })
    }

    #[must_use]
    pub fn has_renames(&self) -> bool {
        self.renames().next().is_some()
    }
}

pub trait SourceFields {
    fn intrinsic_root(&self) -> Option<&Path>;
    fn intrinsic_include(&self) -> &[String];
    fn intrinsic_exclude(&self) -> &[String];
    fn intrinsic_refspec(&self) -> Refspec;
}

impl SourceFields for Source {
    fn intrinsic_root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    fn intrinsic_include(&self) -> &[String] {
        self.include.as_deref().unwrap_or(&[])
    }

    fn intrinsic_exclude(&self) -> &[String] {
        self.exclude.as_deref().unwrap_or(&[])
    }

    fn intrinsic_refspec(&self) -> Refspec {
        if let Some(rev) = &self.rev {
            Refspec::Rev(rev.clone())
        } else if let Some(tag) = &self.tag {
            Refspec::Tag(tag.clone())
        } else if let Some(branch) = &self.branch {
            Refspec::Branch(branch.clone())
        } else {
            Refspec::Default
        }
    }
}

impl SourceFields for ParsedSource {
    fn intrinsic_root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    fn intrinsic_include(&self) -> &[String] {
        self.includes()
    }

    fn intrinsic_exclude(&self) -> &[String] {
        self.excludes()
    }

    fn intrinsic_refspec(&self) -> Refspec {
        self.refspec()
    }
}

impl Target {
    #[must_use]
    pub(super) fn merged_with(mut self, local: Target) -> Target {
        self.path = local.path;
        if local.sources.is_some() {
            self.sources = local.sources;
        }
        if local.layout.is_some() {
            self.layout = local.layout;
        }
        if local.hooks.is_some() {
            // bare [hooks] section replaces base wholesale, matching layout
            self.hooks = local.hooks;
        }
        if local.imports.is_some() {
            self.imports = local.imports;
        }
        if local.take.is_some() {
            self.take = local.take;
        }
        if local.collapse.is_some() {
            self.collapse = local.collapse;
        }
        self
    }

    #[must_use]
    pub fn layout(&self) -> LayoutConfig {
        self.layout.clone().unwrap_or_default()
    }

    pub fn declared_sources(&self) -> impl Iterator<Item = &str> {
        self.sources
            .iter()
            .flatten()
            .map(|(identity, binding)| binding.effective_source(identity))
    }

    #[must_use]
    pub fn resolve_sources<'a, S: SourceFields>(
        &'a self,
        all: &'a BTreeMap<String, S>,
    ) -> Vec<ResolvedBinding<'a>> {
        self.sources
            .iter()
            .flatten()
            .filter_map(|(identity, binding)| resolve_binding(identity, binding, all))
            .collect()
    }

    #[must_use]
    pub fn expanded_path(&self) -> PathBuf {
        let path_str = self.path.to_string_lossy();
        if let Some(rest) = path_str.strip_prefix("~/")
            && let Some(home) = dirs::home_dir()
        {
            return home.join(rest);
        }
        self.path.clone()
    }

    /// Deploy-time snapshot of the target's expanded absolute path, as a lossy string.
    #[must_use]
    pub fn deploy_root(&self) -> String {
        self.expanded_path().to_string_lossy().into_owned()
    }
}

fn resolve_binding<'a, S: SourceFields>(
    identity: &'a str,
    binding: &'a Binding,
    all: &'a BTreeMap<String, S>,
) -> Option<ResolvedBinding<'a>> {
    let source_name = binding.effective_source(identity);
    let source = all.get(source_name)?;
    Some(ResolvedBinding {
        identity,
        source: source_name,
        root: source.intrinsic_root(),
        include: source.intrinsic_include(),
        exclude: source.intrinsic_exclude(),
        effective_ref: binding_refspec(binding).unwrap_or_else(|| source.intrinsic_refspec()),
        template_opt_in: binding.template_opt_in(),
        take: binding.take.as_deref(),
        collapse: binding.collapse,
    })
}

fn binding_refspec(binding: &Binding) -> Option<Refspec> {
    if let Some(rev) = &binding.rev {
        Some(Refspec::Rev(rev.clone()))
    } else if let Some(tag) = &binding.tag {
        Some(Refspec::Tag(tag.clone()))
    } else {
        binding.branch.as_ref().map(|b| Refspec::Branch(b.clone()))
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(try_from = "LayoutConfigRaw")]
pub struct LayoutConfig {
    pub kind: LayoutKind,
    pub separator: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    #[default]
    Flat,
    BySource,
    Prefixed,
}

impl LayoutConfig {
    #[must_use]
    pub fn artifact_path(&self, source: &str, artifact: &str) -> PathBuf {
        match self.kind {
            LayoutKind::Flat => PathBuf::from(artifact),
            LayoutKind::BySource => PathBuf::from(source).join(artifact),
            LayoutKind::Prefixed => PathBuf::from(format!("{source}{}{artifact}", self.separator)),
        }
    }

    /// `Some` separator to persist for reconstruction only under `Prefixed`, which is the sole layout that joins with one.
    #[must_use]
    pub fn persisted_separator(&self) -> Option<String> {
        matches!(self.kind, LayoutKind::Prefixed).then(|| self.separator.clone())
    }
}

enum LayoutConfigRaw {
    Simple(String),
    Full(LayoutTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutTable {
    r#type: String,
    separator: Option<String>,
}

impl<'de> Deserialize<'de> for LayoutConfigRaw {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RawVisitor;

        impl<'de> serde::de::Visitor<'de> for RawVisitor {
            type Value = LayoutConfigRaw;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a layout name string or a layout table")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(LayoutConfigRaw::Simple(v.to_owned()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                LayoutTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(LayoutConfigRaw::Full)
            }
        }

        deserializer.deserialize_any(RawVisitor)
    }
}

impl TryFrom<LayoutConfigRaw> for LayoutConfig {
    type Error = String;

    fn try_from(raw: LayoutConfigRaw) -> std::result::Result<Self, Self::Error> {
        let (kind, sep) = match raw {
            LayoutConfigRaw::Simple(s) => (LayoutKind::parse(&s)?, None),
            LayoutConfigRaw::Full(table) => (LayoutKind::parse(&table.r#type)?, table.separator),
        };
        let separator = sep.unwrap_or_else(|| match kind {
            LayoutKind::Prefixed => "-".into(),
            LayoutKind::Flat | LayoutKind::BySource => String::new(),
        });
        Ok(LayoutConfig { kind, separator })
    }
}

impl LayoutKind {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "flat" => Ok(Self::Flat),
            "by-source" => Ok(Self::BySource),
            "prefixed" => Ok(Self::Prefixed),
            other => Err(format!("unknown layout type `{other}`")),
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::BySource => "by-source",
            Self::Prefixed => "prefixed",
        }
    }

    #[must_use]
    pub fn from_record_label(label: &str) -> Option<Self> {
        match label {
            "flat" => Some(Self::Flat),
            // `bysource` is the label pre-hardening builds wrote via Debug-lowercasing.
            "by-source" | "bysource" => Some(Self::BySource),
            "prefixed" => Some(Self::Prefixed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    use super::{Binding, Source, TakeEntry, Target, resolve_binding};

    fn binding(body: &str) -> Binding {
        toml::from_str::<Binding>(body).expect("binding DTO deserializes")
    }

    fn try_binding(body: &str) -> Result<Binding, toml::de::Error> {
        toml::from_str::<Binding>(body)
    }

    fn source_with(root: Option<&str>, include: &[&str], exclude: &[&str]) -> Source {
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
        toml::from_str::<Source>(&toml).expect("source DTO deserializes")
    }

    #[test]
    fn binding_rejects_include_field() {
        let err = try_binding("include = [\"editor/**\"]\n")
            .expect_err("binding-level `include` must no longer deserialize");
        assert!(
            err.to_string().contains("include"),
            "the rejection must name the removed `include` field; got:\n{err}"
        );
    }

    #[test]
    fn binding_rejects_exclude_field() {
        try_binding("exclude = [\"editor/**\"]\n")
            .expect_err("binding-level `exclude` must no longer deserialize");
    }

    #[test]
    fn binding_rejects_root_field() {
        try_binding("root = \"editor\"\n")
            .expect_err("binding-level `root` must no longer deserialize");
    }

    #[test]
    fn binding_rejects_map_field() {
        try_binding("map = { \"a/X.md\" = \"a/x.md\" }\n")
            .expect_err("binding-level `map` must no longer deserialize");
    }

    #[test]
    fn take_deserializes_mixed_literal_glob_and_rename() {
        let b = binding(
            "take = [\"skills/gestalt/skill.md\", \"skills/**\", { \"a/X.md\" = \"a/x.md\" }]\n",
        );
        let take = b.take.as_deref().expect("a present `take` parses to Some");
        assert_eq!(take.len(), 3, "all three entries parse; got: {take:?}");

        assert!(
            matches!(&take[0], TakeEntry::Leaf(s) if s == "skills/gestalt/skill.md"),
            "the first entry is a literal leaf captured verbatim; got: {:?}",
            take[0]
        );
        assert!(
            matches!(&take[1], TakeEntry::Leaf(s) if s == "skills/**"),
            "a glob string is captured raw as a leaf entry; classification is SMR-021, not here; got: {:?}",
            take[1]
        );
        match &take[2] {
            TakeEntry::Rename { src, dest } => {
                assert_eq!(src, "a/X.md", "rename src captured verbatim; got: {src}");
                assert_eq!(dest, "a/x.md", "rename dest captured verbatim; got: {dest}");
            }
            leaf @ TakeEntry::Leaf(_) => panic!("the third entry is a rename-map; got: {leaf:?}"),
        }
    }

    #[test]
    fn omitted_take_is_none_project_everything() {
        let b = binding("source = \"s\"\n");
        assert!(
            b.take.is_none(),
            "an omitted `take` must parse to None (project EVERYTHING); got: {:?}",
            b.take
        );
    }

    #[test]
    fn empty_take_is_some_empty_project_nothing() {
        let b = binding("take = []\n");
        let take = b
            .take
            .as_deref()
            .expect("an explicit `take = []` must parse to Some, not None");
        assert!(
            take.is_empty(),
            "`take = []` must parse to Some(empty) (project NOTHING); got: {take:?}"
        );
    }

    #[test]
    fn mount_take_table_parses_anchor_keyed_while_imports_stays_string_list() {
        let target: Target = toml::from_str(
            "path = \"~/dst\"\n\
             imports = [\"dep-a\", \"dep-b\"]\n\
             [take]\n\
             \"anchor/one\" = [\"a\", { \"b/X.md\" = \"b/x.md\" }]\n\
             \"anchor/two\" = [\"c\"]\n",
        )
        .expect("a target with a mount take table deserializes");

        assert_eq!(
            target.imports,
            Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
            "`imports` stays a refinement-free Vec<String>; got: {:?}",
            target.imports
        );

        let take = target
            .take
            .as_ref()
            .expect("a present mount take table parses to Some");
        let one = take
            .get("anchor/one")
            .expect("the mount take table is keyed by anchor");
        assert_eq!(
            one.len(),
            2,
            "anchor/one carries both entries; got: {one:?}"
        );
        assert!(
            matches!(&one[0], TakeEntry::Leaf(s) if s == "a"),
            "anchor/one first entry is a literal leaf; got: {:?}",
            one[0]
        );
        assert!(
            matches!(&one[1], TakeEntry::Rename { src, dest } if src == "b/X.md" && dest == "b/x.md"),
            "anchor/one second entry is a rename-map; got: {:?}",
            one[1]
        );

        let two = take
            .get("anchor/two")
            .expect("anchor/two present in the table");
        assert!(
            matches!(two.as_slice(), [TakeEntry::Leaf(s)] if s == "c"),
            "anchor/two carries one literal leaf; got: {two:?}"
        );
    }

    #[test]
    fn omitted_mount_take_table_is_empty() {
        let target: Target =
            toml::from_str("path = \"~/dst\"\n").expect("a bare target deserializes");
        assert!(
            target.take.is_none(),
            "an omitted mount take table defaults to None (inherit, no subsetting); got: {:?}",
            target.take
        );
    }

    #[test]
    fn resolved_binding_surfaces_source_root_include_exclude_without_override() {
        let source = source_with(Some("editor"), &["editor/**"], &["**/*.swp"]);
        let mut all = BTreeMap::new();
        all.insert("s".to_string(), source);

        let b = binding("source = \"s\"\n");
        let resolved =
            resolve_binding("s", &b, &all).expect("binding resolves against the source map");

        assert_eq!(
            resolved.root,
            Some(Path::new("editor")),
            "ResolvedBinding surfaces the SOURCE root (no binding override); got: {:?}",
            resolved.root
        );
        assert_eq!(
            resolved.include,
            &["editor/**".to_string()],
            "ResolvedBinding surfaces the SOURCE include; got: {:?}",
            resolved.include
        );
        assert_eq!(
            resolved.exclude,
            &["**/*.swp".to_string()],
            "ResolvedBinding surfaces the SOURCE exclude; got: {:?}",
            resolved.exclude
        );
    }

    #[test]
    fn resolved_binding_carries_take_not_map() {
        let source = source_with(None, &[], &[]);
        let mut all = BTreeMap::new();
        all.insert("s".to_string(), source);

        let b = binding("source = \"s\"\ntake = [\"a\", { \"b/X.md\" = \"b/x.md\" }]\n");
        let resolved =
            resolve_binding("s", &b, &all).expect("binding resolves against the source map");

        let take = resolved
            .take
            .expect("ResolvedBinding carries the binding `take`");
        assert_eq!(
            take.len(),
            2,
            "both take entries flow through; got: {take:?}"
        );
        assert!(
            matches!(&take[0], TakeEntry::Leaf(s) if s == "a"),
            "first resolved take entry is a literal leaf; got: {:?}",
            take[0]
        );
        assert!(
            matches!(&take[1], TakeEntry::Rename { src, dest } if src == "b/X.md" && dest == "b/x.md"),
            "second resolved take entry is a rename-map; got: {:?}",
            take[1]
        );
    }

    #[test]
    fn binding_collapse_true_parses_to_some_true() {
        let b = binding("source = \"s\"\ncollapse = true\n");
        assert_eq!(
            b.collapse,
            Some(true),
            "`collapse = true` on a binding parses to Some(true); got: {:?}",
            b.collapse
        );
    }

    #[test]
    fn binding_collapse_false_parses_to_some_false() {
        let b = binding("source = \"s\"\ncollapse = false\n");
        assert_eq!(
            b.collapse,
            Some(false),
            "`collapse = false` on a binding parses to Some(false); got: {:?}",
            b.collapse
        );
    }

    #[test]
    fn omitted_binding_collapse_is_none() {
        let b = binding("source = \"s\"\n");
        assert_eq!(
            b.collapse, None,
            "an omitted `collapse` parses to None (algorithmic default); got: {:?}",
            b.collapse
        );
    }

    #[test]
    fn resolved_binding_surfaces_the_binding_collapse() {
        let source = source_with(None, &[], &[]);
        let mut all = BTreeMap::new();
        all.insert("s".to_string(), source);

        let b = binding("source = \"s\"\ncollapse = false\n");
        let resolved = resolve_binding("s", &b, &all).expect("binding resolves");
        assert_eq!(
            resolved.collapse,
            Some(false),
            "ResolvedBinding surfaces the binding `collapse`; got: {:?}",
            resolved.collapse
        );
    }

    #[test]
    fn resolved_binding_collapse_is_none_when_omitted() {
        let source = source_with(None, &[], &[]);
        let mut all = BTreeMap::new();
        all.insert("s".to_string(), source);

        let b = binding("source = \"s\"\n");
        let resolved = resolve_binding("s", &b, &all).expect("binding resolves");
        assert_eq!(
            resolved.collapse, None,
            "an omitted `collapse` stays None through resolution; got: {:?}",
            resolved.collapse
        );
    }

    #[test]
    fn mount_collapse_table_parses_anchor_keyed_while_imports_stays_string_list() {
        let target: Target = toml::from_str(
            "path = \"~/dst\"\n\
             imports = [\"dep-a\", \"dep-b\"]\n\
             [collapse]\n\
             \"anchor/one\" = true\n\
             \"anchor/two\" = false\n",
        )
        .expect("a target with a mount collapse table deserializes");

        assert_eq!(
            target.imports,
            Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
            "`imports` stays a refinement-free Vec<String>; got: {:?}",
            target.imports
        );
        let collapse = target
            .collapse
            .as_ref()
            .expect("a present mount collapse table parses to Some");
        assert_eq!(
            collapse.get("anchor/one"),
            Some(&true),
            "the mount collapse table is keyed by anchor; got: {collapse:?}"
        );
        assert_eq!(
            collapse.get("anchor/two"),
            Some(&false),
            "anchor/two present in the collapse table; got: {collapse:?}"
        );
    }

    #[test]
    fn omitted_mount_collapse_table_is_empty() {
        let target: Target =
            toml::from_str("path = \"~/dst\"\n").expect("a bare target deserializes");
        assert!(
            target.collapse.is_none(),
            "an omitted mount collapse table defaults to None (inherit); got: {:?}",
            target.collapse
        );
    }

    fn target_toml(body: &str) -> Target {
        toml::from_str::<Target>(body).expect("target DTO deserializes")
    }

    #[test]
    fn merge_non_empty_local_collapse_table_replaces_base() {
        let base = target_toml("path = \"~/dst\"\n[collapse]\n\"anchor/base\" = true\n");
        let local = target_toml("path = \"~/dst\"\n[collapse]\n\"anchor/local\" = false\n");
        let merged = base.merged_with(local);
        let collapse = merged
            .collapse
            .as_ref()
            .expect("a non-empty local collapse table merges to Some");
        assert_eq!(
            collapse.get("anchor/local"),
            Some(&false),
            "a non-empty local collapse table replaces the base wholesale; got: {collapse:?}"
        );
        assert!(
            !collapse.contains_key("anchor/base"),
            "the base collapse entry must not survive a non-empty local table; got: {collapse:?}"
        );
    }

    #[test]
    fn merge_omitted_local_collapse_table_inherits_base() {
        let base = target_toml("path = \"~/dst\"\n[collapse]\n\"anchor/base\" = true\n");
        let local = target_toml("path = \"~/dst\"\n");
        let merged = base.merged_with(local);
        let collapse = merged
            .collapse
            .as_ref()
            .expect("an OMITTED local collapse table inherits the base table (Some), not None");
        assert_eq!(
            collapse.get("anchor/base"),
            Some(&true),
            "an omitted (None) local collapse table inherits the base collapse table unchanged; \
             got: {collapse:?}"
        );
    }

    #[test]
    fn merge_explicit_empty_local_collapse_table_clears_base() {
        let base = target_toml("path = \"~/dst\"\n[collapse]\n\"anchor/base\" = true\n");
        let local = target_toml("path = \"~/dst\"\n[collapse]\n");
        let merged = base.merged_with(local);
        let collapse = merged
            .collapse
            .as_ref()
            .expect("an explicit empty local `[collapse]` parses to Some(empty), not None");
        assert!(
            collapse.is_empty(),
            "an explicit present-but-empty local `[collapse]` must CLEAR the base table back to \
             take-all (Some(empty)), not be ignored as if unset; got: {collapse:?}"
        );
    }

    #[test]
    fn merge_non_empty_local_take_table_replaces_base() {
        let base = target_toml("path = \"~/dst\"\n[take]\n\"anchor/base\" = [\"x\"]\n");
        let local = target_toml("path = \"~/dst\"\n[take]\n\"anchor/local\" = [\"y\"]\n");
        let merged = base.merged_with(local);
        let take = merged
            .take
            .as_ref()
            .expect("a non-empty local take table merges to Some");
        assert!(
            take.contains_key("anchor/local"),
            "a non-empty local take table replaces the base wholesale; got: {take:?}"
        );
        assert!(
            !take.contains_key("anchor/base"),
            "the base take entry must not survive a non-empty local table; got: {take:?}"
        );
    }

    #[test]
    fn merge_omitted_local_take_table_inherits_base() {
        let base = target_toml("path = \"~/dst\"\n[take]\n\"anchor/base\" = [\"x\"]\n");
        let local = target_toml("path = \"~/dst\"\n");
        let merged = base.merged_with(local);
        let take = merged
            .take
            .as_ref()
            .expect("an OMITTED local take table inherits the base table (Some), not None");
        assert!(
            take.contains_key("anchor/base"),
            "an omitted (None) local take table inherits the base take table unchanged; \
             got: {take:?}"
        );
    }

    #[test]
    fn merge_explicit_empty_local_take_table_clears_base() {
        let base = target_toml("path = \"~/dst\"\n[take]\n\"anchor/base\" = [\"x\"]\n");
        let local = target_toml("path = \"~/dst\"\n[take]\n");
        let merged = base.merged_with(local);
        let take = merged
            .take
            .as_ref()
            .expect("an explicit empty local `[take]` parses to Some(empty), not None");
        assert!(
            take.is_empty(),
            "an explicit present-but-empty local `[take]` must CLEAR the base table back to \
             take-all (Some(empty)), not be ignored as if unset; got: {take:?}"
        );
    }

    #[test]
    fn omitted_take_table_parses_to_none_while_present_empty_parses_to_some_empty() {
        let omitted = target_toml("path = \"~/dst\"\n");
        assert!(
            omitted.take.is_none(),
            "an OMITTED mount take table must parse to None (inherit); got: {:?}",
            omitted.take
        );
        assert!(
            omitted.collapse.is_none(),
            "an OMITTED mount collapse table must parse to None (inherit); got: {:?}",
            omitted.collapse
        );

        let present_empty = target_toml("path = \"~/dst\"\n[take]\n[collapse]\n");
        assert_eq!(
            present_empty.take.as_ref().map(BTreeMap::len),
            Some(0),
            "a present-but-empty `[take]` must parse to Some(empty), distinct from None; got: {:?}",
            present_empty.take
        );
        assert_eq!(
            present_empty.collapse.as_ref().map(BTreeMap::len),
            Some(0),
            "a present-but-empty `[collapse]` must parse to Some(empty), distinct from None; \
             got: {:?}",
            present_empty.collapse
        );
    }

    #[test]
    fn resolved_binding_take_is_none_when_omitted() {
        let source = source_with(None, &[], &[]);
        let mut all: BTreeMap<String, Source> = BTreeMap::new();
        all.insert("s".to_string(), source);

        let b = binding("source = \"s\"\n");
        let resolved = resolve_binding("s", &b, &all).expect("binding resolves");
        assert!(
            resolved.take.is_none(),
            "an omitted `take` stays None through resolution (project everything); got: {:?}",
            resolved.take
        );
        let _ = PathBuf::new();
    }
}
