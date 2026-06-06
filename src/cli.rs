//! Command-line surface.

use std::collections::BTreeMap;
use std::path::Path;

use clap::{Parser, Subcommand};

use crate::config::{Config, Host, Source};
use crate::error::{Error, Result};
use crate::matcher::PathMatcher;
use crate::paths::{ProjectId, phora_dir};
use crate::registry::{FileRegistry, Registry};

#[derive(Parser, Debug)]
#[command(name = "phora", version, about = "Git-based artifact package manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Parse a URL and add a source to the config.
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        root: Option<String>,
    },
    /// Fetch sources and project them to targets.
    Sync {
        #[arg(long)]
        prune: bool,
        #[arg(long)]
        force: bool,
    },
    /// Bump the lock to latest, then sync.
    Update { source: Option<String> },
    /// Show sources and deployment state.
    List {
        #[arg(long)]
        plan: bool,
    },
    /// Verify deployed files by hashing contents.
    Verify,
    /// Query the global registry.
    Where {
        #[arg(long)]
        digest: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        artifact: Option<String>,
        #[arg(long)]
        commit: Option<String>,
    },
    /// Permanently stop managing an artifact, keeping its files.
    Eject {
        artifact: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
    },
    /// Resume managing a previously ejected artifact.
    Uneject {
        artifact: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
    },
    /// Rebuild the global registry from lock and on-disk targets.
    RebuildRegistry,
    /// Debug include/exclude matching for a path.
    CheckMatch {
        #[arg(long)]
        source: String,
        path: String,
    },
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the command implementation will consume the parsed args"
)]
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Add {
            url,
            name,
            branch,
            tag,
            root,
        } => run_add(&url, name, branch, tag, root),
        Command::Sync { .. } => Err(Error::NotImplemented("sync")),
        Command::Update { .. } => Err(Error::NotImplemented("update")),
        Command::List { .. } => Err(Error::NotImplemented("list")),
        Command::Verify => {
            let config = load_config()?;
            let mismatches = crate::sync::verify(&config, &open_project_registry()?)?;
            print_verify(&mismatches);
            if mismatches.is_empty() {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Command::Where {
            digest,
            source,
            artifact,
            commit,
        } => {
            let matches = where_cmd(
                &open_project_registry()?,
                &WhereFilter {
                    digest,
                    source,
                    artifact,
                    commit,
                },
            )?;
            print_where_matches(&matches);
            Ok(())
        }
        Command::Eject {
            artifact,
            source,
            target,
        } => {
            let config = load_config()?;
            crate::sync::eject(
                &config,
                &open_project_registry()?,
                &artifact,
                &source,
                &target,
            )?;
            println!("ejected {source}/{artifact} from {target} (files kept)");
            Ok(())
        }
        Command::Uneject {
            artifact,
            source,
            target,
        } => {
            let config = load_config()?;
            crate::sync::uneject(
                &config,
                &open_project_registry()?,
                &artifact,
                &source,
                &target,
            )?;
            println!("unejected {source}/{artifact} in {target}");
            Ok(())
        }
        Command::RebuildRegistry => Err(Error::NotImplemented("rebuild-registry")),
        Command::CheckMatch { source, path } => {
            let source = load_source(&source)?;
            let report = check_match_cmd(&source, &path);
            print_check_match(&source, &path, &report);
            Ok(())
        }
    }
}

fn run_add(
    url: &str,
    name: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
    root: Option<String>,
) -> Result<()> {
    let hosts = load_config().map(|c| c.hosts).unwrap_or_default();
    let parsed = parse_add_url(url, &hosts)?;

    let name = name.unwrap_or(parsed.name);
    let branch = branch.or(parsed.branch);
    let root = root.or(parsed.root);

    let doc_text =
        std::fs::read_to_string("phora.toml").unwrap_or_else(|_| "version = 1\n".to_owned());
    let updated = insert_source_with_ref(
        &doc_text,
        &name,
        &parsed.git,
        branch.as_deref(),
        tag.as_deref(),
        root.as_deref(),
    )?;
    std::fs::write("phora.toml", &updated)?;

    let refspec = tag
        .or(branch)
        .map_or_else(String::new, |r| format!(" ({r})"));
    println!("Added source '{name}': {}{refspec}", parsed.git);
    Ok(())
}

fn insert_source_with_ref(
    doc_text: &str,
    name: &str,
    git: &str,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
) -> Result<String> {
    let mut doc = doc_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse phora.toml: {e}")))?;

    let mut table = toml_edit::Table::new();
    table["git"] = toml_edit::value(git);
    if let Some(branch) = branch {
        table["branch"] = toml_edit::value(branch);
    }
    if let Some(tag) = tag {
        table["tag"] = toml_edit::value(tag);
    }
    if let Some(root) = root {
        table["root"] = toml_edit::value(root);
    }

    ensure_sources_table(&mut doc);
    doc["sources"][name] = toml_edit::Item::Table(table);
    Ok(doc.to_string())
}

fn ensure_sources_table(doc: &mut toml_edit::DocumentMut) {
    if doc.get("sources").is_none() {
        let mut sources = toml_edit::Table::new();
        sources.set_implicit(true);
        doc["sources"] = toml_edit::Item::Table(sources);
    }
}

fn open_project_registry() -> Result<FileRegistry> {
    let project = ProjectId::for_path(&std::env::current_dir()?)?;
    let state_root = phora_dir()?
        .join("state")
        .join("projects")
        .join(project.as_str());
    FileRegistry::open(state_root)
}

fn load_config() -> Result<Config> {
    let text = std::fs::read_to_string("phora.toml")
        .map_err(|e| Error::Config(format!("read phora.toml: {e}")))?;
    Config::parse(&text)
}

fn load_source(name: &str) -> Result<Source> {
    load_config()?
        .sources
        .remove(name)
        .ok_or_else(|| Error::Config(format!("source `{name}` not found in phora.toml")))
}

fn print_verify(mismatches: &[crate::sync::VerifyMismatch]) {
    use crate::sync::VerifyReason;
    if mismatches.is_empty() {
        println!("all verified");
        return;
    }
    for m in mismatches {
        let reason = match &m.reason {
            VerifyReason::Missing => "missing".to_owned(),
            VerifyReason::ContentMismatch { .. } => "content mismatch".to_owned(),
        };
        println!(
            "{}/{}: {} ({reason})",
            m.key.source,
            m.key.artifact,
            m.path.display()
        );
    }
}

fn print_where_matches(matches: &[WhereMatch]) {
    for m in matches {
        let commit = m.commit.get(..8).unwrap_or(&m.commit);
        println!(
            "Artifact: {}/{} (commit {commit}, digest {})",
            m.source, m.artifact, m.digest
        );
        for target in &m.targets {
            println!("  - {target}");
        }
    }
}

fn print_check_match(source: &Source, path: &str, report: &CheckMatchReport) {
    let artifact = path.split('/').next().unwrap_or(path);
    println!(
        "artifact `{artifact}`: {}",
        allow_label(report.artifact_allowed)
    );
    println!("path `{path}`: {}", allow_label(report.path_allowed));
    println!("include: {:?}", source.includes());
    println!("exclude: {:?}", source.excludes());
}

fn allow_label(allowed: bool) -> &'static str {
    if allowed { "allowed" } else { "excluded" }
}

/// Reverse-lookup filter over the registry: every `Some` field is an AND constraint.
#[derive(Debug, Default, Clone)]
pub struct WhereFilter {
    pub digest: Option<String>,
    pub source: Option<String>,
    pub artifact: Option<String>,
    pub commit: Option<String>,
}

impl WhereFilter {
    fn matches(&self, record: &crate::registry::RegistryRecord) -> bool {
        let eq = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        eq(&self.digest, &record.digest)
            && eq(&self.source, &record.key.source)
            && eq(&self.artifact, &record.key.artifact)
            && eq(&self.commit, &record.commit)
    }
}

/// One (source, artifact) deployment grouped across the targets it lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhereMatch {
    pub source: String,
    pub artifact: String,
    pub commit: String,
    pub digest: String,
    pub targets: Vec<String>,
}

/// Outcome of debugging include/exclude matching for a path under a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckMatchReport {
    pub artifact_allowed: bool,
    pub path_allowed: bool,
}

/// Filters the registry by the constraints in `filter`, grouping survivors by
/// (source, artifact) and listing the targets each is deployed to.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn where_cmd(registry: &dyn Registry, filter: &WhereFilter) -> Result<Vec<WhereMatch>> {
    let mut groups: BTreeMap<(String, String), WhereMatch> = BTreeMap::new();

    for record in registry.list_all()? {
        if !filter.matches(&record) {
            continue;
        }
        let entry = groups
            .entry((record.key.source.clone(), record.key.artifact.clone()))
            .or_insert_with(|| WhereMatch {
                source: record.key.source.clone(),
                artifact: record.key.artifact.clone(),
                commit: record.commit.clone(),
                digest: record.digest.clone(),
                targets: Vec::new(),
            });
        entry.targets.push(record.key.target.clone());
    }

    Ok(groups
        .into_values()
        .map(|mut m| {
            m.targets.sort();
            m.targets.dedup();
            m
        })
        .collect())
}

/// Reports artifact-level and path-level allow decisions for `path` under `source`.
#[must_use]
pub fn check_match_cmd(source: &Source, path: &str) -> CheckMatchReport {
    let Ok(matcher) = PathMatcher::new(source.includes(), source.excludes()) else {
        return CheckMatchReport {
            artifact_allowed: false,
            path_allowed: false,
        };
    };
    let artifact = path.split('/').next().unwrap_or(path);
    CheckMatchReport {
        artifact_allowed: matcher.allows_artifact(artifact),
        path_allowed: matcher.allows_path(Path::new(path), false),
    }
}

/// A source derived from an `add` URL/shorthand, before name overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSource {
    pub name: String,
    pub git: String,
    pub branch: Option<String>,
    pub root: Option<String>,
}

/// Expands an `add` URL/shorthand into a [`ParsedSource`] using built-in
/// `github`/`gitlab` host templates plus any host in `hosts`.
///
/// # Errors
///
/// Returns [`Error::Config`] if the input cannot be parsed into owner/repo.
pub fn parse_add_url(input: &str, hosts: &BTreeMap<String, Host>) -> Result<ParsedSource> {
    let templates = host_templates(hosts);

    if is_scp_ssh(input) {
        return Ok(parse_scp_ssh(input));
    }
    if let Some((scheme, rest)) = split_scheme(input) {
        return parse_full_url(input, scheme, rest);
    }
    parse_shorthand(input, &templates)
}

/// `(domain, git_url template)` pairs from built-in defaults overlaid by `hosts`.
fn host_templates(hosts: &BTreeMap<String, Host>) -> Vec<(String, String)> {
    let mut templates: BTreeMap<String, String> = BTreeMap::new();
    templates.insert(
        "github".to_owned(),
        "https://github.com/{owner}/{repo}.git".to_owned(),
    );
    templates.insert(
        "gitlab".to_owned(),
        "https://gitlab.com/{owner}/{repo}.git".to_owned(),
    );
    for (name, host) in hosts {
        if let Some(url) = &host.git_url {
            templates.insert(name.clone(), url.clone());
        }
    }
    templates
        .into_values()
        .filter_map(|url| {
            let domain = template_domain(&url)?.to_owned();
            Some((domain, url))
        })
        .collect()
}

/// The host domain embedded in a `git_url` template (between scheme/user and the
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

fn parse_scp_ssh(input: &str) -> ParsedSource {
    ParsedSource {
        name: repo_name(input),
        git: input.to_owned(),
        branch: None,
        root: None,
    }
}

fn split_scheme(input: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = input.split_once("://")?;
    matches!(scheme, "https" | "http" | "ssh").then_some((scheme, rest))
}

fn parse_full_url(input: &str, scheme: &str, rest: &str) -> Result<ParsedSource> {
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
        return Ok(ParsedSource {
            name,
            git: with_git_suffix(&format!("{scheme}://{host}/{owner}/{repo}")),
            branch: Some((*git_ref).to_owned()),
            root: join_root(root_segments),
        });
    }

    Ok(ParsedSource {
        name,
        git: with_git_suffix(input),
        branch: None,
        root: None,
    })
}

fn parse_shorthand(input: &str, templates: &[(String, String)]) -> Result<ParsedSource> {
    let segments: Vec<&str> = input.split('/').filter(|s| !s.is_empty()).collect();
    let first = segments
        .first()
        .ok_or_else(|| Error::Config(format!("empty add target `{input}`")))?;

    if let Some((_, template)) = templates.iter().find(|(domain, _)| domain == first) {
        let [_, owner, repo, root_segments @ ..] = segments.as_slice() else {
            return Err(Error::Config(format!(
                "expected <host>/<owner>/<repo> in `{input}`"
            )));
        };
        return Ok(ParsedSource {
            name: (*repo).to_owned(),
            git: expand_template(template, owner, repo),
            branch: None,
            root: join_root(root_segments),
        });
    }

    let [owner, repo, root_segments @ ..] = segments.as_slice() else {
        return Err(Error::Config(format!(
            "expected <owner>/<repo> shorthand in `{input}`"
        )));
    };
    let template = "https://github.com/{owner}/{repo}.git";
    Ok(ParsedSource {
        name: (*repo).to_owned(),
        git: expand_template(template, owner, repo),
        branch: None,
        root: join_root(root_segments),
    })
}

fn join_root(segments: &[&str]) -> Option<String> {
    (!segments.is_empty()).then(|| segments.join("/"))
}

fn expand_template(template: &str, owner: &str, repo: &str) -> String {
    template.replace("{owner}", owner).replace("{repo}", repo)
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
    git: &str,
    branch: Option<&str>,
    root: Option<&str>,
) -> Result<String> {
    let mut doc = doc_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Config(format!("parse phora.toml: {e}")))?;

    let mut table = toml_edit::Table::new();
    table["git"] = toml_edit::value(git);
    if let Some(branch) = branch {
        table["branch"] = toml_edit::value(branch);
    }
    if let Some(root) = root {
        table["root"] = toml_edit::value(root);
    }

    ensure_sources_table(&mut doc);
    doc["sources"][name] = toml_edit::Item::Table(table);
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ArtifactKey, FileRegistry, ManifestFile, RegistryRecord};
    use clap::CommandFactory;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    fn record(
        target: &str,
        source: &str,
        artifact: &str,
        commit: &str,
        digest: &str,
    ) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: target.to_owned(),
                source: source.to_owned(),
                artifact: artifact.to_owned(),
            },
            commit: commit.to_owned(),
            digest: digest.to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("python.json"),
                size: 12_345,
                mtime: 1_738_329_296,
                blake3: "9e8d7c6b5a4f3e2d".to_owned(),
            }],
        }
    }

    fn seeded_registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        reg.put(&record("nvim", "dotfiles", "init", "aaa111", "blake3:d1"))
            .expect("put nvim/dotfiles/init");
        reg.put(&record(
            "vscode",
            "dotfiles",
            "settings",
            "aaa111",
            "blake3:d2",
        ))
        .expect("put vscode/dotfiles/settings");
        reg.put(&record(
            "vscode",
            "company-configs",
            "python",
            "def456",
            "blake3:dpy",
        ))
        .expect("put vscode/company-configs/python");
        reg.put(&record(
            "agent-1",
            "company-configs",
            "python",
            "def456",
            "blake3:dpy",
        ))
        .expect("put agent-1/company-configs/python");
        (dir, reg)
    }

    fn source_with(include: &[&str], exclude: &[&str]) -> Source {
        use std::fmt::Write as _;
        let mut body = String::from("version = 1\n\n[sources.s]\ngit = \"https://x.git\"\n");
        if !include.is_empty() {
            let list = include
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(body, "include = [{list}]");
        }
        if !exclude.is_empty() {
            let list = exclude
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(body, "exclude = [{list}]");
        }
        crate::config::Config::parse(&body)
            .expect("source toml parses")
            .sources
            .remove("s")
            .expect("source `s` present")
    }

    fn find<'a>(matches: &'a [WhereMatch], source: &str, artifact: &str) -> Option<&'a WhereMatch> {
        matches
            .iter()
            .find(|m| m.source == source && m.artifact == artifact)
    }

    // where_cmd

    #[test]
    fn where_filters_by_source() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("dotfiles".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by source");

        assert!(
            matches.iter().all(|m| m.source == "dotfiles"),
            "every match must come from the requested source, got {matches:?}"
        );
        assert!(
            find(&matches, "dotfiles", "init").is_some(),
            "dotfiles/init must be present"
        );
        assert!(
            find(&matches, "dotfiles", "settings").is_some(),
            "dotfiles/settings must be present"
        );
        assert!(
            find(&matches, "company-configs", "python").is_none(),
            "company-configs must be excluded when filtering by source=dotfiles"
        );
    }

    #[test]
    fn where_filters_by_artifact() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            artifact: Some("python".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by artifact");

        assert!(
            matches.iter().all(|m| m.artifact == "python"),
            "only python artifacts must survive, got {matches:?}"
        );
        assert!(
            find(&matches, "company-configs", "python").is_some(),
            "company-configs/python must be present"
        );
    }

    #[test]
    fn where_filters_by_commit() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            commit: Some("aaa111".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by commit");

        assert!(
            matches.iter().all(|m| m.commit == "aaa111"),
            "only commit aaa111 records must survive, got {matches:?}"
        );
        assert!(
            find(&matches, "company-configs", "python").is_none(),
            "the def456 record must be filtered out by commit=aaa111"
        );
    }

    #[test]
    fn where_filters_by_digest() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            digest: Some("blake3:dpy".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by digest");

        assert!(
            matches.iter().all(|m| m.digest == "blake3:dpy"),
            "only the matching digest must survive, got {matches:?}"
        );
        let m = find(&matches, "company-configs", "python")
            .expect("company-configs/python carries digest blake3:dpy");
        assert_eq!(m.digest, "blake3:dpy", "match must echo the queried digest");
    }

    #[test]
    fn where_combines_filters_with_and_semantics() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("dotfiles".to_owned()),
            artifact: Some("init".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where with source AND artifact");

        assert_eq!(
            matches.len(),
            1,
            "source=dotfiles AND artifact=init must select exactly one group, got {matches:?}"
        );
        assert!(
            find(&matches, "dotfiles", "init").is_some(),
            "the single match must be dotfiles/init"
        );
        assert!(
            find(&matches, "dotfiles", "settings").is_none(),
            "dotfiles/settings fails the artifact=init constraint"
        );
    }

    #[test]
    fn where_groups_a_shared_artifact_across_its_targets() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("company-configs".to_owned()),
            artifact: Some("python".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where company-configs/python");

        assert_eq!(
            matches.len(),
            1,
            "the two deployments of company-configs/python must collapse into one group"
        );
        let m = &matches[0];
        let mut targets = m.targets.clone();
        targets.sort();
        assert_eq!(
            targets,
            vec!["agent-1".to_owned(), "vscode".to_owned()],
            "the grouped match must list both targets the artifact deploys to"
        );
    }

    #[test]
    fn where_with_no_match_is_empty() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("nonexistent".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where with no matching source");

        assert!(
            matches.is_empty(),
            "a filter matching nothing yields an empty result, got {matches:?}"
        );
    }

    // check_match_cmd

    #[test]
    fn check_match_reports_artifact_allowed_for_included_name() {
        let source = source_with(&["editor"], &[]);

        let report = check_match_cmd(&source, "editor");

        assert!(
            report.artifact_allowed,
            "an artifact name on the include list must be reported as artifact-allowed"
        );
    }

    #[test]
    fn check_match_reports_artifact_not_allowed_for_unlisted_name() {
        let source = source_with(&["editor"], &[]);

        let report = check_match_cmd(&source, "vim");

        assert!(
            !report.artifact_allowed,
            "a name absent from a non-empty include list must be reported as not artifact-allowed"
        );
    }

    #[test]
    fn check_match_reports_path_excluded_for_bak_file() {
        let source = source_with(&[], &["**/*.bak"]);

        let report = check_match_cmd(&source, "editor/notes.bak");

        assert!(
            !report.path_allowed,
            "a path matching the `**/*.bak` exclude must be reported as not path-allowed"
        );
    }

    #[test]
    fn check_match_reports_path_allowed_for_non_excluded_file() {
        let source = source_with(&[], &["**/*.bak"]);

        let report = check_match_cmd(&source, "editor/init.lua");

        assert!(
            report.path_allowed,
            "a path not matching any exclude must be reported as path-allowed"
        );
    }

    #[test]
    fn check_match_distinguishes_artifact_and_path_outcomes() {
        let source = source_with(&["editor"], &["**/*.bak"]);

        let allowed = check_match_cmd(&source, "editor");
        assert!(
            allowed.artifact_allowed && allowed.path_allowed,
            "an included artifact name with no exclude hit must be allowed at both levels"
        );

        let mixed = check_match_cmd(&source, "editor/notes.bak");
        assert!(
            mixed.artifact_allowed,
            "the `editor` artifact stays allowed even when its path is excluded"
        );
        assert!(
            !mixed.path_allowed,
            "the path-level exclude must independently reject editor/notes.bak"
        );
        assert_ne!(
            mixed.artifact_allowed, mixed.path_allowed,
            "artifact-level and path-level outcomes must differ for editor/notes.bak"
        );
    }

    // parse_add_url

    fn no_hosts() -> BTreeMap<String, Host> {
        BTreeMap::new()
    }

    fn parse(input: &str) -> ParsedSource {
        parse_add_url(input, &no_hosts()).unwrap_or_else(|e| panic!("parse `{input}`: {e}"))
    }

    #[test]
    fn github_shorthand_expands_to_https_git_url() {
        let parsed = parse("srnnkls/loqui");
        assert_eq!(
            parsed.git, "https://github.com/srnnkls/loqui.git",
            "owner/repo shorthand must expand via the default github template"
        );
        assert_eq!(
            parsed.name, "loqui",
            "default name is the repo segment, not the owner"
        );
        assert!(
            parsed.branch.is_none(),
            "a bare shorthand carries no branch"
        );
        assert!(parsed.root.is_none(), "a bare shorthand carries no root");
    }

    #[test]
    fn github_shorthand_with_extra_path_becomes_root() {
        let parsed = parse("owner/repo/path/to/dir");
        assert_eq!(
            parsed.git, "https://github.com/owner/repo.git",
            "only owner/repo form the git URL; the rest is the root"
        );
        assert_eq!(
            parsed.root.as_deref(),
            Some("path/to/dir"),
            "trailing path segments become the source root"
        );
        assert_eq!(
            parsed.name, "repo",
            "default name is still the repo segment"
        );
        assert!(
            parsed.branch.is_none(),
            "a shorthand+path carries no branch"
        );
    }

    #[test]
    fn host_prefixed_shorthand_expands_to_https() {
        let parsed = parse("github.com/owner/repo");
        assert_eq!(
            parsed.git, "https://github.com/owner/repo.git",
            "a github.com/owner/repo shorthand must expand to the full https git URL"
        );
        assert_eq!(parsed.name, "repo");
        assert!(
            parsed.branch.is_none(),
            "a host-prefixed shorthand carries no branch"
        );
        assert!(
            parsed.root.is_none(),
            "a host-prefixed shorthand carries no root"
        );
    }

    #[test]
    fn full_https_url_gets_git_suffix_appended() {
        let parsed = parse("https://github.com/owner/repo");
        assert_eq!(
            parsed.git, "https://github.com/owner/repo.git",
            "a full https URL without .git must have .git appended"
        );
        assert_eq!(parsed.name, "repo");
        assert!(parsed.branch.is_none());
        assert!(parsed.root.is_none());
    }

    #[test]
    fn tree_url_extracts_branch_and_root() {
        let parsed = parse("https://github.com/company/configs/tree/main/editor");
        assert_eq!(
            parsed.git, "https://github.com/company/configs.git",
            "the /tree/<ref>/<path> tail must be stripped from the git URL"
        );
        assert_eq!(
            parsed.branch.as_deref(),
            Some("main"),
            "the segment after /tree/ is the branch"
        );
        assert_eq!(
            parsed.root.as_deref(),
            Some("editor"),
            "the segments after /tree/<ref>/ are the root"
        );
        assert_eq!(
            parsed.name, "configs",
            "name is the repo, not the path tail"
        );
    }

    #[test]
    fn gitlab_shorthand_uses_gitlab_default_template() {
        let parsed = parse("gitlab.com/owner/repo");
        assert_eq!(
            parsed.git, "https://gitlab.com/owner/repo.git",
            "gitlab.com host must resolve via the built-in gitlab template, not github"
        );
        assert_eq!(parsed.name, "repo");
        assert!(
            parsed.branch.is_none(),
            "a gitlab shorthand carries no branch"
        );
        assert!(parsed.root.is_none(), "a gitlab shorthand carries no root");
    }

    #[test]
    fn ssh_url_is_kept_as_a_git_remote() {
        let parsed = parse("git@github.com:owner/repo.git");
        assert_eq!(
            parsed.git, "git@github.com:owner/repo.git",
            "an ssh scp-style URL is a valid git remote and must be preserved verbatim"
        );
        assert_eq!(
            parsed.name, "repo",
            "the repo segment of an ssh URL (minus .git) is the default name"
        );
        assert!(parsed.branch.is_none(), "an ssh URL carries no branch");
        assert!(parsed.root.is_none(), "an ssh URL carries no root");
    }

    #[test]
    fn custom_host_template_expands_host_prefixed_shorthand() {
        let mut hosts = BTreeMap::new();
        hosts.insert(
            "company".to_owned(),
            Config::parse(
                "version = 1\n\n[hosts.company]\ngit_url = \"ssh://git@git.company.com:2222/scm/{owner}/{repo}.git\"\n",
            )
            .expect("host toml parses")
            .hosts
            .remove("company")
            .expect("company host present"),
        );

        let parsed = parse_add_url("git.company.com/owner/repo", &hosts)
            .expect("custom host shorthand resolves");

        assert_eq!(
            parsed.git, "ssh://git@git.company.com:2222/scm/owner/repo.git",
            "the host template (ssh scheme, :2222 port, /scm/ prefix) must be applied verbatim; \
             a generic default expansion could never produce this URL"
        );
        assert_eq!(parsed.name, "repo");
        assert!(
            parsed.branch.is_none(),
            "a custom-host shorthand carries no branch"
        );
        assert!(
            parsed.root.is_none(),
            "a custom-host shorthand carries no root"
        );
    }

    // insert_source

    const ADD_BASE: &str = "version = 1\n\n[sources.foo]\ngit = \"https://github.com/me/foo.git\"\nbranch = \"main\"\n";

    #[test]
    fn insert_source_preserves_existing_source_and_adds_new() {
        let out = insert_source(
            ADD_BASE,
            "loqui",
            "https://github.com/srnnkls/loqui.git",
            None,
            None,
        )
        .expect("insert into base toml");

        let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");

        let foo = cfg
            .sources
            .get("foo")
            .expect("existing foo source survives");
        assert_eq!(
            foo.git, "https://github.com/me/foo.git",
            "the pre-existing source must be untouched"
        );
        assert_eq!(
            foo.branch.as_deref(),
            Some("main"),
            "the pre-existing source's branch must be preserved"
        );

        let loqui = cfg.sources.get("loqui").expect("new loqui source added");
        assert_eq!(loqui.git, "https://github.com/srnnkls/loqui.git");
        assert!(
            loqui.branch.is_none(),
            "no branch was passed, so no branch key must be emitted"
        );
        assert!(
            loqui.root.is_none(),
            "no root was passed, so no root key must be emitted"
        );
    }

    #[test]
    fn insert_source_emits_branch_and_root_when_some() {
        let out = insert_source(
            ADD_BASE,
            "editor-config",
            "https://github.com/company/configs.git",
            Some("main"),
            Some("editor"),
        )
        .expect("insert with branch and root");

        let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");

        let foo = cfg
            .sources
            .get("foo")
            .expect("pre-existing foo source survives the branch/root insert");
        assert_eq!(
            foo.git, "https://github.com/me/foo.git",
            "the pre-existing source's git must be untouched when inserting a source with branch+root"
        );
        assert_eq!(
            foo.branch.as_deref(),
            Some("main"),
            "the pre-existing source's branch must be preserved"
        );

        let added = cfg
            .sources
            .get("editor-config")
            .expect("new editor-config source added");

        assert_eq!(added.git, "https://github.com/company/configs.git");
        assert_eq!(
            added.branch.as_deref(),
            Some("main"),
            "a Some(branch) must be written as a branch key"
        );
        assert_eq!(
            added.root.as_deref(),
            Some(Path::new("editor")),
            "a Some(root) must be written as a root key"
        );
    }

    #[test]
    fn insert_source_preserves_surrounding_text_verbatim() {
        let seeded =
            "# my comment\nversion = 1\n\n[sources.foo]\ngit = \"https://github.com/me/foo.git\"\n";

        let out = insert_source(
            seeded,
            "loqui",
            "https://github.com/srnnkls/loqui.git",
            None,
            None,
        )
        .expect("insert into seeded toml");

        assert!(
            out.contains("# my comment\nversion = 1"),
            "the leading comment and version line must survive byte-for-byte (no reformatting), got:\n{out}"
        );
        assert!(
            out.contains("[sources.foo]\ngit = \"https://github.com/me/foo.git\""),
            "the existing [sources.foo] block must be present unchanged, not relocated or rewritten, got:\n{out}"
        );
        assert!(
            out.contains("[sources.loqui]"),
            "the new table must be inserted as [sources.loqui]"
        );

        let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");
        let foo = cfg
            .sources
            .get("foo")
            .expect("existing foo source survives");
        assert_eq!(
            foo.git, "https://github.com/me/foo.git",
            "re-parsing the output must yield the original foo git value"
        );
    }

    #[test]
    fn insert_source_uses_standard_table_blocks_not_inline() {
        let first = insert_source(
            "version = 1\n",
            "loqui",
            "https://github.com/srnnkls/loqui.git",
            None,
            None,
        )
        .expect("insert first source into a doc with no sources table");

        assert!(
            first.contains("[sources.loqui]"),
            "the new source must be a standard table header [sources.loqui], not an inline table, got:\n{first}"
        );
        assert!(
            first.contains("git = \"https://github.com/srnnkls/loqui.git\""),
            "the git key must be written on its own line under [sources.loqui], got:\n{first}"
        );
        assert!(
            !first.contains("sources = {"),
            "the sources table must not be rendered as an inline `sources = {{ ... }}` table, got:\n{first}"
        );

        let second = insert_source(
            &first,
            "editor",
            "https://github.com/company/editor.git",
            None,
            None,
        )
        .expect("insert second source after the first");

        assert!(
            second.contains("[sources.loqui]"),
            "the first source must remain a standard [sources.loqui] block after a second insert, got:\n{second}"
        );
        assert!(
            second.contains("[sources.editor]"),
            "the second source must be its own standard [sources.editor] block, got:\n{second}"
        );
        assert!(
            !second.contains("sources = {"),
            "repeated inserts must stay as separate table blocks, never collapse into an inline table, got:\n{second}"
        );

        let cfg = Config::parse(&second).expect("block-form output is valid phora.toml");
        assert_eq!(
            cfg.sources
                .get("loqui")
                .expect("loqui source parses from block form")
                .git,
            "https://github.com/srnnkls/loqui.git"
        );
        assert_eq!(
            cfg.sources
                .get("editor")
                .expect("editor source parses from block form")
                .git,
            "https://github.com/company/editor.git"
        );
    }
}
