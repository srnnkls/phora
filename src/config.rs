//! Config DTOs (`phora.toml`). This module is a boundary, so it carries serde.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::source::ExportPolicy;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
}

#[derive(Debug, Deserialize)]
pub struct Host {
    /// URL template for git operations. Supports: `{owner}`, `{repo}`, `{ref}`, `{path}`.
    pub git_url: Option<String>,
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum AuthConfig {
    #[serde(rename = "ssh")]
    Ssh { key: Option<PathBuf> },
    #[serde(rename = "token")]
    Token { env: String },
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub git: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub allow_symlinks: bool,
    #[serde(default)]
    pub allow_submodules: bool,
    #[serde(default = "default_true")]
    pub preserve_executable: bool,
}

fn default_true() -> bool {
    true
}

impl Source {
    #[must_use]
    pub fn refspec(&self) -> Refspec {
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

    #[must_use]
    pub fn export_policy(&self) -> ExportPolicy {
        ExportPolicy {
            allow_symlinks: self.allow_symlinks,
            allow_submodules: self.allow_submodules,
            preserve_executable: self.preserve_executable,
        }
    }

    /// BLAKE3 over the export-affecting config fields, in a fixed order.
    #[must_use]
    pub fn config_digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        for p in &self.include {
            h.update(b"inc\x00");
            h.update(p.as_bytes());
        }
        for p in &self.exclude {
            h.update(b"exc\x00");
            h.update(p.as_bytes());
        }
        if let Some(r) = &self.root {
            h.update(b"root\x00");
            h.update(r.to_string_lossy().as_bytes());
        }
        h.update(&[
            u8::from(self.allow_symlinks),
            u8::from(self.allow_submodules),
            u8::from(self.preserve_executable),
        ]);
        format!("blake3:{}", h.finalize().to_hex())
    }
}

#[derive(Debug, Clone)]
pub enum Refspec {
    Branch(String),
    Tag(String),
    Rev(String),
}

impl std::fmt::Display for Refspec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Branch(s) | Self::Tag(s) | Self::Rev(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Target {
    pub path: PathBuf,
    pub sources: Option<Vec<String>>,
    #[serde(default)]
    pub layout: LayoutConfig,
}

impl Target {
    #[must_use]
    pub fn resolve_sources<'a>(&'a self, all: &'a BTreeMap<String, Source>) -> Vec<&'a str> {
        match &self.sources {
            Some(names) => names.iter().map(String::as_str).collect(),
            None => all.keys().map(String::as_str).collect(),
        }
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

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(from = "LayoutConfigRaw")]
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

#[derive(Deserialize)]
#[serde(untagged)]
enum LayoutConfigRaw {
    Simple(String),
    Full {
        r#type: String,
        separator: Option<String>,
    },
}

impl From<LayoutConfigRaw> for LayoutConfig {
    fn from(raw: LayoutConfigRaw) -> Self {
        match raw {
            LayoutConfigRaw::Simple(s) => LayoutConfig {
                kind: LayoutKind::parse(&s),
                separator: if s == "prefixed" {
                    "-".into()
                } else {
                    String::new()
                },
            },
            LayoutConfigRaw::Full { r#type, separator } => LayoutConfig {
                kind: LayoutKind::parse(&r#type),
                separator: separator.unwrap_or_else(|| {
                    if r#type == "prefixed" {
                        "-".into()
                    } else {
                        String::new()
                    }
                }),
            },
        }
    }
}

impl LayoutKind {
    fn parse(s: &str) -> Self {
        match s {
            "by-source" => Self::BySource,
            "prefixed" => Self::Prefixed,
            _ => Self::Flat,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lock {
    pub version: u32,
    pub sources: Vec<LockedSource>,
}

impl Lock {
    #[must_use]
    pub fn find_source(&self, name: &str) -> Option<&LockedSource> {
        self.sources.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSource {
    pub name: String,
    pub git: String,
    pub resolved: String,
    pub commit: String,
    pub digest: String,
    /// Hash of export-affecting config; lets sync detect config changes that alter
    /// export output without a commit change.
    pub config_digest: String,
}

/// Effective lock merges base and local locks; local entries override base by name.
#[must_use]
pub fn merge_locks(base: &Lock, local: Option<&Lock>) -> Lock {
    let mut merged = base.clone();
    if let Some(local) = local {
        for local_source in &local.sources {
            merged.sources.retain(|s| s.name != local_source.name);
            merged.sources.push(local_source.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refspec_defaults_to_main_branch() {
        let s = Source {
            git: "x".into(),
            branch: None,
            tag: None,
            rev: None,
            root: None,
            include: vec![],
            exclude: vec![],
            allow_symlinks: false,
            allow_submodules: false,
            preserve_executable: true,
        };
        assert_eq!(s.refspec().to_string(), "main");
    }
}
