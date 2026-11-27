//! Persisted forms of `project.toml` and the state-root index, parsed once into domain types.

use serde::{Deserialize, Serialize};

use super::model::{MetaRead, StateRootIndex, WireDefect};
use crate::mount::NormalPath;
use crate::sync::state::ProjectId;

#[derive(Serialize, Deserialize)]
struct ProjectMetaWire {
    version: u32,
    root: PathWire,
    on_state_mount: bool,
    state_dir: PathWire,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded_by: Option<PathWire>,
}

/// `Utf8` when the path is valid UTF-8, hex-encoded bytes otherwise.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PathWire {
    Utf8(String),
    Bytes { hex: String },
}

#[derive(Serialize, Deserialize)]
struct StateRootIndexWire {
    version: u32,
    default_root: PathWire,
    #[serde(default)]
    roots: Vec<RegisteredRootWire>,
}

#[derive(Serialize, Deserialize)]
struct RegisteredRootWire {
    path: PathWire,
    on_state_mount: bool,
}

impl TryFrom<PathWire> for NormalPath {
    type Error = WireDefect;

    fn try_from(_wire: PathWire) -> Result<Self, Self::Error> {
        todo!()
    }
}

impl TryFrom<StateRootIndexWire> for StateRootIndex {
    type Error = WireDefect;

    fn try_from(_wire: StateRootIndexWire) -> Result<Self, Self::Error> {
        todo!()
    }
}

/// The root must hash to `id`, so the meta parses against the dir it was read from.
pub(super) fn parse_meta(_text: &str, _id: &ProjectId) -> MetaRead {
    todo!()
}

pub(super) fn parse_index(_text: &str) -> Result<StateRootIndex, WireDefect> {
    todo!()
}
