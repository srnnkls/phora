//! Mutating commands: `sync`, `update`, `rebuild-registry`, and lock I/O.

use std::io::IsTerminal;
use std::num::NonZeroUsize;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::kernel::ProjectId;
use crate::lock::{Lock, merge_locks};
use crate::paths::{cache_root_for, state_root_for};
use crate::store::{FileRegistry, StoreError};
use crate::sync::{
    Concurrency, ConflictPolicy, ConflictResolver, HookPolicy, LockSet, MovedPinPolicy,
    PrunePolicy, SkippedChange, SourcePolicy, SyncOptions, SyncReport, SyncRequest, SyncWarning,
    sync_opened,
};

use super::{
    DropSources, TtyResolver, build_router, drop_sources, load_config, load_local_config,
    open_project_registry,
};

fn open_sync_registry(cwd: &Path, config: &Config) -> Result<FileRegistry> {
    let projects_base = state_root_for(config.paths.state.as_deref(), cwd)?.join("projects");
    let project = ProjectId::for_path(cwd)?;
    Ok(FileRegistry::open(projects_base.join(project.as_str()))?)
}

#[expect(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    reason = "independent CLI run flags, not a state machine"
)]
pub(super) fn run_sync(
    prune: bool,
    force: bool,
    no_hooks: bool,
    no_transitive_hooks: bool,
    frozen: bool,
    fast_forward: bool,
    drop: Option<DropSources>,
    jobs: Option<usize>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let (mut base_lock, mut local_lock) = load_locks(&cwd)?;

    if let Some(drop) = drop {
        drop_sources(base_lock.as_mut(), &drop);
        drop_sources(local_lock.as_mut(), &drop);
    }

    let effective = crate::config::merge_configs(base.clone(), local.clone());
    let cache_git = cache_root_for(effective.paths.cache.as_deref(), &cwd)?.join("git");
    let backend = build_router(&effective, cache_git)?;
    let registry = open_sync_registry(&cwd, &effective)?;
    // Only a read-only root under --frozen falls back lockless; contention (exit 75) still propagates.
    let guard = match registry.lock_exclusive() {
        Ok(guard) => Some(guard),
        Err(StoreError::ReadOnly(_)) if frozen => None,
        Err(e) => return Err(e.into()),
    };
    let lockless = guard.is_none();
    let _guard = guard;
    if !lockless && let Some(advisory) = registry.lock_advisory() {
        eprintln!("{advisory}");
    }
    let interactive = std::io::stdin().is_terminal();
    let resolver = TtyResolver;

    let source_policy = if frozen {
        SourcePolicy::Frozen
    } else if force {
        SourcePolicy::Refresh
    } else {
        SourcePolicy::Locked
    };
    let conflict_policy = if force {
        ConflictPolicy::Overwrite
    } else if interactive {
        ConflictPolicy::ResolveInteractively
    } else {
        ConflictPolicy::Refuse
    };
    let hook_policy = if no_hooks {
        HookPolicy::None
    } else if no_transitive_hooks {
        HookPolicy::NoTransitive
    } else {
        HookPolicy::All
    };
    let jobs = jobs
        .map(|jobs| {
            NonZeroUsize::new(jobs)
                .ok_or_else(|| Error::Config("--jobs must be greater than zero".to_owned()))
        })
        .transpose()?;
    let request = SyncRequest {
        base_config: &base,
        local_config: local.as_ref(),
        locks: LockSet {
            base: base_lock,
            local: local_lock,
        },
        options: SyncOptions {
            source_policy,
            conflict_policy,
            prune_policy: if prune {
                PrunePolicy::RemoveOrphans
            } else {
                PrunePolicy::KeepOrphans
            },
            hook_policy,
            moved_pin_policy: if fast_forward {
                MovedPinPolicy::FastForward
            } else {
                MovedPinPolicy::Seal
            },
            concurrency: Concurrency { jobs },
        },
        resolver: interactive.then_some(&resolver as &dyn ConflictResolver),
    };
    let out = sync_opened(&request, &backend, &registry, lockless)?;

    finish_sync(&cwd, &out, interactive)
}

struct StrippedHookNotice {
    message: String,
    fail: bool,
}

fn stripped_hook_notice(stripped: usize, interactive: bool) -> Option<StrippedHookNotice> {
    (stripped > 0).then(|| StrippedHookNotice {
        message: format!(
            "phora: {stripped} untrusted transitive hook(s) were stripped and not run — affected \
             artifacts are deployed but NOT post-processed and may be incomplete\n\
             phora: run `phora trust <name>` to inspect and approve {stripped} hook(s)"
        ),
        fail: interactive,
    })
}

fn finish_sync(cwd: &Path, out: &SyncReport, interactive: bool) -> Result<()> {
    let base_lock = out
        .locks
        .base
        .as_ref()
        .ok_or_else(|| Error::Lock("sync completed without a base lock".to_owned()))?;
    write_locks(cwd, base_lock, out.locks.local.as_ref())?;
    render_sync_warnings(out);
    for applied in &out.applied {
        if let crate::sync::AppliedChange::Removed {
            source, artifact, ..
        } = applied
        {
            eprintln!("phora: pruning orphaned {source}:{artifact}");
        }
    }
    for skipped in &out.skipped {
        if let SkippedChange::Failed {
            source,
            artifact,
            message,
            ..
        } = skipped
        {
            eprintln!("phora: failed to deploy {source}:{artifact}: {message}");
        }
    }
    let report = super::render::render_hook_report(&out.hook_outcomes);
    let stripped = out
        .warnings
        .iter()
        .find_map(|warning| match warning {
            SyncWarning::UntrustedTransitiveHooks { count } => Some(*count),
            _ => None,
        })
        .unwrap_or(0);
    if let Some(notice) = stripped_hook_notice(stripped, interactive) {
        if !report.is_empty() {
            eprint!("{report}");
        }
        eprintln!("{}", notice.message);
        if notice.fail {
            std::process::exit(1);
        }
    }
    if out.status == crate::sync::SyncStatus::Failed {
        if !report.is_empty() {
            eprint!("{report}");
        }
        let hooks_failed = out
            .hook_outcomes
            .iter()
            .any(|o| o.status == crate::sync::HookStatus::Failure);
        let deploy_failures = out
            .skipped
            .iter()
            .any(|skipped| matches!(skipped, SkippedChange::Failed { .. }));
        let message = match (deploy_failures, hooks_failed) {
            (true, true) => "phora: some artifacts failed to deploy and one or more hooks failed",
            (true, false) => "phora: some artifacts failed to deploy",
            (false, _) => "phora: one or more hooks failed",
        };
        eprintln!("{message}");
        std::process::exit(1);
    }
    if !report.is_empty() {
        print!("{report}");
    }
    println!("sync complete");
    Ok(())
}

fn render_sync_warnings(out: &SyncReport) {
    for warning in &out.warnings {
        match warning {
            SyncWarning::Projection(
                crate::projection::diagnostic::ProjectionWarning::TakeNoMatchGlob(pattern),
            ) => eprintln!("phora: take pattern matched no offered leaf: {pattern}"),
            SyncWarning::Projection(
                crate::projection::diagnostic::ProjectionWarning::LostCollapseToExclude(dir),
            ) => eprintln!(
                "phora: dir `{dir}` cannot collapse to one symlink under a within-dir exclude; \
                 falling back to per-leaf links"
            ),
            SyncWarning::Message(message) => eprintln!("phora: {message}"),
            SyncWarning::ConflictModified {
                source,
                artifact,
                changed,
            } => {
                eprintln!("phora: skipping locally modified {source}:{artifact}");
                for path in changed {
                    eprintln!("    {}", path.display());
                }
                eprintln!("  use --force to overwrite");
            }
            SyncWarning::ConflictForeign { path } => eprintln!(
                "phora: skipping foreign content at {}; use --force to overwrite",
                path.display()
            ),
            SyncWarning::UntrustedTransitiveHooks { .. } => {}
        }
    }
}

pub(super) fn run_rebuild_registry() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = load_config()?;
    let local = load_local_config(&cwd)?;
    let mut config = crate::config::merge_configs(base, local);
    config.validate()?;

    let registry = open_project_registry(&config)?;
    let _guard = registry.lock_exclusive()?;

    let (base_lock, local_lock) = load_locks(&cwd)?;
    let lock = match base_lock {
        Some(base) => merge_locks(&base, local_lock.as_ref()),
        None => local_lock
            .ok_or_else(|| Error::Lock("no lock file found; run sync first".to_owned()))?,
    };

    let cache_git = cache_root_for(config.paths.cache.as_deref(), &cwd)?.join("git");
    let backend = build_router(&config, cache_git)?;
    let mut parsed = config.parsed_sources()?;
    let mut remotes = crate::sync::resolved_remotes(&config, &parsed)?;
    crate::sync::inject_composed_graph(
        &mut config,
        &mut parsed,
        &mut remotes,
        &backend,
        Some(&lock),
    );
    let report =
        crate::sync::rebuild_registry_with(&config, &parsed, &remotes, &lock, &backend, &registry)?;

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

pub(super) fn run_update(source: Option<&str>, fast_forward: bool) -> Result<()> {
    let drop = source.map_or(DropSources::All, |s| DropSources::One(s.to_owned()));
    run_sync(
        false,
        false,
        false,
        false,
        false,
        fast_forward,
        Some(drop),
        None,
    )
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

#[cfg(test)]
mod tests {
    use super::stripped_hook_notice;

    #[test]
    fn no_stripped_hooks_yields_no_notice() {
        assert!(stripped_hook_notice(0, true).is_none());
        assert!(stripped_hook_notice(0, false).is_none());
    }

    #[test]
    fn stripped_hooks_under_a_tty_fail_the_command() {
        let notice = stripped_hook_notice(2, true).expect("a notice when hooks are stripped");
        assert!(
            notice.fail,
            "a TTY sync must fail so a human acts on the stripped hooks"
        );
        assert!(notice.message.contains("phora trust") && notice.message.contains("approve"));
        assert!(notice.message.contains("incomplete"));
    }

    #[test]
    fn stripped_hooks_under_non_tty_surface_but_do_not_fail() {
        let notice = stripped_hook_notice(1, false).expect("a notice when hooks are stripped");
        assert!(
            !notice.fail,
            "non-TTY/CI must stay green; the gap is surfaced, not fatal"
        );
        assert!(notice.message.contains("phora trust") && notice.message.contains("approve"));
    }
}
