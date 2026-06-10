//! Source DTOs and their typed, single-kind parsed form.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::source::ExportPolicy;

use super::host::Host;
use super::{Protocol, effective_host, fill_template};

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
    pub repo: Option<String>,
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

/// Which backend a source routes to, derived from its declared fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    Git,
    Host,
    Url,
}

/// A source's resolved kind. Exactly one kind per source; illegal combinations
/// are unrepresentable. Only `Url` carries no refspec/root (rejected at the parse edge).
#[derive(Debug, Clone)]
pub enum Remote {
    /// A literal git remote (`git = <url>`) or the `git = <localpath>` alias.
    Git(String),
    /// A local path source (`path = <local>`), resolved verbatim.
    Path(String),
    /// A static resource fetched once; no git ref.
    Url {
        url: String,
        digest: Option<crate::kernel::Digest>,
    },
    /// A forge source resolved against the host registry.
    Host {
        host: String,
        repo: String,
        protocol: Option<Protocol>,
    },
}

impl Remote {
    #[must_use]
    pub fn mode(&self) -> SourceMode {
        match self {
            Self::Git(_) | Self::Path(_) => SourceMode::Git,
            Self::Host { .. } => SourceMode::Host,
            Self::Url { .. } => SourceMode::Url,
        }
    }
}

/// A `Source` parsed once into a typed, single-kind shape. This is the validated
/// form the rest of the system reasons about; raw `Source` is the wire DTO only.
#[derive(Debug, Clone)]
pub struct ParsedSource {
    pub remote: Remote,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub root: Option<PathBuf>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    allow_symlinks: Option<bool>,
    allow_submodules: Option<bool>,
    preserve_executable: Option<bool>,
    deploy: Option<DeployMode>,
}

impl ParsedSource {
    /// Parses a merged raw `Source` into the typed single-kind shape.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the source does not resolve to exactly one
    /// kind, sets a refspec/root on a `url`, carries an empty `url`/`repo`, or
    /// carries a malformed `url` digest.
    pub fn parse(name: &str, source: &Source) -> Result<Self> {
        let remote = source.classify(name)?.ok_or_else(|| {
            Error::Config(format!(
                "source `{name}` must resolve to exactly one of a local `path`, \
                 a forge `host`/`repo`, a literal `git`, or a `url`"
            ))
        })?;
        Ok(Self {
            remote,
            branch: source.branch.clone(),
            tag: source.tag.clone(),
            rev: source.rev.clone(),
            root: source.root.clone(),
            include: source.include.clone(),
            exclude: source.exclude.clone(),
            allow_symlinks: source.allow_symlinks,
            allow_submodules: source.allow_submodules,
            preserve_executable: source.preserve_executable,
            deploy: source.deploy,
        })
    }

    #[must_use]
    pub fn mode(&self) -> SourceMode {
        self.remote.mode()
    }

    /// Resolves the concrete git remote for `protocol`. `Git`/`Path` resolve
    /// verbatim; `Host` resolves against the host registry; `Url` has no remote.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if a `Host` source's host is unknown or has no
    /// template for `protocol`.
    pub fn resolved_remote(
        &self,
        hosts: &BTreeMap<String, Host>,
        protocol: Protocol,
    ) -> Result<String> {
        match &self.remote {
            Remote::Git(remote) => Ok(remote.clone()),
            Remote::Path(path) => Ok(path.clone()),
            Remote::Url { .. } => Ok(String::new()),
            Remote::Host { host, repo, .. } => resolve_forge(hosts, host, repo, protocol),
        }
    }

    #[must_use]
    pub fn source_url(&self) -> Option<&str> {
        match &self.remote {
            Remote::Url { url, .. } => Some(url),
            _ => None,
        }
    }

    #[must_use]
    pub fn protocol(&self) -> Option<Protocol> {
        match &self.remote {
            Remote::Host { protocol, .. } => *protocol,
            _ => None,
        }
    }

    #[must_use]
    pub fn digest(&self) -> Option<crate::kernel::Digest> {
        match &self.remote {
            Remote::Url { digest, .. } => *digest,
            _ => None,
        }
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
        if matches!(self.remote, Remote::Url { .. }) {
            Refspec::None
        } else if let Some(rev) = &self.rev {
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

/// Resolves a forge `host`/`repo` against the built-in registry overlaid by `hosts`.
fn resolve_forge(
    hosts: &BTreeMap<String, Host>,
    host_name: &str,
    repo: &str,
    protocol: Protocol,
) -> Result<String> {
    let effective = effective_host(hosts, host_name).ok_or_else(|| {
        Error::Config(format!(
            "source `{repo}` references unknown host `{host_name}`"
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
    Ok(fill_template(template, repo))
}

impl Source {
    #[must_use]
    pub(super) fn merged_with(mut self, local: Source) -> Source {
        let local_git_kind = local.git.is_some();
        let local_forge_kind = local.host.is_some() || local.repo.is_some();
        let local_local_kind = local.path.is_some() && !local_forge_kind;
        let local_url_kind = local.url.is_some();
        if local_git_kind {
            self.git = local.git;
            self.host = None;
            self.repo = None;
            self.path = None;
            self.url = None;
            self.digest = None;
        } else if local_forge_kind {
            self.host = local.host;
            self.repo = local.repo;
            self.path = local.path;
            self.git = None;
            self.url = None;
            self.digest = None;
        } else if local_local_kind {
            self.path = local.path;
            self.host = None;
            self.repo = None;
            self.git = None;
            self.url = None;
            self.digest = None;
        } else if local_url_kind {
            self.url = local.url;
            self.git = None;
            self.host = None;
            self.repo = None;
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

    #[must_use]
    fn is_forge(&self) -> bool {
        self.host.is_some() || self.repo.is_some()
    }

    /// `repo`, falling back to the deprecated `path` alias for the forge owner/repo.
    #[must_use]
    fn forge_path(&self) -> Option<&str> {
        self.repo.as_deref().or(self.path.as_deref())
    }

    /// `path` as a local-source key — `None` when it is the `host`+`path` forge alias (host set, no `repo`).
    #[must_use]
    fn local_path(&self) -> Option<&str> {
        if self.host.is_some() && self.repo.is_none() {
            return None;
        }
        self.path.as_deref()
    }

    /// The single declared kind, or `None` for a mode-less partial overlay.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] for more than one kind, an incomplete forge
    /// group, a refspec/root on a `url`, an empty `url`, or a malformed digest.
    pub(super) fn classify(&self, name: &str) -> Result<Option<Remote>> {
        if u8::from(self.branch.is_some())
            + u8::from(self.tag.is_some())
            + u8::from(self.rev.is_some())
            > 1
        {
            return Err(Error::Config(format!(
                "source `{name}` sets more than one of branch/tag/rev"
            )));
        }
        let local = self.local_path();
        let kinds = u8::from(self.git.is_some())
            + u8::from(self.url.is_some())
            + u8::from(self.is_forge())
            + u8::from(local.is_some());
        if kinds > 1 {
            return Err(Error::Config(format!(
                "source `{name}` sets more than one source kind \
                 (local `path`, forge `host`/`repo`, literal `git`, and `url` \
                 are mutually exclusive)"
            )));
        }
        if let Some(url) = &self.url {
            if self.branch.is_some()
                || self.tag.is_some()
                || self.rev.is_some()
                || self.root.is_some()
            {
                return Err(Error::Config(format!(
                    "source `{name}`: `branch`/`tag`/`rev`/`root` are meaningless on a `url` source"
                )));
            }
            if url.trim().is_empty() {
                return Err(Error::Config(format!(
                    "source `{name}`: `url` must not be empty"
                )));
            }
            let digest = self
                .digest
                .as_deref()
                .map(|raw| {
                    raw.parse::<crate::kernel::Digest>()
                        .map_err(|e| Error::Config(format!("source `{name}`: {e}")))
                })
                .transpose()?;
            return Ok(Some(Remote::Url {
                url: url.clone(),
                digest,
            }));
        }
        if let Some(git) = &self.git {
            return Ok(Some(Remote::Git(git.clone())));
        }
        if self.is_forge() {
            let host = self.host.as_deref().unwrap_or("github");
            let repo = self.forge_path().ok_or_else(|| {
                Error::Config(format!(
                    "source `{name}`: `host` set without a `repo` (incomplete forge group)"
                ))
            })?;
            return Ok(Some(Remote::Host {
                host: host.to_owned(),
                repo: repo.to_owned(),
                protocol: self.protocol,
            }));
        }
        if let Some(path) = local {
            return Ok(Some(Remote::Path(path.to_owned())));
        }
        Ok(None)
    }
}

#[derive(Debug, Clone)]
pub enum Refspec {
    Branch(String),
    Tag(String),
    Rev(String),
    /// A url source has no git ref; its mirror lives at refs/heads/phora.
    None,
}

impl std::fmt::Display for Refspec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Branch(s) | Self::Tag(s) | Self::Rev(s) => write!(f, "{s}"),
            Self::None => write!(f, ""),
        }
    }
}
