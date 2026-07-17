use crate::diagnostic::SelectionDiagnostic;
use crate::error::Error;

/// A non-fatal take/collapse outcome carried up from the projection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectionWarning {
    TakeNoMatchGlob(String),
    LostCollapseToExclude(String),
}

/// A structured projection failure. Each variant preserves the rendered diagnostic
/// so the CLI boundary shows the same text; `From<ProjectionError>` unwraps it.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("{rendered}")]
    LeafNotOffered { rendered: Box<Error> },
    #[error("{rendered}")]
    DuplicateDestination { rendered: Box<Error> },
    #[error("{rendered}")]
    CollapseBlocked { rendered: Box<Error> },
    #[error("{rendered}")]
    Other { rendered: Box<Error> },
}

impl From<ProjectionError> for Error {
    fn from(error: ProjectionError) -> Self {
        match error {
            ProjectionError::LeafNotOffered { rendered }
            | ProjectionError::DuplicateDestination { rendered }
            | ProjectionError::CollapseBlocked { rendered }
            | ProjectionError::Other { rendered } => *rendered,
        }
    }
}

pub(crate) fn unsafe_leaf(entry: &str) -> ProjectionError {
    ProjectionError::Other {
        rendered: Box::new(unsafe_target_path(entry)),
    }
}

fn unsafe_target_path(dest: &str) -> Error {
    SelectionDiagnostic {
        entry: dest.to_owned(),
        matched_against: "the target root".to_owned(),
        why: "destination is not a portable relative path".to_owned(),
        did_you_mean: None,
        remedy: "use a forward-slashed relative path inside the target".to_owned(),
        debug_hint: Some("phora preview --files".to_owned()),
        details: Vec::new(),
    }
    .sync()
}
