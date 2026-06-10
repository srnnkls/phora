//! Kernel value objects: parsed-at-the-boundary primitives shared across contexts.

mod commit;
mod digest;
mod name;
mod path;
mod project_id;
mod selection;

pub use commit::Commit;
pub use digest::{Algo, Digest};
pub use name::{ArtifactName, SourceName};
pub use path::RelPath;
pub use project_id::ProjectId;
pub use selection::Selection;

pub(crate) use name::safe_component;
