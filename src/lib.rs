//! Phora: a git-based artifact package manager.
//!
//! Compatibility is guaranteed for the command-line interface and serialized
//! configuration, lock, journal, and state formats. The Rust library API is
//! intentionally unstable and may change between releases.

#![expect(
    clippy::missing_errors_doc,
    reason = "stub signatures return NotImplemented; per-fn `# Errors` docs land with the real bodies"
)]

pub mod cli;
pub mod config;
pub mod diagnostic;
pub mod digest;
pub mod error;
pub mod lock;
pub mod paths;
pub mod projection;
pub mod source;
pub mod sync;
