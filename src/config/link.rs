//! Links to a line of a dependency's file at a pinned commit, for review prompts.

use std::collections::BTreeMap;

use super::host::{Host, builtin_forges};
use super::{effective_host, fill_template};

/// A forge link when a host's remote template matches `remote`, else `remote` with the file.
#[must_use]
pub fn file_link(
    hosts: &BTreeMap<String, Host>,
    remote: &str,
    commit: &str,
    file: &str,
    line: Option<usize>,
) -> String {
    let names = builtin_forges().into_keys().chain(hosts.keys().cloned());
    for name in names {
        let Some(host) = effective_host(hosts, &name) else {
            continue;
        };
        let (Some(web), Some(remote_config)) = (&host.web, &host.remote) else {
            continue;
        };
        let path = [remote_config.https_template(), remote_config.ssh_template()]
            .into_iter()
            .flatten()
            .find_map(|template| template_path(template, remote));
        if let Some(path) = path {
            let web = match line {
                Some(_) => web.as_str(),
                None => web.split_once('#').map_or(web.as_str(), |(page, _)| page),
            };
            let line = line.map(|line| line.to_string()).unwrap_or_default();
            return fill_template(web, &path)
                .replace("{commit}", commit)
                .replace("{file}", file)
                .replace("{line}", &line);
        }
    }
    let at = line.map(|line| format!(":{line}")).unwrap_or_default();
    format!("{}/{file}{at} at {commit}", remote.trim_end_matches('/'))
}

/// The `{path}` a remote template yields `remote` for, ignoring a trailing `.git`.
fn template_path(template: &str, remote: &str) -> Option<String> {
    let (prefix, suffix) = template.split_once("{path}")?;
    let suffix = suffix.trim_end_matches(".git");
    let remote = remote.trim_end_matches('/').trim_end_matches(".git");
    let path = remote.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!path.is_empty()).then(|| path.to_owned())
}

/// The 1-based line where `targets.<target>.hooks.on_change` runs `command`.
#[must_use]
pub fn hook_line(manifest: &str, target: &str, command: &str) -> Option<usize> {
    let document = toml_edit::Document::parse(manifest).ok()?;
    let on_change = document
        .as_table()
        .get("targets")?
        .get(target)?
        .get("hooks")?
        .get("on_change")?;
    let span = on_change
        .as_array()
        .and_then(|commands| {
            commands
                .iter()
                .find(|entry| runs(entry, command))
                .and_then(toml_edit::Value::span)
        })
        .or_else(|| on_change.span())?;
    Some(line_at(manifest, span.start))
}

/// The 1-based line of `sources.<source>.build`.
#[must_use]
pub fn build_line(manifest: &str, source: &str) -> Option<usize> {
    let document = toml_edit::Document::parse(manifest).ok()?;
    let span = document
        .as_table()
        .get("sources")?
        .get(source)?
        .get("build")?
        .span()?;
    Some(line_at(manifest, span.start))
}

fn runs(entry: &toml_edit::Value, command: &str) -> bool {
    match entry {
        toml_edit::Value::String(run) => run.value() == command,
        toml_edit::Value::InlineTable(table) => table
            .get("run")
            .and_then(toml_edit::Value::as_str)
            .is_some_and(|run| run == command),
        _ => false,
    }
}

fn line_at(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}
