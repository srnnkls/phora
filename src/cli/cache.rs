//! `phora cache`: upkeep of the shared cache root.

use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::config::Paths;
use crate::error::Result;
use crate::paths::{cache_root_for, state_root_for};
use crate::sync::content::{self, PruneReport};
use crate::sync::state::lifecycle;

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum CacheCmd {
    /// Remove content-store objects no recorded deployment uses.
    Prune {
        /// List what would be removed without removing it.
        #[arg(long, short = 'n')]
        dry_run: bool,
    },
}

pub(super) fn run_cache(cmd: CacheCmd) -> Result<()> {
    match cmd {
        CacheCmd::Prune { dry_run } => {
            let cwd = std::env::current_dir()?;
            let paths = project_paths(&cwd)?;
            let (store, projects) = store_roots(&paths, &cwd)?;
            let report = content::prune_roots(&store, &projects, dry_run)?;
            let verb = if dry_run { "would remove" } else { "removed" };
            for path in &report.removed {
                println!("{verb} {}", path.display());
            }
            eprintln!("phora: {}", summary(&report, dry_run));
            Ok(())
        }
    }
}

/// Prunes the content store after a sync once a week; a failure never fails the sync.
pub(super) fn auto_prune(paths: &Paths, cwd: &Path) {
    let pruned = store_roots(paths, cwd)
        .and_then(|(store, projects)| content::auto_prune_roots(&store, &projects));
    match pruned {
        Ok(Some(report)) if !report.removed.is_empty() => {
            eprintln!("phora: {}", summary(&report, false));
        }
        Ok(_) => {}
        Err(error) => eprintln!("phora: content store prune skipped: {error}"),
    }
}

/// Outside a project the default roots apply, so the shared store can be pruned from anywhere.
pub(super) fn project_paths(cwd: &Path) -> Result<Paths> {
    if !cwd.join("phora.toml").exists() {
        return Ok(Paths::default());
    }
    let base = super::load_config()?;
    let local = super::load_local_config(cwd)?;
    Ok(crate::config::merge_configs(base, local).paths)
}

/// The content store and its GC roots: the project's own state root, the default one and
/// every registered one.
pub(super) fn store_roots(paths: &Paths, cwd: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let own = state_root_for(paths.state.as_deref(), cwd)?.join("projects");
    let mut projects = lifecycle::projects_dirs(&super::lexical_state_root(None, cwd)?);
    if !projects.contains(&own) {
        projects.push(own);
    }
    Ok((
        cache_root_for(paths.cache.as_deref(), cwd)?.join("content"),
        projects,
    ))
}

pub(super) fn summary(report: &PruneReport, dry_run: bool) -> String {
    let verb = if dry_run { "would prune" } else { "pruned" };
    format!(
        "{verb} {} content-store object(s), {} bytes",
        report.removed.len(),
        report.bytes
    )
}
