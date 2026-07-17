//! Kernel value objects: parsed-at-the-boundary primitives shared across contexts.

mod commit;
mod digest;
mod name;
mod path;
mod project_id;

pub use crate::projection::collapse::{
    CollapseChoice, CollapseMode, CollapsePlan, CollapseWarning, plan_collapse,
};
pub use crate::projection::model::Materialization;
pub use crate::projection::offer::{OfferSelection, compile_take_glob};
pub use crate::projection::take::{
    ResolvedTake, Take, TakeResolution, TakeWarning, is_take_glob, resolve_take,
};
pub use commit::Commit;
pub use digest::{Algo, Digest};
pub use name::{ArtifactName, KernelError, SourceName, TargetName};
pub use path::RelPath;
pub use project_id::ProjectId;

pub(crate) use crate::projection::take::fold_dest;
pub(crate) use name::{safe_component, safe_relpath};
