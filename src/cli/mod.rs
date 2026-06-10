//! Command-line surface.

mod add;
mod query;
mod render;
mod sync;

#[cfg(test)]
mod tests;

#[cfg(test)]
use {
    crate::config::Host,
    crate::deploy::ArtifactState,
    crate::store::Registry,
    add::{insert_source_with_ref, run_add},
    render::state_label,
};

pub use add::{AddTarget, insert_source, parse_add_url};
pub use query::{
    ArtifactStatus, CheckMatchReport, TargetListing, WhereFilter, WhereMatch, check_match_cmd,
    list_statuses, where_cmd,
};
pub use sync::{load_locks, write_locks};

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use clap::{Parser, Subcommand};

use crate::config::{Config, ParsedSource};
use crate::error::{Error, Result};
use crate::kernel::{ProjectId, SourceName};
use crate::paths::phora_dir;
use crate::source::{GitBackend, HttpBackend, RouterBackend};
use crate::store::FileRegistry;
use crate::sync::{Conflict, ConflictResolver, Resolution};

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
        #[arg(long)]
        local: bool,
        #[arg(long)]
        symlink: bool,
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

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Add {
            url,
            name,
            branch,
            tag,
            root,
            local,
            symlink,
        } => add::run_add(&url, name, branch, tag, root, local, symlink),
        Command::Sync { prune, force } => sync::run_sync(prune, force, None),
        Command::Update { source } => sync::run_update(source.as_deref()),
        Command::List { plan } => query::run_list(plan),
        Command::Verify => {
            let config = load_config()?;
            let mismatches = crate::sync::verify(&config, &open_project_registry()?)?;
            render::print_verify(&mismatches);
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
            render::print_where_matches(&matches);
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
        Command::RebuildRegistry => sync::run_rebuild_registry(),
        Command::CheckMatch { source, path } => {
            let source = load_source(&source)?;
            let report = check_match_cmd(&source, &path);
            render::print_check_match(&source, &path, &report);
            Ok(())
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
        DropSources::One(name) => lock.sources.retain(|s| &s.name != name),
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
    Ok(FileRegistry::open(state_root)?)
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
