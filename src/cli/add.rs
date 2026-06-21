//! The `add` command family: URL/shorthand parsing and `phora.toml` writers.

use std::collections::BTreeMap;

use crate::config::{Host, builtin_forges};
use crate::error::{Error, Result};
use crate::source::{Protocol, is_local_path};

use super::config_edit::BindRefinement;
use super::{config_edit, load_config, read_config_text, render, target_config_file};

#[allow(
    clippy::too_many_arguments,
    reason = "CLI flag fan-out mirrors the `phora add` argument surface"
)]
pub(super) fn run_add(
    url: &str,
    targets: &[String],
    name: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
    root: Option<String>,
    local: bool,
    symlink: bool,
    refinement: &BindRefinement,
) -> Result<()> {
    if refinement.r#as.is_some() && targets.len() != 1 {
        return Err(Error::Config(
            "`--as` sets a single binding identity and needs exactly one `--to` target".to_owned(),
        ));
    }
    if !refinement.is_bare() && targets.is_empty() {
        return Err(Error::Config(
            "refinement flags (`--as`/`--include`/`--exclude`) need at least one `--to` target"
                .to_owned(),
        ));
    }
    if (local || symlink) && (!targets.is_empty() || !refinement.is_bare()) {
        return Err(Error::Config(
            "`--local`/`--symlink` overlays do not support `--to`/refinement flags".to_owned(),
        ));
    }

    if !targets.is_empty() {
        return run_add_to_targets(
            url,
            targets,
            name,
            branch,
            tag.as_deref(),
            local,
            symlink,
            refinement,
        );
    }
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

    let parsed = resolve_add_source(url)?;

    let name = name.unwrap_or_else(|| parsed.name.clone());
    let branch = branch.or_else(|| parsed.branch.clone());
    let root = root.or_else(|| parsed.root.clone());

    let doc_text =
        std::fs::read_to_string("phora.toml").unwrap_or_else(|_| "version = 1\n".to_owned());
    let auto_target = super::effective_auto_target();
    let updated = if auto_target {
        add_to_default_target(
            &doc_text,
            &name,
            &parsed,
            branch.as_deref(),
            tag.as_deref(),
            root.as_deref(),
        )?
    } else {
        config_edit::upsert_source(
            &doc_text,
            &name,
            &parsed,
            branch.as_deref(),
            tag.as_deref(),
            root.as_deref(),
        )?
    };
    std::fs::write("phora.toml", &updated)?;

    let refspec = tag
        .or(branch)
        .map_or_else(String::new, |r| format!(" ({r})"));
    let description = format!("{}{refspec}", describe_source(&parsed));
    if auto_target {
        render::print_added_to_default(&name, &description);
    } else {
        render::print_added_declared(&name, &description);
    }

    if parsed.path.is_none() {
        print_add_contribution(&name);
    }
    Ok(())
}

/// Surfaces what an added source's root `phora.toml` would contribute, when one
/// exists in the cache or can be fetched. A source without a phora.toml stays
/// silent (it behaves exactly as a plain `add`); a genuine fetch/parse failure
/// warns to stderr instead of vanishing.
fn print_add_contribution(name: &str) {
    use crate::source::SourceError;

    let Some((backend, source_name, remote, refspec)) = add_contribution_target(name) else {
        return;
    };
    match backend.fetch_root_manifest(&source_name, &remote, &refspec) {
        Ok(bytes) => print_contribution_from_bytes(name, &bytes),
        Err(SourceError::FileAbsent { .. }) => {}
        Err(e) => eprintln!("note: could not inspect {name}'s phora.toml: {e}"),
    }
}

fn add_contribution_target(
    name: &str,
) -> Option<(
    crate::source::GitBackend,
    crate::kernel::SourceName,
    String,
    crate::config::Refspec,
)> {
    use crate::source::{GitBackend, Protocol as SourceProtocol};

    let config = load_config().ok()?;
    let sources = config.parsed_sources().ok()?;
    let source = sources.get(name)?;
    if source.mode() != crate::config::SourceMode::Git {
        return None;
    }
    let protocol = source
        .protocol()
        .or(config.protocol)
        .unwrap_or(SourceProtocol::Https);
    let remote = source.resolved_remote(&config.hosts, protocol).ok()?;
    let git_dir = crate::paths::cache_root().map(|c| c.join("git")).ok()?;
    let backend = GitBackend::new(git_dir);
    let source_name = crate::kernel::SourceName::trusted(name.to_owned());
    Some((backend, source_name, remote, source.refspec()))
}

fn print_contribution_from_bytes(name: &str, bytes: &[u8]) {
    use crate::config::transitive::TransitiveManifest;

    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("note: could not inspect {name}'s phora.toml: {e}");
            return;
        }
    };
    match TransitiveManifest::parse(text) {
        Ok(manifest) => print!("{}", render::render_add_contribution(name, &manifest)),
        Err(e) => eprintln!("note: could not inspect {name}'s phora.toml: {e}"),
    }
}

/// A local path resolves to `path =` (local), not forge shorthand — `add` must agree with the config layer, where bare `path =` already means local.
fn resolve_add_source(url: &str) -> Result<AddTarget> {
    if is_local_path(url) {
        return local_path_source(url);
    }
    let hosts = load_config().map(|c| c.hosts).unwrap_or_default();
    parse_add_url(url, &hosts)
}

fn local_path_source(url: &str) -> Result<AddTarget> {
    let canonical = std::fs::canonicalize(url)
        .map_err(|e| Error::Config(format!("local path source `{url}` does not exist: {e}")))?;
    let path = canonical.to_string_lossy().into_owned();
    let name = canonical.file_name().map_or_else(
        || path.clone(),
        |segment| segment.to_string_lossy().into_owned(),
    );
    Ok(AddTarget {
        name,
        git: None,
        host: None,
        repo: None,
        path: Some(path),
        protocol: None,
        branch: None,
        root: None,
    })
}

fn resolve_local_source(url: &str, name: Option<String>) -> Result<(String, AddTarget)> {
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
        path: Some(path),
        protocol: None,
        branch: None,
        root: None,
    };
    Ok((name, target))
}

fn inject_deploy_link(text: &str, name: &str) -> Result<String> {
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse phora.local.toml: {e}")))?;
    doc["sources"][name]["deploy"] = toml_edit::value("link");
    Ok(doc.to_string())
}

fn add_local(
    url: &str,
    name: Option<String>,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
    symlink: bool,
) -> Result<()> {
    let (name, target) = resolve_local_source(url, name)?;
    let path = target.path.clone().unwrap_or_default();

    let doc_text =
        std::fs::read_to_string("phora.local.toml").unwrap_or_else(|_| "version = 1\n".to_owned());
    let mut updated = config_edit::upsert_source(&doc_text, &name, &target, branch, tag, root)?;
    if symlink {
        updated = inject_deploy_link(&updated, &name)?;
    }
    std::fs::write("phora.local.toml", &updated)?;

    println!("Added local source '{name}': {path}");
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the `phora add` argument surface for the bind-on-add sugar"
)]
fn run_add_to_targets(
    url: &str,
    targets: &[String],
    name: Option<String>,
    branch: Option<String>,
    tag: Option<&str>,
    local: bool,
    symlink: bool,
    refinement: &BindRefinement,
) -> Result<()> {
    let overlay = local || symlink;
    let (name, source, branch) = if overlay {
        let (name, source) = resolve_local_source(url, name)?;
        (name, source, branch)
    } else {
        let parsed = resolve_add_source(url)?;
        let name = name.unwrap_or_else(|| parsed.name.clone());
        let branch = branch.or_else(|| parsed.branch.clone());
        (name, parsed, branch)
    };

    let file = target_config_file(overlay);
    let text = read_config_text(file)?;

    let source_root = source.root.clone();
    let mut updated = add_with_binds(
        &text,
        &name,
        &source,
        branch.as_deref(),
        tag,
        source_root.as_deref(),
        targets,
        refinement,
        &super::TtyMissingTarget,
    )?;
    if symlink {
        updated = inject_deploy_link(&updated, &name)?;
    }
    super::bind::guard_no_dangling_references(&updated, overlay)?;
    std::fs::write(file, &updated)?;

    render::print_added_and_bound(&name, &describe_source(&source), targets);
    Ok(())
}

#[cfg(test)]
pub(super) fn insert_source_with_ref(
    doc_text: &str,
    name: &str,
    source: &AddTarget,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
) -> Result<String> {
    config_edit::upsert_source(doc_text, name, source, branch, tag, root)
}

fn describe_source(source: &AddTarget) -> String {
    match (&source.git, &source.host, &source.repo, &source.path) {
        (Some(git), ..) => git.clone(),
        (None, Some(host), Some(repo), _) => format!("{host}:{repo}"),
        (None, None, None, Some(path)) => path.clone(),
        _ => source.name.clone(),
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
    config_edit::upsert_source(doc_text, name, source, source.branch.as_deref(), None, root)
}

/// How to handle a `--to` target that does not yet exist: create it at `path`
/// (flat layout) or reject the whole command.
pub(super) enum MissingTarget {
    Create { path: String },
    Reject,
}

/// Decides, per missing `--to` target, whether to create it or reject. The real
/// impl prompts on a TTY; tests substitute a fake.
pub(super) trait MissingTargetDecider {
    fn decide(&self, name: &str, default_path: &str) -> MissingTarget;
}

/// Upsert `[sources.<name>]` and bind it into `[targets.default]`, creating that
/// target (flat layout, rooted at `.`) only when it does not already exist.
///
/// # Errors
///
/// Returns [`Error::Config`] if `text` is not valid TOML.
pub(super) fn add_to_default_target(
    text: &str,
    name: &str,
    source: &AddTarget,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
) -> Result<String> {
    let mut current = config_edit::upsert_source(text, name, source, branch, tag, root)?;
    if !target_exists(&current, "default")? {
        current = config_edit::upsert_target(&current, "default", ".", Some("flat"))?;
    }
    Ok(config_edit::bind(
        &current,
        "default",
        &[name.to_owned()],
        &BindRefinement::default(),
    )?
    .text)
}

/// Atomically upsert `[sources.<name>]` and bind it to every target in `targets`
/// over a single config-text string, returning the final text or erring whole.
/// A missing target is resolved by `decider`: create it (flat layout) or reject.
/// Each bind carries `refinement` so an aliased/scoped `add --to` writes a table
/// binding in every target.
///
/// # Errors
///
/// Returns [`Error::Config`] if a target is missing and the decider rejects it
/// (resolved before any bind, so a failure yields no partial text).
#[allow(
    clippy::too_many_arguments,
    reason = "single-string desugar carries the source shape plus refspec and bind targets"
)]
pub(super) fn add_with_binds(
    text: &str,
    name: &str,
    source: &AddTarget,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
    targets: &[String],
    refinement: &BindRefinement,
    decider: &dyn MissingTargetDecider,
) -> Result<String> {
    let mut current = config_edit::upsert_source(text, name, source, branch, tag, root)?;
    let bind_names = [name.to_owned()];
    for target in targets {
        if !target_exists(&current, target)? {
            match decider.decide(target, &format!("./{target}")) {
                MissingTarget::Create { path } => {
                    current = config_edit::upsert_target(&current, target, &path, Some("flat"))?;
                }
                MissingTarget::Reject => {
                    return Err(Error::Config(super::bind::missing_target_message(target)));
                }
            }
        }
        current = config_edit::bind(&current, target, &bind_names, refinement)?.text;
    }
    Ok(current)
}

fn target_exists(text: &str, target: &str) -> Result<bool> {
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse toml: {e}")))?;
    Ok(doc
        .get("targets")
        .and_then(toml_edit::Item::as_table_like)
        .is_some_and(|targets| targets.contains_key(target)))
}
