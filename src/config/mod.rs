//! Config DTOs (`phora.toml`). This module is a boundary, so it carries serde.

mod hooks;
mod host;
mod migrate;
mod source;
mod target;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::error::{Error, Result};
pub use crate::source::Protocol;

pub use hooks::{GlobalHooks, HookCommand, HookWhen, TargetHooks};
pub use host::{AuthConfig, Host, RemoteConfig, builtin_forges};
pub use migrate::MigrationWarning;
pub use source::{DeployMode, ParsedSource, Refspec, Remote, Source, SourceMode};
pub use target::{
    Binding, LayoutConfig, LayoutKind, ResolvedBinding, SourceFields, Target, TemplateOptIn,
};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default)]
    pub auto_target: Option<bool>,
}

impl Defaults {
    #[must_use]
    pub fn auto_target(&self) -> bool {
        self.auto_target.unwrap_or(true)
    }
}

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
    pub defaults: Defaults,
    #[serde(default)]
    pub hooks: Option<GlobalHooks>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

impl Config {
    /// Parses and validates a `phora.toml` document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the document is not valid TOML, contains an
    /// unknown key, or a source sets more than one of `branch`/`tag`/`rev`.
    pub fn parse(s: &str) -> Result<Self> {
        reject_legacy_binding_arrays(s)?;
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
        for (name, source) in &self.sources {
            reject_unsafe_source_selectors(name, source)?;
        }
        self.validate_bindings()?;
        Ok(())
    }

    fn validate_bindings(&self) -> Result<()> {
        for (target_name, target) in &self.targets {
            for (identity, binding) in target.sources.iter().flatten() {
                if crate::kernel::safe_component(identity).is_err() {
                    return Err(Error::Config(format!(
                        "target `{target_name}`: binding identity `{identity}` must be a single safe path component"
                    )));
                }
                let effective = binding.effective_source(identity);
                let Some(source) = self.sources.get(effective) else {
                    return Err(Error::Config(format!(
                        "target `{target_name}` references undefined source `{effective}`"
                    )));
                };
                reject_url_slice(effective, binding, source)?;
                reject_link_ref(effective, binding, source)?;
                reject_multi_ref(effective, binding)?;
                reject_map(effective, binding)?;
            }
            for resolved in target.resolve_sources(&self.sources) {
                reject_unsafe_selectors(target_name, &resolved)?;
                reject_basename_collision(target_name, &resolved)?;
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

    /// `base_dir` resolves relative `path` values for the local-dir check.
    #[must_use]
    pub fn migration_warnings(&self, base_dir: &std::path::Path) -> Vec<MigrationWarning> {
        self.sources
            .iter()
            .filter_map(|(name, source)| migrate::warning_for(name, source, base_dir))
            .collect()
    }
}

fn reject_legacy_binding_arrays(s: &str) -> Result<()> {
    let doc: toml::Value = match toml::from_str(s) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };
    let Some(targets) = doc.get("targets").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    for (target_name, target) in targets {
        let Some(array) = target.get("sources").and_then(toml::Value::as_array) else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for element in array {
            match element {
                toml::Value::String(name) => {
                    if !seen.insert(name.as_str()) {
                        return Err(Error::Config(format!(
                            "target `{target_name}`: duplicate source `{name}` in the `sources` list"
                        )));
                    }
                }
                _ => {
                    return Err(Error::Config(format!(
                        "target `{target_name}`: table entries in a `sources` list are no longer \
                         supported; use a keyed `[targets.{target_name}.sources]` table instead"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn reject_url_slice(source_name: &str, binding: &Binding, source: &Source) -> Result<()> {
    if source.url.is_none() {
        return Ok(());
    }
    let field = if binding.root.is_some() {
        "root"
    } else if binding.include.is_some() {
        "include"
    } else if binding.exclude.is_some() {
        "exclude"
    } else if binding.branch.is_some() {
        "branch"
    } else if binding.tag.is_some() {
        "tag"
    } else if binding.rev.is_some() {
        "rev"
    } else if binding.template.is_some() {
        "template"
    } else if binding.map.is_some() {
        "map"
    } else {
        return Ok(());
    };
    Err(Error::Config(format!(
        "source `{source_name}`: `{field}` is meaningless on a `url` source"
    )))
}

fn reject_link_ref(source_name: &str, binding: &Binding, source: &Source) -> Result<()> {
    if source.deploy != Some(DeployMode::Link) {
        return Ok(());
    }
    let field = if binding.branch.is_some() {
        "branch"
    } else if binding.tag.is_some() {
        "tag"
    } else if binding.rev.is_some() {
        "rev"
    } else {
        return Ok(());
    };
    Err(Error::Config(format!(
        "source `{source_name}`: `{field}` is meaningless on a `link` source"
    )))
}

fn reject_multi_ref(source_name: &str, binding: &Binding) -> Result<()> {
    let set: Vec<&str> = [
        ("branch", binding.branch.is_some()),
        ("tag", binding.tag.is_some()),
        ("rev", binding.rev.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    if set.len() > 1 {
        let fields = set
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Config(format!(
            "source `{source_name}`: sets more than one of branch/tag/rev ({fields})"
        )));
    }
    Ok(())
}

fn reject_unsafe_source_selectors(source_name: &str, source: &Source) -> Result<()> {
    if let Some(root) = &source.root {
        let root = root.to_string_lossy();
        if crate::kernel::safe_relpath(&root).is_err() {
            return Err(Error::Config(format!(
                "source `{source_name}`: `root` `{root}` must be a relative path inside the \
                 source (no leading `/`, `..`, or empty components)"
            )));
        }
    }
    for (field, selectors) in [("include", &source.include), ("exclude", &source.exclude)] {
        for selector in selectors.iter().flatten() {
            if crate::kernel::safe_relpath(selector).is_err() {
                return Err(Error::Config(format!(
                    "source `{source_name}`: `{field}` path `{selector}` must be a relative path \
                     inside the source (no leading `/`, `..`, or empty components)"
                )));
            }
        }
    }
    Ok(())
}

fn reject_unsafe_selectors(target_name: &str, resolved: &ResolvedBinding) -> Result<()> {
    let identity = resolved.identity;
    if let Some(root) = resolved.root {
        let root = root.to_string_lossy();
        if crate::kernel::safe_relpath(&root).is_err() {
            return Err(Error::Config(format!(
                "target `{target_name}` binding `{identity}`: `root` `{root}` must be a relative \
                 path inside the source (no leading `/`, `..`, or empty components)"
            )));
        }
    }
    for (field, selector) in resolved
        .include
        .iter()
        .map(|s| ("include", s))
        .chain(resolved.exclude.iter().map(|s| ("exclude", s)))
    {
        if crate::kernel::safe_relpath(selector).is_err() {
            return Err(Error::Config(format!(
                "target `{target_name}` binding `{identity}`: `{field}` path `{selector}` must be \
                 a relative path inside the source (no leading `/`, `..`, or empty components)"
            )));
        }
    }
    Ok(())
}

fn reject_basename_collision(target_name: &str, resolved: &ResolvedBinding) -> Result<()> {
    let identity = resolved.identity;
    let mut seen = BTreeSet::new();
    let includes = resolved
        .include
        .iter()
        .filter(|locator| !crate::kernel::is_glob(locator));
    let map_dests = resolved.map.into_iter().flat_map(BTreeMap::values);
    for locator in includes.chain(map_dests) {
        let base = crate::kernel::locator_basename(locator);
        if !seen.insert(base) {
            return Err(Error::Config(format!(
                "target `{target_name}` binding `{identity}`: two selectors share basename \
                 `{base}`; they would collide on one record path \
                 `.../artifacts/{identity}/{base}.toml`"
            )));
        }
    }
    Ok(())
}

fn reject_map(source_name: &str, binding: &Binding) -> Result<()> {
    let Some(map) = &binding.map else {
        return Ok(());
    };
    if binding.include.is_some() {
        return Err(Error::Config(format!(
            "source `{source_name}`: `map` cannot be combined with `include`"
        )));
    }
    if binding.exclude.is_some() {
        return Err(Error::Config(format!(
            "source `{source_name}`: `map` cannot be combined with `exclude`"
        )));
    }
    if map.is_empty() {
        return Err(Error::Config(format!(
            "source `{source_name}`: `map` must not be empty"
        )));
    }
    for (key, value) in map {
        if key.is_empty() || value.is_empty() {
            return Err(Error::Config(format!(
                "source `{source_name}`: `map` entry `{key}` -> `{value}` must have a non-empty key and value"
            )));
        }
        if crate::kernel::safe_relpath(value).is_err() {
            return Err(Error::Config(format!(
                "source `{source_name}`: `map` dest `{value}` must be a relative path inside the \
                 target (no leading `/`, `..`, or empty components)"
            )));
        }
        if key.starts_with('/') || key.split('/').any(|c| c == "..") {
            return Err(Error::Config(format!(
                "source `{source_name}`: `map` key `{key}` must stay inside the source root"
            )));
        }
    }
    let mut seen_dests = BTreeSet::new();
    for value in map.values() {
        if !seen_dests.insert(value) {
            return Err(Error::Config(format!(
                "source `{source_name}`: `map` dest `{value}` is the target of more than one key; \
                 distinct sources would clobber one dest"
            )));
        }
    }
    Ok(())
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
    if local.defaults.auto_target.is_some() {
        merged.defaults.auto_target = local.defaults.auto_target;
    }
    if local.hooks.is_some() {
        merged.hooks = local.hooks;
    }
    merged.vars.extend(local.vars);
    merged
}
