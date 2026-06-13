//! Post-commit hook dispatch.

use std::collections::BTreeSet;
use std::process::Command;

use crate::config::{Config, HookCommand, HookWhen};
use crate::error::{Error, Result};
use crate::store::{Registry, digest_set_changed};

const DEFAULT_SHELL: &str = "sh -c";

/// Returns `true` if any hook exited non-zero. Files are never rolled back.
pub(super) fn dispatch_hooks(config: &Config, registry: &dyn Registry) -> Result<bool> {
    let mut had_failures = false;

    for (target_name, target) in &config.targets {
        let Some(hooks) = &target.hooks else { continue };
        let Some(on_change) = &hooks.on_change else {
            continue;
        };

        let records = registry.list_target(target_name)?;
        let current: BTreeSet<String> = records.iter().map(|r| r.digest.clone()).collect();

        for (index, hook) in dedupe(on_change).into_iter().enumerate() {
            let id = format!("{target_name}#{index}");
            let recorded = recorded_set(registry, target_name, &id)?;
            if !digest_set_changed(&current, &recorded) {
                continue;
            }

            let changed = changed_members(&records, &recorded);
            if run_hook(
                hook,
                &[("PHORA_TARGET", target_name), ("PHORA_CHANGED", &changed)],
            )? {
                registry.record_hook_success(target_name, &id, &current)?;
            } else {
                had_failures = true;
            }
        }
    }

    if let Some(global) = &config.hooks
        && let Some(post_sync) = &global.post_sync
        && matches!(global.when, HookWhen::Always)
    {
        for hook in dedupe(post_sync) {
            if !run_hook(hook, &[])? {
                had_failures = true;
            }
        }
    }

    Ok(had_failures)
}

fn dedupe(commands: &[HookCommand]) -> Vec<&HookCommand> {
    let mut seen = BTreeSet::new();
    commands
        .iter()
        .filter(|cmd| seen.insert(cmd.run.as_str()))
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

fn changed_members(
    records: &[crate::store::RegistryRecord],
    recorded: &BTreeSet<String>,
) -> String {
    let mut members: BTreeSet<&str> = BTreeSet::new();
    for record in records {
        if !recorded.contains(&record.digest) {
            members.insert(record.key.artifact.as_str());
        }
    }
    members.into_iter().collect::<Vec<_>>().join(" ")
}

/// `true` on a zero exit; a spawn failure is a hard error, a non-zero exit is not.
fn run_hook(hook: &HookCommand, env: &[(&str, &str)]) -> Result<bool> {
    let shell = hook.shell.as_deref().unwrap_or(DEFAULT_SHELL);
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
    Ok(status.success())
}
