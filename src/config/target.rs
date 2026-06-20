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
}

/// A per-target binding value. The map key is the binding identity; `source`
/// overrides the effective source only when the alias diverges from the key.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub source: Option<String>,
    pub root: Option<PathBuf>,
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    #[serde(default)]
    pub template: Option<TemplateOptIn>,
    #[serde(default)]
    pub map: Option<BTreeMap<String, String>>,
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
    pub map: Option<&'a BTreeMap<String, String>>,
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
            Refspec::Branch("main".into())
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
        root: binding.root.as_deref().or_else(|| source.intrinsic_root()),
        include: binding
            .include
            .as_deref()
            .unwrap_or_else(|| source.intrinsic_include()),
        exclude: binding
            .exclude
            .as_deref()
            .unwrap_or_else(|| source.intrinsic_exclude()),
        effective_ref: binding_refspec(binding).unwrap_or_else(|| source.intrinsic_refspec()),
        template_opt_in: binding.template_opt_in(),
        map: binding.map.as_ref(),
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
}
