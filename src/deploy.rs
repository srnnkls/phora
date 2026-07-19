//! Compatibility facade for the deployment API now owned by `sync`.

pub use crate::sync::apply::{copy_file, copy_tree, deploy_artifact, link_artifact};
pub use crate::sync::inspect::{ArtifactState, check_artifact_state};
pub use crate::sync::journal::{Journal, JournalEntry};
pub use crate::sync::recovery::recovery_sweep;
