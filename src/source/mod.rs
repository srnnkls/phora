//! Immutable source resolution, inventories, reads, and directory listings.

mod archive;
mod cache;
mod git;
pub mod http;
mod import;
mod inventory;
mod model;
mod resolve;
mod router;
mod snapshot;
pub(crate) mod transitive;
mod worktree;
mod worktree_deploy;

#[cfg(test)]
pub(crate) use cache::mirror_path;
pub use git::GitBackend;
pub use import::HttpBackend;
pub use model::{
    Commit, KernelError, SourceEntryKind, SourceEntryMeta, SourceInventory, SourceName, SourcePath,
};
pub(crate) use model::{safe_component, safe_relpath};
pub use router::RouterBackend;
pub use snapshot::{
    CanonicalSourceRoot, ResolvePolicy, ResolveRequest, ResolvedRevision, ResolvedSource,
    RevisionSpec, SnapshotId, SourceDirectoryEntry, SourceDirectoryEntryKind, SourceEntry,
    SourceIdentity, SourceLocation, SourceStore, SourceTimestamp, digest_snapshot,
};
pub use worktree::{capture_worktree, is_local_path, read_local_head};
pub use worktree_deploy::{
    WorktreeAdminId, WorktreeDeployRequest, WorktreeDeployment, WorktreeMirrorAddress,
    WorktreeMirrorGuard, WorktreeObservationLevel, WorktreeObservationLock,
    WorktreeObservationRequest, WorktreeObservationResult, WorktreeRemoveRequest,
    worktree_admin_id,
};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use thiserror::Error;

use crate::config::Refspec;

/// Errors owned by the source capability.
#[derive(Debug, Error)]
pub enum SourceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("source error: {0}")]
    Source(String),

    #[error("{path} is absent at {commit} in {source_name}")]
    FileAbsent {
        source_name: String,
        commit: String,
        path: PathBuf,
    },

    #[error("config error: dependency at `{remote}` has no phora.toml")]
    DependencyManifestMissing {
        remote: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("config error: phora.toml at `{remote}` is not utf-8: {source}")]
    DependencyManifestUtf8 {
        remote: String,
        #[source]
        source: std::string::FromUtf8Error,
    },

    #[error("config error: {source}")]
    DependencyManifestParse {
        #[source]
        source: toml::de::Error,
    },

    #[error(
        "config error: source `{name}`: transitive remote not allowed — `{remote}` is a local path or file:// remote and does not resolve inside the materialized dependency tree"
    )]
    TransitiveRemoteRejected { name: String, remote: String },

    #[error("root path not found in tree: {root}")]
    RootNotFound { root: std::path::PathBuf },

    #[error("mapped key not found in source tree: {key}")]
    MappedKeyNotFound { key: PathBuf },

    #[error("mapped key does not resolve to a regular file: {key}")]
    MappedKeyNotALeaf { key: PathBuf },

    #[error("symlink not allowed: {path} (set allow_symlinks=true to permit)")]
    SymlinkNotAllowed { path: std::path::PathBuf },

    #[error(
        "symlink {path} escapes the artifact root: target {target} resolves outside the deploy tree"
    )]
    SymlinkEscape {
        path: std::path::PathBuf,
        target: String,
    },

    #[error("template render failed for {path}: {message}")]
    Render { path: PathBuf, message: String },

    #[error("deployed-name collision in artifact: {name} (from {first} and {second})")]
    DeployedNameCollision {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("source error: {0}")]
    Kernel(#[from] KernelError),
}

type Result<T> = std::result::Result<T, SourceError>;

/// gix clones origin as refs/remotes/origin/*; a mirror must update refs/heads/* and
/// refs/tags/* directly so tags (and tag-only-reachable commits) resolve after one fetch.
const MIRROR_REFSPECS: &[&str] = &["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Https,
    Ssh,
}

#[derive(Debug, Clone)]
pub struct ExportPolicy {
    pub allow_symlinks: bool,
    pub preserve_executable: bool,
    pub vcs_opt_in: bool,
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self {
            allow_symlinks: false,
            preserve_executable: true,
            vcs_opt_in: false,
        }
    }
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

impl FromStr for MirrorKey {
    type Err = SourceError;

    fn from_str(value: &str) -> Result<Self> {
        let valid = value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(SourceError::Source(format!(
                "invalid mirror key `{value}`: expected 16 lowercase hex chars"
            )))
        }
    }
}

/// Blake3 digest of the full effective vars map.
///
/// Export and `check_artifact_state` MUST agree byte-for-byte; both route through this.
#[must_use]
pub fn vars_digest(vars: &BTreeMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    for (key, value) in vars {
        hash_framed_entry(
            &mut hasher,
            key.as_bytes(),
            b"\x00var\x00",
            value.as_bytes(),
        );
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

pub(crate) fn hash_framed_entry(
    hasher: &mut blake3::Hasher,
    rel_path: &[u8],
    tag: &[u8],
    payload: &[u8],
) {
    hasher.update(&(rel_path.len() as u64).to_le_bytes());
    hasher.update(rel_path);
    hasher.update(tag);
    hasher.update(&(payload.len() as u64).to_le_bytes());
    hasher.update(payload);
}

#[cfg(test)]
mod tests {
    use super::import::import_tree;
    use super::*;

    use gix::object::tree::EntryKind;

    use crate::sync::stage::symlink_target_escapes;

    use crate::source::safe_component;

    use std::process::Command;

    use tempfile::TempDir;

    fn sn(name: &str) -> SourceName {
        SourceName::trusted(name)
    }

    fn git_request(name: &str, url: &str, revision: RevisionSpec) -> ResolveRequest {
        ResolveRequest {
            name: sn(name),
            location: SourceLocation::Git {
                url: url.to_owned(),
            },
            revision,
        }
    }

    fn url_request(name: &str, url: &str) -> ResolveRequest {
        ResolveRequest {
            name: sn(name),
            location: SourceLocation::Url {
                url: url.to_owned(),
            },
            revision: RevisionSpec::None,
        }
    }

    fn refresh_git(
        backend: &GitBackend,
        name: &str,
        url: &str,
        revision: RevisionSpec,
    ) -> Result<ResolvedSource> {
        SourceStore::resolve(
            backend,
            &git_request(name, url, revision),
            ResolvePolicy::Refresh,
        )
    }

    fn cached_git(
        backend: &GitBackend,
        name: &str,
        url: &str,
        revision: RevisionSpec,
    ) -> Result<ResolvedSource> {
        SourceStore::resolve(
            backend,
            &git_request(name, url, revision),
            ResolvePolicy::CachedOnly,
        )
    }

    fn refresh_url(backend: &HttpBackend, name: &str, url: &str) -> Result<ResolvedSource> {
        SourceStore::resolve(backend, &url_request(name, url), ResolvePolicy::Refresh)
    }

    fn snapshot_at(
        backend: &GitBackend,
        name: &str,
        url: &str,
        commit: &str,
    ) -> Result<ResolvedSource> {
        cached_git(
            backend,
            name,
            url,
            RevisionSpec::Commit(commit.parse().map_err(|error| {
                SourceError::Source(format!("invalid fixture commit: {error}"))
            })?),
        )
    }

    /// Author time on the tagged (first) commit; deliberately != committer time.
    const TAGGED_AUTHOR_TIME: u64 = 1_700_000_000;
    /// Committer time on the tagged commit; `ResolvedSource.authored_at` must NOT return this.
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
        /// Commit reachable ONLY via tag `v-orphan`; no branch head leads to it.
        orphan_sha: String,
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
        crate::sync::state::locking::assert_git_sandboxed(cwd);
        let _serial = crate::sync::state::locking::guard_git_fork();
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
    #[expect(
        clippy::too_many_lines,
        reason = "single linear fixture builder; splitting obscures the seeded git history"
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

        run_git(src_path, &["checkout", "--orphan", "orphanbranch"]);
        run_git(src_path, &["rm", "-rf", "--cached", "."]);
        for entry in std::fs::read_dir(src_path).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
        std::fs::write(src_path.join("ORPHAN.md"), b"orphan commit\n").unwrap();
        run_git(src_path, &["add", "ORPHAN.md"]);
        run_git(src_path, &["commit", "-m", "orphan"]);
        let orphan_sha = String::from_utf8(run_git(src_path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        run_git(src_path, &["tag", "-a", "v-orphan", "-m", "orphan tag"]);
        run_git(src_path, &["checkout", "main"]);
        run_git(src_path, &["branch", "-D", "orphanbranch"]);

        let is_ancestor_of_main = {
            let _serial = crate::sync::state::locking::guard_git_fork();
            Command::new("git")
                .args(["merge-base", "--is-ancestor", &orphan_sha, "main"])
                .current_dir(src_path)
                .status()
                .unwrap()
                .success()
        };
        assert!(
            !is_ancestor_of_main,
            "orphan commit must be unreachable from main, else heads-only fetch could pull it incidentally"
        );

        let is_ancestor_of_develop = {
            let _serial = crate::sync::state::locking::guard_git_fork();
            Command::new("git")
                .args(["merge-base", "--is-ancestor", &orphan_sha, "develop"])
                .current_dir(src_path)
                .status()
                .unwrap()
                .success()
        };
        assert!(
            !is_ancestor_of_develop,
            "orphan commit must be unreachable from develop, else heads-only fetch could pull it incidentally"
        );

        for sha in [&tag_sha, &head_sha, &develop_sha] {
            assert_ne!(
                &orphan_sha, sha,
                "orphan commit must be distinct from the reachable commits"
            );
        }

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
            orphan_sha,
        }
    }

    fn is_bare_repo(path: &Path) -> bool {
        path.join("objects").is_dir() && (path.join("refs").is_dir() || path.join("HEAD").is_file())
    }

    const EDITOR_INIT_CONTENT: &[u8] = b"-- editor init\nvim.opt.number = true\n";
    const EDITOR_OPTS_CONTENT: &[u8] = b"-- nested opts\nreturn {}\n";
    const EDITOR_RUN_CONTENT: &[u8] = b"#!/bin/sh\necho run\n";
    const EDITOR_NOTES_CONTENT: &[u8] = b"scratch notes, excluded by **/*.bak\n";
    const LINK_NAME: &str = "link";
    const LINK_TARGET: &str = "init.lua";

    struct ExportFixture {
        _src: TempDir,
        _git_dir: TempDir,
        backend: GitBackend,
        url: String,
        /// Sole commit; its author time equals [`EXPORT_COMMIT_TIME`].
        commit: String,
    }

    /// Clean base with no root-level symlink: `editor/` is symlink-free; the
    /// only symlink lives in a dedicated `linky/` artifact.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_export_fixture() -> ExportFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();

        init_export_repo(src_path);

        let editor = src_path.join("editor");
        std::fs::create_dir_all(editor.join("lua")).unwrap();
        std::fs::create_dir_all(editor.join("bin")).unwrap();
        std::fs::write(editor.join("init.lua"), EDITOR_INIT_CONTENT).unwrap();
        std::fs::write(editor.join("lua/opts.lua"), EDITOR_OPTS_CONTENT).unwrap();
        std::fs::write(editor.join("bin/run.sh"), EDITOR_RUN_CONTENT).unwrap();
        std::fs::write(editor.join("notes.bak"), EDITOR_NOTES_CONTENT).unwrap();

        std::fs::create_dir_all(src_path.join("lint")).unwrap();
        std::fs::write(src_path.join("lint/rules.toml"), b"[rules]\n").unwrap();

        let linky = src_path.join("linky");
        std::fs::create_dir_all(&linky).unwrap();
        std::fs::write(linky.join("init.lua"), EDITOR_INIT_CONTENT).unwrap();

        std::fs::create_dir_all(src_path.join(".hidden")).unwrap();
        std::fs::write(src_path.join(".hidden/secret"), b"nope\n").unwrap();

        run_git(src_path, &["add", "-A"]);
        run_git(
            src_path,
            &["update-index", "--chmod=+x", "editor/bin/run.sh"],
        );

        std::os::unix::fs::symlink(LINK_TARGET, linky.join(LINK_NAME)).unwrap();
        run_git(src_path, &["add", "linky/link"]);

        let commit = commit_export_repo(src_path);

        let link_mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", "linky/link"]).stdout).unwrap();
        assert!(
            link_mode.starts_with("120000"),
            "linky/link must be committed as a git symlink (120000), got: {link_mode}"
        );
        let run_mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", "editor/bin/run.sh"]).stdout)
                .unwrap();
        assert!(
            run_mode.starts_with("100755"),
            "editor/bin/run.sh must be committed executable (100755), got: {run_mode}"
        );

        export_fixture_from(src, commit)
    }

    const FILE_TAG: &[u8] = b"\x00file\x00";

    fn build_collision_fixture(files: &[(&str, &[u8])]) -> ExportFixture {
        let src = TempDir::new().expect("collision src tempdir");
        let src_path = src.path();
        init_export_repo(src_path);

        let art = src_path.join("art");
        std::fs::create_dir_all(&art).expect("create art dir");
        for (name, content) in files {
            std::fs::write(art.join(name), content).expect("write collision file");
        }
        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        export_fixture_from(src, commit)
    }

    struct RootedTestStore<'a> {
        inner: &'a GitBackend,
        root: &'a str,
    }

    impl SourceStore for RootedTestStore<'_> {
        fn resolve(
            &self,
            request: &ResolveRequest,
            policy: ResolvePolicy,
        ) -> Result<ResolvedSource> {
            SourceStore::resolve(self.inner, request, policy)
        }

        fn inventory(
            &self,
            snapshot: &SnapshotId,
            root: Option<&SourcePath>,
        ) -> Result<SourceInventory> {
            SourceStore::inventory(self.inner, snapshot, root)
        }

        fn read(&self, snapshot: &SnapshotId, path: &SourcePath) -> Result<SourceEntry> {
            let rooted = SourcePath::new(&format!("{}/{}", self.root, path.as_str()))?;
            let mut entry = SourceStore::read(self.inner, snapshot, &rooted)?;
            entry.meta.path = path.clone();
            Ok(entry)
        }

        fn list_directory(
            &self,
            snapshot: &SnapshotId,
            path: Option<&SourcePath>,
        ) -> Result<Vec<SourceDirectoryEntry>> {
            SourceStore::list_directory(self.inner, snapshot, path)
        }
    }

    fn digest_of_art(fixture: &ExportFixture) -> String {
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.commit.parse().expect("fixture commit is valid")),
        )
        .expect("refresh and resolve artifact fixture");
        let leaves: Vec<SourcePath> = SourceStore::inventory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("art").expect("safe fixture root")),
        )
        .expect("inventory art subtree")
        .entries
        .into_iter()
        .map(|entry| entry.path)
        .collect();
        digest_snapshot(
            &RootedTestStore {
                inner: &fixture.backend,
                root: "art",
            },
            &resolved.snapshot,
            &leaves,
        )
        .expect("digest computes over the art subtree")
    }

    fn init_export_repo(src_path: &Path) {
        run_git(src_path, &["init", "-b", "main", "."]);
        run_git(src_path, &["config", "user.email", "test@example.com"]);
        run_git(src_path, &["config", "user.name", "Test"]);
        run_git(src_path, &["config", "core.autocrlf", "false"]);
    }

    #[test]
    fn inventory_under_a_root_yields_root_relative_leaves_without_the_root_prefix() {
        let src = TempDir::new().expect("leaf-root src tempdir");
        let src_path = src.path();
        init_export_repo(src_path);

        let art = src_path.join("art");
        std::fs::create_dir_all(art.join("nested")).expect("create art/nested");
        std::fs::write(art.join("top.lua"), b"-- top\n").expect("write art/top.lua");
        std::fs::write(art.join("nested").join("inner.lua"), b"-- inner\n")
            .expect("write art/nested/inner.lua");
        std::fs::write(src_path.join("OUTSIDE.md"), b"sibling outside the root\n")
            .expect("write OUTSIDE.md");
        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        let fixture = export_fixture_from(src, commit);
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.commit.parse().expect("fixture commit is valid")),
        )
        .expect("refresh builds and resolves the mirror");
        let leaves: Vec<String> = SourceStore::inventory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("art").expect("safe fixture root")),
        )
        .expect("inventory under root = art succeeds")
        .entries
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();

        assert_eq!(
            leaves,
            vec!["nested/inner.lua".to_string(), "top.lua".to_string()],
            "leaves under root = `art` must be ROOT-RELATIVE with no `art/` prefix, and must \
             exclude the sibling `OUTSIDE.md` that lives outside the root; got: {leaves:?}"
        );
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn commit_export_repo(src_path: &Path) -> String {
        run_git_dated(
            src_path,
            &["commit", "-m", "artifacts"],
            "@1700000000 +0000",
            "@1800000000 +0000",
        );
        String::from_utf8(run_git(src_path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn export_fixture_from(src: TempDir, commit: String) -> ExportFixture {
        let git_dir = TempDir::new().expect("git dir tempdir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = src.path().to_string_lossy().into_owned();

        ExportFixture {
            _src: src,
            _git_dir: git_dir,
            backend,
            url,
            commit,
        }
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
    fn refresh_creates_real_bare_mirror() {
        let fixture = build_git_fixture();
        let mirror = fixture.backend.mirror_path(&fixture.url);

        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh clones bare mirror");

        assert!(mirror.exists(), "mirror dir should exist after fetch");
        assert!(
            is_bare_repo(&mirror),
            "mirror must be a real bare repo: objects/ and refs|HEAD present"
        );
    }

    #[test]
    fn refresh_updates_existing_mirror_with_new_commits() {
        let fixture = build_git_fixture();

        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("first refresh clones");

        std::fs::write(fixture.src.path().join("THIRD.md"), b"third commit\n")
            .expect("write third file");
        run_git(fixture.src.path(), &["add", "THIRD.md"]);
        run_git(fixture.src.path(), &["commit", "-m", "third"]);
        let third_sha = fixture.rev_parse("HEAD");
        assert_ne!(third_sha, fixture.head_sha, "third commit must be new");

        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("second refresh updates and resolves the existing mirror");

        assert_eq!(
            resolved.snapshot.commit().as_str(),
            third_sha,
            "refresh on an existing mirror must pull new commits, not no-op"
        );
    }

    #[test]
    fn refresh_reclones_a_corrupt_canonical_mirror() {
        let fixture = build_git_fixture();
        let mirror = fixture.backend.mirror_path(&fixture.url);
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("first refresh clones");

        std::fs::remove_dir_all(&mirror).expect("drop the real mirror");
        std::fs::create_dir_all(&mirror).expect("recreate an empty mirror dir");
        std::fs::write(mirror.join("garbage"), b"not a repo").expect("write garbage");
        assert!(gix::open(&mirror).is_err(), "corrupt mirror must not open");

        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh must self-heal a corrupt mirror, not error");

        assert!(
            is_bare_repo(&mirror),
            "mirror must be a valid bare repo after self-heal"
        );
        assert_eq!(resolved.snapshot.commit().as_str(), fixture.head_sha);
    }

    #[test]
    fn cached_only_tracks_refresh_and_a_cleared_cache() {
        let fixture = build_git_fixture();
        let mirror = fixture.backend.mirror_path(&fixture.url);

        assert!(
            cached_git(
                &fixture.backend,
                "src",
                &fixture.url,
                RevisionSpec::Branch("main".into())
            )
            .is_err(),
            "no mirror yet: not ready"
        );

        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh clones");
        assert!(
            cached_git(
                &fixture.backend,
                "src",
                &fixture.url,
                RevisionSpec::Branch("main".into())
            )
            .is_ok(),
            "a freshly cloned mirror is ready"
        );

        std::fs::remove_dir_all(&mirror).expect("clear the mirror cache");
        assert!(
            cached_git(
                &fixture.backend,
                "src",
                &fixture.url,
                RevisionSpec::Branch("main".into())
            )
            .is_err(),
            "a cleared cache is unavailable to CachedOnly, forcing Refresh on the next lock hit"
        );
    }

    #[test]
    fn refresh_sweeps_a_stale_orphan_staging_dir() {
        let fixture = build_git_fixture();
        let git_dir = fixture.backend.git_dir.clone();
        std::fs::create_dir_all(&git_dir).expect("git dir");
        let key = MirrorKey::from_url(&NormalizedUrl::parse(&fixture.url));
        let orphan = git_dir.join(format!(".{}.staging-424242-0", key.as_str()));
        std::fs::create_dir_all(orphan.join("objects/pack")).expect("create orphan staging");
        std::fs::write(orphan.join("objects/pack/partial-pack"), b"x").expect("write partial");
        let stale = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        for entry in walkdir::WalkDir::new(&orphan) {
            let path = entry.expect("walk orphan").into_path();
            filetime::set_file_mtime(&path, stale).expect("backdate orphan past grace");
        }

        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh");

        assert!(
            !orphan.exists(),
            "a stale orphan staging dir must be swept by fetch"
        );
        assert!(is_bare_repo(&fixture.backend.mirror_path(&fixture.url)));
    }

    #[test]
    fn refresh_keeps_a_staging_dir_with_recent_inner_writes() {
        let fixture = build_git_fixture();
        let git_dir = fixture.backend.git_dir.clone();
        std::fs::create_dir_all(&git_dir).expect("git dir");
        let key = MirrorKey::from_url(&NormalizedUrl::parse(&fixture.url));
        let live = git_dir.join(format!(".{}.staging-424242-1", key.as_str()));
        let pack = live.join("objects/pack");
        std::fs::create_dir_all(&pack).expect("create live staging");
        std::fs::write(pack.join("tmp_pack_incoming"), b"receiving").expect("write partial pack");
        let stale = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&live, stale).expect("backdate staging root past grace");

        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh");

        assert!(
            live.exists(),
            "a staging dir whose root mtime is stale but whose pack write is recent is a live \
             clone and must survive"
        );
    }

    #[test]
    fn refresh_reclones_when_in_place_refresh_fails() {
        let fixture = build_git_fixture();
        let mirror = fixture.backend.mirror_path(&fixture.url);
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("first refresh clones");
        assert!(gix::open(&mirror).is_ok(), "mirror opens after first clone");

        run_git(
            &mirror,
            &["remote", "set-url", "origin", "/nonexistent/repo.git"],
        );

        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh must self-heal when an in-place update fails, not error");

        assert!(
            is_bare_repo(&mirror),
            "mirror must be a valid bare repo after self-heal"
        );
        assert_eq!(resolved.snapshot.commit().as_str(), fixture.head_sha);
    }

    #[test]
    fn resolve_branch_main_returns_second_commit_not_tag() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh resolves the branch head");

        assert_eq!(resolved.snapshot.commit().as_str(), fixture.head_sha);
        assert_ne!(
            resolved.snapshot.commit().as_str(),
            fixture.tag_sha,
            "main points at the second commit, not the tagged first commit"
        );
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_trunk_default_repo() -> (TempDir, TempDir, GitBackend, String, String) {
        let src = TempDir::new().unwrap();
        let p = src.path();
        run_git(p, &["init", "-b", "trunk", "."]);
        run_git(p, &["config", "user.email", "test@example.com"]);
        run_git(p, &["config", "user.name", "Test"]);
        std::fs::write(p.join("README.md"), b"on trunk\n").unwrap();
        run_git(p, &["add", "README.md"]);
        run_git(p, &["commit", "-m", "initial"]);
        let trunk_sha = String::from_utf8(run_git(p, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let git_dir = TempDir::new().unwrap();
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = p.to_string_lossy().into_owned();
        (src, git_dir, backend, url, trunk_sha)
    }

    #[test]
    fn resolve_default_follows_remote_default_branch_when_not_main() {
        let (_src, _git_dir, backend, url, trunk_sha) = build_trunk_default_repo();

        let resolved = refresh_git(&backend, "src", &url, RevisionSpec::Default)
            .expect("Default must resolve against a repo that has no `main` branch");

        assert_eq!(
            resolved.snapshot.commit().as_str(),
            trunk_sha,
            "an unspecified ref must follow the repo's actual default branch (trunk), \
             not assume `main` — which does not exist here"
        );
    }

    #[test]
    fn resolve_default_survives_an_incremental_refresh() {
        let (src, _git_dir, backend, url, _first_sha) = build_trunk_default_repo();
        refresh_git(&backend, "src", &url, RevisionSpec::Default).expect("first refresh");

        std::fs::write(src.path().join("SECOND.md"), b"more\n").expect("write");
        run_git(src.path(), &["add", "SECOND.md"]);
        run_git(src.path(), &["commit", "-m", "second"]);
        let advanced = String::from_utf8(run_git(src.path(), &["rev-parse", "HEAD"]).stdout)
            .expect("utf8")
            .trim()
            .to_string();

        let resolved = refresh_git(&backend, "src", &url, RevisionSpec::Default)
            .expect("Default still resolves after an incremental refresh");

        assert_eq!(
            resolved.snapshot.commit().as_str(),
            advanced,
            "Default must track the default branch tip across an incremental refresh, \
             not a stale HEAD"
        );
    }

    #[test]
    fn file_diff_between_reads_both_commits_and_reports_the_changed_path() {
        let fixture = build_git_fixture();
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh");

        let changed = fixture
            .backend
            .file_diff_between(
                &sn("src"),
                &fixture.url,
                &fixture.tag_sha,
                &fixture.head_sha,
            )
            .expect("diff between two commits resolves both trees");

        assert!(
            changed.iter().any(|p| p == "SECOND.md"),
            "the diff must list `SECOND.md`, which exists only in the second commit's tree — \
             proving file_diff_between read BOTH commits, not just one; got: {changed:?}"
        );
        assert!(
            !changed.iter().any(|p| p == "README.md"),
            "`README.md` is byte-identical across both commits and must NOT appear in the diff; \
             got: {changed:?}"
        );
    }

    #[test]
    fn resolve_non_default_branch_after_first_clone() {
        let fixture = build_git_fixture();

        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("develop".into()),
        )
        .expect("non-default branch resolves after a single first-clone refresh");

        assert_eq!(
            resolved.snapshot.commit().as_str(),
            fixture.develop_sha,
            "first clone must mirror all heads, not only the default branch"
        );
    }

    #[test]
    fn resolve_tag_returns_tagged_commit_not_head() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Tag("v1.0".into()),
        )
        .expect("tag resolves to tagged commit");

        assert_eq!(resolved.snapshot.commit().as_str(), fixture.tag_sha);
        assert_ne!(
            resolved.snapshot.commit().as_str(),
            fixture.head_sha,
            "tag must resolve to its commit, not HEAD/main"
        );
    }

    #[test]
    fn resolve_tag_unreachable_from_any_head_after_single_fetch() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Tag("v-orphan".into()),
        )
        .expect("a tag unreachable from every branch head resolves after one refresh");

        assert_eq!(
            resolved.snapshot.commit().as_str(),
            fixture.orphan_sha,
            "the mirror must fetch tags, not only commits reachable from heads"
        );
    }

    #[test]
    fn resolve_rev_unreachable_from_any_head_after_single_fetch() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.orphan_sha.parse().expect("fixture commit is valid")),
        )
        .expect("a bare sha unreachable from every head resolves after one refresh");

        assert_eq!(
            resolved.snapshot.commit().as_str(),
            fixture.orphan_sha,
            "fetching tags must bring the tagged object into the mirror, not just the ref"
        );
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "removing the source repo fails loudly if the fixture path is gone"
    )]
    fn single_refresh_covers_reachable_and_unreachable_tags() {
        let fixture = build_git_fixture();
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("single refresh");

        std::fs::remove_dir_all(fixture.src.path()).unwrap();
        assert!(
            refresh_git(
                &fixture.backend,
                "src",
                &fixture.url,
                RevisionSpec::Branch("main".into())
            )
            .is_err(),
            "guard: after removing the source repo Refresh MUST fail; \
             otherwise this test cannot prove the first refresh was self-contained"
        );

        let reachable = cached_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Tag("v1.0".into()),
        )
        .expect(
            "reachable tag resolves from the mirror after the remote is gone — \
                 no hidden refetch needed",
        );
        let unreachable = cached_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Tag("v-orphan".into()),
        )
        .expect(
            "unreachable tag resolves from the mirror after the remote is gone; \
                 if resolve relied on a fallback fetch-on-miss this would fail, \
                 proving one fetch per source was NOT achieved",
        );
        let orphan_rev = cached_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.orphan_sha.parse().expect("fixture commit is valid")),
        )
        .expect(
            "the bare orphan sha resolves from the mirror after the remote is gone; \
                 the single fetch must have brought the tagged object in, not just the ref",
        );

        assert_eq!(reachable.snapshot.commit().as_str(), fixture.tag_sha);
        assert_eq!(unreachable.snapshot.commit().as_str(), fixture.orphan_sha);
        assert_eq!(orphan_rev.snapshot.commit().as_str(), fixture.orphan_sha);
    }

    #[test]
    fn resolve_rev_returns_same_sha() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.head_sha.parse().expect("fixture commit is valid")),
        )
        .expect("rev resolves to itself");

        assert_eq!(resolved.snapshot.commit().as_str(), fixture.head_sha);
    }

    #[test]
    fn resolve_rev_for_absent_sha_errors() {
        let fixture = build_git_fixture();
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("seed mirror");
        let result = cached_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(ABSENT_SHA.parse().expect("absent SHA is valid")),
        );

        assert!(
            result.is_err(),
            "a well-formed but absent rev must error, proving resolve consults the mirror"
        );
    }

    #[test]
    fn resolve_nonexistent_branch_errors() {
        let fixture = build_git_fixture();
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("seed mirror");
        let result = cached_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("nope".into()),
        );

        assert!(result.is_err(), "missing branch must error");
    }

    #[test]
    fn cached_only_without_refresh_errors() {
        let fixture = build_git_fixture();

        let result = cached_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        );

        assert!(result.is_err(), "resolve without a mirror must error");
    }

    #[test]
    fn resolved_source_carries_author_time_not_committer_time() {
        let fixture = build_git_fixture();
        let time = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Tag("v1.0".into()),
        )
        .expect("tag resolves")
        .authored_at
        .unix_seconds();

        assert_eq!(
            time, TAGGED_AUTHOR_TIME,
            "ResolvedSource.authored_at must return the author timestamp"
        );
        assert_ne!(
            time, TAGGED_COMMITTER_TIME,
            "ResolvedSource.authored_at must NOT return the committer timestamp"
        );
    }

    // ---- MIRROR-LOCK-001: per-mirror BLOCKING flock around fetch ----

    use std::sync::mpsc;
    use std::time::Duration;

    /// The exact per-mirror lock-file path the implementation must use:
    /// `<git_dir>/<MirrorKey>.git.lock` (the mirror dir path with `.lock`
    /// appended). Derived independently from `mirror_path` so the test fails if
    /// the impl picks a different naming.
    fn mirror_lock_path(git_dir: &Path, url: &str) -> PathBuf {
        let mut s = mirror_path(git_dir, url).into_os_string();
        s.push(".lock");
        PathBuf::from(s)
    }

    /// Minimal seeded git repo at `path` with a single commit on `main`; used as
    /// a `url` for a SECOND distinct mirror (different `MirrorKey`).
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_minimal_repo(path: &Path) {
        run_git(path, &["init", "-b", "main", "."]);
        run_git(path, &["config", "user.email", "test@example.com"]);
        run_git(path, &["config", "user.name", "Test"]);
        std::fs::write(path.join("ONLY.md"), b"only commit\n").unwrap();
        run_git(path, &["add", "ONLY.md"]);
        run_git(path, &["commit", "-m", "only"]);
    }

    /// Holds an exclusive advisory lock on `path` (creating it). The test grabs
    /// the lock the binary's `fetch` will contend for; held until dropped.
    fn hold_mirror_lock(path: &Path) -> std::fs::File {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create git_dir for lock");
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .expect("open per-mirror lock file");
        file.try_lock()
            .expect("test acquires the mirror lock first");
        file
    }

    #[test]
    fn refresh_blocks_on_held_per_mirror_lock_then_succeeds_when_released() {
        let fixture = build_git_fixture();
        let git_dir = fixture.backend.git_dir.clone();
        let url = fixture.url.clone();

        let lock_path = mirror_lock_path(&git_dir, &url);
        let held = hold_mirror_lock(&lock_path);

        let (tx, rx) = mpsc::channel();
        let backend = GitBackend::new(git_dir.clone());
        let url_for_thread = url.clone();
        let worker = std::thread::spawn(move || {
            let result = refresh_git(
                &backend,
                "src",
                &url_for_thread,
                RevisionSpec::Branch("main".into()),
            );
            tx.send(result).expect("send refresh result");
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(750)).is_err(),
            "Refresh must BLOCK while the per-mirror lock is held; it completed \
             within the window, so it took no blocking lock on {}",
            lock_path.display()
        );

        drop(held);

        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("Refresh must complete promptly once the mirror lock is released");
        worker.join().expect("refresh thread joins");
        result.expect("Refresh succeeds after waiting for the lock (blocking, not error)");

        let mirror = mirror_path(&git_dir, &url);
        assert!(
            is_bare_repo(&mirror),
            "the serialized fetch must leave a valid, non-corrupt bare mirror"
        );
        let repo = gix::open(&mirror).expect("mirror opens as a git repo");
        assert!(
            repo.find_reference("refs/heads/main").is_ok(),
            "the serialized fetch must populate refs/heads/main"
        );
    }

    #[test]
    fn refresh_creates_the_per_mirror_lock_file() {
        let fixture = build_git_fixture();
        let git_dir = fixture.backend.git_dir.clone();
        let lock_path = mirror_lock_path(&git_dir, &fixture.url);

        assert!(
            !lock_path.exists(),
            "precondition: no lock file before Refresh"
        );

        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("Refresh clones bare mirror");

        assert!(
            lock_path.exists(),
            "Refresh must create/use the per-mirror lock file at {}",
            lock_path.display()
        );
    }

    #[test]
    fn holding_one_mirror_lock_does_not_block_refresh_of_a_different_mirror() {
        let fixture_a = build_git_fixture();
        let git_dir = fixture_a.backend.git_dir.clone();

        let src_b = TempDir::new().expect("src b tempdir");
        build_minimal_repo(src_b.path());
        let url_b = src_b.path().to_string_lossy().into_owned();

        assert_ne!(
            mirror_path(&git_dir, &fixture_a.url),
            mirror_path(&git_dir, &url_b),
            "the two urls must map to distinct mirrors for this test to mean anything"
        );

        let held_a = hold_mirror_lock(&mirror_lock_path(&git_dir, &fixture_a.url));

        let (tx, rx) = mpsc::channel();
        let backend = GitBackend::new(git_dir.clone());
        let worker = std::thread::spawn(move || {
            let result = refresh_git(
                &backend,
                "srcb",
                &url_b,
                RevisionSpec::Branch("main".into()),
            );
            tx.send(result).expect("send refresh-b result");
        });

        let result = rx.recv_timeout(Duration::from_secs(5)).expect(
            "refreshing a DIFFERENT mirror must not block on mirror A's lock; \
             the per-mirror lock must be keyed per MirrorKey",
        );
        worker.join().expect("refresh-b thread joins");
        result.expect("Refresh of mirror B succeeds while A's lock is held");

        drop(held_a);
    }

    #[test]
    fn scp_style_ssh_drops_userinfo_and_strips_git_suffix() {
        assert_eq!(
            NormalizedUrl::parse("git@github.com:user/repo.git").as_str(),
            "github.com/user/repo"
        );
    }

    #[test]
    fn is_local_path_rejects_url_and_scp_forms() {
        for url in [
            "https://github.com/me/dotfiles.git",
            "ssh://git@host/x.git",
            "git@github.com:me/dotfiles.git",
            "github.com:me/repo",
        ] {
            assert!(
                !is_local_path(url),
                "url/scp form must not be classified local: {url}"
            );
        }
    }

    #[test]
    fn is_local_path_accepts_absolute_path() {
        assert!(
            is_local_path("/home/soeren/dev/loqui"),
            "an absolute path is a local path even if it does not exist"
        );
    }

    /// Removes its directory on drop, so a panicking assert never leaks it.
    struct CwdRelDir(PathBuf);

    impl Drop for CwdRelDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[expect(clippy::unwrap_used, reason = "tempdir setup fails loudly in tests")]
    #[test]
    fn is_local_path_accepts_existing_relative_path() {
        let nonce = format!(
            "phora-rel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let cwd = std::env::current_dir().unwrap();
        let abs = cwd.join(&nonce);
        std::fs::create_dir(&abs).unwrap();
        let _cleanup = CwdRelDir(abs);

        assert!(
            is_local_path(&nonce),
            "a relative name that exists under cwd on disk is a local path"
        );
    }

    #[test]
    fn is_local_path_accepts_windows_drive_letter_path() {
        assert!(
            is_local_path("C:/Users/foo/project"),
            "a single-letter drive prefix like `C:` is a filesystem path on every platform \
             (`C:` is even a legal directory name on POSIX), so it must classify as local — \
             not fall through to colon-alias parsing that fabricates host=\"C\", repo=\"Users/foo\""
        );
    }

    #[test]
    fn is_local_path_accepts_lowercase_drive_letter_path() {
        assert!(
            is_local_path("c:/x"),
            "the drive-letter prefix is case-insensitive; a lowercase `c:` prefix is still a \
             filesystem path, not a colon alias with host=\"c\""
        );
    }

    #[test]
    fn is_local_path_keeps_multiletter_colon_alias_non_local() {
        assert!(
            !is_local_path("github:owner/repo"),
            "a multi-letter host before the colon is a forge alias, not a drive letter — the \
             drive-letter special case must not swallow `github:owner/repo` into a local path"
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
    fn symbolic_https_ssh_and_literal_collapse_to_one_mirror_key() {
        use std::collections::BTreeMap;

        use crate::config::{Config, Host, ParsedSource};

        let symbolic = Config::parse(
            r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
        )
        .expect("symbolic config parses");
        let raw = symbolic.sources.get("tropos").expect("tropos source");
        let source = ParsedSource::parse("tropos", raw).expect("tropos parses to typed form");
        let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

        let symbolic_https = source
            .resolved_remote(&no_user_hosts, Protocol::Https)
            .expect("symbolic github https resolves");
        let symbolic_ssh = source
            .resolved_remote(&no_user_hosts, Protocol::Ssh)
            .expect("symbolic github ssh resolves");
        let literal_https = "https://github.com/srnnkls/tropos.git";

        let key = |remote: &str| MirrorKey::from_url(&NormalizedUrl::parse(remote));

        assert_eq!(
            key(&symbolic_https),
            key(&symbolic_ssh),
            "flipping protocol must not change the mirror: symbolic https and ssh of one repo \
             must share a MirrorKey"
        );
        assert_eq!(
            key(&symbolic_https),
            key(literal_https),
            "a symbolic host+path source and its literal-URL twin must collapse to one MirrorKey"
        );
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

    // ---- immutable snapshot reads (the read_manifest seam) ----

    #[test]
    fn snapshot_read_returns_bytes_of_a_file_present_at_the_commit() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.head_sha.parse().expect("fixture commit is valid")),
        )
        .expect("refresh builds and resolves the bare mirror");
        let bytes = SourceStore::read(
            &fixture.backend,
            &resolved.snapshot,
            &SourcePath::new("README.md").expect("safe fixture path"),
        )
        .expect("read returns a tracked file from the immutable snapshot")
        .bytes;

        assert_eq!(
            bytes, b"hello\n",
            "read must return the exact bytes of README.md at the commit, not a digest or path"
        );
    }

    #[test]
    fn snapshot_read_errors_when_the_file_is_absent_at_that_commit() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.tag_sha.parse().expect("fixture commit is valid")),
        )
        .expect("refresh builds and resolves the bare mirror");

        // SECOND.md was added on the second commit (head_sha); it does NOT exist at tag_sha.
        let err = SourceStore::read(
            &fixture.backend,
            &resolved.snapshot,
            &SourcePath::new("SECOND.md").expect("safe fixture path"),
        )
        .expect_err("a file absent at the requested commit must be an error, not empty bytes");

        let msg = err.to_string();
        assert!(
            msg.contains("SECOND.md"),
            "the absent-file error must name the path it could not find, got: {msg}"
        );
    }

    #[test]
    fn snapshot_read_errors_when_the_mirror_is_missing() {
        let git_dir = TempDir::new().expect("git_dir tempdir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());

        let normalized = NormalizedUrl::parse("https://github.com/never/fetched.git");
        let snapshot = SnapshotId::Git {
            mirror: MirrorKey::from_url(&normalized),
            commit: ABSENT_SHA.parse().expect("absent SHA is valid"),
        };
        let err = SourceStore::read(
            &backend,
            &snapshot,
            &SourcePath::new("phora.toml").expect("safe fixture path"),
        )
        .expect_err("reading from a mirror that was never fetched must error, not panic");

        assert!(
            !err.to_string().is_empty(),
            "the missing-mirror error must carry a diagnostic message"
        );
    }

    #[test]
    fn cached_resolution_is_required_before_snapshot_reads() {
        let git_dir = TempDir::new().expect("git_dir tempdir");
        let http = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

        let err = SourceStore::resolve(
            &http,
            &url_request("u", "https://example.com/pkg.tar.gz"),
            ResolvePolicy::CachedOnly,
        )
        .expect_err("CachedOnly must reject a URL source that was never imported");
        let msg = err.to_string();
        assert!(
            !msg.is_empty(),
            "a missing immutable snapshot must retain a diagnostic"
        );
    }

    #[test]
    fn snapshot_read_signals_absent_distinctly_from_other_failures() {
        let fixture = build_git_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.tag_sha.parse().expect("fixture commit is valid")),
        )
        .expect("refresh builds and resolves the bare mirror");

        let absent = SourceStore::read(
            &fixture.backend,
            &resolved.snapshot,
            &SourcePath::new("SECOND.md").expect("safe fixture path"),
        )
        .expect_err("an absent file must error");
        assert!(
            matches!(absent, SourceError::FileAbsent { .. }),
            "an absent entry must surface as FileAbsent so callers can stay silent, got: {absent:?}"
        );

        let other_snapshot = SnapshotId::Git {
            mirror: resolved.snapshot.mirror().clone(),
            commit: ABSENT_SHA.parse().expect("absent SHA is valid"),
        };
        let other = SourceStore::read(
            &fixture.backend,
            &other_snapshot,
            &SourcePath::new("README.md").expect("safe fixture path"),
        )
        .expect_err("an unknown commit must error");
        assert!(
            !matches!(other, SourceError::FileAbsent { .. }),
            "a git/commit failure must NOT be mislabelled as an absent file, got: {other:?}"
        );
    }

    #[test]
    fn snapshot_read_errors_clearly_when_entry_is_a_tree_not_a_blob() {
        let src = TempDir::new().expect("src tempdir");
        let src_path = src.path();
        run_git(src_path, &["init", "-b", "main", "."]);
        run_git(src_path, &["config", "user.email", "t@example.com"]);
        run_git(src_path, &["config", "user.name", "T"]);
        std::fs::create_dir(src_path.join("nested")).expect("mk dir");
        std::fs::write(src_path.join("nested").join("leaf"), b"x\n").expect("write leaf");
        run_git(src_path, &["add", "-A"]);
        run_git(src_path, &["commit", "-m", "tree"]);
        let commit = String::from_utf8(run_git(src_path, &["rev-parse", "HEAD"]).stdout)
            .expect("utf8 sha")
            .trim()
            .to_owned();

        let git_dir = TempDir::new().expect("git_dir tempdir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = src_path.to_string_lossy().into_owned();
        let resolved = refresh_git(
            &backend,
            "src",
            &url,
            RevisionSpec::Commit(commit.parse().expect("fixture commit is valid")),
        )
        .expect("refresh mirror");
        let err = SourceStore::read(
            &backend,
            &resolved.snapshot,
            &SourcePath::new("nested").expect("safe fixture path"),
        )
        .expect_err("a directory entry must not be returned as file bytes");
        let msg = err.to_string();
        assert!(
            msg.contains("nested"),
            "the non-blob error must name the path, got: {msg}"
        );
        assert!(
            !matches!(err, SourceError::FileAbsent { .. }),
            "a present-but-non-blob entry is not 'absent'; it must be a distinct failure, got: {err:?}"
        );
    }

    // ---- list_directory (shallow ls-style listing for `trust --show <dir>`) ----

    fn build_tree_fixture() -> ExportFixture {
        let src = TempDir::new().expect("tree src tempdir");
        let src_path = src.path();
        init_export_repo(src_path);

        let d = src_path.join("d");
        std::fs::create_dir_all(d.join("sub")).expect("create d/sub");
        std::fs::write(d.join("a.txt"), b"a\n").expect("write d/a.txt");
        std::fs::write(d.join("b.txt"), b"b\n").expect("write d/b.txt");
        std::fs::write(d.join("sub").join("c.txt"), b"c\n").expect("write d/sub/c.txt");
        std::fs::write(src_path.join("top.txt"), b"top\n").expect("write top.txt");
        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        let fixture = export_fixture_from(src, commit);
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.commit.parse().expect("fixture commit is valid")),
        )
        .expect("refresh builds the mirror the directory listing reads");
        fixture
    }

    #[test]
    fn list_directory_yields_only_the_direct_children_of_the_directory() {
        let fixture = build_tree_fixture();
        let resolved = snapshot_at(&fixture.backend, "src", &fixture.url, &fixture.commit)
            .expect("resolve fixture snapshot");

        let entries = SourceStore::list_directory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("d").expect("safe fixture path")),
        )
        .expect("list_directory lists the direct children of `d` at the commit");

        let names: Vec<&str> = entries
            .iter()
            .map(|entry| {
                entry
                    .path
                    .as_str()
                    .rsplit_once('/')
                    .map_or(entry.path.as_str(), |(_, name)| name)
            })
            .collect();
        assert_eq!(
            names,
            vec!["a.txt", "b.txt", "sub"],
            "list_directory must return the DIRECT children of `d` (root-relative leaf names, \
             sorted), with the subdir `sub` listed as a single entry; got: {names:?}"
        );
        assert!(
            !names.contains(&"c.txt") && !names.iter().any(|n| n.contains('/')),
            "list_directory is shallow: `d/sub/c.txt` must NOT be flattened in (no recursion), \
             got: {names:?}"
        );
    }

    #[test]
    fn list_directory_marks_subdirectories_as_directories_and_files_as_files() {
        let fixture = build_tree_fixture();
        let resolved = snapshot_at(&fixture.backend, "src", &fixture.url, &fixture.commit)
            .expect("resolve fixture snapshot");

        let entries = SourceStore::list_directory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("d").expect("safe fixture path")),
        )
        .expect("list_directory lists `d`");

        let sub = entries
            .iter()
            .find(|entry| entry.path.as_str() == "d/sub")
            .expect("the `sub` directory entry must be present");
        assert_eq!(
            sub.kind,
            SourceDirectoryEntryKind::Directory,
            "the `sub` entry is a directory and must be reported as is_dir=true so `--show` can \
             render it ls-style; got: {sub:?}"
        );
        for file in entries
            .iter()
            .filter(|entry| entry.path.as_str() != "d/sub")
        {
            assert_eq!(
                file.kind,
                SourceDirectoryEntryKind::File,
                "the regular file `{}` must be reported as a file, got: {file:?}",
                file.path
            );
        }
    }

    #[test]
    fn list_directory_with_none_lists_the_repo_root() {
        let fixture = build_tree_fixture();
        let resolved = snapshot_at(&fixture.backend, "src", &fixture.url, &fixture.commit)
            .expect("resolve fixture snapshot");

        let entries = SourceStore::list_directory(&fixture.backend, &resolved.snapshot, None)
            .expect("an empty path lists the repo root's top-level entries");

        let names: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(
            names,
            vec!["d", "top.txt"],
            "listing the repo root (empty path) must yield the top-level entries `d` and \
             `top.txt`, not the contents of `d`; got: {names:?}"
        );
    }

    #[test]
    fn list_directory_errors_when_the_directory_is_absent_at_the_commit() {
        let fixture = build_tree_fixture();
        let resolved = snapshot_at(&fixture.backend, "src", &fixture.url, &fixture.commit)
            .expect("resolve fixture snapshot");

        let err = SourceStore::list_directory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("nope").expect("safe absent path")),
        )
        .expect_err("listing a path that does not exist must error, not return an empty list");

        assert!(
            matches!(err, SourceError::RootNotFound { .. }),
            "an absent directory must surface as RootNotFound (via subtree_at_root), got: {err:?}"
        );
    }

    #[test]
    fn list_directory_rejects_a_file_path() {
        let fixture = build_tree_fixture();
        let resolved = snapshot_at(&fixture.backend, "src", &fixture.url, &fixture.commit)
            .expect("resolve fixture snapshot");
        let err = SourceStore::list_directory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("d/a.txt").expect("safe file path")),
        )
        .expect_err("list_directory requires a directory, not a file leaf");
        assert!(
            err.to_string().contains("d/a.txt"),
            "the non-directory error must name the requested path: {err}"
        );
    }

    // ---- fetch_root_manifest (single-file / shallow; no full mirror) ----

    #[test]
    fn fetch_root_manifest_uncached_does_not_build_a_full_mirror() {
        let src = TempDir::new().expect("src tempdir");
        let src_path = src.path();
        run_git(src_path, &["init", "-b", "main", "."]);
        run_git(src_path, &["config", "user.email", "t@example.com"]);
        run_git(src_path, &["config", "user.name", "T"]);
        std::fs::write(
            src_path.join("phora.toml"),
            b"version = 1\n\n[sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n",
        )
        .expect("write manifest");
        run_git(src_path, &["add", "-A"]);
        run_git(src_path, &["commit", "-m", "root"]);

        let git_dir = TempDir::new().expect("git_dir tempdir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = src_path.to_string_lossy().into_owned();

        let bytes = backend
            .fetch_root_manifest(&sn("dep"), &url, &Refspec::Branch("main".to_owned()))
            .expect("uncached fetch_root_manifest reads the root phora.toml");
        let text = String::from_utf8(bytes).expect("manifest is utf-8");
        assert!(
            text.contains("[sources.nvim]"),
            "fetch_root_manifest must return the root phora.toml bytes, got: {text}"
        );

        assert!(
            !backend.mirror_path(&url).exists(),
            "the uncached path must NOT create a persistent all-refs mirror — that is the regression \
             this task prevents; it must fetch shallowly into ephemeral storage"
        );
    }

    #[test]
    fn fetch_root_manifest_reuses_a_cached_mirror_offline() {
        let fixture = build_git_fixture();
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("prime the mirror cache");
        std::fs::write(
            fixture.src.path().join("phora.toml"),
            b"version = 1\n\n[sources.x]\ngit = \"https://github.com/dep/x.git\"\n",
        )
        .expect("write manifest");
        run_git(fixture.src.path(), &["add", "-A"]);
        run_git(fixture.src.path(), &["commit", "-m", "add manifest"]);
        refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Branch("main".into()),
        )
        .expect("refresh cached mirror");

        let bytes = fixture
            .backend
            .fetch_root_manifest(
                &sn("src"),
                &fixture.url,
                &Refspec::Branch("main".to_owned()),
            )
            .expect("cached fetch_root_manifest reads via the mirror");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(
            text.contains("[sources.x]"),
            "cached fetch_root_manifest must read from the primed mirror, got: {text}"
        );
        assert!(
            fixture.backend.mirror_path(&fixture.url).exists(),
            "a primed mirror must be reused, not discarded"
        );
    }

    // ---- safe_component (path-traversal guard) ----

    #[test]
    fn safe_component_accepts_normal_single_components() {
        for name in ["init.lua", "lua", "run.sh", "opts.lua", "a"] {
            assert_eq!(
                safe_component(name).expect("normal single component must be accepted"),
                name,
                "{name} is a normal single path component and must pass through unchanged"
            );
        }
    }

    #[test]
    fn safe_component_rejects_traversal_and_separators() {
        for name in [
            "..",
            ".",
            "",
            "a/b",
            "..\\b",
            "lua\\opts",
            "/abs",
            "/etc/passwd",
            "a/../b",
        ] {
            assert!(
                safe_component(name).is_err(),
                "{name:?} escapes a single path component and must be rejected to prevent staging-dir traversal"
            );
        }
    }

    #[test]
    fn symlink_validator_rejects_windows_style_escapes() {
        let dir_link = Path::new("dir/link");
        for target in [
            "C:/Windows/System32",
            "C:\\Windows",
            "\\\\host\\share",
            "\\evil",
            "..\\..\\outside",
        ] {
            assert!(
                symlink_target_escapes(dir_link, target.as_bytes()),
                "{target} escapes the artifact root and must be rejected on every platform"
            );
        }
    }

    #[test]
    fn symlink_validator_accepts_in_root_dotdot() {
        assert!(
            !symlink_target_escapes(Path::new("dir/link"), b"../sibling"),
            "dir/link -> ../sibling stays inside the artifact root and must be accepted"
        );
    }

    // ---- digest_snapshot ----

    #[test]
    fn digest_snapshot_is_blake3_prefixed_and_stable() {
        let fixture = build_export_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.commit.parse().expect("fixture commit is valid")),
        )
        .expect("refresh and resolve fixture");
        let leaves: Vec<SourcePath> =
            SourceStore::inventory(&fixture.backend, &resolved.snapshot, None)
                .expect("inventory fixture")
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect();
        let first = digest_snapshot(&fixture.backend, &resolved.snapshot, &leaves)
            .expect("digest computes");
        let second = digest_snapshot(&fixture.backend, &resolved.snapshot, &leaves)
            .expect("digest computes again");

        assert!(
            first.starts_with("blake3:"),
            "digest must carry the blake3: prefix, got {first}"
        );
        assert_eq!(
            first, second,
            "same (commit, root, matcher) must yield an identical digest"
        );
    }

    #[test]
    fn digest_snapshot_frames_entries_so_content_cannot_bleed_into_next_path() {
        let mut bled_content = b"X".to_vec();
        bled_content.extend_from_slice(b"b");
        bled_content.extend_from_slice(FILE_TAG);
        bled_content.extend_from_slice(b"Y");

        let one_file = build_collision_fixture(&[("a", &bled_content)]);
        let two_files = build_collision_fixture(&[("a", b"X"), ("b", b"Y")]);
        assert_ne!(
            digest_of_art(&one_file),
            digest_of_art(&two_files),
            "distinct layouts whose naive path||tag||content streams are byte-identical \
             must hash differently; entries need length framing"
        );
    }

    // ---- import_tree (HTP-004): deterministic synthetic-commit import ----

    use super::archive::{EntryKind as BlobKind, ExtractedEntry};

    fn entry(path: &str, kind: BlobKind, data: &[u8]) -> ExtractedEntry {
        ExtractedEntry {
            path: PathBuf::from(path),
            kind,
            data: data.to_vec(),
        }
    }

    /// Opens the bare mirror that `import_tree` wrote for `url` under `git_dir`.
    fn open_imported_mirror(git_dir: &Path, url: &str) -> gix::Repository {
        let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
        let mirror = git_dir.join(format!("{}.git", key.as_str()));
        gix::open(&mirror).expect("imported mirror opens as a git repo")
    }

    const IMPORT_URL: &str = "https://example.com/owner/repo";

    #[test]
    fn deterministic_same_entries_same_commit_id() {
        let dir_a = TempDir::new().expect("git_dir a");
        let dir_b = TempDir::new().expect("git_dir b");
        let entries = || {
            vec![
                entry("a.txt", BlobKind::Blob, b"alpha"),
                entry("dir/b.txt", BlobKind::Blob, b"bravo"),
            ]
        };

        let first =
            import_tree(dir_a.path(), IMPORT_URL, &entries()).expect("import into fresh git_dir a");
        let second = import_tree(dir_b.path(), IMPORT_URL, &entries())
            .expect("import the same entries into fresh git_dir b");

        assert_eq!(
            first, second,
            "identical entries must produce an identical content-addressed commit id, \
             independent of which git_dir they land in"
        );
    }

    #[test]
    fn input_order_independent_commit_id() {
        let dir_a = TempDir::new().expect("git_dir a");
        let dir_b = TempDir::new().expect("git_dir b");

        let forward = vec![
            entry("a.txt", BlobKind::Blob, b"alpha"),
            entry("m.txt", BlobKind::Blob, b"mike"),
            entry("z.txt", BlobKind::Blob, b"zulu"),
            entry("dir/b.txt", BlobKind::Blob, b"bravo"),
        ];
        let reversed = vec![
            entry("dir/b.txt", BlobKind::Blob, b"bravo"),
            entry("z.txt", BlobKind::Blob, b"zulu"),
            entry("m.txt", BlobKind::Blob, b"mike"),
            entry("a.txt", BlobKind::Blob, b"alpha"),
        ];

        let forward_id =
            import_tree(dir_a.path(), IMPORT_URL, &forward).expect("import forward order");
        let reversed_id =
            import_tree(dir_b.path(), IMPORT_URL, &reversed).expect("import reversed order");

        assert_eq!(
            forward_id, reversed_id,
            "input order must not affect the commit id: the impl must sort tree entries \
             into git order before writing"
        );
    }

    #[test]
    fn different_content_different_commit_id() {
        let dir_a = TempDir::new().expect("git_dir a");
        let dir_b = TempDir::new().expect("git_dir b");

        let base = import_tree(
            dir_a.path(),
            IMPORT_URL,
            &[entry("a.txt", BlobKind::Blob, b"alpha")],
        )
        .expect("import base content");
        let changed = import_tree(
            dir_b.path(),
            IMPORT_URL,
            &[entry("a.txt", BlobKind::Blob, b"ALPHA")],
        )
        .expect("import changed content");

        assert_ne!(
            base, changed,
            "changing a file's bytes must change the content-addressed commit id"
        );
    }

    #[test]
    fn failed_fresh_import_leaves_no_mirror_at_canonical_path() {
        let dir = TempDir::new().expect("git_dir");
        let key = MirrorKey::from_url(&NormalizedUrl::parse(IMPORT_URL));
        let mirror = dir.path().join(format!("{}.git", key.as_str()));
        let duplicate = vec![
            entry("dup.txt", BlobKind::Blob, b"one"),
            entry("dup.txt", BlobKind::Blob, b"two"),
        ];

        let result = import_tree(dir.path(), IMPORT_URL, &duplicate);

        assert!(result.is_err(), "a duplicate archive entry path must error");
        assert!(
            !mirror.exists(),
            "a failed fresh import must not leave a partial mirror at the canonical path"
        );
    }

    #[test]
    fn commit_has_fixed_identity_time_and_no_parents() {
        let dir_a = TempDir::new().expect("git_dir a");
        let dir_b = TempDir::new().expect("git_dir b");
        let make = || vec![entry("a.txt", BlobKind::Blob, b"alpha")];

        let id_a = import_tree(dir_a.path(), IMPORT_URL, &make()).expect("import a");
        let id_b = import_tree(dir_b.path(), IMPORT_URL, &make()).expect("import b");

        let repo = open_imported_mirror(dir_a.path(), IMPORT_URL);
        let oid = gix::ObjectId::from_hex(id_a.as_bytes()).expect("returned hex is a valid oid");
        let commit = repo
            .find_commit(oid)
            .expect("returned commit id exists in the mirror");

        let author = commit.author().expect("commit has an author");
        let committer = commit.committer().expect("commit has a committer");

        assert_eq!(
            author.time().expect("author time decodes").seconds,
            1,
            "author time must be the fixed epoch+1 second (NOT epoch 0)"
        );
        assert_eq!(
            committer.time().expect("committer time decodes").seconds,
            1,
            "committer time must be the fixed epoch+1 second (NOT epoch 0)"
        );

        assert_eq!(
            commit.parent_ids().count(),
            0,
            "a synthetic import commit must have no parents"
        );

        let author_a = author.name.to_string();
        let email_a = author.email.to_string();

        let repo_b = open_imported_mirror(dir_b.path(), IMPORT_URL);
        let oid_b = gix::ObjectId::from_hex(id_b.as_bytes()).expect("id_b valid oid");
        let commit_b = repo_b.find_commit(oid_b).expect("commit b exists");
        let author_b = commit_b.author().expect("author b");

        assert_eq!(
            author_a,
            author_b.name.to_string(),
            "author name must be a fixed constant, stable across imports"
        );
        assert_eq!(
            email_a,
            author_b.email.to_string(),
            "author email must be a fixed constant, stable across imports"
        );
        assert_eq!(
            id_a, id_b,
            "fixed identity+time+message imply identical commit ids for identical trees"
        );
    }

    #[test]
    fn ref_phora_points_at_commit() {
        let dir = TempDir::new().expect("git_dir");
        let commit_id = import_tree(
            dir.path(),
            IMPORT_URL,
            &[entry("a.txt", BlobKind::Blob, b"alpha")],
        )
        .expect("import");

        let repo = open_imported_mirror(dir.path(), IMPORT_URL);
        let resolved = repo
            .find_reference("refs/heads/phora")
            .expect("refs/heads/phora exists after import")
            .peel_to_commit()
            .expect("phora ref peels to a commit")
            .id()
            .to_hex()
            .to_string();

        assert_eq!(
            resolved, commit_id,
            "refs/heads/phora must resolve to the returned commit id"
        );
    }

    #[test]
    fn nested_tree_roundtrips_paths_kinds_data() {
        let dir = TempDir::new().expect("git_dir");
        let commit_id = import_tree(
            dir.path(),
            IMPORT_URL,
            &[
                entry("a.txt", BlobKind::Blob, b"A"),
                entry("dir/b.sh", BlobKind::BlobExecutable, b"B"),
                entry("dir/link", BlobKind::Link, b"target/x"),
            ],
        )
        .expect("import nested tree");

        let repo = open_imported_mirror(dir.path(), IMPORT_URL);
        let oid = gix::ObjectId::from_hex(commit_id.as_bytes()).expect("valid oid");
        let tree = repo
            .find_commit(oid)
            .expect("commit exists")
            .tree()
            .expect("commit has a tree");

        let lookup = |path: &str| {
            tree.lookup_entry_by_path(Path::new(path))
                .expect("lookup does not error")
                .unwrap_or_else(|| panic!("entry {path} must exist in the imported tree"))
        };

        let blob_data = |entry: &gix::object::tree::Entry<'_>| {
            repo.find_blob(entry.object_id())
                .expect("entry blob exists")
                .data
                .clone()
        };

        let a = lookup("a.txt");
        assert_eq!(a.mode().kind(), EntryKind::Blob, "a.txt is a plain blob");
        assert_eq!(blob_data(&a), b"A", "a.txt content roundtrips");

        let b = lookup("dir/b.sh");
        assert_eq!(
            b.mode().kind(),
            EntryKind::BlobExecutable,
            "dir/b.sh must be an executable blob (mode 100755)"
        );
        assert_eq!(blob_data(&b), b"B", "dir/b.sh content roundtrips");

        let link = lookup("dir/link");
        assert_eq!(
            link.mode().kind(),
            EntryKind::Link,
            "dir/link must be a symlink (mode 120000)"
        );
        assert_eq!(
            blob_data(&link),
            b"target/x",
            "symlink blob content must be the target bytes"
        );

        let dir_entry = lookup("dir");
        assert_eq!(
            dir_entry.mode().kind(),
            EntryKind::Tree,
            "the nested dir must be a real subtree"
        );
    }

    #[test]
    fn git_fsck_accepts_synthetic_commit() {
        let dir = TempDir::new().expect("git_dir");
        import_tree(
            dir.path(),
            IMPORT_URL,
            &[
                entry("a.txt", BlobKind::Blob, b"A"),
                entry("dir/b.sh", BlobKind::BlobExecutable, b"B"),
                entry("dir/link", BlobKind::Link, b"target/x"),
                entry("z.txt", BlobKind::Blob, b"Z"),
            ],
        )
        .expect("import");

        let key = MirrorKey::from_url(&NormalizedUrl::parse(IMPORT_URL));
        let mirror = dir.path().join(format!("{}.git", key.as_str()));

        let out = {
            let _serial = crate::sync::state::locking::guard_git_fork();
            Command::new("git")
                .args([
                    "--git-dir",
                    mirror.to_str().expect("mirror path is utf8"),
                    "fsck",
                    "--strict",
                ])
                .output()
                .expect("git fsck runs")
        };

        assert!(
            out.status.success(),
            "git fsck --strict must accept the synthetic objects; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reported_problem = stderr.lines().any(|line| {
            let lower = line.trim().to_lowercase();
            lower.starts_with("error:") || lower.starts_with("fatal:")
        });
        assert!(
            !reported_problem,
            "git fsck must emit no error:/fatal: lines (catches unsorted/malformed trees): {stderr}"
        );
    }

    #[test]
    fn empty_entries_produce_stable_empty_tree_commit() {
        let dir_a = TempDir::new().expect("git_dir a");
        let dir_b = TempDir::new().expect("git_dir b");

        let first = import_tree(dir_a.path(), IMPORT_URL, &[])
            .expect("importing zero entries yields a commit over an empty tree");
        let second =
            import_tree(dir_b.path(), IMPORT_URL, &[]).expect("re-importing zero entries succeeds");

        assert_eq!(
            first, second,
            "an empty entry list must yield a stable empty-tree commit id"
        );
    }

    #[test]
    fn reimport_changed_content_advances_ref() {
        let dir = TempDir::new().expect("git_dir");

        let first = import_tree(
            dir.path(),
            IMPORT_URL,
            &[entry("a.txt", BlobKind::Blob, b"alpha")],
        )
        .expect("first import into a fresh mirror");

        let second = import_tree(
            dir.path(),
            IMPORT_URL,
            &[entry("a.txt", BlobKind::Blob, b"BRAVO")],
        )
        .expect("re-import of changed content into the SAME mirror must succeed");

        assert_ne!(
            first, second,
            "changed content must yield a different commit id on re-import"
        );

        let repo = open_imported_mirror(dir.path(), IMPORT_URL);
        let resolved = repo
            .find_reference("refs/heads/phora")
            .expect("phora ref exists after re-import")
            .peel_to_commit()
            .expect("phora ref peels to a commit")
            .id()
            .to_hex()
            .to_string();

        assert_eq!(
            resolved, second,
            "refs/heads/phora must advance to the second (changed) commit"
        );
    }

    #[test]
    fn reimport_identical_content_is_stable() {
        let dir = TempDir::new().expect("git_dir");
        let make = || vec![entry("a.txt", BlobKind::Blob, b"alpha")];

        let first = import_tree(dir.path(), IMPORT_URL, &make()).expect("first import");
        let second = import_tree(dir.path(), IMPORT_URL, &make())
            .expect("re-import of identical content into the SAME mirror is idempotent");

        assert_eq!(
            first, second,
            "identical content re-imported into the same mirror must be stable"
        );

        let repo = open_imported_mirror(dir.path(), IMPORT_URL);
        let resolved = repo
            .find_reference("refs/heads/phora")
            .expect("phora ref exists")
            .peel_to_commit()
            .expect("phora ref peels to a commit")
            .id()
            .to_hex()
            .to_string();

        assert_eq!(resolved, first, "ref still resolves to the stable commit");
    }

    #[test]
    fn import_rejects_duplicate_entry_paths() {
        let dir = TempDir::new().expect("git_dir");

        let result = import_tree(
            dir.path(),
            IMPORT_URL,
            &[
                entry("a.txt", BlobKind::Blob, b"first"),
                entry("a.txt", BlobKind::Blob, b"second"),
            ],
        );

        assert!(
            matches!(result, Err(SourceError::Source(_))),
            "two entries with the same path must error, not silently overwrite"
        );
    }

    #[test]
    fn import_rejects_file_dir_collision_either_order() {
        let dir_after_file = import_tree(
            TempDir::new().expect("git_dir").path(),
            IMPORT_URL,
            &[
                entry("dir", BlobKind::Blob, b"file"),
                entry("dir/x", BlobKind::Blob, b"child"),
            ],
        );
        assert!(
            matches!(dir_after_file, Err(SourceError::Source(_))),
            "a file then a directory at the same name must error"
        );

        let file_after_dir = import_tree(
            TempDir::new().expect("git_dir").path(),
            IMPORT_URL,
            &[
                entry("dir/x", BlobKind::Blob, b"child"),
                entry("dir", BlobKind::Blob, b"file"),
            ],
        );
        assert!(
            matches!(file_after_dir, Err(SourceError::Source(_))),
            "a directory then a file at the same name must error"
        );
    }

    #[test]
    fn digest_snapshot_reflects_selected_leaves_not_unused_matcher_config() {
        let fixture = build_export_fixture();
        let resolved = refresh_git(
            &fixture.backend,
            "src",
            &fixture.url,
            RevisionSpec::Commit(fixture.commit.parse().expect("fixture commit is valid")),
        )
        .expect("refresh and resolve fixture");
        let leaves: Vec<SourcePath> =
            SourceStore::inventory(&fixture.backend, &resolved.snapshot, None)
                .expect("inventory fixture")
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect();
        let no_exclude = digest_snapshot(&fixture.backend, &resolved.snapshot, &leaves)
            .expect("full digest computes");
        let exclude_nothing = digest_snapshot(&fixture.backend, &resolved.snapshot, &leaves)
            .expect("same effective leaf set digests identically");
        let non_lua: Vec<SourcePath> = leaves
            .iter()
            .filter(|path| {
                Path::new(path.as_str())
                    .extension()
                    .is_none_or(|extension| extension != "lua")
            })
            .cloned()
            .collect();
        let exclude_lua = digest_snapshot(&fixture.backend, &resolved.snapshot, &non_lua)
            .expect("non-Lua digest computes");

        assert_eq!(
            no_exclude, exclude_nothing,
            "an exclude that matches no entry must not change the digest; \
             digest reflects the matched tree, not the matcher config"
        );
        assert_ne!(
            no_exclude, exclude_lua,
            "excluding entries that exist must change the digest"
        );
    }

    // ---- HTP-005 B/C: HttpBackend (url source) at the trait level ----

    mod http_backend {
        use std::collections::BTreeMap;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::path::Path;
        use std::time::Duration;

        use gix::object::tree::EntryKind;
        use tempfile::TempDir;

        use crate::digest::Digest;
        use crate::source::{HttpBackend, ResolvePolicy, SourceError, SourceStore, mirror_path};

        use super::{refresh_url, sn, url_request};

        const HELLO_BODY: &[u8] = b"hi";
        const RUN_BODY: &[u8] = b"#!/bin/sh\n";

        /// One-shot 127.0.0.1 server returning the canned bytes; accept thread is
        /// detached so a non-connecting fetch never hangs the test on join.
        struct TarServer {
            port: u16,
        }

        impl TarServer {
            fn spawn(body: Vec<u8>) -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
                let port = listener.local_addr().expect("local addr").port();
                std::thread::spawn(move || {
                    if let Ok((stream, _)) = listener.accept() {
                        Self::serve(stream, &body);
                    }
                });
                Self { port }
            }

            fn serve(mut stream: TcpStream, body: &[u8]) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
                let _ = stream.flush();
            }

            fn url(&self) -> String {
                format!("http://127.0.0.1:{}/pkg-1.0.tar.gz", self.port)
            }
        }

        /// A `.tar.gz` of `pkg-1.0/hello.txt`="hi" and `pkg-1.0/bin/run.sh`(0o755)="#!/bin/sh\n".
        /// After auto-strip the entries are `hello.txt` and `bin/run.sh`.
        fn build_pkg_tar_gz() -> Vec<u8> {
            fn append(builder: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8], mode: u32) {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(mode);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, data)
                    .expect("append tar entry");
            }

            let mut builder = tar::Builder::new(Vec::new());
            append(&mut builder, "pkg-1.0/hello.txt", HELLO_BODY, 0o644);
            append(&mut builder, "pkg-1.0/bin/run.sh", RUN_BODY, 0o755);
            let tar_bytes = builder.into_inner().expect("finish tar");

            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&tar_bytes).expect("gzip tar bytes");
            encoder.finish().expect("finish gzip")
        }

        #[test]
        fn refresh_resolves_the_synthetic_phora_head() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

            let resolved = refresh_url(&backend, "pkg", &url)
                .expect("Refresh downloads, extracts, imports, and resolves the synthetic tree");
            let commit = resolved.snapshot.commit().to_string();

            assert_eq!(commit.len(), 40, "resolve returns a 40-hex commit id");
            assert!(
                commit.chars().all(|c| c.is_ascii_hexdigit()),
                "resolve returns a hex commit id, got: {commit}"
            );

            let cached = SourceStore::resolve(
                &backend,
                &url_request("pkg", &url),
                ResolvePolicy::CachedOnly,
            )
            .expect("CachedOnly reads the synthetic phora head");
            assert_eq!(
                cached.snapshot, resolved.snapshot,
                "Refresh and CachedOnly must yield the same synthetic snapshot"
            );
        }

        #[test]
        fn resolved_synthetic_source_authored_at_is_epoch_plus_one() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

            let time = refresh_url(&backend, "pkg", &url)
                .expect("refresh synthetic source")
                .authored_at
                .unix_seconds();
            assert_eq!(
                time, 1,
                "the synthetic import commit's author time is epoch+1 (==1)"
            );
        }

        #[test]
        fn matching_digest_lets_refresh_succeed() {
            let tar_gz = build_pkg_tar_gz();
            let server = TarServer::spawn(tar_gz.clone());
            let url = server.url();

            let mut digests = BTreeMap::new();
            digests.insert(sn("pkg"), Digest::sha256(sha256_of(&tar_gz)));

            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), digests);

            refresh_url(&backend, "pkg", &url)
                .expect("a matching configured digest must let Refresh create the snapshot");
        }

        #[test]
        fn mismatched_digest_errors_before_import_naming_source() {
            let tar_gz = build_pkg_tar_gz();
            let server = TarServer::spawn(tar_gz);
            let url = server.url();

            let mut digests = BTreeMap::new();
            digests.insert(sn("pkg"), Digest::sha256([0u8; 32]));

            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), digests);

            let err = refresh_url(&backend, "pkg", &url)
                .expect_err("a non-matching configured digest must fail Refresh");
            match err {
                SourceError::Source(msg) => assert!(
                    msg.contains("pkg"),
                    "the digest-mismatch error must name the source `pkg`, got: {msg}"
                ),
                other => panic!("expected SourceError::Source on digest mismatch, got: {other:?}"),
            }

            let mirror = mirror_path(git_dir.path(), &url);
            let phora_ref_exists = gix::open(&mirror)
                .ok()
                .is_some_and(|repo| repo.find_reference("refs/heads/phora").is_ok());
            assert!(
                !phora_ref_exists,
                "a digest mismatch must abort BEFORE import: a git-level lookup of \
                 refs/heads/phora must find nothing (packed or loose) — or the mirror \
                 must not even be initialized"
            );
            assert!(
                SourceStore::resolve(
                    &backend,
                    &url_request("pkg", &url),
                    ResolvePolicy::CachedOnly
                )
                .is_err(),
                "with no synthetic head imported, resolve must fail after a rejected fetch"
            );
        }

        fn sha256_of(bytes: &[u8]) -> [u8; 32] {
            use sha2::{Digest, Sha256};
            let mut out = [0u8; 32];
            out.copy_from_slice(&Sha256::digest(bytes));
            out
        }

        #[test]
        fn import_round_trip_preserves_hello_blob() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

            let commit = refresh_url(&backend, "pkg", &url)
                .expect("refresh synthetic source")
                .snapshot
                .commit()
                .to_string();

            let mirror = mirror_path(git_dir.path(), &url);
            let repo = gix::open(&mirror).expect("open synthetic mirror");
            let oid = gix::ObjectId::from_hex(commit.as_bytes()).expect("commit hex");
            let tree = repo
                .find_commit(oid)
                .expect("find synthetic commit")
                .tree()
                .expect("commit tree");
            let entry = tree
                .lookup_entry_by_path(Path::new("hello.txt"))
                .expect("lookup hello.txt")
                .expect("hello.txt present at the stripped tree root");
            assert!(
                matches!(entry.mode().kind(), EntryKind::Blob),
                "hello.txt must import as a plain blob"
            );
            let blob = repo.find_blob(entry.object_id()).expect("hello.txt blob");
            assert_eq!(
                blob.data, HELLO_BODY,
                "the downloaded-extracted-imported hello.txt blob must equal `hi`"
            );
        }

        // ---- MIRROR-LOCK-002: URL Refresh takes the SAME per-mirror flock ----

        use std::path::PathBuf;
        use std::sync::mpsc;

        /// Derived independently from `mirror_path` so the test fails if the impl
        /// picks a different naming; MUST equal `GitBackend`'s per-mirror lock path.
        fn mirror_lock_path(git_dir: &Path, url: &str) -> PathBuf {
            let mut s = mirror_path(git_dir, url).into_os_string();
            s.push(".lock");
            PathBuf::from(s)
        }

        fn hold_mirror_lock(path: &Path) -> std::fs::File {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create git_dir for lock");
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .expect("open per-mirror lock file");
            file.try_lock()
                .expect("test acquires the mirror lock first");
            file
        }

        #[test]
        fn http_refresh_blocks_on_held_per_mirror_lock_then_succeeds_when_released() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let git_dir_path = git_dir.path().to_path_buf();

            let lock_path = mirror_lock_path(&git_dir_path, &url);
            let held = hold_mirror_lock(&lock_path);

            let (tx, rx) = mpsc::channel();
            let backend = HttpBackend::new(git_dir_path.clone(), BTreeMap::new());
            let url_for_thread = url.clone();
            let worker = std::thread::spawn(move || {
                let result = refresh_url(&backend, "pkg", &url_for_thread);
                tx.send(result).expect("send refresh result");
            });

            assert!(
                rx.recv_timeout(Duration::from_millis(750)).is_err(),
                "URL Refresh must BLOCK while the per-mirror lock is held; it \
                 completed within the window, so it took no blocking lock on {} \
                 around its shared-mirror import",
                lock_path.display()
            );

            drop(held);

            let result = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("URL Refresh must complete promptly once the mirror lock is released");
            worker.join().expect("refresh thread joins");
            result.expect("URL Refresh succeeds after waiting for the lock (blocking, not error)");

            let mirror = mirror_path(&git_dir_path, &url);
            let repo = gix::open(&mirror).expect("the serialized fetch leaves a valid mirror");
            assert!(
                repo.find_reference("refs/heads/phora").is_ok(),
                "the serialized http fetch must populate refs/heads/phora"
            );
        }

        #[test]
        fn http_holding_one_mirror_lock_does_not_block_refresh_of_a_different_mirror() {
            let server_a = TarServer::spawn(build_pkg_tar_gz());
            let server_b = TarServer::spawn(build_pkg_tar_gz());
            let url_a = server_a.url();
            let url_b = server_b.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let git_dir_path = git_dir.path().to_path_buf();

            assert_ne!(
                mirror_path(&git_dir_path, &url_a),
                mirror_path(&git_dir_path, &url_b),
                "the two urls must map to distinct mirrors for this test to mean anything"
            );

            let held_a = hold_mirror_lock(&mirror_lock_path(&git_dir_path, &url_a));

            let (tx, rx) = mpsc::channel();
            let backend = HttpBackend::new(git_dir_path.clone(), BTreeMap::new());
            let worker = std::thread::spawn(move || {
                let result = refresh_url(&backend, "pkgb", &url_b);
                tx.send(result).expect("send refresh-b result");
            });

            let result = rx.recv_timeout(Duration::from_secs(10)).expect(
                "refreshing a DIFFERENT mirror must not block on mirror A's lock; \
                 the per-mirror lock must be keyed per MirrorKey",
            );
            worker.join().expect("refresh-b thread joins");
            result.expect("URL Refresh of mirror B succeeds while A's lock is held");

            drop(held_a);
        }
    }

    #[test]
    fn history_worktree_id_is_deterministic_and_ref_safe() {
        let project = TempDir::new().expect("project root");
        let deploy = TempDir::new().expect("deploy root");
        let first = worktree_admin_id(project.path(), deploy.path(), "target", "bad identity:*")
            .expect("derive worktree admin id");
        let repeated = worktree_admin_id(project.path(), deploy.path(), "target", "bad identity:*")
            .expect("derive same worktree admin id");
        let changed = worktree_admin_id(project.path(), deploy.path(), "other", "bad identity:*")
            .expect("derive distinct worktree admin id");

        assert_eq!(
            first, repeated,
            "the same deployment identity must keep its admin id"
        );
        assert_ne!(
            first, changed,
            "the target name is framed input and must change the admin id"
        );
        assert_eq!(first.as_str().len(), 16);
        assert!(
            first
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "the ref component must be lowercase hexadecimal, never the raw identity"
        );
        assert_eq!(
            first
                .as_str()
                .parse::<WorktreeAdminId>()
                .expect("persisted id parses"),
            first
        );
    }

    #[test]
    fn history_worktree_default_lock_api_leaves_cache_absent() {
        let cache = TempDir::new().expect("cache parent");
        let git_root = cache.path().join("git");
        let store = HttpBackend::new(git_root.clone(), BTreeMap::new());
        let source = SourceName::trusted("url-source");
        let key = "a1b2c3d4e5f60708"
            .parse::<MirrorKey>()
            .expect("valid mirror key");
        let address = WorktreeMirrorAddress {
            cache_git_root: git_root.clone(),
            key: key.clone(),
        };

        assert!(
            store.lock_worktree_mirror(&source, &key).is_err(),
            "the default adapter implementation must reject write locking"
        );
        assert!(
            store.lock_worktree_mirror_at(&source, &address).is_err(),
            "the persisted-address default must reject write locking"
        );
        assert!(
            !git_root.exists(),
            "default/read-only worktree administration must not create the cache or lock directory"
        );
    }
}
