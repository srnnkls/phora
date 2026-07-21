//! Compatibility facade for state APIs now owned by `sync::state`.

pub use crate::digest::Digest;
pub use crate::sync::model::ScannedFile;
pub use crate::sync::state::file::{
    ArtifactKey, EjectedEntry, FileRegistry, FrozenReadOnlyRegistry, HookState, ManifestFile,
    ProjectedRecord, RecordKind, Registry, RegistryRecord, StoreError, ejected_index,
    readonly_root_error,
};
pub use crate::sync::state::locking::StateLockGuard;

#[cfg(test)]
pub use crate::sync::state::locking::STATE_LOCK_SERIAL;
#[cfg(test)]
pub use crate::sync::state::locking::assert_git_sandboxed;
#[cfg(test)]
pub use crate::sync::state::locking::guard_git_fork;
