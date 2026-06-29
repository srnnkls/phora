//! Post-commit hook dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use crate::config::{Config, DEFAULT_SHELL_PREFIX, HookCommand, HookWhen, LayoutConfig};
use crate::error::{Error, Result};
use crate::store::{Registry, RegistryRecord};

/// Which hook table a [`HookOutcome`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookScope {
    PreSync,
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
            let (body, suffix) = hook_key(hook);
            let id = format!("{target_name}#{body}#{suffix}");
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
                command: hook.display(),
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
            let (body, suffix) = hook_key(hook);
            outcomes.push(HookOutcome {
                hook_id: format!("post_sync#{body}#{suffix}"),
                command: hook.display(),
                scope: HookScope::PostSync,
                status,
            });
        }
    }

    Ok(outcomes)
}

/// Runs every global `pre_sync` hook with `PHORA_TARGETS`. Unconditional: `when` governs
/// `post_sync` re-fire, never the per-run `pre_sync` gate.
///
/// # Errors
///
/// Returns an error if a hook process fails to spawn.
pub(super) fn dispatch_pre_sync(config: &Config, target_names: &str) -> Result<Vec<HookOutcome>> {
    let Some(pre_sync) = config.hooks.as_ref().and_then(|g| g.pre_sync.as_ref()) else {
        return Ok(Vec::new());
    };
    let mut outcomes = Vec::new();
    for hook in dedupe(pre_sync) {
        let status = run_hook(hook, &[("PHORA_TARGETS", target_names)])?;
        let (body, suffix) = hook_key(hook);
        outcomes.push(HookOutcome {
            hook_id: format!("pre_sync#{body}#{suffix}"),
            command: hook.display(),
            scope: HookScope::PreSync,
            status,
        });
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
    pub(super) source: &'a str,
    pub(super) commit: &'a str,
}

/// A new trust approval the producer must persist to the consumer lock's `trusted_hooks`.
pub(super) struct TransitiveApproval {
    pub(super) dep_instance: String,
    pub(super) hook_id: String,
    pub(super) preimage: String,
    pub(super) source: String,
    pub(super) commit: String,
}

pub(super) trait TrustPrompt {
    fn confirm(&self, candidate: &TransitiveHookRun<'_>) -> bool;
}

/// Reads a y/N answer from stdin; only an explicit `y` confirms, EOF and errors decline.
pub(crate) fn prompt_yes_on_stdin(prompt: &str) -> bool {
    use std::io::Write as _;
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => line.trim().eq_ignore_ascii_case("y"),
    }
}

pub(super) struct TtyTrustPrompt;

impl TrustPrompt for TtyTrustPrompt {
    fn confirm(&self, candidate: &TransitiveHookRun<'_>) -> bool {
        prompt_yes_on_stdin(&format!(
            "phora: composed dep `{}` wants to run on_change hook `{}` — trust it? [y/N] ",
            candidate.dep_instance,
            candidate.command.display()
        ))
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
    // Identical preimages share one decision per run: prompt once, then reuse the answer so
    // duplicate hooks neither re-prompt nor re-run a decline.
    let mut decided: BTreeMap<&str, bool> = BTreeMap::new();
    for candidate in candidates {
        let approved = if trusted.contains(candidate.preimage) {
            true
        } else if let Some(&prior) = decided.get(candidate.preimage) {
            prior
        } else {
            let answer = prompt.confirm(candidate);
            decided.insert(candidate.preimage, answer);
            answer
        };
        if !approved {
            continue;
        }
        if !trusted.contains(candidate.preimage) {
            approvals.push(TransitiveApproval {
                dep_instance: candidate.dep_instance.to_owned(),
                hook_id: candidate.hook_id.to_owned(),
                preimage: candidate.preimage.to_owned(),
                source: candidate.source.to_owned(),
                commit: candidate.commit.to_owned(),
            });
        }
        let status = run_hook(
            candidate.command,
            &[("PHORA_TARGET", &candidate.target_path.to_string_lossy())],
        )?;
        outcomes.push(HookOutcome {
            hook_id: candidate.hook_id.to_owned(),
            command: candidate.command.display(),
            scope: HookScope::OnChange,
            status,
        });
    }
    Ok((outcomes, approvals))
}

fn hook_key(hook: &HookCommand) -> (String, String) {
    match hook {
        HookCommand::Shell { run, shell } => (
            run.clone(),
            shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX).to_owned(),
        ),
        HookCommand::Exec { cmd } => (cmd.join(" "), "exec".to_owned()),
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum DedupeKey<'a> {
    Shell(&'a str, &'a str),
    Exec(&'a [String]),
}

fn dedupe(commands: &[HookCommand]) -> Vec<&HookCommand> {
    let mut seen = BTreeSet::new();
    commands
        .iter()
        .filter(|cmd| {
            let key = match cmd {
                HookCommand::Shell { run, shell } => {
                    DedupeKey::Shell(run, shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX))
                }
                HookCommand::Exec { cmd } => DedupeKey::Exec(cmd),
            };
            seen.insert(key)
        })
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
    let mut command = match hook {
        HookCommand::Shell { run, shell } => {
            let shell = shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX);
            let mut parts = shell.split_whitespace();
            let program = parts
                .next()
                .ok_or_else(|| Error::Sync(format!("hook shell `{shell}` is empty")))?;
            let mut command = Command::new(program);
            command.args(parts).arg(run);
            command
        }
        HookCommand::Exec { cmd } => {
            let (program, args) = cmd
                .split_first()
                .ok_or_else(|| Error::Sync("hook command `cmd` must not be empty".to_owned()))?;
            let mut command = Command::new(program);
            command.args(args);
            command
        }
    };
    for (key, value) in env {
        command.env(key, value);
    }

    let status = command
        .status()
        .map_err(|e| Error::Sync(format!("failed to run hook `{}`: {e}", hook.display())))?;
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

    struct CountingPrompt(std::cell::Cell<usize>, bool);
    impl TrustPrompt for CountingPrompt {
        fn confirm(&self, _candidate: &TransitiveHookRun<'_>) -> bool {
            self.0.set(self.0.get() + 1);
            self.1
        }
    }

    fn noop_hook() -> HookCommand {
        HookCommand::Shell {
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
            source: "mydeps",
            commit: "c0ffeecommit",
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
        assert_eq!(
            (approval.source.as_str(), approval.commit.as_str()),
            ("mydeps", "c0ffeecommit"),
            "the approval must carry the candidate's source and commit so the persisted \
             trusted_hooks entry can be diffed by `phora trust`, never silently emptied"
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

    fn run_keyed<'a>(
        command: &'a HookCommand,
        target: &'a Path,
        dep_instance: &'a str,
        hook_id: &'a str,
        preimage: &'a str,
    ) -> TransitiveHookRun<'a> {
        TransitiveHookRun {
            dep_instance,
            hook_id,
            command,
            preimage,
            target_path: target,
            source: "mydeps",
            commit: "c0ffeecommit",
        }
    }

    #[test]
    fn duplicate_preimages_prompt_once_but_each_records_its_own_approval() {
        let cmd = noop_hook();
        let target = Path::new("/tmp/phora-test-target");
        let a = run_keyed(
            &cmd,
            target,
            "instanceaaaa0001",
            "a#on_change#dead",
            "blake3:shared",
        );
        let b = run_keyed(
            &cmd,
            target,
            "instancebbbb0002",
            "b#on_change#dead",
            "blake3:shared",
        );
        let trusted = BTreeSet::new();
        let prompt = CountingPrompt(std::cell::Cell::new(0), true);

        let (outcomes, approvals) = dispatch_transitive_hooks(&[a, b], &trusted, &prompt)
            .expect("dispatch with two same-preimage candidates must succeed");

        assert_eq!(
            prompt.0.get(),
            1,
            "two candidates sharing a preimage must prompt exactly once"
        );
        assert_eq!(
            approvals.len(),
            2,
            "each distinct (dep_instance, hook_id) must still get its own approval record"
        );
        assert_eq!(
            outcomes.len(),
            2,
            "both shared-preimage hooks must run after the single yes"
        );
    }

    #[test]
    fn declining_a_shared_preimage_skips_all_duplicates_without_re_prompting() {
        let cmd = noop_hook();
        let target = Path::new("/tmp/phora-test-target");
        let a = run_keyed(
            &cmd,
            target,
            "instanceaaaa0001",
            "a#on_change#dead",
            "blake3:shared",
        );
        let b = run_keyed(
            &cmd,
            target,
            "instancebbbb0002",
            "b#on_change#dead",
            "blake3:shared",
        );
        let trusted = BTreeSet::new();
        let prompt = CountingPrompt(std::cell::Cell::new(0), false);

        let (outcomes, approvals) = dispatch_transitive_hooks(&[a, b], &trusted, &prompt)
            .expect("dispatch declining a shared preimage must succeed");

        assert_eq!(
            prompt.0.get(),
            1,
            "a declined preimage must not re-prompt for duplicates"
        );
        assert!(
            approvals.is_empty(),
            "a declined preimage records no approval"
        );
        assert!(outcomes.is_empty(), "a declined preimage runs nothing");
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
