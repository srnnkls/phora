//! Kernel value objects: parsed-at-the-boundary primitives shared across contexts.

mod commit;
mod digest;
mod name;
mod path;
mod project_id;
mod selection;

pub use commit::Commit;
pub use digest::{Algo, Digest};
pub use name::{ArtifactName, KernelError, SourceName, TargetName};
pub use path::RelPath;
pub use project_id::ProjectId;
pub use selection::Selection;

pub(crate) use name::{locator_basename, safe_component, safe_relpath};
