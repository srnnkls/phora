//! Phora: a git-based artifact package manager.
//!
//! The crate follows a hexagonal layering:
//! - **domain** — config DTOs ([`config`]), path/identity newtypes ([`paths`]),
//!   pattern matching ([`matcher`]), and the orchestration in [`sync`]/[`projection`].
//! - **ports** — the [`source::SourceBackend`] and [`registry::Registry`] traits.
//! - **adapters** — [`source::GitBackend`] and [`registry::FileRegistry`], kept beside
//!   their port traits rather than in separate directories.
//!
//! Boundary inputs are parsed into validated newtypes ([`paths::ProjectId`],
//! [`source::NormalizedUrl`], [`source::MirrorKey`], [`registry::Digest`]) so that
//! illegal states are unrepresentable downstream — parse, don't validate.

// Stub signatures return `NotImplemented`; per-function `# Errors` docs land with
// the real bodies.
#![allow(clippy::missing_errors_doc)]

pub mod cli;
pub mod config;
pub mod error;
pub mod matcher;
pub mod paths;
pub mod projection;
pub mod registry;
pub mod source;
pub mod sync;
