//! Config DTOs (`phora.toml`). This module is a boundary, so it carries serde.

mod host;
mod source;
mod target;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{Error, Result};
pub use crate::source::Protocol;

pub use host::{AuthConfig, Host, RemoteConfig, builtin_forges};
pub use source::{DeployMode, ParsedSource, Refspec, Remote, Source, SourceMode};
pub use target::{LayoutConfig, LayoutKind, Target};

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
            source.classify(name)?;
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
            let parsed = ParsedSource::parse(name, source)?;
            let Remote::Host {
                host: host_name,
                repo,
                ..
            } = &parsed.remote
            else {
                continue;
            };
            if repo.trim().is_empty() || repo.split('/').any(str::is_empty) {
                return Err(Error::Config(format!(
                    "source `{name}`: `repo` `{repo}` is not a valid forge path \
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

    /// Parses every source into its typed form, keyed by name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if any source fails the typed parse.
    pub fn parsed_sources(&self) -> Result<BTreeMap<String, ParsedSource>> {
        self.sources
            .iter()
            .map(|(name, source)| Ok((name.clone(), ParsedSource::parse(name, source)?)))
            .collect()
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
    merged
}
