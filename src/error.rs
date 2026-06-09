//! Crate-wide error type.
//!
//! A single [`Error`] spans every domain for now. Per-module error enums can be
//! split out later without a mechanical one-enum-per-module rule.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
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

    #[error("registry error: {0}")]
    Registry(String),

    #[error("projection error: {0}")]
    Projection(String),

    #[error("sync error: {0}")]
    Sync(String),

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
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
