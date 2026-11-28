//! Persisted forms of `project.toml` and the state-root index, parsed once into domain types.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::model::{
    MetaDefect, MetaRead, ProjectMeta, ProjectRoot, RootlessMeta, StateRoot, StateRootIndex,
    Succession, WireDefect,
};
use crate::mount::{AnchorDir, NormalPath, Placement};
use crate::sync::state::ProjectId;

const META_VERSION: u32 = 1;
const INDEX_VERSION: u32 = 1;

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

impl From<&NormalPath> for PathWire {
    fn from(path: &NormalPath) -> Self {
        let os = path.as_path().as_os_str();
        os.to_str().map_or_else(
            || Self::Bytes {
                hex: os.as_bytes().iter().fold(String::new(), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }),
            },
            |utf8| Self::Utf8(utf8.to_owned()),
        )
    }
}

impl TryFrom<PathWire> for NormalPath {
    type Error = WireDefect;

    fn try_from(wire: PathWire) -> Result<Self, Self::Error> {
        let path = match wire {
            PathWire::Utf8(text) => PathBuf::from(text),
            PathWire::Bytes { hex } => PathBuf::from(OsString::from_vec(decode_hex(&hex)?)),
        };
        Self::lexical(&path).ok_or(WireDefect::NotAbsolute)
    }
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, WireDefect> {
    if !hex.len().is_multiple_of(2) {
        return Err(WireDefect::BadHex);
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| {
            hex.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or(WireDefect::BadHex)
        })
        .collect()
}

fn placement_of(on_state_mount: bool) -> Placement {
    if on_state_mount {
        Placement::OnAnchorMount
    } else {
        Placement::Elsewhere
    }
}

impl TryFrom<StateRootIndexWire> for StateRootIndex {
    type Error = WireDefect;

    fn try_from(wire: StateRootIndexWire) -> Result<Self, Self::Error> {
        if wire.version != INDEX_VERSION {
            return Err(WireDefect::UnsupportedVersion(wire.version));
        }
        let roots = wire
            .roots
            .into_iter()
            .map(|entry| {
                Ok((
                    StateRoot::new(entry.path.try_into()?),
                    placement_of(entry.on_state_mount),
                ))
            })
            .collect::<Result<_, WireDefect>>()?;
        Ok(Self::new(
            AnchorDir::new(wire.default_root.try_into()?),
            roots,
        ))
    }
}

/// The root must hash to `id`, so the meta parses against the dir it was read from.
pub(super) fn parse_meta(text: &str, id: &ProjectId) -> MetaRead {
    match meta_from_text(text, id) {
        Ok((meta, succession)) => MetaRead::Valid { meta, succession },
        Err(defect) => MetaRead::Rootless(RootlessMeta::Invalid(defect)),
    }
}

impl From<WireDefect> for MetaDefect {
    fn from(defect: WireDefect) -> Self {
        Self::Wire(defect)
    }
}

fn meta_from_text(text: &str, id: &ProjectId) -> Result<(ProjectMeta, Succession), MetaDefect> {
    let wire: ProjectMetaWire = toml::from_str(text).map_err(|_| WireDefect::Unparsable)?;
    if wire.version != META_VERSION {
        return Err(WireDefect::UnsupportedVersion(wire.version).into());
    }
    let root =
        ProjectRoot::recorded(wire.root.try_into()?, id).ok_or(MetaDefect::RootHashMismatch)?;
    let meta = ProjectMeta::new(
        root,
        placement_of(wire.on_state_mount),
        AnchorDir::new(wire.state_dir.try_into()?),
    );
    let succession = match wire.superseded_by {
        None => Succession::Current,
        Some(successor) => Succession::SupersededBy(StateRoot::new(successor.try_into()?)),
    };
    Ok((meta, succession))
}

pub(super) fn render_meta(
    meta: &ProjectMeta,
    succession: &Succession,
) -> Result<String, toml::ser::Error> {
    toml::to_string(&ProjectMetaWire {
        version: META_VERSION,
        root: meta.root().path().into(),
        on_state_mount: *meta.placement() == Placement::OnAnchorMount,
        state_dir: meta.state_dir().path().into(),
        superseded_by: match succession {
            Succession::Current => None,
            Succession::SupersededBy(successor) => Some(successor.path().into()),
        },
    })
}

pub(super) fn parse_index(text: &str) -> Result<StateRootIndex, WireDefect> {
    toml::from_str::<StateRootIndexWire>(text)
        .map_err(|_| WireDefect::Unparsable)?
        .try_into()
}

pub(super) fn render_index(index: &StateRootIndex) -> Result<String, toml::ser::Error> {
    toml::to_string(&StateRootIndexWire {
        version: INDEX_VERSION,
        default_root: index.anchor().path().into(),
        roots: index
            .roots()
            .iter()
            .map(|(root, placement)| RegisteredRootWire {
                path: root.path().into(),
                on_state_mount: *placement == Placement::OnAnchorMount,
            })
            .collect(),
    })
}
