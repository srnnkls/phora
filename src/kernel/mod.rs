//! Kernel value objects: parsed-at-the-boundary primitives shared across contexts.

mod commit;
mod digest;
mod path;

pub use commit::Commit;
pub use digest::{Algo, Digest};
pub use path::RelPath;
