//! Projection model: the planned deployment unit produced by collapse.

use crate::projection::take::ResolvedTake;

/// One planned deployment unit: a collapsed directory or a single kept leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Materialization {
    /// A whole directory deployed as one artifact, rooted at `dir`.
    CollapsedDir { dir: String },
    /// A single kept leaf deployed on its own.
    Leaf(ResolvedTake),
}

impl Materialization {
    /// The published artifact key: the collapsed dir, or the leaf's destination.
    #[must_use]
    pub fn published_key(&self) -> &str {
        match self {
            Materialization::CollapsedDir { dir } => dir,
            Materialization::Leaf(take) => &take.dest,
        }
    }
}
