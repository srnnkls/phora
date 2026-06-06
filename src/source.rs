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

    #[expect(
        clippy::too_many_arguments,
        reason = "collapses into an ExportRequest struct when GitBackend::export_artifact is implemented (PAM-016)"
    )]
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
    fn scp_style_ssh_drops_userinfo_and_strips_git_suffix() {
        assert_eq!(
            NormalizedUrl::parse("git@github.com:user/repo.git").as_str(),
            "github.com/user/repo"
        );
    }

    #[test]
    fn https_strips_scheme_and_git_suffix() {
        assert_eq!(
            NormalizedUrl::parse("https://github.com/user/repo.git").as_str(),
            "github.com/user/repo"
        );
    }

    #[test]
    fn host_is_lowercased_but_path_case_is_preserved() {
        assert_eq!(
            NormalizedUrl::parse("https://GitHub.com/User/Repo").as_str(),
            "github.com/User/Repo"
        );
    }

    #[test]
    fn ssh_scheme_drops_scheme_and_userinfo() {
        assert_eq!(
            NormalizedUrl::parse("ssh://git@github.com/user/repo.git").as_str(),
            "github.com/user/repo"
        );
    }

    #[test]
    fn trailing_slash_is_trimmed() {
        assert_eq!(
            NormalizedUrl::parse("https://github.com/user/repo/").as_str(),
            "github.com/user/repo"
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            NormalizedUrl::parse("  https://github.com/user/repo.git  ").as_str(),
            "github.com/user/repo"
        );
    }

    #[test]
    fn local_path_normalizes_deterministically() {
        let first = NormalizedUrl::parse("/home/x/dev/loqui");
        let second = NormalizedUrl::parse("/home/x/dev/loqui");
        assert_eq!(first, second);
        assert_eq!(first.as_str(), "/home/x/dev/loqui");
    }

    #[test]
    fn equivalent_ssh_and_https_forms_share_one_mirror_key() {
        let ssh = MirrorKey::from_url(&NormalizedUrl::parse("git@github.com:user/repo.git"));
        let https = MirrorKey::from_url(&NormalizedUrl::parse("https://github.com/user/repo.git"));
        let ssh_scheme =
            MirrorKey::from_url(&NormalizedUrl::parse("ssh://git@github.com/user/repo"));
        assert_eq!(ssh, https);
        assert_eq!(https, ssh_scheme);
    }

    #[test]
    fn mirror_key_is_sixteen_hex_chars() {
        let key = MirrorKey::from_url(&NormalizedUrl::parse("https://github.com/user/repo.git"));
        assert_eq!(key.as_str().len(), 16);
        assert!(key.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mirror_key_is_deterministic_for_same_input() {
        let first = MirrorKey::from_url(&NormalizedUrl::parse("https://github.com/user/repo"));
        let second = MirrorKey::from_url(&NormalizedUrl::parse("https://github.com/user/repo"));
        assert_eq!(first, second);
    }

    #[test]
    fn different_repos_produce_different_keys() {
        let one = MirrorKey::from_url(&NormalizedUrl::parse("https://github.com/user/repo-a"));
        let two = MirrorKey::from_url(&NormalizedUrl::parse("https://github.com/user/repo-b"));
        assert_ne!(one, two);
    }

    #[test]
    fn mirror_key_matches_blake3_of_normalized_url_truncated_to_sixteen() {
        let url = "git@github.com:user/repo.git";
        let normalized = NormalizedUrl::parse(url);
        let expected = blake3::hash(b"github.com/user/repo").to_hex()[..16].to_string();
        assert_eq!(MirrorKey::from_url(&normalized).as_str(), expected);
    }

    #[test]
    fn mirror_path_is_git_dir_joined_with_key_dot_git() {
        let git_dir = PathBuf::from("/var/phora/git");
        let backend = GitBackend::new(git_dir.clone());
        let url = "git@github.com:user/repo.git";
        let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
        assert_eq!(
            backend.mirror_path(url),
            git_dir.join(format!("{}.git", key.as_str()))
        );
    }

    #[test]
    fn mirror_path_unifies_equivalent_urls_to_one_directory() {
        let backend = GitBackend::new(PathBuf::from("/var/phora/git"));
        assert_eq!(
            backend.mirror_path("git@github.com:user/repo.git"),
            backend.mirror_path("https://github.com/user/repo")
        );
    }
}
