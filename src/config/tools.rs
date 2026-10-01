//! Consumer grants for the tools dependency builds name.

use std::path::PathBuf;

use serde::Deserialize;

/// How a granted tool identity becomes an executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGrant {
    /// The dependency's pinned version, resolved and installed through mise.
    Mise,
    /// This executable, whatever version the dependency pins.
    Path(PathBuf),
}

impl<'de> Deserialize<'de> for ToolGrant {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct GrantTable {
            mise: Option<bool>,
            path: Option<PathBuf>,
        }
        match GrantTable::deserialize(deserializer)? {
            GrantTable {
                mise: Some(true),
                path: None,
            } => Ok(Self::Mise),
            GrantTable {
                mise: None | Some(false),
                path: Some(path),
            } => Ok(Self::Path(path)),
            _ => Err(serde::de::Error::custom(
                "a tool grant sets exactly one of `mise = true` or `path = \"…\"`",
            )),
        }
    }
}

pub(crate) fn grant_hint(identity: &str) -> String {
    format!("grant it under `[tools.\"{identity}\"]` with `mise = true` or `path = \"…\"`")
}
