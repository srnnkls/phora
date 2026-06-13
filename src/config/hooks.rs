//! Hook DTOs for `[targets.X.hooks]` and the global `[hooks]` table.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommand {
    pub run: String,
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
                command(v.to_owned(), None)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let table =
                    HookTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                command(table.run, table.shell)
            }
        }

        deserializer.deserialize_any(CommandVisitor)
    }
}

fn command<E: serde::de::Error>(
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
            Ok(vec![command(v.to_owned(), None)?])
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
