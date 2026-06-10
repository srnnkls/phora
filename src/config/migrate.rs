//! Deprecation warnings for legacy source-key spellings.

use std::path::Path;

use super::source::Source;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationWarning {
    GitLocalpath { source: String },
    HostPathForge { source: String },
    PathShorthand { source: String },
}

impl MigrationWarning {
    #[must_use]
    pub fn source(&self) -> &str {
        match self {
            Self::GitLocalpath { source }
            | Self::HostPathForge { source }
            | Self::PathShorthand { source } => source,
        }
    }

    #[must_use]
    pub fn suggested_key(&self) -> &str {
        match self {
            Self::GitLocalpath { .. } => "path",
            Self::HostPathForge { .. } | Self::PathShorthand { .. } => "repo",
        }
    }
}

impl std::fmt::Display for MigrationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = self.source();
        match self {
            Self::GitLocalpath { .. } => write!(
                f,
                "source `{source}`: `git = <local path>` is deprecated; use the `path` key for local sources"
            ),
            Self::HostPathForge { .. } => write!(
                f,
                "source `{source}`: `host` + `path` is the deprecated forge alias; use the `repo` key"
            ),
            Self::PathShorthand { .. } => write!(
                f,
                "source `{source}`: bare `path` no longer means the github shorthand; use the bare `repo` key for owner/repo"
            ),
        }
    }
}

/// `base_dir` resolves a relative `path` for the local-dir existence check.
pub(super) fn warning_for(
    name: &str,
    source: &Source,
    base_dir: &Path,
) -> Option<MigrationWarning> {
    if let Some(git) = &source.git {
        return (!is_git_url(git)).then(|| MigrationWarning::GitLocalpath {
            source: name.to_owned(),
        });
    }
    if source.host.is_some() && source.repo.is_none() && source.path.is_some() {
        return Some(MigrationWarning::HostPathForge {
            source: name.to_owned(),
        });
    }
    if source.host.is_none()
        && source.repo.is_none()
        && let Some(path) = &source.path
        && looks_like_github_shorthand(path, base_dir)
    {
        return Some(MigrationWarning::PathShorthand {
            source: name.to_owned(),
        });
    }
    None
}

fn is_git_url(value: &str) -> bool {
    if value.contains("://") {
        return true;
    }
    match (value.find('@'), value.find(':')) {
        (Some(at), Some(colon)) => at < colon,
        _ => false,
    }
}

fn looks_like_github_shorthand(path: &str, base_dir: &Path) -> bool {
    if path.starts_with('/') || path.starts_with("./") || path.starts_with('~') {
        return false;
    }
    let mut segments = path.split('/');
    let (Some(owner), Some(repo), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    if owner.is_empty() || repo.is_empty() {
        return false;
    }
    !base_dir.join(path).is_dir()
}
