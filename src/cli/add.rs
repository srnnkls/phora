//! The `add` command family: URL/shorthand parsing and `phora.toml` writers.

use std::collections::BTreeMap;

use crate::config::{Host, builtin_forges};
use crate::error::{Error, Result};
use crate::source::Protocol;

use super::load_config;

pub(super) fn run_add(
    url: &str,
    name: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
    root: Option<String>,
    local: bool,
    symlink: bool,
) -> Result<()> {
    if local || symlink {
        return add_local(
            url,
            name,
            branch.as_deref(),
            tag.as_deref(),
            root.as_deref(),
            symlink,
        );
    }

    let hosts = load_config().map(|c| c.hosts).unwrap_or_default();
    let parsed = parse_add_url(url, &hosts)?;

    let name = name.unwrap_or_else(|| parsed.name.clone());
    let branch = branch.or_else(|| parsed.branch.clone());
    let root = root.or_else(|| parsed.root.clone());

    let doc_text =
        std::fs::read_to_string("phora.toml").unwrap_or_else(|_| "version = 1\n".to_owned());
    let updated = insert_source_with_ref(
        &doc_text,
        &name,
        &parsed,
        branch.as_deref(),
        tag.as_deref(),
        root.as_deref(),
    )?;
    std::fs::write("phora.toml", &updated)?;

    let refspec = tag
        .or(branch)
        .map_or_else(String::new, |r| format!(" ({r})"));
    println!(
        "Added source '{name}': {}{refspec}",
        describe_source(&parsed)
    );
    Ok(())
}

fn add_local(
    url: &str,
    name: Option<String>,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
    symlink: bool,
) -> Result<()> {
    let canonical = std::fs::canonicalize(url).map_err(|e| {
        Error::Config(format!(
            "`--local`/`--symlink` require an existing local checkout, but `{url}` is not one: {e}"
        ))
    })?;
    if !canonical.is_dir() {
        return Err(Error::Config(format!(
            "`--local`/`--symlink` require a directory, but `{url}` is not one"
        )));
    }

    let path = canonical.to_string_lossy().into_owned();
    let name = name.unwrap_or_else(|| {
        canonical
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned())
    });

    let target = AddTarget {
        name: name.clone(),
        git: None,
        host: None,
        repo: None,
        path: Some(path.clone()),
        protocol: None,
        branch: None,
        root: None,
    };

    let mut table = source_table(&target);
    if let Some(branch) = branch {
        table["branch"] = toml_edit::value(branch);
    }
    if let Some(tag) = tag {
        table["tag"] = toml_edit::value(tag);
    }
    if let Some(root) = root {
        table["root"] = toml_edit::value(root);
    }
    if symlink {
        table["deploy"] = toml_edit::value("link");
    }

    let doc_text =
        std::fs::read_to_string("phora.local.toml").unwrap_or_else(|_| "version = 1\n".to_owned());
    let mut doc = doc_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse phora.local.toml: {e}")))?;
    ensure_sources_table(&mut doc);
    doc["sources"][&name] = toml_edit::Item::Table(table);
    std::fs::write("phora.local.toml", doc.to_string())?;

    println!("Added local source '{name}': {path}");
    Ok(())
}

fn describe_source(source: &AddTarget) -> String {
    match (&source.git, &source.host, &source.repo) {
        (Some(git), _, _) => git.clone(),
        (None, Some(host), Some(repo)) => format!("{host}:{repo}"),
        _ => source.name.clone(),
    }
}

pub(super) fn insert_source_with_ref(
    doc_text: &str,
    name: &str,
    source: &AddTarget,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
) -> Result<String> {
    let mut doc = doc_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse phora.toml: {e}")))?;

    let mut table = source_table(source);
    if let Some(branch) = branch {
        table["branch"] = toml_edit::value(branch);
    }
    if let Some(tag) = tag {
        table["tag"] = toml_edit::value(tag);
    }
    if let Some(root) = root.or(source.root.as_deref()) {
        table["root"] = toml_edit::value(root);
    }

    ensure_sources_table(&mut doc);
    doc["sources"][name] = toml_edit::Item::Table(table);
    Ok(doc.to_string())
}

fn source_table(source: &AddTarget) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    if let Some(git) = &source.git {
        table["git"] = toml_edit::value(git.as_str());
        return table;
    }
    if let Some(path) = &source.path {
        table["path"] = toml_edit::value(path.as_str());
        return table;
    }
    if let Some(host) = &source.host {
        table["host"] = toml_edit::value(host.as_str());
    }
    if let Some(repo) = &source.repo {
        table["repo"] = toml_edit::value(repo.as_str());
    }
    if let Some(Protocol::Ssh) = source.protocol {
        table["protocol"] = toml_edit::value("ssh");
    }
    table
}

fn ensure_sources_table(doc: &mut toml_edit::DocumentMut) {
    if doc.get("sources").is_none() {
        let mut sources = toml_edit::Table::new();
        sources.set_implicit(true);
        doc["sources"] = toml_edit::Item::Table(sources);
    }
}

/// A source derived from an `add` URL/shorthand, before name overrides. Either
/// literal (`git` is `Some`), symbolic forge (`host`/`repo` are `Some`), or
/// local path (`path` is `Some`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddTarget {
    pub name: String,
    pub git: Option<String>,
    pub host: Option<String>,
    pub repo: Option<String>,
    pub path: Option<String>,
    pub protocol: Option<Protocol>,
    pub branch: Option<String>,
    pub root: Option<String>,
}

/// Expands an `add` URL/shorthand into an [`AddTarget`] using the built-in
/// forge registry overlaid by any host in `hosts`.
///
/// # Errors
///
/// Returns [`Error::Config`] if the input cannot be parsed into owner/repo.
pub fn parse_add_url(input: &str, hosts: &BTreeMap<String, Host>) -> Result<AddTarget> {
    if is_scp_ssh(input) {
        return Ok(parse_scp_ssh(input));
    }
    if let Some((scheme, rest)) = split_scheme(input) {
        return parse_full_url(input, scheme, rest);
    }
    if let Some((host, path)) = input.split_once(':') {
        return parse_colon_alias(input, host, path);
    }
    parse_shorthand(input, &domain_to_name(hosts))
}

fn parse_colon_alias(input: &str, host: &str, path: &str) -> Result<AddTarget> {
    if host.is_empty() {
        return Err(Error::Config(format!("empty host in `{input}`")));
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let [owner, repo, root_segments @ ..] = segments.as_slice() else {
        return Err(Error::Config(format!(
            "expected <host>:<owner>/<repo> in `{input}`"
        )));
    };
    Ok(symbolic_source(
        host.to_owned(),
        &format!("{owner}/{repo}"),
        join_root(root_segments),
    ))
}

/// `domain -> forge name` from the built-in forge registry overlaid by `hosts`.
fn domain_to_name(hosts: &BTreeMap<String, Host>) -> BTreeMap<String, String> {
    let builtins = builtin_forges();
    builtins
        .iter()
        .chain(hosts)
        .filter_map(|(name, host)| {
            let url = host.remote.as_ref()?.https_template()?;
            Some((template_domain(url)?.to_owned(), name.clone()))
        })
        .collect()
}

fn symbolic_source(host: String, repo: &str, root: Option<String>) -> AddTarget {
    AddTarget {
        name: repo_name(repo),
        git: None,
        host: Some(host),
        repo: Some(repo.to_owned()),
        path: None,
        protocol: None,
        branch: None,
        root,
    }
}

/// The host domain embedded in a `remote` template (between scheme/user and the
/// next `/` or `:` port). `ssh://git@git.company.com:2222/...` yields `git.company.com`.
fn template_domain(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let after_user = after_scheme
        .split_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    let end = after_user.find(['/', ':']).unwrap_or(after_user.len());
    let domain = &after_user[..end];
    (!domain.is_empty()).then_some(domain)
}

fn is_scp_ssh(input: &str) -> bool {
    if input.contains("://") {
        return false;
    }
    match (input.find('@'), input.find(':')) {
        (Some(at), Some(colon)) => at < colon,
        _ => false,
    }
}

fn parse_scp_ssh(input: &str) -> AddTarget {
    literal_source(repo_name(input), input.to_owned(), None, None)
}

fn literal_source(
    name: String,
    git: String,
    branch: Option<String>,
    root: Option<String>,
) -> AddTarget {
    AddTarget {
        name,
        git: Some(git),
        host: None,
        repo: None,
        path: None,
        protocol: None,
        branch,
        root,
    }
}

fn split_scheme(input: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = input.split_once("://")?;
    matches!(scheme, "https" | "http" | "ssh").then_some((scheme, rest))
}

fn parse_full_url(input: &str, scheme: &str, rest: &str) -> Result<AddTarget> {
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| Error::Config(format!("cannot parse owner/repo from `{input}`")))?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let [owner, repo, tail @ ..] = segments.as_slice() else {
        return Err(Error::Config(format!(
            "expected <owner>/<repo> in `{input}`"
        )));
    };
    let name = strip_git(repo).to_owned();

    if let ["tree", git_ref, root_segments @ ..] = tail {
        return Ok(literal_source(
            name,
            with_git_suffix(&format!("{scheme}://{host}/{owner}/{repo}")),
            Some((*git_ref).to_owned()),
            join_root(root_segments),
        ));
    }

    Ok(literal_source(name, with_git_suffix(input), None, None))
}

fn parse_shorthand(input: &str, domains: &BTreeMap<String, String>) -> Result<AddTarget> {
    let segments: Vec<&str> = input.split('/').filter(|s| !s.is_empty()).collect();
    let first = segments
        .first()
        .ok_or_else(|| Error::Config(format!("empty add target `{input}`")))?;

    if let Some(name) = domains.get(*first) {
        let [_, owner, repo, root_segments @ ..] = segments.as_slice() else {
            return Err(Error::Config(format!(
                "expected <host>/<owner>/<repo> in `{input}`"
            )));
        };
        return Ok(symbolic_source(
            name.clone(),
            &format!("{owner}/{repo}"),
            join_root(root_segments),
        ));
    }

    let [owner, repo, root_segments @ ..] = segments.as_slice() else {
        return Err(Error::Config(format!(
            "expected <owner>/<repo> shorthand in `{input}`"
        )));
    };
    Ok(symbolic_source(
        "github".to_owned(),
        &format!("{owner}/{repo}"),
        join_root(root_segments),
    ))
}

fn join_root(segments: &[&str]) -> Option<String> {
    (!segments.is_empty()).then(|| segments.join("/"))
}

fn repo_name(input: &str) -> String {
    let last = input.rsplit('/').next().unwrap_or(input);
    strip_git(last).to_owned()
}

fn strip_git(segment: &str) -> &str {
    segment.strip_suffix(".git").unwrap_or(segment)
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "git remote URLs are case-sensitive strings, not filesystem paths"
)]
fn with_git_suffix(url: &str) -> String {
    if url.ends_with(".git") {
        url.to_owned()
    } else {
        format!("{url}.git")
    }
}

/// Inserts a `[sources.<name>]` table into existing `phora.toml` text,
/// preserving surrounding content and formatting, and returns the new text.
///
/// # Errors
///
/// Returns [`Error::Config`] if `doc_text` is not valid TOML.
pub fn insert_source(
    doc_text: &str,
    name: &str,
    source: &AddTarget,
    root: Option<&str>,
) -> Result<String> {
    let mut doc = doc_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse phora.toml: {e}")))?;

    let mut table = source_table(source);
    if let Some(branch) = &source.branch {
        table["branch"] = toml_edit::value(branch.as_str());
    }
    if let Some(root) = root.or(source.root.as_deref()) {
        table["root"] = toml_edit::value(root);
    }

    ensure_sources_table(&mut doc);
    doc["sources"][name] = toml_edit::Item::Table(table);
    Ok(doc.to_string())
}
