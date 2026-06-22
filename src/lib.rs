//! Phora: a git-based artifact package manager.
//!
//! The crate follows a hexagonal layering:
//! - **domain** — config DTOs ([`config`]), path/identity newtypes ([`paths`]),
//!   offer selection ([`kernel::OfferSelection`]), and the orchestration in [`sync`]/[`deploy`].
//! - **ports** — the [`source::SourceBackend`] and [`store::Registry`] traits.
//! - **adapters** — [`source::GitBackend`] and [`store::FileRegistry`], kept beside
//!   their port traits rather than in separate directories.
//!
//! Boundary inputs are parsed into validated newtypes ([`kernel::ProjectId`],
//! [`source::NormalizedUrl`], [`source::MirrorKey`], [`kernel::Digest`]) so that
//! illegal states are unrepresentable downstream — parse, don't validate.

#![expect(
    clippy::missing_errors_doc,
    reason = "stub signatures return NotImplemented; per-fn `# Errors` docs land with the real bodies"
)]

pub mod archive;
pub mod backend;
pub mod cli;
pub mod config;
pub mod deploy;
pub mod diagnostic;
pub mod error;
pub mod http;
pub mod kernel;
pub mod lock;
pub mod paths;
pub mod source;
pub mod store;
pub mod sync;
