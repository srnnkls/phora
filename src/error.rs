//! Crate-wide error aggregate at the CLI edge.
//!
//! Bounded contexts own their own enums ([`crate::source::SourceError`],
//! [`crate::store::StoreError`]); this type aggregates them via `From` for
//! propagation and exit-code mapping at the binary boundary.

use thiserror::Error;

use crate::kernel::KernelError;
use crate::source::SourceError;
use crate::store::StoreError;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    SourceCtx(#[from] SourceError),

    #[error(transparent)]
    StoreCtx(#[from] StoreError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("lock error: {0}")]
    Lock(String),

    #[error("matcher error: {0}")]
    Matcher(#[from] globset::Error),

    #[error("source error: {0}")]
    Source(String),

    #[error("root path not found in tree: {root}")]
    RootNotFound { root: std::path::PathBuf },

    #[error("artifact not found in tree: {artifact}")]
    ArtifactNotFound { artifact: String },

    #[error("symlink not allowed: {path} (set allow_symlinks=true to permit)")]
    SymlinkNotAllowed { path: std::path::PathBuf },

    #[error("submodule not allowed: {path} (set allow_submodules=true to permit)")]
    SubmoduleNotAllowed { path: std::path::PathBuf },

    #[error("registry error: {0}")]
    Registry(String),

    #[error("projection error: {0}")]
    Projection(String),

    #[error("sync error: {0}")]
    Sync(String),

    #[error("artifact `{artifact}` collides in target `{target}` from sources: {sources:?}")]
    Collision {
        artifact: String,
        sources: Vec<String>,
        target: String,
    },

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("aborted by user")]
    Aborted,
}

impl From<KernelError> for Error {
    fn from(err: KernelError) -> Self {
        Self::SourceCtx(SourceError::from(err))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_implemented_renders() {
        assert!(
            Error::NotImplemented("x")
                .to_string()
                .contains("not implemented")
        );
    }
}
