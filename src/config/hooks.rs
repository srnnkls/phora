//! Hook DTOs for `[targets.X.hooks]` and the global `[hooks]` table.

use serde::Deserialize;

use crate::config::transitive::Instance;

pub(crate) const DEFAULT_SHELL_PREFIX: &str = "sh -c";

/// Keyed blake3 is cryptographically independent of the unkeyed `Shell` hash, so an Exec
/// hook can never share a preimage or discriminator with a Shell hook — a swap re-prompts.
const EXEC_PREIMAGE_CONTEXT: &str = "phora hook exec preimage v1";
const EXEC_DISCRIMINATOR_CONTEXT: &str = "phora hook exec discriminator v1";

/// An interpreted transitive `on_change` hook awaiting the trust decision; trust is
/// consumer-owned, so a candidate carries no approval state — a dep can never self-approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateHook {
    pub dep_instance: String,
    pub hook_id: String,
    pub command: HookCommand,
}

/// Interprets a dep target's retained opaque `[targets.X.hooks]` into candidate hooks keyed by
/// the confined [`Instance`], surfacing a diagnostic for each sub-table that fails to deserialize
/// rather than silently dropping it. Strip-by-default holds: this produces candidates only — it
/// runs nothing, retains nothing, and never marks a hook trusted.
#[must_use]
pub fn admit_transitive_hooks(
    opaque: &toml::Value,
    dep_target_name: &str,
    composed_target_name: &str,
    instance: &Instance,
) -> (Vec<CandidateHook>, Vec<String>) {
    let Some(hooks) = opaque.get(dep_target_name) else {
        return (Vec::new(), Vec::new());
    };
    let hooks = match hooks.clone().try_into::<TargetHooks>() {
        Ok(hooks) => hooks,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!(
                    "imported dep target `{dep_target_name}`: malformed `[targets.{dep_target_name}.hooks]`: {e}"
                )],
            );
        }
    };
    let Some(on_change) = hooks.on_change else {
        return (Vec::new(), Vec::new());
    };
    let candidates = on_change
        .into_iter()
        .map(|command| CandidateHook {
            dep_instance: instance.stable_key(),
            hook_id: format!(
                "{composed_target_name}#on_change#{}",
                command_discriminator(&command)
            ),
            command,
        })
        .collect();
    (candidates, Vec::new())
}

/// Behavior preimage binding a candidate hook to the dep's resolved commit (`blake3:<hex>`).
/// `commit_sha` is the git commit, not the export digest: a swapped hook script excluded from
/// the export set still re-prompts.
#[must_use]
pub fn hook_preimage(command: &HookCommand, kind: &str, commit_sha: &str) -> String {
    let digest = match command {
        HookCommand::Shell { run, shell } => {
            let shell = shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX);
            frame(
                blake3::Hasher::new(),
                [run.as_str(), shell, kind, commit_sha],
            )
        }
        HookCommand::Exec { cmd } => frame(
            blake3::Hasher::new_derive_key(EXEC_PREIMAGE_CONTEXT),
            cmd.iter().map(String::as_str).chain([kind, commit_sha]),
        ),
    };
    format!("blake3:{digest}")
}

/// Length-framed so an injected `#` cannot forge a trust key by colliding two distinct commands.
fn command_discriminator(command: &HookCommand) -> String {
    let digest = match command {
        HookCommand::Shell { run, shell } => {
            let shell = shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX);
            frame(blake3::Hasher::new(), [run.as_str(), shell])
        }
        HookCommand::Exec { cmd } => frame(
            blake3::Hasher::new_derive_key(EXEC_DISCRIMINATOR_CONTEXT),
            cmd.iter().map(String::as_str),
        ),
    };
    digest[..16].to_owned()
}

fn frame<'a>(mut hasher: blake3::Hasher, fields: impl IntoIterator<Item = &'a str>) -> String {
    for field in fields {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCommand {
    /// `shell` defaults to `sh -c`. Split on whitespace at dispatch, so an interpreter
    /// path containing spaces is unsupported.
    Shell { run: String, shell: Option<String> },
    /// Shell-free argv: spawned directly, no shell, no `$VAR` expansion. Non-empty.
    Exec { cmd: Vec<String> },
}

impl HookCommand {
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            HookCommand::Shell { run, .. } => run.clone(),
            HookCommand::Exec { cmd } => cmd.join(" "),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookWhen {
    #[default]
    Always,
}

/// Disposition when a target's `pre_deploy` gate fails: `abort` halts the whole sync, `skip`
/// skips only that target's deploy and continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreDeployOnFail {
    #[default]
    Abort,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetHooks {
    #[serde(default, deserialize_with = "deserialize_commands")]
    pub on_change: Option<Vec<HookCommand>>,
    #[serde(default, deserialize_with = "deserialize_commands")]
    pub pre_deploy: Option<Vec<HookCommand>>,
    #[serde(default)]
    pub pre_deploy_on_fail: PreDeployOnFail,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalHooks {
    /// Lifecycle gate: runs once after fetch, before deploy; a non-zero exit aborts the sync.
    #[serde(default, deserialize_with = "deserialize_commands")]
    pub pre_sync: Option<Vec<HookCommand>>,
    #[serde(default, deserialize_with = "deserialize_commands")]
    pub post_sync: Option<Vec<HookCommand>>,
    #[serde(default)]
    pub when: HookWhen,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookTable {
    run: Option<String>,
    shell: Option<String>,
    cmd: Option<Vec<String>>,
}

impl<'de> Deserialize<'de> for HookCommand {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CommandVisitor;

        impl<'de> serde::de::Visitor<'de> for CommandVisitor {
            type Value = HookCommand;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "a command string, a `{ run, shell }` table, or a `{ cmd = [...] }` table",
                )
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(HookCommand::Shell {
                    run: validated_run(v.to_owned(), None)?,
                    shell: None,
                })
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let table =
                    HookTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                validated_command(table.run, table.shell, table.cmd)
            }
        }

        deserializer.deserialize_any(CommandVisitor)
    }
}

fn validated_command<E: serde::de::Error>(
    run: Option<String>,
    shell: Option<String>,
    cmd: Option<Vec<String>>,
) -> std::result::Result<HookCommand, E> {
    match (run, cmd) {
        (Some(_), Some(_)) => Err(E::custom(
            "hook command must set either `run` (shell) or `cmd` (exec), not both",
        )),
        (None, None) => Err(E::custom(
            "hook command must set `run` (shell) or `cmd` (exec)",
        )),
        (Some(run), None) => Ok(HookCommand::Shell {
            run: validated_run(run, shell.as_deref())?,
            shell,
        }),
        (None, Some(cmd)) => {
            if shell.is_some() {
                return Err(E::custom(
                    "hook command `cmd` (exec form) runs no shell, so `shell` is not allowed",
                ));
            }
            if cmd.is_empty() {
                return Err(E::custom("hook command `cmd` must not be empty"));
            }
            Ok(HookCommand::Exec { cmd })
        }
    }
}

fn validated_run<E: serde::de::Error>(
    run: String,
    shell: Option<&str>,
) -> std::result::Result<String, E> {
    if run.trim().is_empty() {
        return Err(E::custom("hook command `run` must not be empty"));
    }
    if let Some(shell) = shell
        && shell.trim().is_empty()
    {
        return Err(E::custom("hook command `shell` must not be empty"));
    }
    Ok(run)
}

fn deserialize_commands<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<HookCommand>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CommandsVisitor;

    impl<'de> serde::de::Visitor<'de> for CommandsVisitor {
        type Value = Vec<HookCommand>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a command, a `{ run, shell }` table, or an array of either")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
            Ok(vec![HookCommand::Shell {
                run: validated_run(v.to_owned(), None)?,
                shell: None,
            }])
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let cmd = HookCommand::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
            Ok(vec![cmd])
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(cmd) = seq.next_element::<HookCommand>()? {
                out.push(cmd);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_any(CommandsVisitor).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::transitive::{FetchNode, Instance};

    fn cmd(run: &str, shell: Option<&str>) -> HookCommand {
        HookCommand::Shell {
            run: run.to_owned(),
            shell: shell.map(str::to_owned),
        }
    }

    // TDEP-HOOK-TRUST-001 R5: preimage binds run + shell + kind + resolved commit SHA.

    #[test]
    fn hook_preimage_is_a_blake3_hex_string() {
        let preimage = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_a");
        assert!(
            preimage.starts_with("blake3:"),
            "the recorded preimage must use the `blake3:<hex>` format, got: {preimage}"
        );
        let hex = &preimage["blake3:".len()..];
        assert!(
            !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()),
            "the preimage payload must be a non-empty hex digest, got: {preimage}"
        );
    }

    #[test]
    fn hook_preimage_matches_when_run_shell_kind_and_commit_all_agree() {
        let a = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_a");
        let b = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_a");
        assert_eq!(
            a, b,
            "identical (run, shell, kind, commit) must produce the same preimage so a recorded \
             approval keeps matching across syncs"
        );
    }

    #[test]
    fn hook_preimage_rebinds_when_only_the_commit_sha_changes() {
        let at_a = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_a");
        let at_b = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_b");
        assert_ne!(
            at_a, at_b,
            "AC1: the preimage must bind the dep's RESOLVED COMMIT SHA — an identical hook command \
             at a different commit must produce a DIFFERENT preimage, forcing a re-prompt even when \
             the export set is unchanged"
        );
    }

    #[test]
    fn hook_preimage_rebinds_on_shell_swap() {
        let sh = hook_preimage(&cmd("./install.sh", Some("sh -c")), "on_change", "commit_a");
        let bash = hook_preimage(
            &cmd("./install.sh", Some("bash -c")),
            "on_change",
            "commit_a",
        );
        assert_ne!(
            sh, bash,
            "AC2: swapping the shell (sh -c -> bash -c) must change the preimage, re-prompting"
        );
    }

    #[test]
    fn hook_preimage_default_shell_equals_explicit_sh_dash_c() {
        let implicit = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_a");
        let explicit = hook_preimage(
            &cmd("./install.sh", Some(DEFAULT_SHELL_PREFIX)),
            "on_change",
            "commit_a",
        );
        assert_eq!(
            implicit, explicit,
            "an absent shell must hash as the default `{DEFAULT_SHELL_PREFIX}`, so adding the \
             explicit default does not spuriously re-prompt"
        );
    }

    #[test]
    fn hook_preimage_rebinds_on_run_string_change() {
        let one = hook_preimage(&cmd("./install.sh", None), "on_change", "commit_a");
        let two = hook_preimage(&cmd("./other.sh", None), "on_change", "commit_a");
        assert_ne!(
            one, two,
            "AC3: changing the hook `run` string must change the preimage, re-prompting"
        );
    }

    // TDEP-HOOK-TRUST-001 AC7: a malformed dep hooks sub-table must surface a diagnostic,
    // not silently drop (today admit_transitive_hooks returns empty with no signal).

    #[test]
    fn malformed_dep_hooks_subtable_surfaces_a_diagnostic_instead_of_silent_drop() {
        let opaque: toml::Value = toml::from_str("[editor]\non_change = { bogus = true }\n")
            .expect("an opaque hooks payload with an undeserializable sub-table parses as toml");
        let node = FetchNode::new("https://github.com/dep/nvim.git", "main", "deadbeef");
        let instance = Instance::new("root", "dep", "anchor", node);

        let (candidates, diagnostics) =
            admit_transitive_hooks(&opaque, "editor", "ns%1%editor", &instance);

        assert!(
            candidates.is_empty(),
            "a hooks sub-table that fails to deserialize must yield no candidates"
        );
        assert!(
            !diagnostics.is_empty(),
            "AC7: a `[targets.editor.hooks]` sub-table that fails to deserialize as TargetHooks \
             must surface a diagnostic, not be silently dropped"
        );
        assert!(
            diagnostics.iter().any(|d| d.contains("editor")),
            "the parse-failure diagnostic must name the offending dep target `editor`, got: {diagnostics:?}"
        );
    }

    // HOOK-EXEC-001: shell-free argv form `{ cmd = [...] }` deserialization + validation.

    #[test]
    fn cmd_table_parses_as_an_exec_hook() {
        let parsed = toml::from_str::<TargetHooks>("on_change = { cmd = [\"echo\", \"hi\"] }");
        let hooks = parsed.expect(
            "HOOK-EXEC-001: a `{ cmd = [...] }` table must deserialize into the Exec variant; today \
             deny_unknown_fields on HookTable rejects `cmd` before any branching",
        );
        let commands = hooks
            .on_change
            .expect("a parsed `on_change` cmd table must yield a command list");
        assert_eq!(
            commands.len(),
            1,
            "a single `{{ cmd = [...] }}` table must produce exactly one hook command"
        );
        assert!(
            matches!(&commands[0], HookCommand::Exec { cmd } if cmd.as_slice() == ["echo", "hi"]),
            "HOOK-EXEC-001: `{{ cmd = [...] }}` must parse into the Exec variant carrying the exact \
             argv in order, not a shell command; got {:?}",
            commands[0]
        );
    }

    #[test]
    fn array_of_cmd_tables_parses_to_two_exec_hooks() {
        let parsed = toml::from_str::<TargetHooks>(
            "on_change = [{ cmd = [\"a\"] }, { cmd = [\"b\", \"c\"] }]",
        );
        let hooks = parsed.expect(
            "HOOK-EXEC-001: an array of `{ cmd = [...] }` tables must deserialize (array-of-either \
             back-compat extends to the exec form)",
        );
        let commands = hooks.on_change.expect("array must yield commands");
        assert_eq!(
            commands.len(),
            2,
            "two cmd-table entries must produce two hook commands"
        );
        assert!(
            matches!(&commands[0], HookCommand::Exec { cmd } if cmd.as_slice() == ["a"]),
            "HOOK-EXEC-001: the first array entry must parse into the Exec variant with argv \
             [\"a\"], not a shell command; got {:?}",
            commands[0]
        );
        assert!(
            matches!(&commands[1], HookCommand::Exec { cmd } if cmd.as_slice() == ["b", "c"]),
            "HOOK-EXEC-001: the second array entry must parse into the Exec variant with the exact \
             argv [\"b\", \"c\"] in order, not a shell command; got {:?}",
            commands[1]
        );
    }

    #[test]
    fn both_run_and_cmd_is_a_descriptive_combination_error() {
        let err = toml::from_str::<TargetHooks>(
            "on_change = { run = \"echo hi\", cmd = [\"echo\", \"hi\"] }",
        )
        .expect_err("specifying both `run` and `cmd` must be rejected at deserialize time");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("both"),
            "HOOK-EXEC-001: a hook giving BOTH `run` and `cmd` must fail with a descriptive \
             combination error naming the conflict, not the generic `unknown field` rejection; \
             got: {err}"
        );
    }

    #[test]
    fn cmd_with_shell_is_rejected_exec_form_takes_no_shell() {
        let err = toml::from_str::<TargetHooks>(
            "on_change = { cmd = [\"echo\", \"hi\"], shell = \"bash -c\" }",
        )
        .expect_err(
            "a `{ cmd, shell }` table must be rejected: the exec form runs no shell, so `shell` \
             cannot silently be dropped",
        );
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("shell") && (msg.contains("cmd") || msg.contains("exec")),
            "HOOK-EXEC-001: pairing `cmd` with `shell` must fail with an error naming the conflict \
             (`shell` plus `cmd`/`exec`), not silently route to Exec and drop `shell`; got: {err}"
        );
    }

    #[test]
    fn neither_run_nor_cmd_is_a_descriptive_error() {
        let err = toml::from_str::<TargetHooks>("on_change = {}")
            .expect_err("a hook table with neither `run` nor `cmd` must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("cmd"),
            "HOOK-EXEC-001: a hook giving NEITHER `run` nor `cmd` must fail with an error naming \
             `cmd` as an alternative (not the run-only `missing field run`); got: {err}"
        );
    }

    #[test]
    fn empty_cmd_is_rejected_at_the_deserialize_layer() {
        let err = toml::from_str::<TargetHooks>("on_change = { cmd = [] }")
            .expect_err("an empty `cmd` array must be rejected, mirroring the empty-`run` guard");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("empty"),
            "HOOK-EXEC-001: `cmd = []` must be rejected at deserialize time with an emptiness \
             error (mirror `validated_command`'s empty-run guard), not the generic unknown-field \
             rejection; got: {err}"
        );
    }

    #[test]
    fn bare_string_hook_still_parses() {
        let hooks = toml::from_str::<TargetHooks>("on_change = \"echo hi\"")
            .expect("back-compat: a bare command string must keep parsing");
        assert_eq!(
            hooks.on_change.expect("bare string yields a command").len(),
            1,
            "a bare string must remain a single shell hook"
        );
    }

    #[test]
    fn run_shell_table_still_parses() {
        let hooks =
            toml::from_str::<TargetHooks>("on_change = { run = \"echo hi\", shell = \"bash -c\" }")
                .expect("back-compat: a `{ run, shell }` table must keep parsing");
        assert_eq!(
            hooks.on_change.expect("table yields a command").len(),
            1,
            "a `{{ run, shell }}` table must remain a single shell hook"
        );
    }

    #[test]
    fn mixed_array_of_string_and_run_table_still_parses() {
        let hooks = toml::from_str::<TargetHooks>("on_change = [\"echo a\", { run = \"echo b\" }]")
            .expect("back-compat: an array mixing a string and a run table must keep parsing");
        assert_eq!(
            hooks.on_change.expect("array yields commands").len(),
            2,
            "a mixed string + run-table array must remain two shell hooks"
        );
    }

    // HOOK-PRESYNC-001: the global `[hooks]` table accepts a `pre_sync` field in every
    // surface form `post_sync` accepts. Today GlobalHooks carries `deny_unknown_fields`
    // with no `pre_sync`, so each of these is rejected as an unknown field.

    #[test]
    fn global_hooks_accepts_pre_sync_string() {
        toml::from_str::<GlobalHooks>("pre_sync = \"reload-everything\"").expect(
            "HOOK-PRESYNC-001: the global `[hooks]` table must accept a bare-string `pre_sync` \
             hook; today deny_unknown_fields rejects the `pre_sync` key",
        );
    }

    #[test]
    fn global_hooks_accepts_pre_sync_run_shell_table() {
        toml::from_str::<GlobalHooks>("pre_sync = { run = \"reload\", shell = \"bash -c\" }")
            .expect(
                "HOOK-PRESYNC-001: `pre_sync` must accept the `{ run, shell }` table form like \
             `post_sync` does",
            );
    }

    #[test]
    fn global_hooks_accepts_pre_sync_array() {
        toml::from_str::<GlobalHooks>("pre_sync = [\"first\", \"second\"]").expect(
            "HOOK-PRESYNC-001: `pre_sync` must accept an array of commands like `post_sync` does",
        );
    }

    #[test]
    fn global_hooks_accepts_both_pre_sync_and_post_sync() {
        toml::from_str::<GlobalHooks>("pre_sync = \"gate\"\npost_sync = \"reload\"").expect(
            "HOOK-PRESYNC-001: `pre_sync` is additive — declaring it alongside `post_sync` must \
             parse, not replace or conflict with `post_sync`",
        );
    }

    #[test]
    fn empty_pre_sync_command_is_rejected_naming_emptiness() {
        let err = toml::from_str::<GlobalHooks>("pre_sync = \"\"")
            .expect_err("an empty `pre_sync` command must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("empty"),
            "HOOK-PRESYNC-001: `pre_sync = \"\"` must be rejected through the same validated \
             command deserializer as `post_sync` (an emptiness error), not the generic \
             unknown-field rejection; got: {err}"
        );
    }

    // HOOK-PREDEPLOY-001: the per-target `[targets.X.hooks]` table accepts a `pre_deploy`
    // field in every surface form `on_change` accepts, plus a `pre_deploy_on_fail` enum
    // (default abort). Today `TargetHooks` carries `deny_unknown_fields` with neither key,
    // so each of these is rejected as an unknown field.

    #[test]
    fn target_hooks_accepts_pre_deploy_string() {
        toml::from_str::<TargetHooks>("pre_deploy = \"gate\"").expect(
            "HOOK-PREDEPLOY-001: `[targets.X.hooks]` must accept a bare-string `pre_deploy` hook; \
             today deny_unknown_fields rejects the `pre_deploy` key",
        );
    }

    #[test]
    fn target_hooks_accepts_pre_deploy_run_shell_table() {
        let custom_shell =
            toml::from_str::<TargetHooks>("pre_deploy = { run = \"gate\", shell = \"bash -c\" }")
                .expect(
                    "HOOK-PREDEPLOY-001: `pre_deploy` must accept the `{ run, shell }` table form \
                     like `on_change` does",
                );
        let default_shell = toml::from_str::<TargetHooks>("pre_deploy = \"gate\"")
            .expect("a bare-string `pre_deploy` parses with the default shell");
        assert_ne!(
            custom_shell, default_shell,
            "HOOK-PREDEPLOY-001: a custom `shell = \"bash -c\"` must SURVIVE parsing — the \
             `{{ run, shell }}` table must parse to a DIFFERENT value than the same `run` with the \
             default shell; an impl that accepts the table but drops the custom `shell` would pass \
             the bare parse yet fail this"
        );
    }

    #[test]
    fn target_hooks_accepts_pre_deploy_array() {
        let two = toml::from_str::<TargetHooks>("pre_deploy = [\"first\", \"second\"]").expect(
            "HOOK-PREDEPLOY-001: `pre_deploy` must accept an array of commands like `on_change` does",
        );
        let one = toml::from_str::<TargetHooks>("pre_deploy = [\"first\"]")
            .expect("a single-entry pre_deploy array parses");
        assert_ne!(
            two, one,
            "HOOK-PREDEPLOY-001: a two-command `pre_deploy` array must parse to a DIFFERENT value \
             than a one-command array — a stub dropping array entries would fail this"
        );
    }

    #[test]
    fn target_hooks_accepts_pre_deploy_cmd_exec_table() {
        let exec = toml::from_str::<TargetHooks>("pre_deploy = { cmd = [\"echo\", \"hi\"] }")
            .expect(
                "HOOK-PREDEPLOY-001: `pre_deploy` must accept the shell-free `{ cmd = [...] }` exec \
                 form like `on_change` does",
            );
        let shell = toml::from_str::<TargetHooks>("pre_deploy = \"echo hi\"")
            .expect("a shell-form pre_deploy parses");
        assert_ne!(
            exec, shell,
            "HOOK-PREDEPLOY-001: the `{{ cmd = [...] }}` exec form must parse to a DIFFERENT \
             command than the shell-string form — a stub coercing every form to Shell would fail \
             this"
        );
    }

    #[test]
    fn empty_pre_deploy_command_is_rejected_naming_emptiness() {
        let err = toml::from_str::<TargetHooks>("pre_deploy = \"\"")
            .expect_err("an empty `pre_deploy` command must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("empty"),
            "HOOK-PREDEPLOY-001: `pre_deploy = \"\"` must be rejected through the same validated \
             command deserializer as `on_change` (an emptiness error), not the generic \
             unknown-field rejection; got: {err}"
        );
    }

    #[test]
    fn pre_deploy_on_fail_abort_and_skip_parse_to_distinct_values() {
        let abort = toml::from_str::<TargetHooks>(
            "pre_deploy = \"gate\"\npre_deploy_on_fail = \"abort\"",
        )
        .expect(
            "HOOK-PREDEPLOY-001: `pre_deploy_on_fail = \"abort\"` must parse; today the key \
                     is rejected by deny_unknown_fields",
        );
        let skip =
            toml::from_str::<TargetHooks>("pre_deploy = \"gate\"\npre_deploy_on_fail = \"skip\"")
                .expect(
                    "HOOK-PREDEPLOY-001: `pre_deploy_on_fail = \"skip\"` must parse; today the key \
                     is rejected by deny_unknown_fields",
                );
        assert_ne!(
            abort, skip,
            "HOOK-PREDEPLOY-001: `abort` and `skip` must parse to DIFFERENT on-fail values — a stub \
             that ignores the field (parsing both identically) would fail this"
        );
    }

    #[test]
    fn omitting_pre_deploy_on_fail_defaults_to_abort() {
        let defaulted = toml::from_str::<TargetHooks>("pre_deploy = \"gate\"").expect(
            "HOOK-PREDEPLOY-001: a `pre_deploy` hook with no `pre_deploy_on_fail` must parse",
        );
        let explicit_abort =
            toml::from_str::<TargetHooks>("pre_deploy = \"gate\"\npre_deploy_on_fail = \"abort\"")
                .expect("explicit abort must parse");
        let explicit_skip =
            toml::from_str::<TargetHooks>("pre_deploy = \"gate\"\npre_deploy_on_fail = \"skip\"")
                .expect("explicit skip must parse");
        assert_eq!(
            defaulted, explicit_abort,
            "HOOK-PREDEPLOY-001: omitting `pre_deploy_on_fail` must default to `abort` — the \
             defaulted value must equal the explicit `abort` value"
        );
        assert_ne!(
            defaulted, explicit_skip,
            "HOOK-PREDEPLOY-001: the default must NOT be `skip` — abort is the documented default"
        );
    }

    #[test]
    fn invalid_pre_deploy_on_fail_is_rejected_naming_valid_variants() {
        let err =
            toml::from_str::<TargetHooks>("pre_deploy = \"gate\"\npre_deploy_on_fail = \"bogus\"")
                .expect_err("an unknown `pre_deploy_on_fail` value must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("abort") || msg.contains("skip") || msg.contains("variant"),
            "HOOK-PREDEPLOY-001: an invalid `pre_deploy_on_fail` value must fail with an \
             unknown-variant error naming the valid `abort`/`skip` choices, not the generic \
             `unknown field` rejection; got: {err}"
        );
    }

    #[test]
    fn well_formed_dep_hooks_subtable_produces_a_candidate_and_no_diagnostic() {
        let opaque: toml::Value = toml::from_str("[editor]\non_change = \"./install.sh\"\n")
            .expect("a well-formed opaque hooks payload parses as toml");
        let node = FetchNode::new("https://github.com/dep/nvim.git", "main", "deadbeef");
        let instance = Instance::new("root", "dep", "anchor", node);

        let (candidates, diagnostics) =
            admit_transitive_hooks(&opaque, "editor", "ns%1%editor", &instance);

        assert_eq!(
            candidates.len(),
            1,
            "a well-formed dep hook must still be admitted as exactly one candidate"
        );
        assert!(
            diagnostics.is_empty(),
            "a well-formed sub-table must produce no parse-failure diagnostic, got: {diagnostics:?}"
        );
    }
}
