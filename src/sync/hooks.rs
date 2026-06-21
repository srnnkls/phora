//! Post-commit hook dispatch.

use std::collections::BTreeSet;
use std::process::Command;

use crate::config::{Config, DEFAULT_SHELL_PREFIX, HookCommand, HookWhen, LayoutConfig};
use crate::error::{Error, Result};
use crate::store::{Registry, RegistryRecord};

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
            let changed = changed_records(&records, &recorded);
            if changed.is_empty() {
                continue;
            }

            let names = changed_names(&changed);
            let paths = changed_paths(&changed, &target_path, &layout);
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

/// A commit-pinned transitive `on_change` hook candidate plus the trust decision context.
pub(super) struct TransitiveHookRun<'a> {
    pub(super) dep_instance: &'a str,
    pub(super) hook_id: &'a str,
    pub(super) command: &'a HookCommand,
    pub(super) preimage: &'a str,
    pub(super) target_path: &'a std::path::Path,
}

/// A new trust approval the producer must persist to the consumer lock's `trusted_hooks`.
pub(super) struct TransitiveApproval {
    pub(super) dep_instance: String,
    pub(super) hook_id: String,
    pub(super) preimage: String,
}

pub(super) trait TrustPrompt {
    fn confirm(&self, candidate: &TransitiveHookRun<'_>) -> bool;
}

/// Only an explicit `y` trusts; EOF/error declines.
pub(super) struct TtyTrustPrompt;

impl TrustPrompt for TtyTrustPrompt {
    fn confirm(&self, candidate: &TransitiveHookRun<'_>) -> bool {
        use std::io::Write as _;
        eprint!(
            "phora: composed dep `{}` wants to run on_change hook `{}` — trust it? [y/N] ",
            candidate.dep_instance, candidate.command.run
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => false,
            Ok(_) => line.trim().eq_ignore_ascii_case("y"),
        }
    }
}

pub(super) struct DeclineAll;

impl TrustPrompt for DeclineAll {
    fn confirm(&self, _candidate: &TransitiveHookRun<'_>) -> bool {
        false
    }
}

/// Runs each commit-pinned transitive hook whose preimage a consumer `trusted_hooks` entry
/// already pins; an unpinned hook consults `prompt` (approval persists) and is skipped on a
/// decline. A dep can never self-approve: trust is keyed on the consumer lock.
pub(super) fn dispatch_transitive_hooks(
    candidates: &[TransitiveHookRun<'_>],
    trusted: &BTreeSet<String>,
    prompt: &dyn TrustPrompt,
) -> Result<(Vec<HookOutcome>, Vec<TransitiveApproval>)> {
    let mut outcomes = Vec::new();
    let mut approvals = Vec::new();
    for candidate in candidates {
        let pinned = trusted.contains(candidate.preimage);
        if !pinned {
            if !prompt.confirm(candidate) {
                continue;
            }
            approvals.push(TransitiveApproval {
                dep_instance: candidate.dep_instance.to_owned(),
                hook_id: candidate.hook_id.to_owned(),
                preimage: candidate.preimage.to_owned(),
            });
        }
        let status = run_hook(
            candidate.command,
            &[("PHORA_TARGET", &candidate.target_path.to_string_lossy())],
        )?;
        outcomes.push(HookOutcome {
            hook_id: candidate.hook_id.to_owned(),
            command: candidate.command.run.clone(),
            scope: HookScope::OnChange,
            status,
        });
    }
    Ok((outcomes, approvals))
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
fn changed_records<'a>(
    records: &'a [RegistryRecord],
    recorded: &BTreeSet<String>,
) -> Vec<&'a RegistryRecord> {
    records
        .iter()
        .filter(|record| !recorded.contains(&record.digest))
        .collect()
}

fn changed_names(changed: &[&RegistryRecord]) -> String {
    let names: BTreeSet<&str> = changed.iter().map(|r| r.key.artifact.as_str()).collect();
    names.into_iter().collect::<Vec<_>>().join("\n")
}

fn changed_paths(
    changed: &[&RegistryRecord],
    target_path: &std::path::Path,
    layout: &LayoutConfig,
) -> String {
    let paths: BTreeSet<String> = changed
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

#[cfg(test)]
mod transitive_trust_tests {
    use super::*;
    use crate::config::HookCommand;
    use std::path::Path;

    struct CannedPrompt(bool);

    impl TrustPrompt for CannedPrompt {
        fn confirm(&self, _candidate: &TransitiveHookRun<'_>) -> bool {
            self.0
        }
    }

    struct NeverPrompt;
    impl TrustPrompt for NeverPrompt {
        fn confirm(&self, _candidate: &TransitiveHookRun<'_>) -> bool {
            panic!("a hook already pinned in trusted_hooks must run without prompting");
        }
    }

    fn noop_hook() -> HookCommand {
        HookCommand {
            run: "true".to_owned(),
            shell: Some("sh -c".to_owned()),
        }
    }

    fn run<'a>(command: &'a HookCommand, target: &'a Path) -> TransitiveHookRun<'a> {
        TransitiveHookRun {
            dep_instance: "owninginstance01",
            hook_id: "composed#on_change#deadbeef",
            command,
            preimage: "blake3:candidatepreimage",
            target_path: target,
        }
    }

    #[test]
    fn answering_yes_yields_an_approval_matching_the_candidate_preimage_and_id() {
        let cmd = noop_hook();
        let target = Path::new("/tmp/phora-test-target");
        let candidate = run(&cmd, target);
        let trusted = BTreeSet::new();

        let (_outcomes, approvals) =
            dispatch_transitive_hooks(&[candidate], &trusted, &CannedPrompt(true))
                .expect("dispatch with a yes-answering prompt must succeed");

        assert_eq!(
            approvals.len(),
            1,
            "an untrusted hook approved with `y` must produce exactly one persistable approval"
        );
        let approval = &approvals[0];
        assert_eq!(
            approval.preimage, "blake3:candidatepreimage",
            "the persisted approval must carry the candidate's commit-bound preimage verbatim, so \
             trust is pinned to the resolved commit"
        );
        assert_eq!(
            approval.dep_instance, "owninginstance01",
            "the approval must address the candidate's owning dep instance"
        );
        assert_eq!(
            approval.hook_id, "composed#on_change#deadbeef",
            "the approval must address the candidate's hook id"
        );
    }

    #[test]
    fn answering_no_yields_no_approval_and_does_not_run_the_hook() {
        let cmd = noop_hook();
        let target = Path::new("/tmp/phora-test-target");
        let candidate = run(&cmd, target);
        let trusted = BTreeSet::new();

        let (outcomes, approvals) =
            dispatch_transitive_hooks(&[candidate], &trusted, &CannedPrompt(false))
                .expect("dispatch with a no-answering prompt must succeed");

        assert!(
            approvals.is_empty(),
            "anti-TOFU: declining the prompt must write NO trusted_hooks approval"
        );
        assert!(
            outcomes.is_empty(),
            "a declined untrusted hook must NOT run, so it produces no outcome"
        );
    }

    #[test]
    fn an_already_trusted_hook_runs_without_prompting_or_re_approving() {
        let cmd = noop_hook();
        let target = Path::new("/tmp/phora-test-target");
        let candidate = run(&cmd, target);
        let mut trusted = BTreeSet::new();
        trusted.insert("blake3:candidatepreimage".to_owned());

        let (outcomes, approvals) = dispatch_transitive_hooks(&[candidate], &trusted, &NeverPrompt)
            .expect("a pinned hook dispatches without error");

        assert_eq!(
            outcomes.len(),
            1,
            "a preimage-pinned hook must RUN without consulting the prompt"
        );
        assert!(
            approvals.is_empty(),
            "a hook already trusted needs no fresh approval to persist"
        );
    }
}
