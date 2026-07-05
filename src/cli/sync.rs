//! Mutating commands: `sync`, `update`, `rebuild-registry`, and lock I/O.

use std::io::IsTerminal;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::kernel::ProjectId;
use crate::lock::{Lock, merge_locks};
use crate::paths::{cache_root_for, state_root_for};
use crate::store::{FileRegistry, StoreError};
use crate::sync::{ConflictResolver, SyncInput, SyncOutput, sync};

use super::{
    DropSources, TtyResolver, build_router, drop_sources, load_config, load_local_config,
    open_project_registry,
};

/// Open the per-clone registry, generating `.phora-id` and adopting a legacy
/// path-hash registry when the file is absent. Generation is confined to this
/// sync path; read-only commands resolve identity without ever writing it.
fn open_sync_registry(cwd: &Path, config: &Config) -> Result<FileRegistry> {
    let projects_base = state_root_for(config.paths.state.as_deref(), cwd)?.join("projects");
    let project = if let Some(id) = ProjectId::read_identity_file(cwd)? {
        id
    } else {
        let generated = ProjectId::generate()?;
        let legacy = ProjectId::for_path(cwd)?;
        let adopted = crate::store::adopt_registry_dir(
            &projects_base.join(legacy.as_str()),
            &projects_base,
            generated.as_str(),
        )?;
        let project = ProjectId::from_raw(adopted);
        project.write_identity_file(cwd)?;
        project
    };
    if let Err(e) = crate::kernel::exclude_identity_from_git(cwd) {
        eprintln!(
            "phora: could not exclude {} from git ({e}); sync continues",
            crate::kernel::IDENTITY_FILE
        );
    }
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

    let out = sync(
        &SyncInput {
            base_config: &base,
            local_config: local.as_ref(),
            base_lock,
            local_lock,
            force,
            interactive,
            prune,
            no_hooks,
            no_transitive_hooks,
            frozen,
            lockless,
            fast_forward,
            resolver: interactive.then_some(&resolver as &dyn ConflictResolver),
            jobs,
        },
        &backend,
        &registry,
    )?;

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

fn finish_sync(cwd: &Path, out: &SyncOutput, interactive: bool) -> Result<()> {
    write_locks(cwd, &out.base_lock, out.local_lock.as_ref())?;
    let report = super::render::render_hook_report(&out.hook_results);
    if let Some(notice) = stripped_hook_notice(out.stripped_transitive_hooks, interactive) {
        if !report.is_empty() {
            eprint!("{report}");
        }
        eprintln!("{}", notice.message);
        if notice.fail {
            std::process::exit(1);
        }
    }
    if out.had_failures {
        if !report.is_empty() {
            eprint!("{report}");
        }
        let hooks_failed = out
            .hook_results
            .iter()
            .any(|o| o.status == crate::sync::HookStatus::Failure);
        let message = match (out.deploy_failures, hooks_failed) {
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
