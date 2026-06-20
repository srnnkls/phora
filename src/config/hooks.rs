//! Hook DTOs for `[targets.X.hooks]` and the global `[hooks]` table.

use serde::Deserialize;

use crate::config::transitive::Instance;

pub(crate) const DEFAULT_SHELL_PREFIX: &str = "sh -c";

/// An interpreted transitive `on_change` hook awaiting the trust decision; trust is
/// consumer-owned, so a candidate carries no approval state — a dep can never self-approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateHook {
    pub dep_instance: String,
    pub hook_id: String,
    pub command: HookCommand,
}

/// Interprets a dep target's retained opaque `[targets.X.hooks]` into candidate hooks
/// keyed by the confined [`Instance`]. Strip-by-default holds: this produces candidates
/// only — it runs nothing, retains nothing, and never marks a hook trusted.
#[must_use]
pub fn admit_transitive_hooks(
    opaque: &toml::Value,
    dep_target_name: &str,
    composed_target_name: &str,
    instance: &Instance,
) -> Vec<CandidateHook> {
    let Some(hooks) = opaque.get(dep_target_name) else {
        return Vec::new();
    };
    let Ok(hooks) = hooks.clone().try_into::<TargetHooks>() else {
        return Vec::new();
    };
    let Some(on_change) = hooks.on_change else {
        return Vec::new();
    };
    on_change
        .into_iter()
        .map(|command| CandidateHook {
            dep_instance: instance.stable_key(),
            hook_id: format!(
                "{composed_target_name}#on_change#{}",
                command_discriminator(&command)
            ),
            command,
        })
        .collect()
}

/// Like [`admit_transitive_hooks`] but surfaces a diagnostic for each `[targets.X.hooks]`
/// sub-table that fails to deserialize, instead of silently dropping it.
#[must_use]
pub fn admit_transitive_hooks_checked(
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
    let shell = command.shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX);
    let mut hasher = blake3::Hasher::new();
    for field in [command.run.as_str(), shell, kind, commit_sha] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Length-framed so an injected `#` cannot forge a trust key by colliding two distinct commands.
fn command_discriminator(command: &HookCommand) -> String {
    let shell = command.shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX);
    let mut hasher = blake3::Hasher::new();
    for field in [command.run.as_str(), shell] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.finalize().to_hex()[..16].to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommand {
    pub run: String,
    /// Defaults to `sh -c`. Split on whitespace at dispatch, so an interpreter
    /// path containing spaces is unsupported.
    pub shell: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookWhen {
    #[default]
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetHooks {
    #[serde(default, deserialize_with = "deserialize_commands")]
    pub on_change: Option<Vec<HookCommand>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalHooks {
    #[serde(default, deserialize_with = "deserialize_commands")]
    pub post_sync: Option<Vec<HookCommand>>,
    #[serde(default)]
    pub when: HookWhen,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookTable {
    run: String,
    shell: Option<String>,
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
                f.write_str("a command string or a `{ run, shell }` table")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                validated_command(v.to_owned(), None)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let table =
                    HookTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                validated_command(table.run, table.shell)
            }
        }

        deserializer.deserialize_any(CommandVisitor)
    }
}

fn validated_command<E: serde::de::Error>(
    run: String,
    shell: Option<String>,
) -> std::result::Result<HookCommand, E> {
    if run.trim().is_empty() {
        return Err(E::custom("hook command `run` must not be empty"));
    }
    if let Some(shell) = &shell
        && shell.trim().is_empty()
    {
        return Err(E::custom("hook command `shell` must not be empty"));
    }
    Ok(HookCommand { run, shell })
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
            Ok(vec![validated_command(v.to_owned(), None)?])
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
        HookCommand {
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
            admit_transitive_hooks_checked(&opaque, "editor", "ns%1%editor", &instance);

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

    #[test]
    fn well_formed_dep_hooks_subtable_produces_a_candidate_and_no_diagnostic() {
        let opaque: toml::Value = toml::from_str("[editor]\non_change = \"./install.sh\"\n")
            .expect("a well-formed opaque hooks payload parses as toml");
        let node = FetchNode::new("https://github.com/dep/nvim.git", "main", "deadbeef");
        let instance = Instance::new("root", "dep", "anchor", node);

        let (candidates, diagnostics) =
            admit_transitive_hooks_checked(&opaque, "editor", "ns%1%editor", &instance);

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
