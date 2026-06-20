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
