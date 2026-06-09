//! Command-line surface.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;

use clap::{Parser, Subcommand};

use crate::config::{Config, DownloadDigest, Host, Source, SourceMode, builtin_forges};
use crate::error::{Error, Result};
use crate::lock::{Lock, merge_locks};
use crate::matcher::PathMatcher;
use crate::paths::{ProjectId, phora_dir};
use crate::projection::{ArtifactState, check_artifact_state};
use crate::registry::{FileRegistry, Registry};
use crate::source::{GitBackend, HttpBackend, Protocol, RouterBackend};
use crate::sync::{Conflict, ConflictResolver, Resolution, SyncInput, SyncOutput, sync};

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
    /// Worktree include operations.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum WorktreeCommand {
    /// Materialize configured includes in the current linked worktree.
    Apply { path: Option<std::path::PathBuf> },
    /// Convert a legacy `.worktreeinclude` manifest into `[worktree].includes`.
    ImportLegacy { file: std::path::PathBuf },
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Add {
            url,
            name,
            branch,
            tag,
            root,
        } => run_add(&url, name, branch, tag, root),
        Command::Sync { prune, force } => run_sync(prune, force, None),
        Command::Update { source } => run_update(source.as_deref()),
        Command::List { plan } => run_list(plan),
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
        Command::RebuildRegistry => run_rebuild_registry(),
        Command::CheckMatch { source, path } => {
            let source = load_source(&source)?;
            let report = check_match_cmd(&source, &path);
            print_check_match(&source, &path, &report);
            Ok(())
        }
        Command::Worktree { command } => match command {
            WorktreeCommand::Apply { path } => run_worktree_apply(path.as_deref()),
            WorktreeCommand::ImportLegacy { file } => run_worktree_import_legacy(&file),
        },
    }
}

fn run_worktree_import_legacy(file: &Path) -> Result<()> {
    let contents = std::fs::read_to_string(file)
        .map_err(|e| Error::Config(format!("read {}: {e}", file.display())))?;
    let import = crate::import_legacy::convert_legacy(&contents);
    print!(
        "{}",
        crate::import_legacy::render_worktree_toml(&import.includes)
    );
    for skipped in &import.skipped {
        eprintln!("phora: skipped {} ({})", skipped.line, skipped.reason);
    }
    Ok(())
}

fn run_worktree_apply(path: Option<&Path>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let worktree_root = path.unwrap_or(&cwd);
    let base = load_config_from(worktree_root)?;
    let local = load_local_config(worktree_root)?;
    let config = crate::config::merge_configs(base, local);
    let includes = config
        .worktree
        .as_ref()
        .map_or(&[][..], |w| w.includes.as_slice());

    let primary = crate::worktree::primary_worktree(worktree_root)?;
    let repo = gix::discover(worktree_root)
        .map_err(|e| Error::Worktree(format!("open repository: {e}")))?;
    let report = crate::worktree_apply::apply(worktree_root, &primary, &repo, includes)?;
    if report.primary_noop {
        eprintln!("phora: worktree apply is a no-op in the primary worktree");
        return Ok(());
    }
    for entry in &report.entries {
        println!("{}: {:?}", entry.path.display(), entry.outcome);
    }
    if report.had_failures() {
        eprintln!("phora: some includes failed to apply");
        std::process::exit(1);
    }
    Ok(())
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
    println!("Added source '{name}': {}{refspec}", describe_source(&parsed));
    Ok(())
}

fn describe_source(source: &ParsedSource) -> String {
    match (&source.git, &source.host, &source.path) {
        (Some(git), _, _) => git.clone(),
        (None, Some(host), Some(path)) => format!("{host}:{path}"),
        _ => source.name.clone(),
    }
}

fn insert_source_with_ref(
    doc_text: &str,
    name: &str,
    source: &ParsedSource,
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

fn source_table(source: &ParsedSource) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    if let Some(git) = &source.git {
        table["git"] = toml_edit::value(git.as_str());
        return table;
    }
    if let Some(host) = &source.host {
        table["host"] = toml_edit::value(host.as_str());
    }
    if let Some(path) = &source.path {
        table["path"] = toml_edit::value(path.as_str());
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

fn run_list(plan: bool) -> Result<()> {
    let config = load_config()?;
    let registry = open_project_registry()?;
    if plan {
        println!("plan: run `phora sync` to apply pending changes");
        return Ok(());
    }
    let listings = list_statuses(&config, &registry)?;
    print_listings(&listings);
    Ok(())
}

fn print_listings(listings: &[TargetListing]) {
    for listing in listings {
        println!("{}:", listing.target);
        for artifact in &listing.artifacts {
            println!(
                "  {}/{}  {}",
                artifact.source, artifact.artifact, artifact.state
            );
        }
    }
}

/// Builds the mode-aware router for `config`, parsing each url source's `digest`.
fn build_router(
    config: &Config,
    git_dir: std::path::PathBuf,
) -> Result<RouterBackend<GitBackend, HttpBackend>> {
    let mut modes = BTreeMap::new();
    let mut digests = BTreeMap::new();
    for (name, source) in &config.sources {
        modes.insert(name.clone(), source.mode());
        if source.mode() == SourceMode::Url
            && let Some(raw) = source.digest.as_deref()
        {
            let digest = DownloadDigest::parse(raw)
                .map_err(|e| Error::Config(format!("source `{name}`: {e}")))?;
            digests.insert(name.clone(), digest);
        }
    }
    let git = GitBackend::new(git_dir.clone());
    let http = HttpBackend::new(git_dir, digests);
    Ok(RouterBackend::new(git, http, modes))
}

fn run_sync(prune: bool, force: bool, drop: Option<DropSources>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let (mut base_lock, mut local_lock) = load_locks(&cwd)?;

    if let Some(drop) = drop {
        drop_sources(base_lock.as_mut(), &drop);
        drop_sources(local_lock.as_mut(), &drop);
    }

    let effective = crate::config::merge_configs(base.clone(), local.clone());
    let backend = build_router(&effective, phora_dir()?.join("git"))?;
    let registry = open_project_registry()?;
    let interactive = std::io::stdin().is_terminal();
    let resolver = TtyResolver;

    let out = sync(
        &SyncInput {
            base_config: &base,
            local_config: local.as_ref(),
            base_lock,
            local_lock,
            force,
            interactive,
            prune,
            resolver: interactive.then_some(&resolver as &dyn ConflictResolver),
        },
        &backend,
        &registry,
    )?;

    finish_sync(&cwd, &out)
}

fn finish_sync(cwd: &Path, out: &SyncOutput) -> Result<()> {
    write_locks(cwd, &out.base_lock, out.local_lock.as_ref())?;
    if out.had_failures {
        eprintln!("phora: some artifacts failed to deploy");
        std::process::exit(1);
    }
    println!("sync complete");
    Ok(())
}

fn run_rebuild_registry() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = load_config()?;
    config.validate()?;
    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = match base_lock {
        Some(base) => merge_locks(&base, local_lock.as_ref()),
        None => local_lock
            .ok_or_else(|| Error::Lock("no lock file found; run sync first".to_owned()))?,
    };

    let backend = build_router(&config, phora_dir()?.join("git"))?;
    let registry = open_project_registry()?;
    let report = crate::sync::rebuild_registry(&config, &lock, &backend, &registry)?;

    println!("reconstructed {}", report.reconstructed.len());
    if !report.modified.is_empty() {
        let modified: Vec<String> = report
            .modified
            .iter()
            .map(|k| format!("{}/{}", k.source, k.artifact))
            .collect();
        println!("modified [{}]", modified.join(", "));
    }
    if !report.foreign.is_empty() {
        let foreign: Vec<String> = report
            .foreign
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        println!("foreign [{}]", foreign.join(", "));
    }
    Ok(())
}

fn run_update(source: Option<&str>) -> Result<()> {
    let drop = source.map_or(DropSources::All, |s| DropSources::One(s.to_owned()));
    run_sync(false, false, Some(drop))
}

/// Which locked source entries to drop before a sync so they get re-resolved.
enum DropSources {
    All,
    One(String),
}

fn drop_sources(lock: Option<&mut Lock>, drop: &DropSources) {
    let Some(lock) = lock else { return };
    match drop {
        DropSources::All => lock.sources.clear(),
        DropSources::One(name) => lock.sources.retain(|s| &s.name != name),
    }
}

fn load_local_config(cwd: &Path) -> Result<Option<Config>> {
    let path = cwd.join("phora.local.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Config(format!("read {}: {e}", path.display()))),
    }
}

/// Prompts on stderr and reads a resolution character from stdin for each conflict.
struct TtyResolver;

impl ConflictResolver for TtyResolver {
    fn resolve(&self, conflict: &Conflict) -> Resolution {
        loop {
            eprint!(
                "phora: conflict at {}/{} in {} — [s]kip/[o]verwrite/[e]ject/[a]bort? ",
                conflict.source, conflict.artifact, conflict.target
            );
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => return Resolution::Skip,
                Ok(_) => {
                    if let Some(resolution) = line.chars().next().and_then(resolution_from_char) {
                        return resolution;
                    }
                }
            }
        }
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
    load_config_from(Path::new("."))
}

fn load_config_from(dir: &Path) -> Result<Config> {
    let path = dir.join("phora.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Config(format!("read {}: {e}", path.display())))?;
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

/// A source derived from an `add` URL/shorthand, before name overrides. Either
/// literal (`git` is `Some`) or symbolic (`host`/`path` are `Some`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSource {
    pub name: String,
    pub git: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub protocol: Option<Protocol>,
    pub branch: Option<String>,
    pub root: Option<String>,
}

/// Expands an `add` URL/shorthand into a [`ParsedSource`] using the built-in
/// forge registry overlaid by any host in `hosts`.
///
/// # Errors
///
/// Returns [`Error::Config`] if the input cannot be parsed into owner/repo.
pub fn parse_add_url(input: &str, hosts: &BTreeMap<String, Host>) -> Result<ParsedSource> {
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

fn parse_colon_alias(input: &str, host: &str, path: &str) -> Result<ParsedSource> {
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

fn symbolic_source(host: String, path: &str, root: Option<String>) -> ParsedSource {
    ParsedSource {
        name: repo_name(path),
        git: None,
        host: Some(host),
        path: Some(path.to_owned()),
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

fn parse_scp_ssh(input: &str) -> ParsedSource {
    literal_source(repo_name(input), input.to_owned(), None, None)
}

fn literal_source(
    name: String,
    git: String,
    branch: Option<String>,
    root: Option<String>,
) -> ParsedSource {
    ParsedSource {
        name,
        git: Some(git),
        host: None,
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
        return Ok(literal_source(
            name,
            with_git_suffix(&format!("{scheme}://{host}/{owner}/{repo}")),
            Some((*git_ref).to_owned()),
            join_root(root_segments),
        ));
    }

    Ok(literal_source(name, with_git_suffix(input), None, None))
}

fn parse_shorthand(
    input: &str,
    domains: &BTreeMap<String, String>,
) -> Result<ParsedSource> {
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
    source: &ParsedSource,
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

/// A `phora list` row for one managed artifact under a target: its source, the
/// artifact name, and a human-readable state label (`✓`, `[modified]`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStatus {
    pub source: String,
    pub artifact: String,
    pub state: String,
}

/// `phora list` grouped by target: every managed artifact's status under one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetListing {
    pub target: String,
    pub artifacts: Vec<ArtifactStatus>,
}

/// Writes the base lock to `<dir>/phora.lock` and, when `local` is `Some`, the
/// local lock to `<dir>/phora.local.lock`; when `local` is `None`, removes any
/// stale `<dir>/phora.local.lock`.
///
/// # Errors
///
/// Returns an error if serialization or filesystem I/O fails.
pub fn write_locks(dir: &Path, base: &Lock, local: Option<&Lock>) -> Result<()> {
    let base_path = dir.join("phora.lock");
    let base_text =
        toml::to_string(base).map_err(|e| Error::Lock(format!("serialize phora.lock: {e}")))?;
    std::fs::write(&base_path, base_text)
        .map_err(|e| Error::Lock(format!("write {}: {e}", base_path.display())))?;

    let local_path = dir.join("phora.local.lock");
    match local {
        Some(local) => {
            let local_text = toml::to_string(local)
                .map_err(|e| Error::Lock(format!("serialize phora.local.lock: {e}")))?;
            std::fs::write(&local_path, local_text)
                .map_err(|e| Error::Lock(format!("write {}: {e}", local_path.display())))?;
        }
        None => match std::fs::remove_file(&local_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::Lock(format!(
                    "remove stale {}: {e}",
                    local_path.display()
                )));
            }
        },
    }
    Ok(())
}

/// Reads `<dir>/phora.lock` and `<dir>/phora.local.lock`, returning `None` for
/// either file that is absent.
///
/// # Errors
///
/// Returns an error if a present lock file cannot be read or parsed.
pub fn load_locks(dir: &Path) -> Result<(Option<Lock>, Option<Lock>)> {
    Ok((
        read_lock(&dir.join("phora.lock"))?,
        read_lock(&dir.join("phora.local.lock"))?,
    ))
}

fn read_lock(path: &Path) -> Result<Option<Lock>> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| Error::Lock(format!("parse {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Lock(format!("read {}: {e}", path.display()))),
    }
}

/// Registry-driven `phora list`: per target, the status of every managed artifact,
/// computed via [`check_artifact_state`](crate::projection::check_artifact_state).
///
/// # Errors
///
/// Returns an error if the registry or on-disk targets cannot be read.
pub fn list_statuses(config: &Config, registry: &dyn Registry) -> Result<Vec<TargetListing>> {
    let mut listings = Vec::new();
    for (target_name, target) in &config.targets {
        let ejected = registry.load_ejected(target_name)?;
        let mut artifacts = Vec::new();
        for rec in registry.list_target(target_name)? {
            let artifact_dst = target.expanded_path().join(
                target
                    .layout()
                    .artifact_path(&rec.key.source, &rec.key.artifact),
            );
            let state = check_artifact_state(
                &artifact_dst,
                &rec.key.source,
                &rec.commit,
                &ejected,
                &rec.key.artifact,
                registry,
                &rec.key,
            )?;
            artifacts.push(ArtifactStatus {
                source: rec.key.source,
                artifact: rec.key.artifact,
                state: state_label(&state).to_owned(),
            });
        }
        listings.push(TargetListing {
            target: target_name.clone(),
            artifacts,
        });
    }
    Ok(listings)
}

fn state_label(state: &ArtifactState) -> &'static str {
    match state {
        ArtifactState::Clean => "✓ clean",
        ArtifactState::Modified { .. } => "modified",
        ArtifactState::Foreign => "foreign",
        ArtifactState::Missing => "missing",
        ArtifactState::Ejected => "ejected",
        ArtifactState::Linked => "linked",
    }
}

/// Maps an interactive prompt character to a [`Resolution`]: `s`→Skip, `o`→Overwrite,
/// `e`→Eject, `a`→Abort; any other character yields `None`.
#[must_use]
pub fn resolution_from_char(c: char) -> Option<Resolution> {
    match c {
        's' => Some(Resolution::Skip),
        'o' => Some(Resolution::Overwrite),
        'e' => Some(Resolution::Eject),
        'a' => Some(Resolution::Abort),
        _ => None,
    }
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

    #[test]
    fn cli_parses_worktree_apply() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from(["phora", "worktree", "apply"])
            .expect("`phora worktree apply` must parse as a nested subcommand");
        match cli.command {
            Command::Worktree {
                command: WorktreeCommand::Apply { path },
            } => assert!(
                path.is_none(),
                "bare `worktree apply` must leave the optional path unset"
            ),
            other => panic!("expected Worktree {{ Apply }}, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_worktree_apply_with_path() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from(["phora", "worktree", "apply", "/tmp/wt"])
            .expect("`phora worktree apply <path>` must parse the positional path arg");
        match cli.command {
            Command::Worktree {
                command: WorktreeCommand::Apply { path },
            } => assert_eq!(
                path,
                Some(PathBuf::from("/tmp/wt")),
                "the positional path arg must route into Apply {{ path: Some(..) }}"
            ),
            other => panic!("expected Worktree {{ Apply }}, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_worktree_import_legacy() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from(["phora", "worktree", "import-legacy", ".worktreeinclude"])
            .expect("`phora worktree import-legacy <file>` must parse as a nested subcommand");
        match cli.command {
            Command::Worktree {
                command: WorktreeCommand::ImportLegacy { file },
            } => assert_eq!(
                file,
                PathBuf::from(".worktreeinclude"),
                "the positional file arg must route into ImportLegacy {{ file }}"
            ),
            other => panic!("expected Worktree {{ ImportLegacy }}, got {other:?}"),
        }
    }

    #[test]
    fn state_label_renders_linked_artifact_as_linked() {
        assert_eq!(
            state_label(&ArtifactState::Linked),
            "linked",
            "`phora list` must label a Linked artifact `linked`"
        );
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
            linked: false,
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
    fn github_shorthand_yields_symbolic_github_source() {
        let parsed = parse("srnnkls/loqui");
        assert_eq!(
            parsed.host.as_deref(),
            Some("github"),
            "a bare owner/repo shorthand must default to the symbolic github host, not an expanded git URL (got git={:?})",
            parsed.git
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("srnnkls/loqui"),
            "the shorthand must carry path=srnnkls/loqui symbolically"
        );
        assert!(
            parsed.git.is_none(),
            "owner/repo must stay symbolic, not pre-expand to a github git URL, got {:?}",
            parsed.git
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
            parsed.host.as_deref(),
            Some("github"),
            "only owner/repo feed the symbolic host/path; the rest is the root"
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("owner/repo"),
            "the symbolic path is exactly owner/repo, with trailing segments split off as root"
        );
        assert!(
            parsed.git.is_none(),
            "a shorthand+path stays symbolic, got {:?}",
            parsed.git
        );
        assert_eq!(
            parsed.root.as_deref(),
            Some("path/to/dir"),
            "trailing path segments beyond owner/repo become the source root"
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
    fn domain_shorthand_yields_symbolic_github_source() {
        let parsed = parse("github.com/owner/repo");
        assert_eq!(
            parsed.host.as_deref(),
            Some("github"),
            "a github.com/owner/repo domain shorthand must map to the symbolic host name `github`, not an expanded https URL (got git={:?})",
            parsed.git
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("owner/repo"),
            "the domain shorthand must carry path=owner/repo symbolically"
        );
        assert!(
            parsed.git.is_none(),
            "a domain shorthand stays symbolic, got {:?}",
            parsed.git
        );
        assert_eq!(parsed.name, "repo");
        assert!(
            parsed.branch.is_none(),
            "a domain shorthand carries no branch"
        );
        assert!(
            parsed.root.is_none(),
            "a domain shorthand carries no root"
        );
    }

    #[test]
    fn srht_domain_shorthand_maps_to_symbolic_srht_host() {
        let parsed = parse("git.sr.ht/~rjarry/aerc");
        assert_eq!(
            parsed.host.as_deref(),
            Some("sr.ht"),
            "the git.sr.ht DOMAIN must map to the forge NAME `sr.ht`, not the domain string"
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("~rjarry/aerc"),
            "the `~` owner segment must be preserved verbatim in the symbolic path"
        );
        assert!(
            parsed.git.is_none(),
            "a non-github domain shorthand stays symbolic, got {:?}",
            parsed.git
        );
        assert_eq!(parsed.name, "aerc");
    }

    #[test]
    fn full_https_url_stays_a_literal_git_remote() {
        let parsed = parse("https://github.com/owner/repo");
        assert_eq!(
            parsed.git.as_deref(),
            Some("https://github.com/owner/repo.git"),
            "a full https URL stays a LITERAL git remote (back-compat) with .git appended"
        );
        assert!(
            parsed.host.is_none() && parsed.path.is_none(),
            "a literal scheme URL must NOT become a symbolic host/path source"
        );
        assert_eq!(parsed.name, "repo");
        assert!(parsed.branch.is_none());
        assert!(parsed.root.is_none());
    }

    #[test]
    fn tree_url_stays_literal_and_extracts_branch_and_root() {
        let parsed = parse("https://github.com/company/configs/tree/main/editor");
        assert_eq!(
            parsed.git.as_deref(),
            Some("https://github.com/company/configs.git"),
            "a scheme URL stays a LITERAL git remote; the /tree/<ref>/<path> tail is stripped from it"
        );
        assert!(
            parsed.host.is_none() && parsed.path.is_none(),
            "a tree URL is a literal scheme URL, not a symbolic host/path source"
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
    fn gitlab_domain_shorthand_maps_to_symbolic_gitlab_host() {
        let parsed = parse("gitlab.com/owner/repo");
        assert_eq!(
            parsed.host.as_deref(),
            Some("gitlab"),
            "the gitlab.com DOMAIN must map to the symbolic forge NAME `gitlab`, not github and not an expanded URL (got git={:?})",
            parsed.git
        );
        assert_eq!(parsed.path.as_deref(), Some("owner/repo"));
        assert!(
            parsed.git.is_none(),
            "a gitlab domain shorthand stays symbolic, got {:?}",
            parsed.git
        );
        assert_eq!(parsed.name, "repo");
        assert!(
            parsed.branch.is_none(),
            "a gitlab shorthand carries no branch"
        );
        assert!(parsed.root.is_none(), "a gitlab shorthand carries no root");
    }

    #[test]
    fn codeberg_domain_shorthand_maps_to_symbolic_codeberg_host() {
        let parsed = parse("codeberg.org/owner/repo");
        assert_eq!(
            parsed.host.as_deref(),
            Some("codeberg"),
            "codeberg.org must map to the symbolic forge NAME `codeberg` via the SINGLE built-in forge registry"
        );
        assert_eq!(parsed.path.as_deref(), Some("owner/repo"));
        assert!(
            parsed.git.is_none(),
            "a codeberg domain shorthand stays symbolic, got {:?}",
            parsed.git
        );
        assert_eq!(parsed.name, "repo");
    }

    #[test]
    fn bitbucket_domain_shorthand_maps_to_symbolic_bitbucket_host() {
        let parsed = parse("bitbucket.org/owner/repo");
        assert_eq!(
            parsed.host.as_deref(),
            Some("bitbucket"),
            "bitbucket.org must map to the symbolic forge NAME `bitbucket`; it can only resolve if \
             the forge registry derives from builtin_forges()"
        );
        assert_eq!(parsed.path.as_deref(), Some("owner/repo"));
        assert!(
            parsed.git.is_none(),
            "a bitbucket domain shorthand stays symbolic, got {:?}",
            parsed.git
        );
        assert_eq!(parsed.name, "repo");
    }

    #[test]
    fn scp_ssh_url_is_kept_as_a_literal_git_remote() {
        let parsed = parse("git@github.com:owner/repo.git");
        assert_eq!(
            parsed.git.as_deref(),
            Some("git@github.com:owner/repo.git"),
            "an ssh scp-style URL is a literal git remote and must be preserved verbatim"
        );
        assert!(
            parsed.host.is_none() && parsed.path.is_none(),
            "an scp literal must NOT become a symbolic host/path source"
        );
        assert_eq!(
            parsed.name, "repo",
            "the repo segment of an ssh URL (minus .git) is the default name"
        );
        assert!(parsed.branch.is_none(), "an ssh URL carries no branch");
        assert!(parsed.root.is_none(), "an ssh URL carries no root");
    }

    #[test]
    fn custom_host_domain_shorthand_maps_to_symbolic_custom_host() {
        let mut hosts = BTreeMap::new();
        hosts.insert(
            "company".to_owned(),
            Config::parse(
                "version = 1\n\n[hosts.company]\nremote = \"ssh://git@git.company.com:2222/scm/{owner}/{repo}.git\"\n",
            )
            .expect("host toml parses")
            .hosts
            .remove("company")
            .expect("company host present"),
        );

        let parsed = parse_add_url("git.company.com/owner/repo", &hosts)
            .expect("custom host shorthand resolves");

        assert_eq!(
            parsed.host.as_deref(),
            Some("company"),
            "the custom host's DOMAIN (git.company.com) must map to its symbolic host NAME `company`, \
             not be expanded into the template URL"
        );
        assert_eq!(parsed.path.as_deref(), Some("owner/repo"));
        assert!(
            parsed.git.is_none(),
            "a custom-host domain shorthand stays symbolic, got {:?}",
            parsed.git
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

    fn lit(git: &str, branch: Option<&str>) -> ParsedSource {
        ParsedSource {
            name: String::new(),
            git: Some(git.to_owned()),
            host: None,
            path: None,
            protocol: None,
            branch: branch.map(str::to_owned),
            root: None,
        }
    }

    #[test]
    fn insert_source_preserves_existing_source_and_adds_new() {
        let out = insert_source(
            ADD_BASE,
            "loqui",
            &lit("https://github.com/srnnkls/loqui.git", None),
            None,
        )
        .expect("insert into base toml");

        let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");

        let foo = cfg
            .sources
            .get("foo")
            .expect("existing foo source survives");
        assert_eq!(
            foo.git.as_deref(),
            Some("https://github.com/me/foo.git"),
            "the pre-existing source must be untouched"
        );
        assert_eq!(
            foo.branch.as_deref(),
            Some("main"),
            "the pre-existing source's branch must be preserved"
        );

        let loqui = cfg.sources.get("loqui").expect("new loqui source added");
        assert_eq!(
            loqui.git.as_deref(),
            Some("https://github.com/srnnkls/loqui.git")
        );
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
            &lit("https://github.com/company/configs.git", Some("main")),
            Some("editor"),
        )
        .expect("insert with branch and root");

        let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");

        let foo = cfg
            .sources
            .get("foo")
            .expect("pre-existing foo source survives the branch/root insert");
        assert_eq!(
            foo.git.as_deref(),
            Some("https://github.com/me/foo.git"),
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

        assert_eq!(
            added.git.as_deref(),
            Some("https://github.com/company/configs.git")
        );
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
            &lit("https://github.com/srnnkls/loqui.git", None),
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
            foo.git.as_deref(),
            Some("https://github.com/me/foo.git"),
            "re-parsing the output must yield the original foo git value"
        );
    }

    #[test]
    fn insert_source_uses_standard_table_blocks_not_inline() {
        let first = insert_source(
            "version = 1\n",
            "loqui",
            &lit("https://github.com/srnnkls/loqui.git", None),
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
            &lit("https://github.com/company/editor.git", None),
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
                .git
                .as_deref(),
            Some("https://github.com/srnnkls/loqui.git")
        );
        assert_eq!(
            cfg.sources
                .get("editor")
                .expect("editor source parses from block form")
                .git
                .as_deref(),
            Some("https://github.com/company/editor.git")
        );
    }

    // ── HAS-004: add writes symbolic host/path (both writer paths) ──

    use crate::source::Protocol;

    /// Re-parses inserted text and returns the named source, asserting validity.
    fn source_from(out: &str, name: &str) -> Source {
        Config::parse(out)
            .unwrap_or_else(|e| panic!("inserted text must be valid phora.toml: {e}\n{out}"))
            .sources
            .remove(name)
            .unwrap_or_else(|| panic!("source `{name}` must be present in:\n{out}"))
    }

    #[test]
    fn parse_colon_alias_yields_symbolic_github_source() {
        let parsed = parse("github:srnnkls/tropos");
        assert_eq!(
            parsed.host.as_deref(),
            Some("github"),
            "`github:srnnkls/tropos` must parse to a symbolic source with host=github, not an expanded git URL (got git={:?})",
            parsed.git
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("srnnkls/tropos"),
            "the colon alias must carry path=srnnkls/tropos symbolically"
        );
        assert_eq!(
            parsed.name, "tropos",
            "the default name is the repo segment of the alias"
        );
        assert!(
            parsed.git.is_none(),
            "a symbolic colon alias must NOT be pre-expanded into a literal git URL, got {:?}",
            parsed.git
        );
    }

    #[test]
    fn parse_colon_alias_is_not_mistaken_for_scp() {
        let parsed = parse("github:owner/repo");
        assert!(
            parsed.git.is_none(),
            "`github:owner/repo` has no `@`, so it must NOT be dispatched to the scp-ssh path that keeps the literal string; git must be None, got {:?}",
            parsed.git
        );
        assert_eq!(
            parsed.host.as_deref(),
            Some("github"),
            "the colon alias must resolve symbolically to host=github, not be treated as an scp remote"
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("owner/repo"),
            "the colon alias must carry path=owner/repo, not swallow it into a literal scp string"
        );
    }

    #[test]
    fn parse_gitlab_colon_alias_yields_symbolic_gitlab_source() {
        let parsed = parse("gitlab:owner/repo");
        assert_eq!(
            parsed.host.as_deref(),
            Some("gitlab"),
            "`gitlab:owner/repo` must parse to a symbolic source with host=gitlab"
        );
        assert_eq!(parsed.path.as_deref(), Some("owner/repo"));
        assert!(
            parsed.git.is_none(),
            "a symbolic gitlab alias must not be pre-expanded"
        );
    }

    #[test]
    fn colon_alias_splits_extra_path_into_root() {
        let parsed = parse("github:owner/repo/sub/dir");
        assert_eq!(
            parsed.host.as_deref(),
            Some("github"),
            "only owner/repo feed the symbolic host/path; the rest is the root"
        );
        assert_eq!(
            parsed.path.as_deref(),
            Some("owner/repo"),
            "the colon-alias path is exactly owner/repo, with trailing segments split off as root"
        );
        assert_eq!(
            parsed.root.as_deref(),
            Some("sub/dir"),
            "segments past owner/repo become the root, mirroring the slash shorthand"
        );
        assert!(
            parsed.git.is_none(),
            "a colon alias with extra path stays symbolic, got {:?}",
            parsed.git
        );
    }

    #[test]
    fn colon_alias_empty_host_errors() {
        let err = parse_add_url(":owner/repo", &no_hosts())
            .expect_err("`:owner/repo` has an empty host and must be rejected");
        assert!(
            err.to_string().contains(":owner/repo"),
            "the error must name the offending input, got `{err}`"
        );
    }

    #[test]
    fn colon_alias_empty_path_errors() {
        let err = parse_add_url("github:", &no_hosts())
            .expect_err("`github:` has an empty path and must be rejected");
        assert!(
            err.to_string().contains("github:"),
            "the error must name the offending input, got `{err}`"
        );
    }

    #[test]
    fn parse_bare_owner_repo_defaults_to_symbolic_github() {
        let parsed = parse("owner/repo");
        assert_eq!(
            parsed.host.as_deref(),
            Some("github"),
            "a bare owner/repo shorthand must default to the symbolic github host"
        );
        assert_eq!(parsed.path.as_deref(), Some("owner/repo"));
        assert!(
            parsed.git.is_none(),
            "a bare owner/repo must be symbolic, not expanded to a github git URL"
        );
    }

    #[test]
    fn scp_ssh_url_stays_a_literal_git_remote_not_symbolic() {
        let parsed = parse("git@github.com:owner/repo.git");
        assert_eq!(
            parsed.git.as_deref(),
            Some("git@github.com:owner/repo.git"),
            "an scp-ssh remote (has `@` before `:`) is a literal git remote, preserved verbatim"
        );
        assert!(
            parsed.host.is_none() && parsed.path.is_none(),
            "an scp literal must NOT be turned into a symbolic host/path source"
        );
    }

    #[test]
    fn full_https_url_stays_a_literal_git_remote_not_symbolic() {
        let parsed = parse("https://github.com/owner/repo");
        assert_eq!(
            parsed.git.as_deref(),
            Some("https://github.com/owner/repo.git"),
            "a full scheme URL stays a literal git remote (back-compat)"
        );
        assert!(
            parsed.host.is_none() && parsed.path.is_none(),
            "a literal scheme URL must NOT become a symbolic host/path source"
        );
    }

    #[test]
    fn run_add_writer_writes_symbolic_host_path_for_colon_alias() {
        let parsed = parse("github:srnnkls/tropos");
        let out = insert_source_with_ref("version = 1\n", &parsed.name, &parsed, None, None, None)
            .expect("run_add's writer must accept a symbolic ParsedSource");

        assert!(
            out.contains("[sources.tropos]"),
            "run_add's writer must emit a [sources.tropos] table, got:\n{out}"
        );
        assert!(
            !out.contains("git ="),
            "a symbolic add must NOT write an expanded `git =` key (run_add writer), got:\n{out}"
        );

        let src = source_from(&out, "tropos");
        assert_eq!(
            src.host.as_deref(),
            Some("github"),
            "run_add's writer must persist host = \"github\""
        );
        assert_eq!(
            src.path.as_deref(),
            Some("srnnkls/tropos"),
            "run_add's writer must persist path = \"srnnkls/tropos\""
        );
        assert!(
            src.git.is_none(),
            "run_add's writer must not persist a literal git for a symbolic add"
        );
    }

    /// Serializes cwd-mutating tests: `run_add` reads/writes `phora.toml` in the
    /// process cwd, so two such tests must not run concurrently.
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs `body` with the process cwd set to `dir`, restoring it on the way out
    /// even on panic. Holds [`CWD_LOCK`]; never run alongside other cwd tests.
    fn with_cwd<T>(dir: &std::path::Path, body: impl FnOnce() -> T) -> T {
        let _guard = CWD_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::current_dir().expect("read cwd");
        std::env::set_current_dir(dir).expect("enter temp cwd");
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        std::env::set_current_dir(&prev).expect("restore cwd");
        match out {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    #[test]
    fn run_add_end_to_end_persists_symbolic_source_to_phora_toml() {
        let dir = tempfile::TempDir::new().expect("temp project dir");
        let toml_path = dir.path().join("phora.toml");

        with_cwd(dir.path(), || {
            run_add("github:srnnkls/tropos", None, None, None, None)
                .expect("run_add must succeed for a symbolic colon alias");
        });

        let written = std::fs::read_to_string(&toml_path)
            .expect("run_add must write phora.toml in the cwd");
        assert!(
            !written.contains("git ="),
            "run_add's parse->writer glue must persist the SYMBOLIC source, never an expanded `git =`, got:\n{written}"
        );

        let src = source_from(&written, "tropos");
        assert_eq!(
            src.host.as_deref(),
            Some("github"),
            "run_add end-to-end must persist host = \"github\""
        );
        assert_eq!(
            src.path.as_deref(),
            Some("srnnkls/tropos"),
            "run_add end-to-end must persist path = \"srnnkls/tropos\""
        );
        assert!(
            src.git.is_none(),
            "run_add end-to-end must not persist a literal git for a symbolic add"
        );
    }

    #[test]
    fn insert_source_also_writes_symbolic_host_path_for_colon_alias() {
        let parsed = parse("gitlab:owner/repo");
        let out = insert_source("version = 1\n", &parsed.name, &parsed, None)
            .expect("the second writer must also accept a symbolic ParsedSource");

        assert!(
            !out.contains("git ="),
            "both writers must agree: a symbolic add writes no `git =` key, got:\n{out}"
        );
        let src = source_from(&out, "repo");
        assert_eq!(
            src.host.as_deref(),
            Some("gitlab"),
            "insert_source must persist host = \"gitlab\" symbolically"
        );
        assert_eq!(src.path.as_deref(), Some("owner/repo"));
        assert!(
            src.git.is_none(),
            "insert_source must not expand a symbolic add into a literal git"
        );
    }

    #[test]
    fn symbolic_add_omits_default_protocol_and_writes_non_default() {
        let default_src = parse("github:srnnkls/tropos");
        let default_out =
            insert_source_with_ref("version = 1\n", &default_src.name, &default_src, None, None, None)
                .expect("write default-protocol symbolic source");
        assert!(
            !default_out.contains("protocol"),
            "a default (https) protocol must be omitted from the written table, got:\n{default_out}"
        );
        assert!(
            default_out.contains("host = \"github\"") && default_out.contains("path = \"srnnkls/tropos\""),
            "the default-protocol silence is only meaningful if host+path are still written, got:\n{default_out}"
        );

        let ssh_src = ParsedSource {
            protocol: Some(Protocol::Ssh),
            ..parse("github:srnnkls/tropos")
        };
        let ssh_out =
            insert_source_with_ref("version = 1\n", &ssh_src.name, &ssh_src, None, None, None)
                .expect("write non-default-protocol symbolic source");
        assert!(
            ssh_out.contains("protocol = \"ssh\""),
            "a non-default protocol must be written literally as `protocol = \"ssh\"`, got:\n{ssh_out}"
        );
        assert!(
            ssh_out.contains("host = \"github\"") && ssh_out.contains("path = \"srnnkls/tropos\""),
            "a non-default-protocol source must still carry its symbolic host+path, got:\n{ssh_out}"
        );
        let src = source_from(&ssh_out, "tropos");
        assert_eq!(
            src.protocol,
            Some(Protocol::Ssh),
            "a non-default protocol on a symbolic source must round-trip through the parser, got:\n{ssh_out}"
        );
    }

    // ── write_locks / load_locks ───────────────────────────────────

    use crate::lock::{Lock, LockedSource};
    use crate::sync::Resolution;

    fn lock_with(name: &str, git: &str, resolved: &str) -> Lock {
        Lock {
            version: 1,
            sources: vec![LockedSource {
                name: name.to_owned(),
                git: git.to_owned(),
                resolved: resolved.to_owned(),
                commit: "c0ffeec0ffee".to_owned(),
                digest: "blake3:artifact".to_owned(),
                config_digest: "blake3:cfg".to_owned(),
            }],
        }
    }

    #[test]
    fn write_locks_base_only_writes_phora_lock_and_no_local_file() {
        let dir = TempDir::new().expect("locks dir");
        let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");

        write_locks(dir.path(), &base, None).expect("write base-only locks");

        assert!(
            dir.path().join("phora.lock").is_file(),
            "base-only write must create phora.lock"
        );
        assert!(
            !dir.path().join("phora.local.lock").exists(),
            "a base-only write (local=None) must NOT create phora.local.lock"
        );
    }

    #[test]
    fn load_locks_round_trips_base_only_write() {
        let dir = TempDir::new().expect("locks dir");
        let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");

        write_locks(dir.path(), &base, None).expect("write base-only locks");
        let (loaded_base, loaded_local) = load_locks(dir.path()).expect("load locks");

        let loaded_base = loaded_base.expect("phora.lock present after a base write");
        assert!(
            loaded_local.is_none(),
            "no phora.local.lock on disk must load as None"
        );
        let src = loaded_base
            .find_source("dotfiles")
            .expect("the base source survives the round-trip");
        assert_eq!(
            src.git, "https://github.com/me/dotfiles.git",
            "round-tripped base lock must echo the source git URL"
        );
        assert_eq!(
            src.resolved, "main",
            "round-tripped base lock must echo the resolved refspec"
        );
        assert_eq!(
            loaded_base.sources.len(),
            1,
            "exactly the one written source must come back"
        );
    }

    #[test]
    fn write_then_load_locks_round_trips_base_and_local() {
        let dir = TempDir::new().expect("locks dir");
        let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");
        let local = lock_with("loqui", "/home/soeren/dev/loqui", "dev");

        write_locks(dir.path(), &base, Some(&local)).expect("write base+local locks");

        assert!(
            dir.path().join("phora.lock").is_file(),
            "phora.lock must exist"
        );
        assert!(
            dir.path().join("phora.local.lock").is_file(),
            "a Some(local) write must create phora.local.lock"
        );

        let (loaded_base, loaded_local) = load_locks(dir.path()).expect("load both locks");
        assert!(
            loaded_base
                .expect("base present")
                .find_source("dotfiles")
                .is_some(),
            "base lock must round-trip its source"
        );
        let local = loaded_local.expect("local lock present when phora.local.lock exists");
        let loqui = local
            .find_source("loqui")
            .expect("local lock must round-trip its overridden source");
        assert_eq!(
            loqui.git, "/home/soeren/dev/loqui",
            "round-tripped local lock must echo the local checkout path"
        );
    }

    #[test]
    fn write_locks_removes_stale_local_lock_when_local_is_none() {
        let dir = TempDir::new().expect("locks dir");
        let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");
        let local = lock_with("loqui", "/home/soeren/dev/loqui", "dev");

        write_locks(dir.path(), &base, Some(&local)).expect("seed both locks");
        assert!(
            dir.path().join("phora.local.lock").is_file(),
            "premise: phora.local.lock must exist before the base-only rewrite"
        );

        write_locks(dir.path(), &base, None).expect("rewrite base-only");

        assert!(
            !dir.path().join("phora.local.lock").exists(),
            "a base-only rewrite (local=None) must DELETE the stale phora.local.lock"
        );
        let (_, loaded_local) = load_locks(dir.path()).expect("reload after stale removal");
        assert!(
            loaded_local.is_none(),
            "after the stale local lock is removed, load_locks must report no local lock"
        );
    }

    // ── list_statuses ──────────────────────────────────────────────

    /// Writes `file` with `content` under `<target_dir>/<artifact>/` and returns a
    /// [`ManifestFile`] whose size+mtime match what landed on disk, so a record built
    /// from it reads Clean through `check_artifact_state`.
    fn deploy_matching_file(
        target_dir: &Path,
        artifact: &str,
        file: &str,
        content: &[u8],
    ) -> ManifestFile {
        let artifact_dir = target_dir.join(artifact);
        std::fs::create_dir_all(&artifact_dir).expect("create artifact dir");
        let path = artifact_dir.join(file);
        std::fs::write(&path, content).expect("write deployed file");
        let meta = std::fs::metadata(&path).expect("stat deployed file");
        let mtime = meta
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after epoch")
            .as_secs();
        ManifestFile {
            path: PathBuf::from(file),
            size: meta.len(),
            mtime,
            blake3: blake3::hash(content).to_hex().to_string(),
        }
    }

    fn record_for(
        target: &str,
        source: &str,
        artifact: &str,
        commit: &str,
        files: Vec<ManifestFile>,
    ) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: target.to_owned(),
                source: source.to_owned(),
                artifact: artifact.to_owned(),
            },
            commit: commit.to_owned(),
            digest: "blake3:rec".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files,
            linked: false,
        }
    }

    fn config_one_flat_target(target: &str, source: &str, target_path: &Path) -> Config {
        let toml = format!(
            "version = 1\n\n\
             [sources.{source}]\ngit = \"https://example.com/x.git\"\nbranch = \"main\"\n\n\
             [targets.{target}]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"flat\"\n",
            target_path.display(),
        );
        Config::parse(&toml).expect("one-target flat config parses")
    }

    fn config_two_flat_targets(
        target_a: &str,
        source_a: &str,
        path_a: &Path,
        target_b: &str,
        source_b: &str,
        path_b: &Path,
    ) -> Config {
        let toml = format!(
            "version = 1\n\n\
             [sources.{source_a}]\ngit = \"https://example.com/a.git\"\nbranch = \"main\"\n\n\
             [sources.{source_b}]\ngit = \"https://example.com/b.git\"\nbranch = \"main\"\n\n\
             [targets.{target_a}]\npath = \"{}\"\nsources = [\"{source_a}\"]\nlayout = \"flat\"\n\n\
             [targets.{target_b}]\npath = \"{}\"\nsources = [\"{source_b}\"]\nlayout = \"flat\"\n",
            path_a.display(),
            path_b.display(),
        );
        Config::parse(&toml).expect("two-target flat config parses")
    }

    fn status_for<'a>(
        listings: &'a [TargetListing],
        target: &str,
        artifact: &str,
    ) -> Option<&'a ArtifactStatus> {
        listings
            .iter()
            .find(|l| l.target == target)
            .and_then(|l| l.artifacts.iter().find(|a| a.artifact == artifact))
    }

    #[test]
    fn list_statuses_reports_clean_for_matching_deployment() {
        let state_dir = TempDir::new().expect("state root");
        let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
        let target_root = TempDir::new().expect("target root");
        let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

        let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
        reg.put(&record_for(
            "dest",
            "editor-src",
            "editor",
            "aaa111",
            vec![mf],
        ))
        .expect("seed registry record");

        let listings = list_statuses(&cfg, &reg).expect("list statuses");

        let st = status_for(&listings, "dest", "editor")
            .expect("the editor artifact must appear under target dest");
        assert_eq!(
            st.source, "editor-src",
            "the status row must carry the artifact's source"
        );
        assert!(
            st.state.contains('✓') || st.state.to_lowercase().contains("clean"),
            "a deployment whose files match its record must read Clean (✓), got state {:?}",
            st.state
        );
        assert!(
            !st.state.to_lowercase().contains("modified"),
            "a matching deployment must NOT be labelled modified, got {:?}",
            st.state
        );
    }

    #[test]
    fn list_statuses_reports_modified_for_edited_deployment() {
        let state_dir = TempDir::new().expect("state root");
        let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
        let target_root = TempDir::new().expect("target root");
        let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

        let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
        reg.put(&record_for(
            "dest",
            "editor-src",
            "editor",
            "aaa111",
            vec![mf],
        ))
        .expect("seed an accurate (would-be-Clean) registry record");

        // Record stays accurate; the deployed file drifts on disk (real user edit).
        std::fs::write(
            target_root.path().join("editor").join("init.lua"),
            b"-- init\nvim.opt.number = true\n",
        )
        .expect("edit deployed file on disk");

        let listings = list_statuses(&cfg, &reg).expect("list statuses");

        let st = status_for(&listings, "dest", "editor")
            .expect("the editor artifact must appear even when modified");
        assert!(
            st.state.to_lowercase().contains("modified"),
            "a deployment whose on-disk file differs from its record must read Modified, got {:?}",
            st.state
        );
        assert!(
            !st.state.contains('✓'),
            "a Modified artifact must NOT be shown as clean (✓), got {:?}",
            st.state
        );
    }

    #[test]
    fn list_statuses_reports_ejected_for_ejected_artifact() {
        let state_dir = TempDir::new().expect("state root");
        let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
        let target_root = TempDir::new().expect("target root");
        let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

        let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
        reg.put(&record_for(
            "dest",
            "editor-src",
            "editor",
            "aaa111",
            vec![mf],
        ))
        .expect("seed registry record");
        reg.save_ejected(
            "dest",
            &[crate::registry::EjectedEntry {
                source: "editor-src".to_owned(),
                artifact: "editor".to_owned(),
                ejected_at: "2026-01-31T14:00:00Z".to_owned(),
            }],
        )
        .expect("mark editor ejected");

        let listings = list_statuses(&cfg, &reg).expect("list statuses");

        let st = status_for(&listings, "dest", "editor")
            .expect("an ejected artifact must still be listed");
        assert!(
            st.state.to_lowercase().contains("ejected"),
            "an artifact in the target's ejected list must read Ejected, got {:?}",
            st.state
        );
    }

    #[test]
    fn list_statuses_groups_by_target_and_names_source_and_artifact() {
        let state_dir = TempDir::new().expect("state root");
        let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
        let root_a = TempDir::new().expect("target a root");
        let root_b = TempDir::new().expect("target b root");
        let cfg = config_two_flat_targets(
            "home",
            "editor-src",
            root_a.path(),
            "xdg",
            "snippets-src",
            root_b.path(),
        );

        let lua = deploy_matching_file(root_a.path(), "editor", "init.lua", b"-- init\n");
        reg.put(&record_for(
            "home",
            "editor-src",
            "editor",
            "aaa111",
            vec![lua],
        ))
        .expect("seed editor record under home");
        let json = deploy_matching_file(root_b.path(), "snippets", "py.json", b"{}\n");
        reg.put(&record_for(
            "xdg",
            "snippets-src",
            "snippets",
            "bbb222",
            vec![json],
        ))
        .expect("seed snippets record under xdg");

        let listings = list_statuses(&cfg, &reg).expect("list statuses");

        assert_eq!(
            listings.len(),
            2,
            "each configured target must get its own listing entry, got {listings:?}"
        );

        let home = listings
            .iter()
            .find(|l| l.target == "home")
            .expect("the home target must be present as its own grouping");
        let home_names: Vec<&str> = home.artifacts.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(
            home_names,
            vec!["editor"],
            "the home group must carry only its own editor artifact, got {home_names:?}"
        );
        assert!(
            home.artifacts.iter().all(|a| a.source == "editor-src"),
            "every row in the home group must name the home source, got {:?}",
            home.artifacts
        );

        let xdg = listings
            .iter()
            .find(|l| l.target == "xdg")
            .expect("the xdg target must be present as its own grouping");
        let xdg_names: Vec<&str> = xdg.artifacts.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(
            xdg_names,
            vec!["snippets"],
            "the xdg group must carry only its own snippets artifact, got {xdg_names:?}"
        );
        assert!(
            xdg.artifacts.iter().all(|a| a.source == "snippets-src"),
            "every row in the xdg group must name the xdg source, got {:?}",
            xdg.artifacts
        );

        assert!(
            !xdg_names.contains(&"editor"),
            "an artifact deployed under home must NOT leak into the xdg group, got {xdg_names:?}"
        );
        assert!(
            !home_names.contains(&"snippets"),
            "an artifact deployed under xdg must NOT leak into the home group, got {home_names:?}"
        );
    }

    // ── resolution_from_char ───────────────────────────────────────

    #[test]
    fn resolution_from_char_maps_skip() {
        assert_eq!(
            resolution_from_char('s'),
            Some(Resolution::Skip),
            "`s` must map to Skip"
        );
    }

    #[test]
    fn resolution_from_char_maps_overwrite() {
        assert_eq!(
            resolution_from_char('o'),
            Some(Resolution::Overwrite),
            "`o` must map to Overwrite"
        );
    }

    #[test]
    fn resolution_from_char_maps_eject() {
        assert_eq!(
            resolution_from_char('e'),
            Some(Resolution::Eject),
            "`e` must map to Eject"
        );
    }

    #[test]
    fn resolution_from_char_maps_abort() {
        assert_eq!(
            resolution_from_char('a'),
            Some(Resolution::Abort),
            "`a` must map to Abort"
        );
    }

    #[test]
    fn resolution_from_char_rejects_unknown() {
        assert_eq!(
            resolution_from_char('x'),
            None,
            "an unrecognized prompt character must map to None, not a default Resolution"
        );
    }
}
