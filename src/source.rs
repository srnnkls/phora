//! Source port (`SourceBackend`) and its git adapter (`GitBackend`).

use std::path::{Path, PathBuf};

use crate::config::Refspec;
use crate::error::{Error, Result};
use crate::matcher::PathMatcher;
use crate::registry::ManifestFile;

/// gix clones origin as refs/remotes/origin/*; a mirror must update refs/heads/* directly.
const MIRROR_REFSPEC: &str = "+refs/heads/*:refs/heads/*";

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
    fn fetch(&self, source: &str, url: &str) -> Result<()> {
        let mirror = self.mirror_path(url);

        if mirror.exists() {
            let repo = gix::open(&mirror)
                .map_err(|e| Error::Source(format!("open mirror {source}: {e}")))?;
            let mut remote = repo
                .find_remote("origin")
                .map_err(|e| Error::Source(format!("find origin in {source}: {e}")))?;
            remote
                .replace_refspecs([MIRROR_REFSPEC], gix::remote::Direction::Fetch)
                .map_err(|e| Error::Source(format!("set mirror refspec in {source}: {e}")))?;
            remote
                .connect(gix::remote::Direction::Fetch)
                .map_err(|e| Error::Source(format!("connect origin in {source}: {e}")))?
                .prepare_fetch(
                    gix::progress::Discard,
                    gix::remote::ref_map::Options::default(),
                )
                .map_err(|e| Error::Source(format!("prepare fetch in {source}: {e}")))?
                .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
                .map_err(|e| Error::Source(format!("receive pack in {source}: {e}")))?;
        } else {
            gix::prepare_clone_bare(url, &mirror)
                .map_err(|e| Error::Source(format!("prepare clone {source}: {e}")))?
                .configure_remote(|mut remote| {
                    remote.replace_refspecs([MIRROR_REFSPEC], gix::remote::Direction::Fetch)?;
                    Ok(remote)
                })
                .fetch_only(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
                .map_err(|e| Error::Source(format!("clone bare {source}: {e}")))?;
        }

        Ok(())
    }

    fn resolve(&self, source: &str, url: &str, refspec: &Refspec) -> Result<String> {
        let mirror = self.mirror_path(url);
        let repo =
            gix::open(&mirror).map_err(|e| Error::Source(format!("open mirror {source}: {e}")))?;

        let commit = match refspec {
            Refspec::Branch(name) => repo
                .find_reference(&format!("refs/heads/{name}"))
                .map_err(|e| Error::Source(format!("branch {name} in {source}: {e}")))?
                .peel_to_commit()
                .map_err(|e| Error::Source(format!("peel branch {name} in {source}: {e}")))?,
            Refspec::Tag(name) => repo
                .find_reference(&format!("refs/tags/{name}"))
                .map_err(|e| Error::Source(format!("tag {name} in {source}: {e}")))?
                .peel_to_commit()
                .map_err(|e| Error::Source(format!("peel tag {name} in {source}: {e}")))?,
            Refspec::Rev(rev) => {
                let oid = gix::ObjectId::from_hex(rev.as_bytes())
                    .map_err(|e| Error::Source(format!("parse rev {rev} in {source}: {e}")))?;
                repo.find_commit(oid)
                    .map_err(|e| Error::Source(format!("rev {rev} in {source}: {e}")))?
            }
        };

        Ok(commit.id().to_hex().to_string())
    }

    fn commit_time(&self, source: &str, url: &str, commit: &str) -> Result<u64> {
        let mirror = self.mirror_path(url);
        let repo =
            gix::open(&mirror).map_err(|e| Error::Source(format!("open mirror {source}: {e}")))?;
        let oid = gix::ObjectId::from_hex(commit.as_bytes())
            .map_err(|e| Error::Source(format!("parse commit {commit} in {source}: {e}")))?;
        let commit_obj = repo
            .find_commit(oid)
            .map_err(|e| Error::Source(format!("commit {commit} in {source}: {e}")))?;
        let seconds = commit_obj
            .author()
            .map_err(|e| Error::Source(format!("author of {commit} in {source}: {e}")))?
            .time()
            .map_err(|e| Error::Source(format!("author time of {commit} in {source}: {e}")))?
            .seconds;
        u64::try_from(seconds)
            .map_err(|e| Error::Source(format!("author time of {commit} in {source}: {e}")))
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

    use std::process::Command;

    use tempfile::TempDir;

    /// Author time on the tagged (first) commit; deliberately != committer time.
    const TAGGED_AUTHOR_TIME: u64 = 1_700_000_000;
    /// Committer time on the tagged commit; `commit_time` must NOT return this.
    const TAGGED_COMMITTER_TIME: u64 = 1_800_000_000;
    /// Well-formed 40-hex SHA that is guaranteed absent from the repo.
    const ABSENT_SHA: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    struct GitFixture {
        src: TempDir,
        _git_dir: TempDir,
        backend: GitBackend,
        url: String,
        /// First commit; pointed at by tag `v1.0`. Author != committer time.
        tag_sha: String,
        /// Second commit on `main`; distinct from `tag_sha`.
        head_sha: String,
        /// Tip of the non-default `develop` branch; not pointed at by HEAD/main.
        develop_sha: String,
    }

    impl GitFixture {
        fn rev_parse(&self, rev: &str) -> String {
            let out = run_git(self.src.path(), &["rev-parse", rev]);
            String::from_utf8(out.stdout)
                .expect("rev-parse output is utf8")
                .trim()
                .to_string()
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn run_git_dated(
        cwd: &Path,
        args: &[&str],
        author_date: &str,
        committer_date: &str,
    ) -> std::process::Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_DATE", author_date)
            .env("GIT_COMMITTER_DATE", committer_date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
        run_git_dated(cwd, args, "@1700000000 +0000", "@1700000000 +0000")
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_git_fixture() -> GitFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();

        run_git(src_path, &["init", "-b", "main", "."]);
        run_git(src_path, &["config", "user.email", "test@example.com"]);
        run_git(src_path, &["config", "user.name", "Test"]);

        std::fs::write(src_path.join("README.md"), b"hello\n").unwrap();
        run_git(src_path, &["add", "README.md"]);
        run_git_dated(
            src_path,
            &["commit", "-m", "initial"],
            "@1700000000 +0000",
            "@1800000000 +0000",
        );
        run_git(src_path, &["tag", "v1.0"]);

        let tag_out = run_git(src_path, &["rev-parse", "v1.0^{commit}"]);
        let tag_sha = String::from_utf8(tag_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        std::fs::write(src_path.join("SECOND.md"), b"second commit\n").unwrap();
        run_git(src_path, &["add", "SECOND.md"]);
        run_git(src_path, &["commit", "-m", "second"]);

        let head_out = run_git(src_path, &["rev-parse", "HEAD"]);
        let head_sha = String::from_utf8(head_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        assert_ne!(
            tag_sha, head_sha,
            "fixture must produce two distinct commits so tag != HEAD is meaningful"
        );

        run_git(src_path, &["checkout", "-b", "develop"]);
        std::fs::write(src_path.join("DEVELOP.md"), b"develop branch\n").unwrap();
        run_git(src_path, &["add", "DEVELOP.md"]);
        run_git(src_path, &["commit", "-m", "develop"]);
        let develop_out = run_git(src_path, &["rev-parse", "develop"]);
        let develop_sha = String::from_utf8(develop_out.stdout)
            .unwrap()
            .trim()
            .to_string();
        run_git(src_path, &["checkout", "main"]);

        let head_after_checkout =
            String::from_utf8(run_git(src_path, &["rev-parse", "HEAD"]).stdout)
                .unwrap()
                .trim()
                .to_string();

        assert_ne!(
            develop_sha, head_sha,
            "develop must advance past main so resolving it is a non-default-branch test"
        );
        assert_eq!(
            head_after_checkout, head_sha,
            "HEAD must point at main, leaving develop as a non-default branch"
        );

        let git_dir = TempDir::new().unwrap();
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = src_path.to_string_lossy().into_owned();

        GitFixture {
            src,
            _git_dir: git_dir,
            backend,
            url,
            tag_sha,
            head_sha,
            develop_sha,
        }
    }

    fn is_bare_repo(path: &Path) -> bool {
        path.join("objects").is_dir() && (path.join("refs").is_dir() || path.join("HEAD").is_file())
    }

    #[test]
    fn fixture_tag_and_head_are_distinct_forty_hex_commits() {
        let fixture = build_git_fixture();
        for sha in [&fixture.tag_sha, &fixture.head_sha] {
            assert_eq!(sha.len(), 40);
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(fixture.tag_sha, fixture.head_sha);
    }

    #[test]
    fn fetch_creates_real_bare_mirror() {
        let fixture = build_git_fixture();
        let mirror = fixture.backend.mirror_path(&fixture.url);

        fixture
            .backend
            .fetch("src", &fixture.url)
            .expect("fetch clones bare mirror");

        assert!(mirror.exists(), "mirror dir should exist after fetch");
        assert!(
            is_bare_repo(&mirror),
            "mirror must be a real bare repo: objects/ and refs|HEAD present"
        );
    }

    #[test]
    fn fetch_updates_existing_mirror_with_new_commits() {
        let fixture = build_git_fixture();

        fixture
            .backend
            .fetch("src", &fixture.url)
            .expect("first fetch clones");

        std::fs::write(fixture.src.path().join("THIRD.md"), b"third commit\n")
            .expect("write third file");
        run_git(fixture.src.path(), &["add", "THIRD.md"]);
        run_git(fixture.src.path(), &["commit", "-m", "third"]);
        let third_sha = fixture.rev_parse("HEAD");
        assert_ne!(third_sha, fixture.head_sha, "third commit must be new");

        fixture
            .backend
            .fetch("src", &fixture.url)
            .expect("second fetch updates existing mirror");

        let resolved = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Branch("main".into()))
            .expect("branch resolves after update fetch");

        assert_eq!(
            resolved, third_sha,
            "fetch on an existing mirror must pull new commits, not no-op"
        );
    }

    #[test]
    fn resolve_branch_main_returns_second_commit_not_tag() {
        let fixture = build_git_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let resolved = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Branch("main".into()))
            .expect("branch resolves to head commit");

        assert_eq!(resolved, fixture.head_sha);
        assert_ne!(
            resolved, fixture.tag_sha,
            "main points at the second commit, not the tagged first commit"
        );
    }

    #[test]
    fn resolve_non_default_branch_after_first_clone() {
        let fixture = build_git_fixture();

        fixture
            .backend
            .fetch("src", &fixture.url)
            .expect("first fetch clones bare mirror");

        let resolved = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Branch("develop".into()))
            .expect("non-default branch resolves after a single first-clone fetch");

        assert_eq!(
            resolved, fixture.develop_sha,
            "first clone must mirror all heads, not only the default branch"
        );
    }

    #[test]
    fn resolve_tag_returns_tagged_commit_not_head() {
        let fixture = build_git_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let resolved = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Tag("v1.0".into()))
            .expect("tag resolves to tagged commit");

        assert_eq!(resolved, fixture.tag_sha);
        assert_ne!(
            resolved, fixture.head_sha,
            "tag must resolve to its commit, not HEAD/main"
        );
    }

    #[test]
    fn resolve_rev_returns_same_sha() {
        let fixture = build_git_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let resolved = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Rev(fixture.head_sha.clone()))
            .expect("rev resolves to itself");

        assert_eq!(resolved, fixture.head_sha);
    }

    #[test]
    fn resolve_rev_for_absent_sha_errors() {
        let fixture = build_git_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let result = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Rev(ABSENT_SHA.into()));

        assert!(
            result.is_err(),
            "a well-formed but absent rev must error, proving resolve consults the mirror"
        );
    }

    #[test]
    fn resolve_nonexistent_branch_errors() {
        let fixture = build_git_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let result = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Branch("nope".into()));

        assert!(result.is_err(), "missing branch must error");
    }

    #[test]
    fn resolve_without_fetch_errors() {
        let fixture = build_git_fixture();

        let result = fixture
            .backend
            .resolve("src", &fixture.url, &Refspec::Branch("main".into()));

        assert!(result.is_err(), "resolve without a mirror must error");
    }

    #[test]
    fn commit_time_returns_author_time_not_committer_time() {
        let fixture = build_git_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let time = fixture
            .backend
            .commit_time("src", &fixture.url, &fixture.tag_sha)
            .expect("commit time resolves");

        assert_eq!(
            time, TAGGED_AUTHOR_TIME,
            "commit_time must return the author timestamp"
        );
        assert_ne!(
            time, TAGGED_COMMITTER_TIME,
            "commit_time must NOT return the committer timestamp"
        );
    }

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
