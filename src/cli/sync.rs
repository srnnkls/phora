//! Mutating commands: `sync`, `update`, `rebuild-registry`, and lock I/O.

use std::io::IsTerminal;
use std::path::Path;

use crate::error::{Error, Result};
use crate::lock::{Lock, merge_locks};
use crate::paths::cache_root;
use crate::sync::{ConflictResolver, SyncInput, SyncOutput, sync};

use super::{
    DropSources, TtyResolver, build_router, drop_sources, load_config, load_local_config,
    open_project_registry,
};

pub(super) fn run_sync(prune: bool, force: bool, drop: Option<DropSources>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let (mut base_lock, mut local_lock) = load_locks(&cwd)?;

    if let Some(drop) = drop {
        drop_sources(base_lock.as_mut(), &drop);
        drop_sources(local_lock.as_mut(), &drop);
    }

    let effective = crate::config::merge_configs(base.clone(), local.clone());
    let backend = build_router(&effective, cache_root()?.join("git"))?;
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

pub(super) fn run_rebuild_registry() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = load_config()?;
    config.validate()?;
    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = match base_lock {
        Some(base) => merge_locks(&base, local_lock.as_ref()),
        None => local_lock
            .ok_or_else(|| Error::Lock("no lock file found; run sync first".to_owned()))?,
    };

    let backend = build_router(&config, cache_root()?.join("git"))?;
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

pub(super) fn run_update(source: Option<&str>) -> Result<()> {
    let drop = source.map_or(DropSources::All, |s| DropSources::One(s.to_owned()));
    run_sync(false, false, Some(drop))
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
