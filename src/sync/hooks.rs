//! Post-commit hook dispatch.

use std::collections::BTreeSet;
use std::process::Command;

use crate::config::{Config, HookCommand, HookWhen, LayoutConfig};
use crate::error::{Error, Result};
use crate::store::{Registry, RegistryRecord};

/// `shell` is split on whitespace, so an interpreter path containing spaces is
/// unsupported; the first token is the program, the rest are leading args.
const DEFAULT_SHELL_PREFIX: &str = "sh -c";

/// Which hook table a [`HookOutcome`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookScope {
    OnChange,
    PostSync,
}

/// Whether a dispatched hook exited zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    Success,
    Failure,
}

/// One dispatched hook's identity and outcome, for per-hook reporting (TPH-004).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    pub hook_id: String,
    pub command: String,
    pub scope: HookScope,
    pub status: HookStatus,
}

/// Runs every target's `on_change` hook whose deployed digest-set gained members
/// since its last success, then every global `post_sync` hook. Files are never
/// rolled back; a non-zero exit surfaces as a [`HookStatus::Failure`] outcome.
///
/// # Errors
///
/// Returns an error if the registry cannot be read, hook state cannot be
/// recorded, or a hook process fails to spawn.
pub(super) fn dispatch_hooks(config: &Config, registry: &dyn Registry) -> Result<Vec<HookOutcome>> {
    let mut outcomes = Vec::new();

    for (target_name, target) in &config.targets {
        let Some(hooks) = &target.hooks else { continue };
        let Some(on_change) = &hooks.on_change else {
            continue;
        };

        let records = registry.list_target(target_name)?;
        let current: BTreeSet<String> = records.iter().map(|r| r.digest.clone()).collect();
        let layout = target.layout();
        let target_path = target.expanded_path();

        for hook in dedupe(on_change) {
            let id = format!(
                "{target_name}#{}#{}",
                hook.run,
                hook.shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX)
            );
            let recorded = recorded_set(registry, target_name, &id)?;
            let added = added_records(&records, &recorded);
            if added.is_empty() {
                continue;
            }

            let names = changed_names(&added);
            let paths = changed_paths(&added, &target_path, &layout);
            let status = run_hook(
                hook,
                &[
                    ("PHORA_TARGET", target_name.as_str()),
                    ("PHORA_CHANGED", &paths),
                    ("PHORA_CHANGED_NAMES", &names),
                ],
            )?;
            if status == HookStatus::Success {
                registry.record_hook_success(target_name, &id, &current)?;
            }
            outcomes.push(HookOutcome {
                hook_id: id,
                command: hook.run.clone(),
                scope: HookScope::OnChange,
                status,
            });
        }
    }

    if let Some(global) = &config.hooks
        && let Some(post_sync) = &global.post_sync
        && matches!(global.when, HookWhen::Always)
    {
        for hook in dedupe(post_sync) {
            let status = run_hook(hook, &[])?;
            outcomes.push(HookOutcome {
                hook_id: format!(
                    "post_sync#{}#{}",
                    hook.run,
                    hook.shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX)
                ),
                command: hook.run.clone(),
                scope: HookScope::PostSync,
                status,
            });
        }
    }

    Ok(outcomes)
}

fn dedupe(commands: &[HookCommand]) -> Vec<&HookCommand> {
    let mut seen = BTreeSet::new();
    commands
        .iter()
        .filter(|cmd| seen.insert((cmd.run.as_str(), cmd.shell.as_deref())))
        .collect()
}

fn recorded_set(registry: &dyn Registry, target: &str, id: &str) -> Result<BTreeSet<String>> {
    Ok(registry
        .load_hook_state(target)?
        .into_iter()
        .find(|h| h.hook_id == id)
        .map(|h| h.last_success)
        .unwrap_or_default())
}

/// Records carrying a digest absent from the hook's last-success set: the
/// directional changed set (additions/modifications). Pure removals yield none.
fn added_records<'a>(
    records: &'a [RegistryRecord],
    recorded: &BTreeSet<String>,
) -> Vec<&'a RegistryRecord> {
    records
        .iter()
        .filter(|record| !recorded.contains(&record.digest))
        .collect()
}

fn changed_names(added: &[&RegistryRecord]) -> String {
    let names: BTreeSet<&str> = added.iter().map(|r| r.key.artifact.as_str()).collect();
    names.into_iter().collect::<Vec<_>>().join("\n")
}

fn changed_paths(
    added: &[&RegistryRecord],
    target_path: &std::path::Path,
    layout: &LayoutConfig,
) -> String {
    let paths: BTreeSet<String> = added
        .iter()
        .map(|r| {
            target_path
                .join(layout.artifact_path(&r.key.source, &r.key.artifact))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    paths.into_iter().collect::<Vec<_>>().join("\n")
}

/// `Success` on a zero exit; a spawn failure is a hard error, a non-zero exit is not.
///
/// # Errors
///
/// Returns an error if the hook shell is empty or the process fails to spawn.
fn run_hook(hook: &HookCommand, env: &[(&str, &str)]) -> Result<HookStatus> {
    let shell = hook.shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX);
    let mut parts = shell.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| Error::Sync(format!("hook shell `{shell}` is empty")))?;

    let mut command = Command::new(program);
    command.args(parts).arg(&hook.run);
    for (key, value) in env {
        command.env(key, value);
    }

    let status = command
        .status()
        .map_err(|e| Error::Sync(format!("failed to run hook `{}`: {e}", hook.run)))?;
    Ok(if status.success() {
        HookStatus::Success
    } else {
        HookStatus::Failure
    })
}
