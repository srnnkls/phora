//! Command-line surface.

mod add;
mod bind;
mod config_edit;
mod query;
mod render;
mod sync;
mod trust;

#[cfg(test)]
mod tests;

#[cfg(test)]
use {
    crate::config::{Host, LayoutKind},
    crate::deploy::ArtifactState,
    crate::store::Registry,
    add::{
        MissingTarget, MissingTargetDecider, add_to_default_target, add_with_binds,
        insert_source_with_ref, run_add,
    },
    render::state_label,
};

use config_edit::BindRefinement;

pub(crate) use query::PreviewSelectors;

#[cfg(test)]
pub(crate) use {
    query::{PreviewPlan, preview_plan},
    render::{render_preview_json, render_preview_tree},
};

pub use add::{AddTarget, insert_source, parse_add_url};
pub use config_edit::remove_source;
pub use query::{
    ArtifactStatus, CheckMatchReport, SourceResolution, SourceRow, SourceSummary, TargetDetail,
    TargetListing, TargetRow, WhereFilter, WhereMatch, check_match_cmd, list_statuses,
    source_listing, source_summary, target_detail, target_has_deployed_artifacts, target_listing,
    targets_receiving, where_cmd,
};
pub use sync::{load_locks, write_locks};

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;

use clap::{Parser, Subcommand};

use crate::config::{Config, ParsedSource, merge_configs};
use crate::error::{Error, Result};
use crate::kernel::{ProjectId, SourceName, TargetName};
use crate::paths::state_root_for;
use crate::source::{GitBackend, HttpBackend, RouterBackend};
use crate::store::{FileRegistry, StoreError};
use crate::sync::{Conflict, ConflictResolver, Resolution};
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(name = "phora", version, about = "Git-based artifact package manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Parse a URL, add a source, and optionally bind it to one or more targets.
    Add {
        url: String,
        #[arg(long = "to")]
        to: Vec<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// Scope the NEW source's offer to `root` (source-owned, not a binding).
        #[arg(long)]
        root: Option<String>,
        /// Keep only matching paths in the NEW source's offer (repeatable; source-owned).
        #[arg(long = "include")]
        include: Vec<String>,
        /// Drop matching paths from the NEW source's offer (repeatable; source-owned).
        #[arg(long = "exclude")]
        exclude: Vec<String>,
        #[arg(long)]
        local: bool,
        #[arg(long)]
        symlink: bool,
        #[arg(long = "as")]
        r#as: Option<String>,
    },
    /// Remove a source and scrub it from every target (alias for `source rm`).
    Rm { name: String },
    /// Fetch sources and project them to targets.
    Sync {
        #[arg(long)]
        prune: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        no_hooks: bool,
        #[arg(long)]
        no_transitive_hooks: bool,
        #[arg(long)]
        frozen: bool,
        #[arg(long, short = 'j')]
        jobs: Option<usize>,
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
    /// Manage sources (`add`, `rm`, `list`, `show`).
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Manage targets (`add`, `rm`, `list`, `show`).
    Target {
        #[command(subcommand)]
        cmd: TargetCmd,
    },
    /// Bind one or more sources to a target, optionally refining each binding.
    Bind {
        #[arg(required = true)]
        sources: Vec<String>,
        #[arg(long)]
        to: String,
        #[arg(long)]
        local: bool,
        #[arg(long = "as")]
        r#as: Option<String>,
        #[arg(long)]
        root: Option<String>,
        #[arg(long = "take")]
        take: Vec<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        rev: Option<String>,
    },
    /// Remove one or more bindings from a target by their identity.
    Unbind {
        #[arg(required = true)]
        sources: Vec<String>,
        #[arg(long)]
        from: String,
        #[arg(long)]
        local: bool,
    },
    /// Inspect and approve transitive (composed-dep) hooks before they run.
    Trust {
        source: Option<String>,
        #[arg(long)]
        list: bool,
        #[arg(long)]
        revoke: bool,
        /// Print a dep file (or list a dep directory) at the recorded commit, offline.
        #[arg(long, value_name = "PATH")]
        show: Option<String>,
    },
    /// Show an offline deployment preview from the lock.
    Preview {
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        files: bool,
        #[arg(long)]
        json: bool,
    },
    /// Attribute a path's offer/take decision under a target, offline.
    Explain {
        target: String,
        source: String,
        path: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum SourceCmd {
    /// Parse a URL and add a source to the config (same as top-level `add`).
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// Scope the NEW source's offer to `root` (source-owned, not a binding).
        #[arg(long)]
        root: Option<String>,
        /// Keep only matching paths in the NEW source's offer (repeatable; source-owned).
        #[arg(long = "include")]
        include: Vec<String>,
        /// Drop matching paths from the NEW source's offer (repeatable; source-owned).
        #[arg(long = "exclude")]
        exclude: Vec<String>,
        #[arg(long)]
        local: bool,
        #[arg(long)]
        symlink: bool,
    },
    /// Remove a source and scrub it from every target's source list.
    Rm { name: String },
    /// List sources over the merged config.
    List,
    /// Show one source's effective config and the targets that deploy it.
    Show { name: String },
}

#[derive(Subcommand, Debug)]
pub enum TargetCmd {
    /// Add a target with a path and optional layout.
    Add {
        name: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        layout: Option<String>,
        #[arg(long)]
        local: bool,
    },
    /// Remove a target.
    Rm {
        name: String,
        #[arg(long)]
        local: bool,
    },
    /// List targets over the merged config.
    List,
    /// Show one target's effective config, bound sources, and deployment state.
    Show { name: String },
}

/// 75 is `EX_TEMPFAIL` (sysexits.h): a contended lock is "busy, retry", not a hard failure.
#[must_use]
pub fn exit_code(err: &Error) -> i32 {
    match err {
        Error::StoreCtx(StoreError::Lock(_)) => 75,
        _ => 1,
    }
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        cmd @ Command::Add { .. } => dispatch_add(cmd),
        Command::Rm { name } => run_source_rm(&name),
        Command::Sync {
            prune,
            force,
            no_hooks,
            no_transitive_hooks,
            frozen,
            jobs,
        } => sync::run_sync(
            prune,
            force,
            no_hooks,
            no_transitive_hooks,
            frozen,
            None,
            jobs,
        ),
        Command::Update { source } => sync::run_update(source.as_deref()),
        Command::List { plan } => query::run_list(plan),
        Command::Verify => run_verify(),
        Command::Where {
            digest,
            source,
            artifact,
            commit,
        } => {
            let filter = WhereFilter {
                digest,
                source,
                artifact,
                commit,
            };
            let config = load_config()?;
            let matches = where_cmd(&open_project_registry(&config)?, &filter)?;
            render::print_where_matches(&matches, &filter);
            Ok(())
        }
        Command::Eject {
            artifact,
            source,
            target,
        } => {
            let config = load_config()?;
            let registry = open_project_registry(&config)?;
            let _guard = registry.lock_exclusive()?;
            crate::sync::eject(&config, &registry, &artifact, &source, &target)?;
            println!("ejected {source}/{artifact} from {target} (files kept)");
            Ok(())
        }
        Command::Uneject {
            artifact,
            source,
            target,
        } => {
            let config = load_config()?;
            let registry = open_project_registry(&config)?;
            let _guard = registry.lock_exclusive()?;
            crate::sync::uneject(&config, &registry, &artifact, &source, &target)?;
            println!("unejected {source}/{artifact} in {target}");
            Ok(())
        }
        Command::RebuildRegistry => sync::run_rebuild_registry(),
        Command::CheckMatch { source, path } => {
            let source = load_source(&source)?;
            let report = check_match_cmd(&source, &path);
            render::print_check_match(&source, &path, &report);
            Ok(())
        }
        Command::Source { cmd } => run_source(cmd),
        Command::Target { cmd } => run_target(cmd),
        cmd @ Command::Bind { .. } => dispatch_bind(cmd),
        Command::Unbind {
            sources,
            from,
            local,
        } => bind::run_unbind(&sources, &from, local),
        cmd @ Command::Trust { .. } => dispatch_trust(cmd),
        Command::Preview {
            source,
            target,
            files,
            json,
        } => query::run_preview(
            &PreviewSelectors {
                source,
                target,
                files,
            },
            json,
        ),
        Command::Explain {
            target,
            source,
            path,
        } => query::run_explain(&target, &source, path.as_deref()),
    }
}

fn dispatch_add(cmd: Command) -> Result<()> {
    let Command::Add {
        url,
        to,
        name,
        branch,
        tag,
        root,
        include,
        exclude,
        local,
        symlink,
        r#as,
    } = cmd
    else {
        unreachable!("dispatch_add only handles Command::Add")
    };
    let refinement = BindRefinement {
        r#as,
        ..BindRefinement::default()
    };
    add::run_add(
        &url,
        &to,
        name,
        branch,
        tag,
        root,
        include,
        exclude,
        local,
        symlink,
        &refinement,
    )
}

fn run_verify() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let mut config = crate::config::merge_configs(base, local);

    let registry = open_project_registry(&config)?;
    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = base_lock.map_or_else(
        || local_lock.clone(),
        |base| Some(crate::lock::merge_locks(&base, local_lock.as_ref())),
    );

    let cache_git = crate::paths::cache_root_for(config.paths.cache.as_deref(), &cwd)?.join("git");
    let backend = build_router(&config, cache_git)?;
    let mut parsed = config.parsed_sources()?;
    let mut remotes = crate::sync::resolved_remotes(&config, &parsed)?;
    crate::sync::inject_composed_graph(
        &mut config,
        &mut parsed,
        &mut remotes,
        &backend,
        lock.as_ref(),
    );

    let report = crate::sync::verify(&config, &registry, lock.as_ref())?;
    render::print_verify(&report);
    if report.is_clean() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn dispatch_trust(cmd: Command) -> Result<()> {
    let Command::Trust {
        source,
        list,
        revoke,
        show,
    } = cmd
    else {
        unreachable!("dispatch_trust only handles Command::Trust")
    };
    trust::run_trust(source.as_deref(), list, revoke, show.as_deref())
}

fn dispatch_bind(cmd: Command) -> Result<()> {
    let Command::Bind {
        sources,
        to,
        local,
        r#as,
        root,
        take,
        branch,
        tag,
        rev,
    } = cmd
    else {
        unreachable!("dispatch_bind only handles Command::Bind")
    };
    bind::run_bind(
        &sources,
        &to,
        local,
        &BindRefinement {
            r#as,
            root,
            branch,
            tag,
            rev,
            take: take
                .iter()
                .map(|t| config_edit::TakeArg::parse(t))
                .collect(),
        },
    )
}

fn run_source(cmd: SourceCmd) -> Result<()> {
    match cmd {
        SourceCmd::Add {
            url,
            name,
            branch,
            tag,
            root,
            include,
            exclude,
            local,
            symlink,
        } => add::run_add(
            &url,
            &[],
            name,
            branch,
            tag,
            root,
            include,
            exclude,
            local,
            symlink,
            &BindRefinement::default(),
        ),
        SourceCmd::Rm { name } => run_source_rm(&name),
        SourceCmd::List => {
            render::print_source_rows(&source_listing(&load_config()?)?);
            Ok(())
        }
        SourceCmd::Show { name } => {
            render::print_source_summary(&source_summary(&load_config()?, &name)?);
            Ok(())
        }
    }
}

fn read_config_text(path: &str) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("version = 1\n".to_owned()),
        Err(e) => Err(Error::Config(format!("read {path}: {e}"))),
    }
}

fn run_source_rm(name: &str) -> Result<()> {
    let main = read_config_text("phora.toml")?;
    let local = read_config_text("phora.local.toml")?;
    let result = config_edit::remove_source(&main, &local, name)?;
    if result.main != main {
        std::fs::write("phora.toml", &result.main)?;
    }
    if result.local != local {
        std::fs::write("phora.local.toml", &result.local)?;
    }
    render::print_source_removed(name);
    Ok(())
}

fn run_target(cmd: TargetCmd) -> Result<()> {
    match cmd {
        TargetCmd::Add {
            name,
            path,
            layout,
            local,
        } => run_target_add(&name, &path, layout.as_deref(), local),
        TargetCmd::Rm { name, local } => run_target_rm(&name, local),
        TargetCmd::List => {
            render::print_target_rows(&target_listing(&load_config()?));
            Ok(())
        }
        TargetCmd::Show { name } => {
            let config = load_config()?;
            render::print_target_detail(&target_detail(
                &config,
                &open_project_registry(&config)?,
                &name,
            )?);
            Ok(())
        }
    }
}

fn target_config_file(local: bool) -> &'static str {
    if local {
        "phora.local.toml"
    } else {
        "phora.toml"
    }
}

fn run_target_add(name: &str, path: &str, layout: Option<&str>, local: bool) -> Result<()> {
    TargetName::from_str(name)?;
    let file = target_config_file(local);
    let text = read_config_text(file)?;
    let updated = config_edit::upsert_target(&text, name, path, layout)?;
    std::fs::write(file, &updated)?;
    render::print_target_added(name, path);
    Ok(())
}

fn run_target_rm(name: &str, local: bool) -> Result<()> {
    if target_has_deployed_artifacts(&open_project_registry(&load_config()?)?, name)? {
        render::warn_target_rm_deployed(name);
    }
    let file = target_config_file(local);
    let text = read_config_text(file)?;
    let updated = config_edit::remove_target(&text, name)?;
    std::fs::write(file, &updated)?;
    render::print_target_removed(name);
    Ok(())
}

/// Builds the mode-aware router for `config`, parsing each url source's `digest`.
fn build_router(
    config: &Config,
    git_dir: std::path::PathBuf,
) -> Result<RouterBackend<GitBackend, HttpBackend>> {
    std::fs::create_dir_all(&git_dir)
        .map_err(|e| Error::Config(format!("create mirror dir {}: {e}", git_dir.display())))?;
    let mut modes = BTreeMap::new();
    let mut digests = BTreeMap::new();
    for (name, source) in &config.parsed_sources()? {
        let source_name = SourceName::trusted(name.clone());
        if let Some(digest) = source.digest() {
            digests.insert(source_name.clone(), digest);
        }
        modes.insert(source_name, source.mode());
    }
    let git = GitBackend::new(git_dir.clone());
    let http = HttpBackend::new(git_dir, digests);
    Ok(RouterBackend::new(git, http, modes))
}

/// Which locked source entries to drop before a sync so they get re-resolved.
enum DropSources {
    All,
    One(String),
}

fn drop_sources(lock: Option<&mut crate::lock::Lock>, drop: &DropSources) {
    let Some(lock) = lock else { return };
    match drop {
        DropSources::All => lock.sources.clear(),
        // Drop the named consumer source plus every transitive node, forcing the subtree to re-resolve.
        DropSources::One(name) => lock
            .sources
            .retain(|s| &s.name != name && s.instance.is_none()),
    }
}

fn load_local_config(cwd: &Path) -> Result<Option<Config>> {
    let path = cwd.join("phora.local.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let config = Config::parse(&text)?;
            for warning in config.migration_warnings(cwd) {
                eprintln!("phora: phora.local.toml: {warning}");
            }
            Ok(Some(config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Config(format!("read {}: {e}", path.display()))),
    }
}

/// Effective `[defaults] auto_target` over the merged base+local config, or `true`
/// when no `phora.toml` exists yet.
pub(super) fn effective_auto_target() -> bool {
    let cwd = Path::new(".");
    let Ok(base) = load_config_from(cwd) else {
        return true;
    };
    let local = load_local_config(cwd).ok().flatten();
    merge_configs(base, local).defaults.auto_target()
}

/// On a TTY, prompts on stderr to create a missing `--to` target and reads a path
/// from stdin (empty line keeps the default); off a TTY, rejects.
pub(super) struct TtyMissingTarget;

impl add::MissingTargetDecider for TtyMissingTarget {
    fn decide(&self, name: &str, default_path: &str) -> add::MissingTarget {
        if !std::io::stdin().is_terminal() {
            return add::MissingTarget::Reject;
        }
        eprint!("phora: target '{name}' does not exist — create it at [{default_path}]? ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => add::MissingTarget::Reject,
            Ok(_) => {
                let typed = line.trim();
                let path = if typed.is_empty() {
                    default_path.to_owned()
                } else {
                    typed.to_owned()
                };
                add::MissingTarget::Create { path }
            }
        }
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

fn open_project_registry(config: &Config) -> Result<FileRegistry> {
    let cwd = std::env::current_dir()?;
    let project = ProjectId::for_path(&cwd)?;
    let registry_root = state_root_for(config.paths.state.as_deref(), &cwd)?
        .join("projects")
        .join(project.as_str());
    Ok(FileRegistry::open(registry_root)?)
}

fn load_config() -> Result<Config> {
    load_config_from(Path::new("."))
}

fn load_config_from(dir: &Path) -> Result<Config> {
    let path = dir.join("phora.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Config(format!("read {}: {e}", path.display())))?;
    let config = Config::parse(&text)?;
    for warning in config.migration_warnings(dir) {
        eprintln!("phora: {warning}");
    }
    Ok(config)
}

fn load_source(name: &str) -> Result<ParsedSource> {
    let source = load_config()?
        .sources
        .remove(name)
        .ok_or_else(|| Error::Config(format!("source `{name}` not found in phora.toml")))?;
    ParsedSource::parse(name, &source)
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
