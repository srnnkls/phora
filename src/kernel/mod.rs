//! Kernel value objects: parsed-at-the-boundary primitives shared across contexts.

mod collapse;
mod commit;
mod digest;
mod name;
mod path;
mod project_id;
mod selection;
pub(crate) mod take;

pub use collapse::{
    CollapseChoice, CollapseMode, CollapsePlan, CollapseWarning, Materialization, plan_collapse,
};
pub use commit::Commit;
pub use digest::{Algo, Digest};
pub use name::{ArtifactName, KernelError, SourceName, TargetName};
pub use path::RelPath;
pub use project_id::ProjectId;
pub use selection::{OfferSelection, compile_take_glob};
pub use take::{ResolvedTake, Take, TakeResolution, TakeWarning, is_take_glob, resolve_take};

pub(crate) use name::{safe_component, safe_relpath};
pub(crate) use take::fold_dest;
