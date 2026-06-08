//! Config DTOs (`phora.toml`). This module is a boundary, so it carries serde.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::source::ExportPolicy;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
}

impl Config {
    /// Parses and validates a `phora.toml` document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the document is not valid TOML, contains an
    /// unknown key, or a source sets more than one of `branch`/`tag`/`rev`.
    pub fn parse(s: &str) -> Result<Self> {
        let config: Self = toml::from_str(s).map_err(|e| Error::Config(e.to_string()))?;
        for (name, source) in &config.sources {
            let set = u8::from(source.branch.is_some())
                + u8::from(source.tag.is_some())
                + u8::from(source.rev.is_some());
            if set > 1 {
                return Err(Error::Config(format!(
                    "source `{name}` sets more than one of branch/tag/rev"
                )));
            }
        }
        Ok(config)
    }
}

/// Computes the effective config: `base` overlaid by `local` per spec merge semantics.
#[must_use]
pub fn merge_configs(base: Config, local: Option<Config>) -> Config {
    let Some(local) = local else { return base };
    let mut merged = base;
    merged.version = local.version;
    for (name, host) in local.hosts {
        match merged.hosts.remove(&name) {
            Some(base_host) => {
                merged.hosts.insert(name, base_host.merged_with(host));
            }
            None => {
                merged.hosts.insert(name, host);
            }
        }
    }
    for (name, source) in local.sources {
        match merged.sources.remove(&name) {
            Some(base_source) => {
                merged.sources.insert(name, base_source.merged_with(source));
            }
            None => {
                merged.sources.insert(name, source);
            }
        }
    }
    for (name, target) in local.targets {
        match merged.targets.remove(&name) {
            Some(base_target) => {
                merged.targets.insert(name, base_target.merged_with(target));
            }
            None => {
                merged.targets.insert(name, target);
            }
        }
    }
    merged
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// URL template for git operations. Supports: `{owner}`, `{repo}`, `{ref}`, `{path}`.
    pub git_url: Option<String>,
    pub auth: Option<AuthConfig>,
}

impl Host {
    #[must_use]
    fn merged_with(mut self, local: Host) -> Host {
        if local.git_url.is_some() {
            self.git_url = local.git_url;
        }
        if local.auth.is_some() {
            self.auth = local.auth;
        }
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum AuthConfig {
    #[serde(rename = "ssh")]
    Ssh { key: Option<PathBuf> },
    #[serde(rename = "token")]
    Token { env: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub git: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    pub allow_symlinks: Option<bool>,
    pub allow_submodules: Option<bool>,
    pub preserve_executable: Option<bool>,
    #[serde(default)]
    pub deploy: Option<DeployMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeployMode {
    Copy,
    Link,
}

impl Source {
    #[must_use]
    fn merged_with(mut self, local: Source) -> Source {
        self.git = local.git;
        if local.branch.is_some() || local.tag.is_some() || local.rev.is_some() {
            self.branch = local.branch;
            self.tag = local.tag;
            self.rev = local.rev;
        }
        if local.root.is_some() {
            self.root = local.root;
        }
        if local.include.is_some() {
            self.include = local.include;
        }
        if local.exclude.is_some() {
            self.exclude = local.exclude;
        }
        if local.allow_symlinks.is_some() {
            self.allow_symlinks = local.allow_symlinks;
        }
        if local.allow_submodules.is_some() {
            self.allow_submodules = local.allow_submodules;
        }
        if local.preserve_executable.is_some() {
            self.preserve_executable = local.preserve_executable;
        }
        if local.deploy.is_some() {
            self.deploy = local.deploy;
        }
        self
    }

    #[must_use]
    pub fn deploy_mode(&self) -> DeployMode {
        self.deploy.unwrap_or(DeployMode::Copy)
    }

    #[must_use]
    pub fn includes(&self) -> &[String] {
        self.include.as_deref().unwrap_or(&[])
    }

    #[must_use]
    pub fn excludes(&self) -> &[String] {
        self.exclude.as_deref().unwrap_or(&[])
    }

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
            allow_symlinks: self.allow_symlinks.unwrap_or(false),
            allow_submodules: self.allow_submodules.unwrap_or(false),
            preserve_executable: self.preserve_executable.unwrap_or(true),
        }
    }

    /// BLAKE3 over the export-affecting config fields, in a fixed order.
    #[must_use]
    pub fn config_digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        for p in self.includes() {
            h.update(b"inc\x00");
            h.update(p.as_bytes());
        }
        for p in self.excludes() {
            h.update(b"exc\x00");
            h.update(p.as_bytes());
        }
        if let Some(r) = &self.root {
            h.update(b"root\x00");
            h.update(r.to_string_lossy().as_bytes());
        }
        let policy = self.export_policy();
        h.update(&[
            u8::from(policy.allow_symlinks),
            u8::from(policy.allow_submodules),
            u8::from(policy.preserve_executable),
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: PathBuf,
    pub sources: Option<Vec<String>>,
    pub layout: Option<LayoutConfig>,
}

impl Target {
    #[must_use]
    fn merged_with(mut self, local: Target) -> Target {
        self.path = local.path;
        if local.sources.is_some() {
            self.sources = local.sources;
        }
        if local.layout.is_some() {
            self.layout = local.layout;
        }
        self
    }

    #[must_use]
    pub fn layout(&self) -> LayoutConfig {
        self.layout.clone().unwrap_or_default()
    }

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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const EXAMPLE_TOML: &str = r#"
version = 1

[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
root = "modules"

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
tag = "v2.1"
root = "configs"
include = ["editor", "lint"]
exclude = ["**/test/**", "**/*.bak"]

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
root = "languages"
allow_symlinks = false
preserve_executable = true

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]

[targets.vscode]
path = "~/.config/Code/User"
sources = ["dotfiles", "company-configs"]
layout = "flat"

[targets.cupcake-policies]
path = "~/.cupcake/policies/claude"
sources = ["loqui"]
layout = { type = "prefixed", separator = "/" }
"#;

    fn parse_source(toml_body: &str) -> Source {
        let toml =
            format!("version = 1\n\n[sources.s]\ngit = \"https://example.com/x.git\"\n{toml_body}");
        Config::parse(&toml)
            .expect("source toml parses")
            .sources
            .remove("s")
            .expect("source `s` present")
    }

    fn source(branch: Option<&str>, tag: Option<&str>, rev: Option<&str>) -> Source {
        use std::fmt::Write as _;
        let mut body = String::new();
        if let Some(b) = branch {
            let _ = writeln!(body, "branch = \"{b}\"");
        }
        if let Some(t) = tag {
            let _ = writeln!(body, "tag = \"{t}\"");
        }
        if let Some(r) = rev {
            let _ = writeln!(body, "rev = \"{r}\"");
        }
        parse_source(&body)
    }

    fn target_of<'a>(cfg: &'a Config, name: &str) -> &'a Target {
        cfg.targets.get(name).expect("target present")
    }

    fn effective_layout(target: &Target) -> LayoutConfig {
        target.layout()
    }

    // PAM-001: config parses from phora.toml

    #[test]
    fn parses_version_and_all_sections_from_example() {
        let cfg = Config::parse(EXAMPLE_TOML).expect("example toml should parse");
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.sources.len(), 3);
        assert_eq!(cfg.targets.len(), 3);
    }

    #[test]
    fn parses_source_fields_from_example() {
        let cfg = Config::parse(EXAMPLE_TOML).expect("example toml should parse");

        let dotfiles = cfg.sources.get("dotfiles").expect("dotfiles source");
        assert_eq!(dotfiles.git, "https://github.com/me/dotfiles.git");
        assert_eq!(dotfiles.branch.as_deref(), Some("main"));
        assert_eq!(dotfiles.root.as_deref(), Some(Path::new("modules")));

        let company = cfg
            .sources
            .get("company-configs")
            .expect("company-configs source");
        assert_eq!(company.tag.as_deref(), Some("v2.1"));
        assert_eq!(company.includes(), ["editor", "lint"]);
        assert_eq!(company.excludes(), ["**/test/**", "**/*.bak"]);
    }

    #[test]
    fn parses_target_sources_and_layout_from_example() {
        let cfg = Config::parse(EXAMPLE_TOML).expect("example toml should parse");

        let vscode = cfg.targets.get("vscode").expect("vscode target");
        assert_eq!(
            vscode.sources.as_deref(),
            Some(["dotfiles".to_string(), "company-configs".to_string()].as_slice())
        );
        assert_eq!(
            effective_layout(vscode).artifact_path("loqui", "python"),
            PathBuf::from("python"),
            "flat layout drops the source prefix"
        );

        let cupcake = cfg
            .targets
            .get("cupcake-policies")
            .expect("cupcake-policies target");
        assert_eq!(
            effective_layout(cupcake).artifact_path("loqui", "python"),
            PathBuf::from("loqui/python"),
            "prefixed layout with `/` separator joins source and artifact"
        );
    }

    #[test]
    fn parses_host_auth_token_config() {
        let toml = r#"
version = 1

[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }
"#;
        let cfg = Config::parse(toml).expect("host toml should parse");
        let github = cfg.hosts.get("github").expect("github host");
        assert_eq!(
            github.git_url.as_deref(),
            Some("https://github.com/{owner}/{repo}.git")
        );
        match github.auth.as_ref().expect("auth config") {
            AuthConfig::Token { env } => assert_eq!(env, "GITHUB_TOKEN"),
            AuthConfig::Ssh { .. } => panic!("expected token auth, got ssh"),
        }
    }

    // PAM-002: refspec priority and export policy defaults

    #[test]
    fn refspec_defaults_to_main_branch() {
        assert!(matches!(
            source(None, None, None).refspec(),
            Refspec::Branch(b) if b == "main"
        ));
    }

    #[test]
    fn refspec_uses_rev_when_only_rev_set() {
        let s = source(None, None, Some("abc123"));
        assert!(matches!(s.refspec(), Refspec::Rev(r) if r == "abc123"));
    }

    #[test]
    fn refspec_uses_tag_when_only_tag_set() {
        let s = source(None, Some("v2.1"), None);
        assert!(matches!(s.refspec(), Refspec::Tag(t) if t == "v2.1"));
    }

    #[test]
    fn refspec_uses_branch_when_only_branch_set() {
        let s = source(Some("dev"), None, None);
        assert!(matches!(s.refspec(), Refspec::Branch(b) if b == "dev"));
    }

    #[test]
    fn export_policy_uses_spec_defaults() {
        let policy = source(None, None, None).export_policy();
        assert!(!policy.allow_symlinks);
        assert!(!policy.allow_submodules);
        assert!(policy.preserve_executable);
    }

    // PAM-003: layout path computation

    #[test]
    fn flat_layout_places_artifact_at_root() {
        let layout = LayoutConfig::default();
        assert_eq!(layout.kind, LayoutKind::Flat);
        assert_eq!(
            layout.artifact_path("loqui", "python"),
            PathBuf::from("python")
        );
    }

    #[test]
    fn by_source_layout_nests_under_source_dir() {
        let layout: LayoutConfig = toml::from_str("layout = \"by-source\"")
            .map(|w: LayoutWrapper| w.layout)
            .expect("by-source layout parses");
        assert_eq!(
            layout.artifact_path("loqui", "python"),
            PathBuf::from("loqui").join("python")
        );
    }

    #[test]
    fn prefixed_layout_table_uses_given_separator() {
        let layout: LayoutConfig =
            toml::from_str("layout = { type = \"prefixed\", separator = \"/\" }")
                .map(|w: LayoutWrapper| w.layout)
                .expect("prefixed layout parses");
        assert_eq!(
            layout.artifact_path("loqui", "python"),
            PathBuf::from("loqui/python")
        );
    }

    #[test]
    fn prefixed_layout_defaults_separator_to_dash() {
        let layout: LayoutConfig = toml::from_str("layout = { type = \"prefixed\" }")
            .map(|w: LayoutWrapper| w.layout)
            .expect("prefixed layout parses");
        assert_eq!(
            layout.artifact_path("loqui", "python"),
            PathBuf::from("loqui-python")
        );
    }

    #[derive(Deserialize)]
    struct LayoutWrapper {
        layout: LayoutConfig,
    }

    // PAM-004: effective-config merge

    #[test]
    fn merge_replaces_base_scalar_with_local() {
        let base = Config::parse(EXAMPLE_TOML).expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let loqui = effective.sources.get("loqui").expect("loqui source kept");
        assert_eq!(loqui.git, "/home/soeren/dev/loqui");
        assert_eq!(loqui.branch.as_deref(), Some("main"));
        assert!(
            loqui.tag.is_none(),
            "local branch override must clear the base refspec group (tag)"
        );
        assert_eq!(
            loqui.root.as_deref(),
            Some(Path::new("languages")),
            "base-only field must survive when local does not set it"
        );
        assert!(matches!(loqui.refspec(), Refspec::Branch(b) if b == "main"));
    }

    #[test]
    fn merge_replaces_base_array_no_concatenation() {
        let base = Config::parse(EXAMPLE_TOML).expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
include = ["only-this"]
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let company = effective
            .sources
            .get("company-configs")
            .expect("company-configs kept");
        assert_eq!(company.includes(), ["only-this"]);
    }

    #[test]
    fn merge_explicit_empty_array_clears_base_array() {
        let base = Config::parse(EXAMPLE_TOML).expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
include = []
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let company = effective
            .sources
            .get("company-configs")
            .expect("company-configs kept");
        assert!(
            company.includes().is_empty(),
            "an explicit empty `include = []` in local must replace (clear) the base array, \
             not be ignored as if unset"
        );
    }

    #[test]
    fn merge_adds_local_only_source() {
        let base = Config::parse(EXAMPLE_TOML).expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.local-extra]
git = "/home/soeren/dev/extra"
branch = "main"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        assert!(effective.sources.contains_key("local-extra"));
        assert!(
            effective.sources.contains_key("dotfiles"),
            "base-only source must be kept"
        );
    }

    #[test]
    fn merge_without_local_is_identity() {
        let base = Config::parse(EXAMPLE_TOML).expect("base parses");
        let effective = merge_configs(base, None);
        assert_eq!(effective.sources.len(), 3);
        assert_eq!(effective.targets.len(), 3);
        assert_eq!(effective.hosts.len(), 1);
        assert!(effective.hosts.contains_key("github"), "host survives");
        assert_eq!(
            effective.sources.get("loqui").expect("loqui kept").git,
            "https://github.com/srnnkls/loqui.git"
        );
        assert_eq!(
            effective
                .targets
                .get("neovim")
                .expect("neovim target kept")
                .path,
            PathBuf::from("~/.config/nvim")
        );
        assert_eq!(
            effective_layout(target_of(&effective, "cupcake-policies"))
                .artifact_path("loqui", "python"),
            PathBuf::from("loqui/python"),
            "identity merge preserves the prefixed `/` layout"
        );
    }

    #[test]
    fn merge_path_only_target_override_preserves_base_layout() {
        let base = Config::parse(EXAMPLE_TOML).expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[targets.cupcake-policies]
path = "/local/override/policies"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let cupcake = target_of(&effective, "cupcake-policies");

        assert_eq!(
            cupcake.path,
            PathBuf::from("/local/override/policies"),
            "local path override must take effect"
        );
        assert_eq!(
            effective_layout(cupcake).artifact_path("loqui", "python"),
            PathBuf::from("loqui/python"),
            "a path-only override must NOT reset the base prefixed `/` layout to flat"
        );
    }

    #[test]
    fn merge_partial_source_override_preserves_base_policy_flags() {
        let base = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
root = "languages"
allow_symlinks = true
preserve_executable = false
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let policy = effective
            .sources
            .get("loqui")
            .expect("loqui kept")
            .export_policy();

        assert!(
            policy.allow_symlinks,
            "git+branch-only override must NOT reset base allow_symlinks=true to default"
        );
        assert!(
            !policy.preserve_executable,
            "git+branch-only override must NOT reset base preserve_executable=false to default"
        );
    }

    #[test]
    fn merge_host_auth_only_override_preserves_base_git_url() {
        let base = Config::parse(
            r#"
version = 1

[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[hosts.github]
auth = { type = "token", env = "GITHUB_TOKEN_WORK" }
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let github = effective.hosts.get("github").expect("github host kept");

        assert_eq!(
            github.git_url.as_deref(),
            Some("https://github.com/{owner}/{repo}.git"),
            "an auth-only local override must NOT clear the base git_url"
        );
        match github.auth.as_ref().expect("auth config") {
            AuthConfig::Token { env } => assert_eq!(env, "GITHUB_TOKEN_WORK"),
            AuthConfig::Ssh { .. } => panic!("expected token auth, got ssh"),
        }
    }

    // PAM-005: validation

    #[test]
    fn unknown_auth_key_is_rejected() {
        let toml = r#"
version = 1

[hosts.github]
auth = { type = "token", env = "X", bogus = 1 }
"#;
        let err = Config::parse(toml).expect_err("unknown auth key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("bogus"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_source_key_is_rejected() {
        let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
brunch = "main"
"#;
        assert!(
            matches!(Config::parse(toml), Err(Error::Config(_))),
            "unknown source key must produce a config error"
        );
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let toml = r#"
version = 1
bogus = "value"
"#;
        let err = Config::parse(toml).expect_err("unknown top-level key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("bogus"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_target_key_is_rejected() {
        let toml = r#"
version = 1

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
destination = "elsewhere"
"#;
        let err = Config::parse(toml).expect_err("unknown target key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("destination"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_host_key_is_rejected() {
        let toml = r#"
version = 1

[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"
proxy = "http://localhost"
"#;
        let err = Config::parse(toml).expect_err("unknown host key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("proxy"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn source_with_branch_and_tag_is_rejected() {
        let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
tag = "v1.0"
"#;
        assert!(
            matches!(Config::parse(toml), Err(Error::Config(_))),
            "specifying both branch and tag must be rejected"
        );
    }

    #[test]
    fn source_with_tag_and_rev_is_rejected() {
        let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
tag = "v1.0"
rev = "abc123"
"#;
        assert!(matches!(Config::parse(toml), Err(Error::Config(_))));
    }

    #[test]
    fn source_with_branch_and_rev_is_rejected() {
        let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
rev = "abc123"
"#;
        assert!(
            matches!(Config::parse(toml), Err(Error::Config(_))),
            "specifying both branch and rev must be rejected"
        );
    }

    #[test]
    fn invalid_layout_kind_is_rejected() {
        let toml = r#"
version = 1

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
layout = "fnord"
"#;
        assert!(
            matches!(Config::parse(toml), Err(Error::Config(_))),
            "an unrecognized layout type must be rejected, not silently coerced to flat"
        );
    }

    #[test]
    fn unknown_layout_table_key_is_rejected() {
        let toml = r#"
version = 1

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
layout = { type = "prefixed", seperator = "/" }
"#;
        let err = Config::parse(toml).expect_err("unknown layout key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("seperator"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    // DLD-001: deploy mode field, merge, digest exclusion

    fn deploy_of(cfg: &Config, source: &str) -> Option<DeployMode> {
        cfg.sources.get(source).expect("source present").deploy
    }

    #[test]
    fn deploy_absent_is_copy_and_link_parses() {
        let copy_default = parse_source("");
        assert_eq!(
            copy_default.deploy.unwrap_or(DeployMode::Copy),
            DeployMode::Copy,
            "an absent `deploy` must resolve to the Copy default"
        );

        let linked = parse_source("deploy = \"link\"\n");
        assert_eq!(
            linked.deploy,
            Some(DeployMode::Link),
            "deploy = \"link\" must parse to DeployMode::Link"
        );

        let explicit_copy = parse_source("deploy = \"copy\"\n");
        assert_eq!(explicit_copy.deploy, Some(DeployMode::Copy));
    }

    #[test]
    fn merge_local_deploy_override_replaces_base() {
        let base = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
deploy = "copy"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
deploy = "link"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        assert_eq!(
            deploy_of(&effective, "loqui"),
            Some(DeployMode::Link),
            "a local `deploy = link` must override the base `deploy = copy`"
        );
    }

    #[test]
    fn merge_partial_override_preserves_base_deploy() {
        let base = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
tag = "v1.0"
deploy = "link"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        assert_eq!(
            deploy_of(&effective, "loqui"),
            Some(DeployMode::Link),
            "a git+branch-only override that does not set deploy must keep the base `deploy = link`"
        );
    }

    #[test]
    fn config_digest_ignores_deploy_for_lock_stability() {
        let without = parse_source("root = \"languages\"\ninclude = [\"editor\"]\n");
        let with_link =
            parse_source("root = \"languages\"\ninclude = [\"editor\"]\ndeploy = \"link\"\n");
        assert_eq!(
            with_link.config_digest(),
            without.config_digest(),
            "deploy mode does not change exported ODB content; it must be excluded from \
             config_digest or a link flip would invalidate the lock (source_matches, lock.rs:50)"
        );
    }

    #[test]
    fn unknown_deploy_value_is_rejected_naming_it() {
        let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
deploy = "wormhole"
"#;
        let err = Config::parse(toml).expect_err("unknown deploy value must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("wormhole"),
                "error should name the offending deploy value, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn valid_config_parses_ok() {
        assert!(
            Config::parse(EXAMPLE_TOML).is_ok(),
            "a single-refspec, no-unknown-keys config must parse cleanly"
        );
    }
}
