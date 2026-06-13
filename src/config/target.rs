//! Target DTOs and deploy layout resolution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::hooks::TargetHooks;
use super::source::{ParsedSource, Refspec, Source};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: PathBuf,
    pub sources: Option<Vec<Binding>>,
    pub layout: Option<LayoutConfig>,
    #[serde(default)]
    pub hooks: Option<TargetHooks>,
}

#[derive(Debug, Clone)]
pub enum Binding {
    Source(String),
    Refined(RefinedBinding),
}

impl Binding {
    #[must_use]
    pub fn source(&self) -> &str {
        match self {
            Binding::Source(name) => name,
            Binding::Refined(refined) => &refined.source,
        }
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        match self {
            Binding::Source(name) => name,
            Binding::Refined(refined) => refined.r#as.as_deref().unwrap_or(&refined.source),
        }
    }
}

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct BindingVisitor;

        impl<'de> serde::de::Visitor<'de> for BindingVisitor {
            type Value = Binding;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a source name string or a refinement table")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(Binding::Source(v.to_owned()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                RefinedBinding::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(Binding::Refined)
            }
        }

        deserializer.deserialize_any(BindingVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefinedBinding {
    pub source: String,
    pub r#as: Option<String>,
    pub root: Option<PathBuf>,
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
}

#[derive(Debug)]
pub struct ResolvedBinding<'a> {
    pub identity: &'a str,
    pub source: &'a str,
    pub root: Option<&'a Path>,
    pub include: &'a [String],
    pub exclude: &'a [String],
    pub effective_ref: Refspec,
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
            self.hooks = local.hooks;
        }
        self
    }

    #[must_use]
    pub fn layout(&self) -> LayoutConfig {
        self.layout.clone().unwrap_or_default()
    }

    pub fn declared_sources(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().flatten().map(Binding::source)
    }

    #[must_use]
    pub fn resolve_sources<'a, S: SourceFields>(
        &'a self,
        all: &'a BTreeMap<String, S>,
    ) -> Vec<ResolvedBinding<'a>> {
        self.sources
            .iter()
            .flatten()
            .filter_map(|binding| resolve_binding(binding, all))
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
    binding: &'a Binding,
    all: &'a BTreeMap<String, S>,
) -> Option<ResolvedBinding<'a>> {
    let identity = binding.identity();
    let source_name = binding.source();
    let (root, include, exclude) = match binding {
        Binding::Source(_) => (None, None, None),
        Binding::Refined(refined) => (
            refined.root.as_deref(),
            refined.include.as_deref(),
            refined.exclude.as_deref(),
        ),
    };
    let binding_ref = match binding {
        Binding::Source(_) => None,
        Binding::Refined(refined) => binding_refspec(refined),
    };
    let source = all.get(source_name)?;
    Some(ResolvedBinding {
        identity,
        source: source_name,
        root: root.or_else(|| source.intrinsic_root()),
        include: include.unwrap_or_else(|| source.intrinsic_include()),
        exclude: exclude.unwrap_or_else(|| source.intrinsic_exclude()),
        effective_ref: binding_ref.unwrap_or_else(|| source.intrinsic_refspec()),
    })
}

fn binding_refspec(refined: &RefinedBinding) -> Option<Refspec> {
    if let Some(rev) = &refined.rev {
        Some(Refspec::Rev(rev.clone()))
    } else if let Some(tag) = &refined.tag {
        Some(Refspec::Tag(tag.clone()))
    } else {
        refined.branch.as_ref().map(|b| Refspec::Branch(b.clone()))
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
