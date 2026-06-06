//! Source port (`SourceBackend`) and its git adapter (`GitBackend`).

use std::path::{Path, PathBuf};

use crate::config::Refspec;
use crate::error::{Error, Result};
use crate::matcher::PathMatcher;
use crate::registry::ManifestFile;

#[derive(Debug, Clone)]
pub struct ExportPolicy {
    pub allow_symlinks: bool,
    pub allow_submodules: bool,
    pub preserve_executable: bool,
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self {
            allow_symlinks: false,
            allow_submodules: false,
            preserve_executable: true,
        }
    }
}

#[derive(Debug)]
pub struct ExportResult {
    pub files: Vec<ManifestFile>,
    pub digest: String,
}

/// `source` is the human name (diagnostics); `url` identifies the bare mirror,
/// keyed by normalized-URL hash.
pub trait SourceBackend {
    fn fetch(&self, source: &str, url: &str) -> Result<()>;

    fn resolve(&self, source: &str, url: &str, refspec: &Refspec) -> Result<String>;

    fn commit_time(&self, source: &str, url: &str, commit: &str) -> Result<u64>;

    fn discover_artifacts(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        matcher: &PathMatcher,
    ) -> Result<Vec<String>>;

    #[allow(clippy::too_many_arguments)]
    fn export_artifact(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        artifact: &str,
        matcher: &PathMatcher,
        policy: &ExportPolicy,
        staging_dir: &Path,
        commit_time: u64,
    ) -> Result<ExportResult>;

    fn compute_digest(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        matcher: &PathMatcher,
    ) -> Result<String>;
}

/// Canonical git URL: equivalent forms collapse to one mirror key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedUrl(String);

impl NormalizedUrl {
    /// Strips a trailing `.git`, rewrites scp-style ssh (`git@host:owner/repo`) to
    /// `host/owner/repo`, drops scheme/userinfo, and lowercases the host.
    #[must_use]
    pub fn parse(url: &str) -> Self {
        let s = url.trim().trim_end_matches('/');
        let s = if let Some(rest) = s.strip_prefix("git@") {
            rest.replacen(':', "/", 1)
        } else {
            let no_scheme = s.split_once("://").map_or(s, |(_, rest)| rest);
            match no_scheme.split_once('@') {
                Some((_, host_and_path)) => host_and_path.to_string(),
                None => no_scheme.to_string(),
            }
        };
        let s = s.strip_suffix(".git").unwrap_or(&s);
        let normalized = match s.split_once('/') {
            Some((host, path)) => format!("{}/{path}", host.to_lowercase()),
            None => s.to_lowercase(),
        };
        Self(normalized)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Mirror directory key: BLAKE3 of a [`NormalizedUrl`], first 16 hex chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorKey(String);

impl MirrorKey {
    #[must_use]
    pub fn from_url(url: &NormalizedUrl) -> Self {
        let hash = blake3::hash(url.as_str().as_bytes());
        Self(hash.to_hex()[..16].to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct GitBackend {
    git_dir: PathBuf,
}

impl GitBackend {
    #[must_use]
    pub fn new(git_dir: PathBuf) -> Self {
        Self { git_dir }
    }

    fn mirror_path(&self, url: &str) -> PathBuf {
        let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
        self.git_dir.join(format!("{}.git", key.as_str()))
    }
}

impl SourceBackend for GitBackend {
    fn fetch(&self, _source: &str, url: &str) -> Result<()> {
        let _mirror = self.mirror_path(url);
        Err(Error::NotImplemented("GitBackend::fetch"))
    }

    fn resolve(&self, _source: &str, _url: &str, _refspec: &Refspec) -> Result<String> {
        Err(Error::NotImplemented("GitBackend::resolve"))
    }

    fn commit_time(&self, _source: &str, _url: &str, _commit: &str) -> Result<u64> {
        Err(Error::NotImplemented("GitBackend::commit_time"))
    }

    fn discover_artifacts(
        &self,
        _source: &str,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _matcher: &PathMatcher,
    ) -> Result<Vec<String>> {
        Err(Error::NotImplemented("GitBackend::discover_artifacts"))
    }

    fn export_artifact(
        &self,
        _source: &str,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _artifact: &str,
        _matcher: &PathMatcher,
        _policy: &ExportPolicy,
        _staging_dir: &Path,
        _commit_time: u64,
    ) -> Result<ExportResult> {
        Err(Error::NotImplemented("GitBackend::export_artifact"))
    }

    fn compute_digest(
        &self,
        _source: &str,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _matcher: &PathMatcher,
    ) -> Result<String> {
        Err(Error::NotImplemented("GitBackend::compute_digest"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ssh_and_https_to_same_form() {
        assert_eq!(
            NormalizedUrl::parse("git@github.com:user/repo.git").as_str(),
            "github.com/user/repo"
        );
        assert_eq!(
            NormalizedUrl::parse("https://GitHub.com/user/repo.git").as_str(),
            "github.com/user/repo"
        );
    }
}
