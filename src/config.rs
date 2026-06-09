//! Config DTOs (`phora.toml`). This module is a boundary, so it carries serde.

use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::source::ExportPolicy;
pub use crate::source::Protocol;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub protocol: Option<Protocol>,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
    #[serde(default)]
    pub worktree: Option<WorktreeConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeConfig {
    #[serde(default)]
    pub includes: Vec<Include>,
}

impl WorktreeConfig {
    #[must_use]
    #[allow(clippy::unused_self)] // local wins wholesale; `self` kept to mirror the sibling merged_with API
    fn merged_with(self, local: WorktreeConfig) -> WorktreeConfig {
        local
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Include {
    pub path: PathBuf,
    #[serde(default)]
    pub mode: IncludeMode,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IncludeMode {
    #[default]
    Symlink,
    Copy,
    #[serde(rename = "submodule-walk")]
    SubmoduleWalk,
}

impl Config {
    /// Parses and validates a `phora.toml` document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the document is not valid TOML, contains an
    /// unknown key, a source sets more than one of `branch`/`tag`/`rev`, or a
    /// worktree include path is empty, absolute, or contains a `.` or `..`
    /// component.
    pub fn parse(s: &str) -> Result<Self> {
        let config: Self = toml::from_str(s).map_err(|e| Error::Config(e.to_string()))?;
        for (name, host) in &config.hosts {
            if let Some(remote) = &host.remote
                && remote.https_template().is_none()
                && remote.ssh_template().is_none()
            {
                return Err(Error::Config(format!(
                    "host `{name}`: `remote` must set at least one protocol template (https or ssh)"
                )));
            }
        }
        for (name, source) in &config.sources {
            let set = u8::from(source.branch.is_some())
                + u8::from(source.tag.is_some())
                + u8::from(source.rev.is_some());
            if set > 1 {
                return Err(Error::Config(format!(
                    "source `{name}` sets more than one of branch/tag/rev"
                )));
            }
            if source.git.is_some() && (source.host.is_some() || source.path.is_some()) {
                return Err(Error::Config(format!(
                    "source `{name}` sets both a literal `git` and a `host`/`path` \
                     (the git and host modes are mutually exclusive)"
                )));
            }
            if source.url.is_some()
                && (source.git.is_some() || source.host.is_some() || source.path.is_some())
            {
                return Err(Error::Config(format!(
                    "source `{name}` sets `url` together with a `git`/`host`/`path` \
                     (the url, git, and host modes are mutually exclusive)"
                )));
            }
            if source.url.is_some()
                && (source.branch.is_some()
                    || source.tag.is_some()
                    || source.rev.is_some()
                    || source.root.is_some())
            {
                return Err(Error::Config(format!(
                    "source `{name}`: `branch`/`tag`/`rev`/`root` are meaningless on a `url` source"
                )));
            }
            if source.url.as_deref().is_some_and(|u| u.trim().is_empty()) {
                return Err(Error::Config(format!(
                    "source `{name}`: `url` must not be empty"
                )));
            }
            if source.host.is_some() && source.path.is_none() {
                return Err(Error::Config(format!(
                    "source `{name}`: `host` set without a `path`"
                )));
            }
        }
        if let Some(worktree) = &config.worktree {
            for include in &worktree.includes {
                let path = &include.path;
                if path.as_os_str().is_empty() {
                    return Err(Error::Config(
                        "worktree include path must not be empty".to_owned(),
                    ));
                }
                if path.is_absolute() {
                    return Err(Error::Config(format!(
                        "worktree include path `{}` must be relative, not absolute",
                        path.display()
                    )));
                }
                if path.components().any(|c| matches!(c, Component::ParentDir)) {
                    return Err(Error::Config(format!(
                        "worktree include path `{}` must not contain a `..` component",
                        path.display()
                    )));
                }
                if path
                    .to_string_lossy()
                    .split(['/', '\\'])
                    .any(|segment| segment == ".")
                {
                    return Err(Error::Config(format!(
                        "worktree include path `{}` must not contain a `.` component",
                        path.display()
                    )));
                }
            }
        }
        Ok(config)
    }

    /// Post-merge validation: host references resolve, the effective protocol has
    /// a matching remote template, and every source resolves to exactly one mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if a source references an unknown host, requests
    /// a protocol its host's `remote` does not provide, or does not resolve to a
    /// single complete mode (git, or host+path).
    pub fn validate(&self) -> Result<()> {
        for (name, source) in &self.sources {
            let modes = u8::from(source.git.is_some())
                + u8::from(source.path.is_some())
                + u8::from(source.url.is_some());
            if modes != 1 {
                return Err(Error::Config(format!(
                    "source `{name}` must resolve to exactly one of a literal `git`, \
                     a `host`/`path` pair, or a `url`"
                )));
            }
            if let Some(url) = source.url.as_deref() {
                if url.trim().is_empty() {
                    return Err(Error::Config(format!(
                        "source `{name}`: `url` must not be empty"
                    )));
                }
                if source.branch.is_some()
                    || source.tag.is_some()
                    || source.rev.is_some()
                    || source.root.is_some()
                {
                    return Err(Error::Config(format!(
                        "source `{name}`: `branch`/`tag`/`rev`/`root` are meaningless on a `url` source"
                    )));
                }
            }
            let Some(path) = source.path.as_deref() else {
                continue;
            };
            let host_name = source.host.as_deref().unwrap_or("github");
            if path.trim().is_empty() || path.split('/').any(str::is_empty) {
                return Err(Error::Config(format!(
                    "source `{name}`: `path` `{path}` is not a valid forge path \
                     (no empty, leading, trailing, or doubled `/` segments)"
                )));
            }
            let Some(host) = self.effective_host(host_name) else {
                return Err(Error::Config(format!(
                    "source `{name}` references unknown host `{host_name}`"
                )));
            };
            let protocol = source.protocol.or(self.protocol).unwrap_or(Protocol::Https);
            let template = host.remote.as_ref().and_then(|remote| match protocol {
                Protocol::Https => remote.https_template(),
                Protocol::Ssh => remote.ssh_template(),
            });
            if template.is_none() {
                let proto = match protocol {
                    Protocol::Https => "https",
                    Protocol::Ssh => "ssh",
                };
                return Err(Error::Config(format!(
                    "source `{name}`: protocol `{proto}` but host `{host_name}` has no {proto} remote template"
                )));
            }
        }
        Ok(())
    }

    fn effective_host(&self, name: &str) -> Option<Host> {
        effective_host(&self.hosts, name)
    }
}

/// The effective host for `name`: the built-in forge overlaid by a user
/// `[hosts]` entry of the same name (user wins), or whichever is defined.
#[must_use]
fn effective_host(hosts: &BTreeMap<String, Host>, name: &str) -> Option<Host> {
    match (builtin_forges().remove(name), hosts.get(name).cloned()) {
        (Some(b), Some(u)) => Some(b.merged_with(u)),
        (Some(h), None) | (None, Some(h)) => Some(h),
        (None, None) => None,
    }
}

/// The shipped forge registry: the single source of truth for built-in hosts.
#[must_use]
pub fn builtin_forges() -> BTreeMap<String, Host> {
    fn forge(https: &str, ssh: &str) -> Host {
        Host {
            remote: Some(RemoteConfig {
                https: Some(https.to_owned()),
                ssh: Some(ssh.to_owned()),
            }),
            auth: None,
        }
    }

    BTreeMap::from([
        (
            "github".to_owned(),
            forge("https://github.com/{path}.git", "git@github.com:{path}.git"),
        ),
        (
            "gitlab".to_owned(),
            forge("https://gitlab.com/{path}.git", "git@gitlab.com:{path}.git"),
        ),
        (
            "codeberg".to_owned(),
            forge(
                "https://codeberg.org/{path}.git",
                "git@codeberg.org:{path}.git",
            ),
        ),
        (
            "sr.ht".to_owned(),
            forge("https://git.sr.ht/{path}", "git@git.sr.ht:{path}"),
        ),
        (
            "bitbucket".to_owned(),
            forge(
                "https://bitbucket.org/{path}.git",
                "git@bitbucket.org:{path}.git",
            ),
        ),
    ])
}

/// Fills a remote template from `path`: `{path}` verbatim, `{owner}` the first
/// `/`-segment, `{repo}` the remainder (so `{owner}/{repo}` ≡ `{path}` at any depth).
#[must_use]
pub fn fill_template(template: &str, path: &str) -> String {
    let (owner, repo) = path.split_once('/').unwrap_or((path, ""));
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        if let Some((token, value)) = [("{path}", path), ("{owner}", owner), ("{repo}", repo)]
            .into_iter()
            .find(|(token, _)| tail.starts_with(token))
        {
            out.push_str(value);
            rest = &tail[token.len()..];
        } else {
            out.push('{');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Computes the effective config: `base` overlaid by `local` per spec merge semantics.
#[must_use]
pub fn merge_configs(base: Config, local: Option<Config>) -> Config {
    let Some(local) = local else { return base };
    let mut merged = base;
    merged.version = local.version;
    if local.protocol.is_some() {
        merged.protocol = local.protocol;
    }
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
    if let Some(local_wt) = local.worktree {
        merged.worktree = Some(match merged.worktree {
            Some(base_wt) => base_wt.merged_with(local_wt),
            None => local_wt,
        });
    }
    merged
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    #[serde(default)]
    pub remote: Option<RemoteConfig>,
    pub auth: Option<AuthConfig>,
}

impl Host {
    #[must_use]
    fn merged_with(mut self, local: Host) -> Host {
        if local.remote.is_some() {
            self.remote = local.remote;
        }
        if local.auth.is_some() {
            self.auth = local.auth;
        }
        self
    }
}

/// A host's remote URL templates. A bare string is the https template; a table
/// carries explicit `https`/`ssh` keys. Templates support `{owner}`, `{repo}`,
/// `{path}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RemoteConfigRaw")]
pub struct RemoteConfig {
    https: Option<String>,
    ssh: Option<String>,
}

impl RemoteConfig {
    #[must_use]
    pub fn https_template(&self) -> Option<&str> {
        self.https.as_deref()
    }

    #[must_use]
    pub fn ssh_template(&self) -> Option<&str> {
        self.ssh.as_deref()
    }
}

enum RemoteConfigRaw {
    Simple(String),
    Table(RemoteTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteTable {
    https: Option<String>,
    ssh: Option<String>,
}

impl<'de> Deserialize<'de> for RemoteConfigRaw {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RawVisitor;

        impl<'de> serde::de::Visitor<'de> for RawVisitor {
            type Value = RemoteConfigRaw;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a remote URL string or a { https, ssh } table")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(RemoteConfigRaw::Simple(v.to_owned()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                RemoteTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(RemoteConfigRaw::Table)
            }
        }

        deserializer.deserialize_any(RawVisitor)
    }
}

impl From<RemoteConfigRaw> for RemoteConfig {
    fn from(raw: RemoteConfigRaw) -> Self {
        match raw {
            RemoteConfigRaw::Simple(https) => RemoteConfig {
                https: Some(https),
                ssh: None,
            },
            RemoteConfigRaw::Table(t) => RemoteConfig {
                https: t.https,
                ssh: t.ssh,
            },
        }
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
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub protocol: Option<Protocol>,
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

/// A download-integrity digest: `<algo>:<64 hex>` where algo is `sha256` or `blake3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadDigest {
    Sha256([u8; 32]),
    Blake3([u8; 32]),
}

impl DownloadDigest {
    /// Parses an `<algo>:<hex>` digest. The body must be exactly 64 hex chars.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] for a missing `<algo>:` prefix, an unknown algo,
    /// a body that is not exactly 32 bytes of hex, or non-hex characters.
    pub fn parse(s: &str) -> Result<Self> {
        let (algo, hex) = s.split_once(':').ok_or_else(|| {
            Error::Config(format!("invalid digest `{s}`: missing `<algo>:` prefix"))
        })?;
        let bytes = decode_hex32(hex).ok_or_else(|| {
            Error::Config(format!("invalid digest `{s}`: body must be 64 hex chars"))
        })?;
        match algo {
            "sha256" => Ok(Self::Sha256(bytes)),
            "blake3" => Ok(Self::Blake3(bytes)),
            other => Err(Error::Config(format!(
                "invalid digest `{s}`: unknown algorithm `{other}` (expected sha256 or blake3)"
            ))),
        }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Sha256(b) | Self::Blake3(b) => b,
        }
    }
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        *slot = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

impl Source {
    #[must_use]
    fn merged_with(mut self, local: Source) -> Source {
        let local_git_mode = local.git.is_some();
        let local_host_mode = local.host.is_some() || local.path.is_some();
        let local_url_mode = local.url.is_some();
        if local_git_mode {
            self.git = local.git;
            self.host = None;
            self.path = None;
            self.url = None;
            self.digest = None;
        } else if local_host_mode {
            self.host = local.host;
            self.path = local.path;
            self.git = None;
            self.url = None;
            self.digest = None;
        } else if local_url_mode {
            self.url = local.url;
            self.git = None;
            self.host = None;
            self.path = None;
            self.branch = None;
            self.tag = None;
            self.rev = None;
            self.root = None;
        }
        if local.digest.is_some() {
            self.digest = local.digest;
        }
        if local.protocol.is_some() {
            self.protocol = local.protocol;
        }
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

    /// Resolves the concrete git remote for `protocol`. A git-mode source returns
    /// its literal `git` verbatim; a host-mode source resolves against the built-in
    /// forge registry overlaid by `hosts` (a user remote wins, else the built-in).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the host is in neither `hosts` nor the built-in
    /// registry, or if the effective host has no template for `protocol`.
    pub fn resolved_remote(
        &self,
        hosts: &BTreeMap<String, Host>,
        protocol: Protocol,
    ) -> Result<String> {
        if let Some(git) = &self.git {
            return Ok(git.clone());
        }
        let host_name = self.host.as_deref().unwrap_or("github");
        let path = self.path.as_deref().unwrap_or_default();

        let effective = effective_host(hosts, host_name).ok_or_else(|| {
            Error::Config(format!(
                "source `{path}` references unknown host `{host_name}`"
            ))
        })?;

        let template = effective
            .remote
            .as_ref()
            .and_then(|remote| match protocol {
                Protocol::Https => remote.https_template(),
                Protocol::Ssh => remote.ssh_template(),
            })
            .ok_or_else(|| {
                let proto = match protocol {
                    Protocol::Https => "https",
                    Protocol::Ssh => "ssh",
                };
                Error::Config(format!("host `{host_name}` has no {proto} remote template"))
            })?;
        Ok(fill_template(template, path))
    }

    #[must_use]
    pub fn source_url(&self) -> Option<&str> {
        self.url.as_deref()
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
remote = "https://github.com/{owner}/{repo}.git"
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
        assert_eq!(
            dotfiles.git.as_deref(),
            Some("https://github.com/me/dotfiles.git")
        );
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
remote = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }
"#;
        let cfg = Config::parse(toml).expect("host toml should parse");
        let github = cfg.hosts.get("github").expect("github host");
        assert_eq!(
            github
                .remote
                .as_ref()
                .expect("remote present")
                .https_template(),
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
        assert_eq!(loqui.git.as_deref(), Some("/home/soeren/dev/loqui"));
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
            effective
                .sources
                .get("loqui")
                .expect("loqui kept")
                .git
                .as_deref(),
            Some("https://github.com/srnnkls/loqui.git")
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
    fn merge_host_auth_only_override_preserves_base_remote() {
        let base = Config::parse(
            r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
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
            github
                .remote
                .as_ref()
                .expect("remote present")
                .https_template(),
            Some("https://github.com/{owner}/{repo}.git"),
            "an auth-only local override must NOT clear the base remote"
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
remote = "https://github.com/{owner}/{repo}.git"
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

    // WTI-001: [worktree].includes config + IncludeMode

    fn worktree_includes(cfg: &Config) -> &[Include] {
        cfg.worktree
            .as_ref()
            .map_or(&[][..], |w| w.includes.as_slice())
    }

    #[test]
    fn worktree_include_omitted_mode_defaults_to_symlink() {
        let cfg = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = ".envrc"

[[worktree.includes]]
path = "secrets/local.env"
mode = "copy"
"#,
        )
        .expect("worktree includes should parse");

        let includes = worktree_includes(&cfg);
        assert_eq!(includes.len(), 2, "both includes must parse");
        assert_eq!(includes[0].path, PathBuf::from(".envrc"));
        assert_eq!(
            includes[0].mode,
            IncludeMode::Symlink,
            "an omitted `mode` must default to Symlink, not Copy"
        );
        assert_eq!(includes[1].path, PathBuf::from("secrets/local.env"));
        assert_eq!(
            includes[1].mode,
            IncludeMode::Copy,
            "mode = \"copy\" must parse to IncludeMode::Copy"
        );
    }

    #[test]
    fn worktree_section_absent_is_none() {
        let cfg = Config::parse(EXAMPLE_TOML).expect("example without [worktree] still parses");
        assert!(
            cfg.worktree.is_none(),
            "an absent [worktree] section must yield None"
        );
        assert!(
            worktree_includes(&cfg).is_empty(),
            "no [worktree] means no includes"
        );
    }

    #[test]
    fn merge_local_worktree_replaces_base_includes() {
        let base = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = "a"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = "b"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let includes = worktree_includes(&effective);
        assert_eq!(
            includes.len(),
            1,
            "local [worktree] must replace the base array, never concatenate"
        );
        assert_eq!(
            includes[0].path,
            PathBuf::from("b"),
            "only the local include `b` must survive the replace"
        );
    }

    #[test]
    fn merge_local_empty_worktree_includes_clears_base() {
        let base = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = "a"

[[worktree.includes]]
path = "b"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r"
version = 1

[worktree]
includes = []
",
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        assert!(
            worktree_includes(&effective).is_empty(),
            "an explicit empty `includes = []` in local [worktree] must replace (clear) the base \
             includes, not be ignored as if unset"
        );
    }

    #[test]
    fn merge_without_local_worktree_preserves_base_worktree() {
        let base = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = "a"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.extra]
git = "/home/soeren/dev/extra"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let includes = worktree_includes(&effective);
        assert_eq!(
            includes.len(),
            1,
            "a local config without [worktree] must preserve the base [worktree]"
        );
        assert_eq!(includes[0].path, PathBuf::from("a"));
    }

    #[test]
    fn absolute_include_path_is_rejected() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = "/etc/passwd"
"#;
        let err = Config::parse(toml).expect_err("an absolute include path must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("/etc/passwd"),
                "error should name the offending absolute path, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn traversal_include_path_is_rejected() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = "../outside/secret"
"#;
        let err = Config::parse(toml)
            .expect_err("a leading `..` traversal include path must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("../outside/secret"),
                "error should name the offending traversal path, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn interior_traversal_include_path_is_rejected() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = "sub/../../escape"
"#;
        let err = Config::parse(toml)
            .expect_err("an interior `..` segment must be rejected, not just a leading one");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("sub/../../escape"),
                "error should name the offending path; an interior `..` segment must be caught \
                 by component-wise scanning, not a naive starts_with(\"..\") guard, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn empty_include_path_is_rejected() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = ""
"#;
        let err = Config::parse(toml).expect_err("an empty include path must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("empty"),
                "error should make clear the path is empty, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn current_dir_include_path_is_rejected() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = "."
"#;
        let err = Config::parse(toml).expect_err(
            "a lone `.` include path must be rejected; it would link the worktree root",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains('.'),
                "error should name the offending `.` path, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn interior_current_dir_include_path_is_rejected() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = "a/./b"
"#;
        let err = Config::parse(toml).expect_err(
            "an interior `.` segment must be rejected as a non-canonical relative path",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("a/./b"),
                "error should name the offending path; an interior `.` segment must be caught \
                 by component-wise scanning, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_include_mode_value_is_rejected_naming_it() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = ".envrc"
mode = "hardlink"
"#;
        let err = Config::parse(toml).expect_err("unknown include mode must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("hardlink"),
                "error should name the offending mode value, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    // WTI-007: IncludeMode::SubmoduleWalk

    #[test]
    fn include_mode_submodule_walk_parses() {
        let cfg = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = "vendor/lib"
mode = "submodule-walk"
"#,
        )
        .expect("a submodule-walk include mode must parse");

        let includes = worktree_includes(&cfg);
        assert_eq!(includes.len(), 1, "the single include must parse");
        assert_eq!(includes[0].path, PathBuf::from("vendor/lib"));
        assert_eq!(
            includes[0].mode,
            IncludeMode::SubmoduleWalk,
            "mode = \"submodule-walk\" must parse to IncludeMode::SubmoduleWalk via an explicit \
             serde rename (lowercase rename_all would render the variant as `submodulewalk`)"
        );
    }

    #[test]
    fn include_mode_existing_variants_undisturbed_by_submodule_walk() {
        let cfg = Config::parse(
            r#"
version = 1

[[worktree.includes]]
path = ".envrc"

[[worktree.includes]]
path = "secrets/local.env"
mode = "copy"

[[worktree.includes]]
path = "config/app.toml"
mode = "symlink"
"#,
        )
        .expect("symlink/copy/default includes must still parse alongside the new variant");

        let includes = worktree_includes(&cfg);
        assert_eq!(includes.len(), 3);
        assert_eq!(
            includes[0].mode,
            IncludeMode::Symlink,
            "an omitted mode must still default to Symlink"
        );
        assert_eq!(
            includes[1].mode,
            IncludeMode::Copy,
            "mode = \"copy\" must still parse to IncludeMode::Copy"
        );
        assert_eq!(
            includes[2].mode,
            IncludeMode::Symlink,
            "mode = \"symlink\" must still parse to IncludeMode::Symlink"
        );
    }

    #[test]
    fn unknown_submodule_walk_lookalike_mode_is_rejected_naming_it() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = "vendor/lib"
mode = "submodulewalk"
"#;
        let err = Config::parse(toml).expect_err(
            "the bare `submodulewalk` rendering must be rejected: the explicit \
             #[serde(rename = \"submodule-walk\")] is required, so the lowercase-rename form is invalid",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("submodulewalk"),
                "error should name the offending `submodulewalk` value (not merely list valid variants, \
                 which would also contain the substring `submodule`), got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_worktree_key_is_rejected_naming_it() {
        let toml = r#"
version = 1

[worktree]
bogus = "value"
"#;
        let err = Config::parse(toml).expect_err("unknown [worktree] key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("bogus"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_include_key_is_rejected_naming_it() {
        let toml = r#"
version = 1

[[worktree.includes]]
path = ".envrc"
destination = "elsewhere"
"#;
        let err = Config::parse(toml).expect_err("unknown include key must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("destination"),
                "error should name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    // HAS-001: host-aliased sources — host/path/protocol + Host.remote string-or-table

    #[test]
    fn host_remote_parses_as_single_string_template() {
        let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"
"#;
        let cfg = Config::parse(toml).expect("a string `remote` template must parse");
        let github = cfg.hosts.get("github").expect("github host");
        let remote = github.remote.as_ref().expect("remote present");
        assert_eq!(
            remote.https_template(),
            Some("https://github.com/{path}.git"),
            "a bare string `remote` is the https template"
        );
        assert_eq!(
            remote.ssh_template(),
            None,
            "a bare string `remote` carries no ssh shape"
        );
    }

    #[test]
    fn host_remote_parses_as_https_ssh_table() {
        let toml = r#"
version = 1

[hosts.company]
remote = { https = "https://git.co/{path}.git", ssh = "git@git.co:{path}.git" }
"#;
        let cfg = Config::parse(toml).expect("a `{ https, ssh }` remote table must parse");
        let company = cfg.hosts.get("company").expect("company host");
        let remote = company.remote.as_ref().expect("remote present");
        assert_eq!(
            remote.https_template(),
            Some("https://git.co/{path}.git"),
            "the https key must be exposed"
        );
        assert_eq!(
            remote.ssh_template(),
            Some("git@git.co:{path}.git"),
            "the ssh key must be exposed"
        );
    }

    #[test]
    fn host_remote_table_with_unknown_key_is_rejected_naming_it() {
        let toml = r#"
version = 1

[hosts.company]
remote = { https = "https://git.co/{path}.git", gopher = "x" }
"#;
        let err =
            Config::parse(toml).expect_err("an unknown key in the remote table must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("gopher"),
                "error should name the offending remote-table key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn host_remote_empty_table_is_rejected() {
        let toml = r"
version = 1

[hosts.company]
remote = {}
";
        let err = Config::parse(toml)
            .expect_err("an empty `remote = {}` table with no protocol keys must be rejected");
        match err {
            Error::Config(msg) => {
                let m = msg.to_lowercase();
                assert!(
                    m.contains("company")
                        || m.contains("at least one")
                        || m.contains("protocol")
                        || m.contains("empty"),
                    "empty-remote-table rejection must be a domain error explaining the \
                     missing protocol key (mention the host `company`, or \"at least one\"/\
                     \"protocol\"/\"empty\"), not a generic serde error, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn host_path_source_parses_and_exposes_fields() {
        let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
branch = "main"
"#;
        let cfg = Config::parse(toml).expect("a host+path source must parse");
        let tropos = cfg.sources.get("tropos").expect("tropos source");
        assert_eq!(tropos.host.as_deref(), Some("github"));
        assert_eq!(tropos.path.as_deref(), Some("srnnkls/tropos"));
        assert_eq!(tropos.branch.as_deref(), Some("main"));
        assert!(
            tropos.git.is_none(),
            "a host+path source must carry no literal git remote"
        );
    }

    #[test]
    fn source_with_both_git_and_host_is_rejected_naming_source() {
        let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
git = "https://github.com/srnnkls/tropos.git"
host = "github"
path = "srnnkls/tropos"
"#;
        let err = Config::parse(toml)
            .expect_err("a source that sets both git and host must be rejected (mode exclusivity)");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("tropos"),
                "mode-exclusivity error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn source_with_git_and_path_is_rejected_naming_source() {
        let toml = r#"
version = 1

[sources.tropos]
git = "https://github.com/srnnkls/tropos.git"
path = "srnnkls/tropos"
"#;
        let err = Config::parse(toml).expect_err(
            "a source that sets both `git` and `path` is dual-mode (path implies host-mode) \
             and must be rejected",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("tropos"),
                "mode-exclusivity error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn source_with_host_but_no_path_is_rejected_naming_source() {
        let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
"#;
        let err = Config::parse(toml)
            .expect_err("a host source without a path must be rejected (incomplete mode group)");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("tropos"),
                "error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn top_level_protocol_ssh_parses_and_default_is_https() {
        let with_ssh = Config::parse(
            r#"
version = 1
protocol = "ssh"
"#,
        )
        .expect("a top-level protocol = \"ssh\" must parse");
        assert_eq!(
            with_ssh.protocol,
            Some(Protocol::Ssh),
            "top-level `protocol = ssh` must parse to Protocol::Ssh"
        );

        let with_https = Config::parse(
            r#"
version = 1
protocol = "https"
"#,
        )
        .expect("a top-level protocol = \"https\" must parse");
        assert_eq!(
            with_https.protocol,
            Some(Protocol::Https),
            "top-level `protocol = https` must parse to Protocol::Https (both enum arms reachable)"
        );

        let defaulted = Config::parse("version = 1\n").expect("omitting protocol must parse");
        assert!(
            defaulted.protocol.is_none(),
            "an omitted top-level protocol is None (https is the effective default downstream)"
        );
    }

    #[test]
    fn merge_host_path_source_branch_only_override_preserves_mode_and_remote() {
        let base = Config::parse(
            r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
tag = "v1.0"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.tropos]
branch = "main"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        let tropos = effective.sources.get("tropos").expect("tropos kept");
        assert_eq!(
            tropos.host.as_deref(),
            Some("github"),
            "a branch-only local override must NOT clear the base host (mode group is atomic)"
        );
        assert_eq!(
            tropos.path.as_deref(),
            Some("srnnkls/tropos"),
            "a branch-only local override must preserve the base path"
        );
        assert!(
            tropos.git.is_none(),
            "the host+path mode must not flip to literal-git on a partial override"
        );
        assert_eq!(
            tropos.branch.as_deref(),
            Some("main"),
            "the local branch override must take effect"
        );
        assert!(
            tropos.tag.is_none(),
            "the local branch override clears the base refspec group (tag)"
        );
    }

    #[test]
    fn merge_local_source_referencing_base_only_host_validates_after_merge() {
        let base = Config::parse(
            r#"
version = 1

[hosts.company]
remote = "https://git.co/{path}.git"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1

[sources.internal]
host = "company"
path = "team/sub/proj"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        effective.validate().expect(
            "a local source referencing a host defined only in the base must pass POST-MERGE \
             validation (the host is unknown per-file but known after merge)",
        );
    }

    #[test]
    fn protocol_ssh_with_https_only_remote_fails_post_merge_validation() {
        let cfg = Config::parse(
            r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
protocol = "ssh"
"#,
        )
        .expect("the document parses; the protocol/remote mismatch is a post-merge validation");
        let err = cfg
            .validate()
            .expect_err("protocol = ssh against an https-only remote must fail validation");
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("tropos"),
                    "validation error must name the offending source, got: {msg}"
                );
                assert!(
                    msg.contains("github"),
                    "validation error must name the offending host, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn unknown_host_reference_fails_post_merge_validation_naming_source_and_host() {
        let cfg = Config::parse(
            r#"
version = 1

[sources.tropos]
host = "ghost"
path = "srnnkls/tropos"
"#,
        )
        .expect(
            "a single-file source referencing an undefined host parses; validity is post-merge",
        );
        let err = cfg
            .validate()
            .expect_err("a host with no built-in or [hosts] definition must fail validation");
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("tropos"),
                    "unknown-host error must name the source, got: {msg}"
                );
                assert!(
                    msg.contains("ghost"),
                    "unknown-host error must name the host, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn merge_configs_overlays_top_level_protocol() {
        let base = Config::parse(
            r#"
version = 1
protocol = "https"
"#,
        )
        .expect("base parses");
        let local = Config::parse(
            r#"
version = 1
protocol = "ssh"
"#,
        )
        .expect("local parses");

        let effective = merge_configs(base, Some(local));
        assert_eq!(
            effective.protocol,
            Some(Protocol::Ssh),
            "merge_configs must overlay the top-level protocol (local wins)"
        );
    }

    #[test]
    fn merge_configs_keeps_base_protocol_when_local_omits_it() {
        let base = Config::parse(
            r#"
version = 1
protocol = "ssh"
"#,
        )
        .expect("base parses");
        let local = Config::parse("version = 1\n").expect("local parses");

        let effective = merge_configs(base, Some(local));
        assert_eq!(
            effective.protocol,
            Some(Protocol::Ssh),
            "a local config that omits protocol must preserve the base protocol"
        );
    }

    #[test]
    fn source_with_no_mode_is_allowed_as_partial_overlay() {
        let toml = r#"
version = 1

[sources.x]
branch = "main"
"#;
        let cfg = Config::parse(toml).expect(
            "a mode-less source fragment (no git, no host/path) must parse so a local override \
             like `[sources.x]\\nbranch = \"main\"` works as a partial overlay",
        );
        let x = cfg.sources.get("x").expect("x source");
        assert!(x.git.is_none(), "no literal git on a mode-less fragment");
        assert!(x.host.is_none(), "no host on a mode-less fragment");
        assert!(x.path.is_none(), "no path on a mode-less fragment");
        assert_eq!(
            x.branch.as_deref(),
            Some("main"),
            "the overlay field must survive parsing"
        );
    }

    #[test]
    fn source_with_path_and_no_host_defaults_to_github() {
        let toml = r#"
version = 1

[sources.tropos]
path = "srnnkls/tropos"
"#;
        let cfg = Config::parse(toml)
            .expect("a source with `path` but no `host` defaults host to github and parses");
        cfg.validate()
            .expect("a path-only source must validate (host defaults to github)");
        let tropos = cfg.sources.get("tropos").expect("tropos source present");
        assert_eq!(
            tropos
                .resolved_remote(&BTreeMap::new(), Protocol::Https)
                .expect("path-only source resolves against the built-in github forge"),
            "https://github.com/srnnkls/tropos.git",
            "an omitted `host` with `path` set must default to github, not merely parse Ok"
        );
    }

    #[test]
    fn host_source_with_protocol_matching_remote_passes_validation() {
        let cfg = Config::parse(
            r#"
version = 1

[hosts.company]
remote = { https = "https://git.co/{path}.git", ssh = "git@git.co:{path}.git" }

[sources.internal]
host = "company"
path = "team/sub/proj"
protocol = "ssh"
"#,
        )
        .expect("a host+path source with a matching protocol must parse");
        cfg.validate().expect(
            "protocol = ssh against a remote table that HAS an ssh key must pass validation \
             (guards against a validate() that always errors)",
        );
    }

    #[test]
    fn protocol_https_with_ssh_only_remote_fails_validation() {
        let cfg = Config::parse(
            r#"
version = 1

[hosts.sshonly]
remote = { ssh = "git@h:{path}.git" }

[sources.repo]
host = "sshonly"
path = "o/r"
"#,
        )
        .expect(
            "the document parses; the effective-protocol/remote mismatch is post-merge validation",
        );
        let err = cfg.validate().expect_err(
            "a source whose effective protocol is the default https against an ssh-only remote \
             (no https template) must fail validation",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("repo"),
                    "validation error must name the offending source, got: {msg}"
                );
                assert!(
                    msg.contains("sshonly"),
                    "validation error must name the offending host, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn protocol_ssh_with_ssh_only_remote_passes_validation() {
        let cfg = Config::parse(
            r#"
version = 1

[hosts.sshonly]
remote = { ssh = "git@h:{path}.git" }

[sources.repo]
host = "sshonly"
path = "o/r"
protocol = "ssh"
"#,
        )
        .expect("a source against an ssh-only host with protocol = ssh must parse");
        cfg.validate().expect(
            "protocol = ssh against an ssh-only remote that HAS an ssh template must pass \
             validation (guards against an over-broad missing-template error)",
        );
    }

    #[test]
    fn shipped_example_toml_parses_and_validates() {
        let cfg = Config::parse(include_str!("../phora.example.toml"))
            .expect("the shipped phora.example.toml must parse");
        cfg.validate()
            .expect("the shipped phora.example.toml must pass post-merge validation");
    }

    // HAS-002: resolved_remote + single built-in forge registry

    fn hosts_of(toml: &str) -> BTreeMap<String, Host> {
        Config::parse(toml).expect("hosts toml parses").hosts
    }

    fn source_of(toml: &str, name: &str) -> Source {
        Config::parse(toml)
            .expect("source toml parses")
            .sources
            .remove(name)
            .expect("named source present")
    }

    #[test]
    fn resolved_remote_github_https_and_ssh_for_owner_repo_path() {
        let host_toml = r#"
version = 1

[hosts.github]
remote = { https = "https://github.com/{owner}/{repo}.git", ssh = "git@github.com:{owner}/{repo}.git" }
"#;
        let hosts = hosts_of(host_toml);
        let source = source_of(
            r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
            "tropos",
        );

        assert_eq!(
            source
                .resolved_remote(&hosts, Protocol::Https)
                .expect("https resolves"),
            "https://github.com/srnnkls/tropos.git",
            "https template must fill {{owner}}/{{repo}} from the path"
        );
        assert_eq!(
            source
                .resolved_remote(&hosts, Protocol::Ssh)
                .expect("ssh resolves"),
            "git@github.com:srnnkls/tropos.git",
            "ssh template must produce the scp-style remote"
        );
    }

    #[test]
    fn resolved_remote_github_uses_builtin_when_no_user_host() {
        let source = source_of(
            r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
            "tropos",
        );
        let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

        let https = source
            .resolved_remote(&no_user_hosts, Protocol::Https)
            .expect("built-in github https resolves with no user hosts");
        assert_eq!(
            https, "https://github.com/srnnkls/tropos.git",
            "the built-in github forge must resolve EXACTLY without a user [hosts.github] def"
        );

        let ssh = source
            .resolved_remote(&no_user_hosts, Protocol::Ssh)
            .expect("built-in github ssh resolves with no user hosts");
        assert_eq!(
            ssh, "git@github.com:srnnkls/tropos.git",
            "the built-in github forge must ship the EXACT scp-style ssh shape"
        );
    }

    #[test]
    fn resolved_remote_gitlab_subgroup_path_reconstructs_full_path() {
        let source = source_of(
            r#"
version = 1

[sources.internal]
host = "gitlab"
path = "group/sub/proj"
"#,
            "internal",
        );
        let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

        let https = source
            .resolved_remote(&no_user_hosts, Protocol::Https)
            .expect("gitlab subgroup https resolves");
        assert!(
            https.contains("group/sub/proj"),
            "a gitlab subgroup path must reconstruct fully via {{owner}}/{{repo}} ≡ {{path}}, \
             not collapse to the first/last segment, got: {https}"
        );

        let ssh = source
            .resolved_remote(&no_user_hosts, Protocol::Ssh)
            .expect("gitlab subgroup ssh resolves");
        assert!(
            ssh.contains("group/sub/proj"),
            "the ssh shape must also carry the full subgroup path, got: {ssh}"
        );
    }

    #[test]
    fn resolved_remote_srht_uses_tilde_path_shape() {
        let source = source_of(
            r#"
version = 1

[sources.aerc]
host = "sr.ht"
path = "~rjarry/aerc"
"#,
            "aerc",
        );
        let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

        let https = source
            .resolved_remote(&no_user_hosts, Protocol::Https)
            .expect("sr.ht https resolves via the built-in {path} template");
        assert!(
            https.contains("~rjarry/aerc"),
            "the built-in sr.ht template must use {{path}} verbatim so the ~user shape survives, \
             got: {https}"
        );
        assert!(
            https.contains('~'),
            "sr.ht resolved remote must retain the leading ~, got: {https}"
        );

        let ssh = source
            .resolved_remote(&no_user_hosts, Protocol::Ssh)
            .expect("sr.ht ssh resolves via the built-in {path} template");
        assert!(
            ssh.contains("~rjarry/aerc"),
            "the built-in sr.ht ssh template must also use {{path}} verbatim so the ~user shape \
             survives under ssh, got: {ssh}"
        );
        assert!(
            ssh.contains('~'),
            "sr.ht ssh resolved remote must retain the leading ~, got: {ssh}"
        );
    }

    #[test]
    fn resolved_remote_user_host_overrides_builtin_github() {
        let host_toml = r#"
version = 1

[hosts.github]
remote = { https = "https://ghe.corp.example/{owner}/{repo}.git", ssh = "git@ghe.corp.example:{owner}/{repo}.git" }
"#;
        let hosts = hosts_of(host_toml);
        let source = source_of(
            r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
            "tropos",
        );

        let https = source
            .resolved_remote(&hosts, Protocol::Https)
            .expect("override resolves");
        assert_eq!(
            https, "https://ghe.corp.example/srnnkls/tropos.git",
            "a user [hosts.github] must override the built-in github forge in resolved_remote"
        );

        let ssh = source
            .resolved_remote(&hosts, Protocol::Ssh)
            .expect("override resolves under ssh");
        assert_eq!(
            ssh, "git@ghe.corp.example:srnnkls/tropos.git",
            "the user [hosts.github] ssh template must override the built-in github ssh shape too, \
             not fall back to git@github.com"
        );
    }

    #[test]
    fn resolved_remote_git_mode_returns_literal_verbatim_ignoring_protocol() {
        let source = source_of(
            r#"
version = 1

[sources.dotfiles]
git = "https://example.com/me/dotfiles.git"
"#,
            "dotfiles",
        );
        let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

        assert_eq!(
            source
                .resolved_remote(&no_user_hosts, Protocol::Https)
                .expect("git-mode resolves"),
            "https://example.com/me/dotfiles.git",
            "a git-mode source returns its literal git verbatim under https"
        );
        assert_eq!(
            source
                .resolved_remote(&no_user_hosts, Protocol::Ssh)
                .expect("git-mode resolves under ssh too"),
            "https://example.com/me/dotfiles.git",
            "a git-mode source ignores protocol: the literal git is returned verbatim under ssh"
        );
    }

    #[test]
    fn resolved_remote_unknown_host_errors() {
        let source = source_of(
            r#"
version = 1

[sources.tropos]
host = "ghost"
path = "srnnkls/tropos"
"#,
            "tropos",
        );
        let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

        let err = source
            .resolved_remote(&no_user_hosts, Protocol::Https)
            .expect_err("an unknown host (no built-in, no user def) must error");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("ghost") && msg.contains("tropos"),
                "the unknown-host error must name BOTH the offending source and the host, \
                 consistent with HAS-001's validate() wording \
                 (`source `tropos` references unknown host `ghost``), got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn resolved_remote_ssh_without_ssh_template_errors() {
        let host_toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
"#;
        let hosts = hosts_of(host_toml);
        let source = source_of(
            r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
            "tropos",
        );

        let err = source
            .resolved_remote(&hosts, Protocol::Ssh)
            .expect_err("protocol = ssh against an https-only remote must error");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("github") && (msg.contains("ssh") || msg.contains("template")),
                "the missing-template error must NAME the offending host AND indicate the missing \
                 ssh/template; an error that merely contains \"ssh\" without naming the host \
                 must fail this test, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn builtin_forges_ship_all_five_with_both_shapes() {
        let forges = builtin_forges();
        for name in ["github", "gitlab", "codeberg", "sr.ht", "bitbucket"] {
            let host = forges
                .get(name)
                .unwrap_or_else(|| panic!("built-in forge `{name}` must ship"));
            let remote = host
                .remote
                .as_ref()
                .unwrap_or_else(|| panic!("built-in forge `{name}` must carry a remote"));
            assert!(
                remote.https_template().is_some(),
                "built-in forge `{name}` must ship an https shape"
            );
            assert!(
                remote.ssh_template().is_some(),
                "built-in forge `{name}` must ship an ssh shape"
            );
        }
    }

    // HTP-001: url mode + DownloadDigest

    /// Build a `[sources.s]` document body with no implicit `git` line (unlike the
    /// `parse_source` helper, which always injects a git remote and would make a
    /// `url` source dual-mode).
    fn parse_url_source(body: &str) -> Result<Source> {
        let toml = format!("version = 1\n\n[sources.s]\n{body}");
        Config::parse(&toml).map(|mut cfg| {
            cfg.sources
                .remove("s")
                .expect("source `s` present after parse")
        })
    }

    #[test]
    fn url_only_source_parses_and_source_url_returns_it() {
        let s = parse_url_source("url = \"https://example.com/foo.tar.gz\"\n")
            .expect("a url-only source must parse");
        assert_eq!(
            s.source_url(),
            Some("https://example.com/foo.tar.gz"),
            "source_url() must return the configured url for a url-mode source"
        );
        assert!(
            s.git.is_none(),
            "a url-mode source carries no literal git remote"
        );
        assert!(
            s.host.is_none() && s.path.is_none(),
            "a url-mode source carries neither host nor path"
        );
    }

    #[test]
    fn non_url_source_has_no_source_url() {
        let git_mode = parse_source("");
        assert_eq!(
            git_mode.source_url(),
            None,
            "a git-mode source must not report a source_url()"
        );
    }

    #[test]
    fn url_with_digest_parses_and_exposes_digest_string() {
        let s = parse_url_source(
            "url = \"https://example.com/foo.tar.gz\"\n\
             digest = \"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n",
        )
        .expect("a url source with a digest must parse");
        assert_eq!(
            s.digest.as_deref(),
            Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            "the optional `digest` string must round-trip onto the Source"
        );
    }

    #[test]
    fn source_with_url_and_git_is_rejected_naming_source() {
        let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
git = "https://github.com/me/foo.git"
"#;
        let err = Config::parse(toml).expect_err(
            "a source that sets both `url` and `git` is dual-mode and must be rejected",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "three-way mode-exclusivity error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn source_with_url_and_host_path_is_rejected_naming_source() {
        let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
host = "github"
path = "me/foo"
"#;
        let err = Config::parse(toml).expect_err(
            "a source that sets both `url` and host+path is dual-mode and must be rejected",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "three-way mode-exclusivity error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn url_source_with_branch_is_rejected_naming_source() {
        let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
branch = "main"
"#;
        let err = Config::parse(toml)
            .expect_err("`branch` is meaningless on a static url resource and must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "the url-vs-refspec error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn url_source_with_root_is_rejected_naming_source() {
        let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
root = "subdir"
"#;
        let err = Config::parse(toml)
            .expect_err("`root` is meaningless on a pre-stripped url archive and must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "the url-vs-root error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn url_source_with_tag_is_rejected() {
        let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
tag = "v1.0"
"#;
        let err = Config::parse(toml).expect_err(
            "`tag` on a url source must be rejected (a static resource has no refspec)",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "the url-vs-refspec error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn url_source_with_rev_is_rejected() {
        let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
rev = "abc123"
"#;
        let err = Config::parse(toml).expect_err(
            "`rev` on a url source must be rejected (a static resource has no refspec)",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "the url-vs-refspec error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn merge_local_url_override_clears_base_git_refspec_and_root() {
        let base = Config::parse(
            r#"
version = 1

[sources.pkg]
git = "https://github.com/me/pkg.git"
branch = "main"
root = "subdir"
"#,
        )
        .expect("base parses");
        let local = parse_url_source("url = \"https://example.com/foo.tar.gz\"\n")
            .expect("local url source parses");
        let merged = base
            .sources
            .get("pkg")
            .expect("pkg present")
            .clone()
            .merged_with(local);

        assert_eq!(
            merged.source_url(),
            Some("https://example.com/foo.tar.gz"),
            "a local url override must switch the merged source into url mode"
        );
        assert!(merged.git.is_none(), "switching to url mode must clear git");
        assert!(
            merged.branch.is_none() && merged.tag.is_none() && merged.rev.is_none(),
            "switching to url mode must clear the stale base refspec"
        );
        assert!(
            merged.root.is_none(),
            "switching to url mode must clear the stale base root"
        );
    }

    #[test]
    fn validate_rejects_url_source_with_stale_refspec() {
        let mut cfg = Config::parse(
            r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
"#,
        )
        .expect("a url-only source parses; the stale refspec is injected post-parse");
        let pkg = cfg.sources.get_mut("pkg").expect("pkg present");
        pkg.branch = Some("main".to_owned());
        pkg.root = Some(PathBuf::from("subdir"));

        let err = cfg.validate().expect_err(
            "a url source carrying a stale `branch`/`root` must be rejected by validate()",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "validate() url-vs-refspec error must name the source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn empty_url_is_rejected_naming_source() {
        let toml = r#"
version = 1

[sources.pkg]
url = ""
"#;
        let err = Config::parse(toml)
            .expect_err("an empty `url` is not a usable resource and must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("pkg"),
                "empty-url error must name the offending source, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn download_digest_parses_sha256_into_sha256_variant_with_bytes() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let expected: [u8; 32] = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
            0x89, 0xab, 0xcd, 0xef,
        ];
        let digest =
            DownloadDigest::parse(&format!("sha256:{hex}")).expect("a sha256 digest must parse");
        assert!(
            matches!(digest, DownloadDigest::Sha256(_)),
            "a `sha256:` prefix must parse into the Sha256 variant, not Blake3"
        );
        assert_eq!(
            digest.bytes(),
            expected.as_slice(),
            "the decoded sha256 digest bytes must match the hex, not merely be non-empty"
        );
    }

    #[test]
    fn download_digest_parses_blake3_into_blake3_variant_with_bytes() {
        let hex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let expected: [u8; 32] = [
            0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad,
            0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
            0xde, 0xad, 0xbe, 0xef,
        ];
        let digest =
            DownloadDigest::parse(&format!("blake3:{hex}")).expect("a blake3 digest must parse");
        assert!(
            matches!(digest, DownloadDigest::Blake3(_)),
            "a `blake3:` prefix must parse into the Blake3 variant, not Sha256"
        );
        assert_eq!(
            digest.bytes(),
            expected.as_slice(),
            "the decoded blake3 digest bytes must match the hex"
        );
    }

    #[test]
    fn download_digest_rejects_unknown_algo_prefix() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(
            DownloadDigest::parse(&format!("md5:{hex}")).is_err(),
            "an unknown algo prefix (md5) must be rejected, not coerced to a known variant"
        );
        assert!(
            DownloadDigest::parse(hex).is_err(),
            "a bare hex string with no `<algo>:` prefix must be rejected"
        );
    }

    #[test]
    fn download_digest_rejects_wrong_length_and_non_hex() {
        assert!(
            DownloadDigest::parse("sha256:abcd").is_err(),
            "a too-short hex body must be rejected (digest must be 32 bytes / 64 hex chars)"
        );
        assert!(
            DownloadDigest::parse(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdefff"
            )
            .is_err(),
            "a too-long hex body must be rejected"
        );
        assert!(
            DownloadDigest::parse(
                "blake3:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
            )
            .is_err(),
            "non-hex characters must be rejected"
        );
    }

    #[test]
    fn registry_digest_still_rejects_sha256_prefix_not_widened() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(
            crate::registry::Digest::parse(&format!("sha256:{hex}")).is_err(),
            "registry::Digest must remain blake3-only; adding DownloadDigest's sha256 support \
             must NOT widen the registry content digest to accept a `sha256:` prefix"
        );
        assert!(
            crate::registry::Digest::parse(&format!("blake3:{hex}")).is_ok(),
            "registry::Digest must still accept a blake3 prefix (guards against an over-broad \
             rejection that breaks the existing content digest)"
        );
    }
}
