//! `phora cache`: upkeep of the shared cache root.

use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::config::Paths;
use crate::error::Result;
use crate::paths::{cache_root_for, state_root_for};
use crate::sync::content::{self, PruneReport};

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
            let report = content::prune(&store, &projects, dry_run)?;
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
        .and_then(|(store, projects)| content::auto_prune(&store, &projects));
    match pruned {
        Ok(Some(report)) if !report.removed.is_empty() => {
            eprintln!("phora: {}", summary(&report, false));
        }
        Ok(_) => {}
        Err(error) => eprintln!("phora: content store prune skipped: {error}"),
    }
}

/// Outside a project the default roots apply, so the shared store can be pruned from anywhere.
fn project_paths(cwd: &Path) -> Result<Paths> {
    if !cwd.join("phora.toml").exists() {
        return Ok(Paths::default());
    }
    let base = super::load_config()?;
    let local = super::load_local_config(cwd)?;
    Ok(crate::config::merge_configs(base, local).paths)
}

fn store_roots(paths: &Paths, cwd: &Path) -> Result<(PathBuf, PathBuf)> {
    Ok((
        cache_root_for(paths.cache.as_deref(), cwd)?.join("content"),
        state_root_for(paths.state.as_deref(), cwd)?.join("projects"),
    ))
}

fn summary(report: &PruneReport, dry_run: bool) -> String {
    let verb = if dry_run { "would prune" } else { "pruned" };
    format!(
        "{verb} {} content-store object(s), {} bytes",
        report.removed.len(),
        report.bytes
    )
}
