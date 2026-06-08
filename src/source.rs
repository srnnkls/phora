//! Source port (`SourceBackend`) and its git adapter (`GitBackend`).

use std::path::{Path, PathBuf};

use gix::object::tree::EntryKind;

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

/// Borrowed parameters of [`SourceBackend::export_artifact`].
pub struct ExportRequest<'a> {
    pub source: &'a str,
    pub url: &'a str,
    pub commit: &'a str,
    pub root: Option<&'a Path>,
    pub artifact: &'a str,
    pub matcher: &'a PathMatcher,
    pub policy: &'a ExportPolicy,
    pub staging_dir: &'a Path,
    pub commit_time: u64,
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

    fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult>;

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
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        matcher: &PathMatcher,
    ) -> Result<Vec<String>> {
        let repo = self.open_mirror(source, url)?;
        let subtree = Self::subtree_at_root(&repo, source, commit, root)?;

        let mut artifacts = Vec::new();
        for entry in subtree.iter() {
            let entry =
                entry.map_err(|e| Error::Source(format!("read tree entry in {source}: {e}")))?;
            let name = safe_component(&entry.filename().to_string())?.to_string();

            if matches!(entry.kind(), EntryKind::Link) {
                return Err(Error::SymlinkNotAllowed {
                    path: PathBuf::from(name),
                });
            }

            let is_candidate = !name.starts_with('.') && matcher.allows_artifact(&name);
            if !is_candidate {
                continue;
            }
            if matches!(entry.kind(), EntryKind::Tree) {
                artifacts.push(name);
            }
        }

        artifacts.sort();
        Ok(artifacts)
    }

    fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult> {
        let repo = self.open_mirror(req.source, req.url)?;
        let root_tree = Self::subtree_at_root(&repo, req.source, req.commit, req.root)?;
        let artifact_tree = Self::lookup_subtree(&repo, &root_tree, req.source, req.artifact)?;

        std::fs::create_dir_all(req.staging_dir)?;

        let mut walk = ExportWalk {
            repo: &repo,
            source: req.source,
            out_base: req.staging_dir,
            matcher: req.matcher,
            policy: req.policy,
            commit_time: req.commit_time,
            files: Vec::new(),
            hasher: blake3::Hasher::new(),
        };
        walk.run(&artifact_tree, Path::new(""))?;

        let digest = format!("blake3:{}", walk.hasher.finalize().to_hex());
        Ok(ExportResult {
            files: walk.files,
            digest,
        })
    }

    fn compute_digest(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        matcher: &PathMatcher,
    ) -> Result<String> {
        let repo = self.open_mirror(source, url)?;
        let subtree = Self::subtree_at_root(&repo, source, commit, root)?;

        let mut hasher = blake3::Hasher::new();
        Self::hash_tree(&repo, source, &subtree, Path::new(""), matcher, &mut hasher)?;
        Ok(format!("blake3:{}", hasher.finalize().to_hex()))
    }
}

impl GitBackend {
    fn open_mirror(&self, source: &str, url: &str) -> Result<gix::Repository> {
        let mirror = self.mirror_path(url);
        gix::open(&mirror).map_err(|e| Error::Source(format!("open mirror {source}: {e}")))
    }

    fn commit_tree<'repo>(
        repo: &'repo gix::Repository,
        source: &str,
        commit: &str,
    ) -> Result<gix::Tree<'repo>> {
        let oid = gix::ObjectId::from_hex(commit.as_bytes())
            .map_err(|e| Error::Source(format!("parse commit {commit} in {source}: {e}")))?;
        repo.find_commit(oid)
            .map_err(|e| Error::Source(format!("commit {commit} in {source}: {e}")))?
            .tree()
            .map_err(|e| Error::Source(format!("tree of {commit} in {source}: {e}")))
    }

    fn subtree_at_root<'repo>(
        repo: &'repo gix::Repository,
        source: &str,
        commit: &str,
        root: Option<&Path>,
    ) -> Result<gix::Tree<'repo>> {
        let tree = Self::commit_tree(repo, source, commit)?;
        match root {
            Some(r) => {
                let entry = tree
                    .lookup_entry_by_path(r)
                    .map_err(|e| {
                        Error::Source(format!("lookup root {} in {source}: {e}", r.display()))
                    })?
                    .ok_or_else(|| Error::RootNotFound {
                        root: r.to_path_buf(),
                    })?;
                repo.find_tree(entry.object_id()).map_err(|e| {
                    Error::Source(format!("root tree {} in {source}: {e}", r.display()))
                })
            }
            None => Ok(tree),
        }
    }

    fn lookup_subtree<'repo>(
        repo: &'repo gix::Repository,
        tree: &gix::Tree<'repo>,
        source: &str,
        artifact: &str,
    ) -> Result<gix::Tree<'repo>> {
        let entry = tree
            .lookup_entry_by_path(Path::new(artifact))
            .map_err(|e| Error::Source(format!("lookup artifact {artifact} in {source}: {e}")))?
            .ok_or_else(|| Error::ArtifactNotFound {
                artifact: artifact.to_string(),
            })?;
        repo.find_tree(entry.object_id())
            .map_err(|e| Error::Source(format!("artifact tree {artifact} in {source}: {e}")))
    }

    fn find_blob_data(repo: &gix::Repository, source: &str, oid: gix::ObjectId) -> Result<Vec<u8>> {
        let blob = repo
            .find_blob(oid)
            .map_err(|e| Error::Source(format!("blob {oid} in {source}: {e}")))?;
        Ok(blob.data.clone())
    }

    fn hash_tree(
        repo: &gix::Repository,
        source: &str,
        tree: &gix::Tree<'_>,
        rel_path: &Path,
        matcher: &PathMatcher,
        hasher: &mut blake3::Hasher,
    ) -> Result<()> {
        for entry in tree.iter() {
            let entry =
                entry.map_err(|e| Error::Source(format!("read tree entry in {source}: {e}")))?;
            let component = safe_component(&entry.filename().to_string())?.to_string();
            let entry_rel = rel_path.join(component);
            let is_dir = matches!(entry.kind(), EntryKind::Tree);

            if !matcher.allows_path(&entry_rel, is_dir) {
                continue;
            }

            match entry.kind() {
                EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                    let tag: &[u8] = match entry.kind() {
                        EntryKind::BlobExecutable => b"\x00exec\x00",
                        EntryKind::Link => b"\x00link\x00",
                        _ => b"\x00file\x00",
                    };
                    let data = Self::find_blob_data(repo, source, entry.object_id())?;
                    hash_framed_entry(hasher, entry_rel.to_string_lossy().as_bytes(), tag, &data);
                }
                EntryKind::Tree => {
                    let subtree = repo
                        .find_tree(entry.object_id())
                        .map_err(|e| Error::Source(format!("subtree in {source}: {e}")))?;
                    Self::hash_tree(repo, source, &subtree, &entry_rel, matcher, hasher)?;
                }
                EntryKind::Commit => {}
            }
        }
        Ok(())
    }
}

struct ExportWalk<'a> {
    repo: &'a gix::Repository,
    source: &'a str,
    out_base: &'a Path,
    matcher: &'a PathMatcher,
    policy: &'a ExportPolicy,
    commit_time: u64,
    files: Vec<ManifestFile>,
    hasher: blake3::Hasher,
}

impl ExportWalk<'_> {
    fn run(&mut self, tree: &gix::Tree<'_>, rel_path: &Path) -> Result<()> {
        for entry in tree.iter() {
            let entry = entry
                .map_err(|e| Error::Source(format!("read tree entry in {}: {e}", self.source)))?;
            let component = safe_component(&entry.filename().to_string())?.to_string();
            let entry_rel = rel_path.join(component);
            let out_path = self.out_base.join(&entry_rel);
            let is_dir = matches!(entry.kind(), EntryKind::Tree);

            if !self.matcher.allows_path(&entry_rel, is_dir) {
                continue;
            }

            let kind = entry.kind();
            match kind {
                EntryKind::Blob | EntryKind::BlobExecutable => {
                    let executable = kind == EntryKind::BlobExecutable;
                    self.write_blob(&entry, &entry_rel, &out_path, executable)?;
                }
                EntryKind::Link => self.write_link(&entry, &entry_rel, &out_path)?,
                EntryKind::Tree => {
                    std::fs::create_dir_all(&out_path)?;
                    let subtree = self
                        .repo
                        .find_tree(entry.object_id())
                        .map_err(|e| Error::Source(format!("subtree in {}: {e}", self.source)))?;
                    self.run(&subtree, &entry_rel)?;
                }
                EntryKind::Commit => {
                    if !self.policy.allow_submodules {
                        return Err(Error::SubmoduleNotAllowed { path: entry_rel });
                    }
                }
            }
        }
        Ok(())
    }

    fn write_blob(
        &mut self,
        entry: &gix::object::tree::EntryRef<'_, '_>,
        entry_rel: &Path,
        out_path: &Path,
        executable: bool,
    ) -> Result<()> {
        let data = GitBackend::find_blob_data(self.repo, self.source, entry.object_id())?;
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out_path, &data)?;
        set_deterministic_mtime(out_path, self.commit_time)?;

        if executable && self.policy.preserve_executable {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(out_path)?.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(out_path, perms)?;
            }
        }

        let tag: &[u8] = if executable {
            b"\x00exec\x00"
        } else {
            b"\x00file\x00"
        };
        hash_framed_entry(
            &mut self.hasher,
            entry_rel.to_string_lossy().as_bytes(),
            tag,
            &data,
        );

        self.files.push(ManifestFile {
            path: entry_rel.to_path_buf(),
            size: data.len() as u64,
            mtime: self.commit_time,
            blake3: blake3::hash(&data).to_hex().to_string(),
        });
        Ok(())
    }

    fn write_link(
        &mut self,
        entry: &gix::object::tree::EntryRef<'_, '_>,
        entry_rel: &Path,
        out_path: &Path,
    ) -> Result<()> {
        if !self.policy.allow_symlinks {
            return Err(Error::SymlinkNotAllowed {
                path: entry_rel.to_path_buf(),
            });
        }
        let target = GitBackend::find_blob_data(self.repo, self.source, entry.object_id())?;
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        materialize_symlink(out_path, &target)?;

        hash_framed_entry(
            &mut self.hasher,
            entry_rel.to_string_lossy().as_bytes(),
            b"\x00link\x00",
            &target,
        );
        Ok(())
    }
}

/// HEAD of a local working-tree repo; a non-repo or unborn HEAD yields the
/// `"link"` sentinel, since link mode must tolerate a plain directory.
pub fn read_local_head(path: &str) -> Result<String> {
    let Ok(repo) = gix::open(path) else {
        return Ok("link".to_owned());
    };
    match repo.head_id() {
        Ok(id) => Ok(id.to_hex().to_string()),
        Err(_) => Ok("link".to_owned()),
    }
}

/// True when `git` is a local filesystem path (absolute or existing), not a scheme/scp-style URL.
#[must_use]
pub fn is_local_path(git: &str) -> bool {
    if git.contains("://") {
        return false;
    }
    let first_slash = git.find('/');
    if let Some(colon) = git.find(':')
        && first_slash.is_none_or(|slash| colon < slash)
    {
        return false;
    }
    let path = Path::new(git);
    path.is_absolute() || path.exists()
}

/// Rejects any git tree filename that is not a single inert path component, so a
/// malicious tree can never escape the staging dir when joined onto a path.
fn safe_component(name: &str) -> Result<&str> {
    let unsafe_component =
        name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\');
    if unsafe_component {
        return Err(Error::Source(format!("unsafe path component: {name:?}")));
    }
    Ok(name)
}

fn hash_framed_entry(hasher: &mut blake3::Hasher, rel_path: &[u8], tag: &[u8], payload: &[u8]) {
    hasher.update(&(rel_path.len() as u64).to_le_bytes());
    hasher.update(rel_path);
    hasher.update(tag);
    hasher.update(&(payload.len() as u64).to_le_bytes());
    hasher.update(payload);
}

fn set_deterministic_mtime(path: &Path, commit_time: u64) -> Result<()> {
    let seconds = i64::try_from(commit_time)
        .map_err(|e| Error::Source(format!("commit_time out of range: {e}")))?;
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(seconds, 0))?;
    Ok(())
}

#[cfg(unix)]
fn materialize_symlink(out_path: &Path, target: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::OsStr::from_bytes(target);
    std::os::unix::fs::symlink(target, out_path)?;
    Ok(())
}

#[cfg(windows)]
fn materialize_symlink(out_path: &Path, target: &[u8]) -> Result<()> {
    let target = String::from_utf8_lossy(target);
    std::os::windows::fs::symlink_file(target.as_ref(), out_path)?;
    Ok(())
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

    /// Author timestamp of the single commit in [`build_export_fixture`]; every
    /// exported file's mtime must equal this.
    const EXPORT_COMMIT_TIME: u64 = 1_700_000_000;

    const EDITOR_INIT_CONTENT: &[u8] = b"-- editor init\nvim.opt.number = true\n";
    const EDITOR_OPTS_CONTENT: &[u8] = b"-- nested opts\nreturn {}\n";
    const EDITOR_RUN_CONTENT: &[u8] = b"#!/bin/sh\necho run\n";
    const EDITOR_NOTES_CONTENT: &[u8] = b"scratch notes, excluded by **/*.bak\n";
    const LINK_NAME: &str = "link";
    const LINK_TARGET: &str = "init.lua";
    const ROOT_SYMLINK_NAME: &str = "badlink";

    struct ExportFixture {
        _src: TempDir,
        _git_dir: TempDir,
        backend: GitBackend,
        url: String,
        /// Sole commit; its author time equals [`EXPORT_COMMIT_TIME`].
        commit: String,
    }

    fn matcher(include: &[&str], exclude: &[&str]) -> PathMatcher {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        PathMatcher::new(&inc, &exc).expect("patterns build into a matcher")
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

    /// Base layout plus a top-level symlink `badlink`; used ONLY to test that a
    /// symlink-as-artifact at root is rejected.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_fixture_with_root_symlink() -> ExportFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();

        init_export_repo(src_path);

        let editor = src_path.join("editor");
        std::fs::create_dir_all(&editor).unwrap();
        std::fs::write(editor.join("init.lua"), EDITOR_INIT_CONTENT).unwrap();
        run_git(src_path, &["add", "-A"]);

        std::os::unix::fs::symlink("editor", src_path.join(ROOT_SYMLINK_NAME)).unwrap();
        run_git(src_path, &["add", ROOT_SYMLINK_NAME]);

        let commit = commit_export_repo(src_path);

        let badlink_mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", ROOT_SYMLINK_NAME]).stdout)
                .unwrap();
        assert!(
            badlink_mode.starts_with("120000"),
            "{ROOT_SYMLINK_NAME} must be committed as a git symlink (120000), got: {badlink_mode}"
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

    fn digest_of_art(fixture: &ExportFixture) -> String {
        let m = matcher(&[], &[]);
        fixture
            .backend
            .compute_digest(
                "src",
                &fixture.url,
                &fixture.commit,
                Some(Path::new("art")),
                &m,
            )
            .expect("digest computes over the art subtree")
    }

    fn init_export_repo(src_path: &Path) {
        run_git(src_path, &["init", "-b", "main", "."]);
        run_git(src_path, &["config", "user.email", "test@example.com"]);
        run_git(src_path, &["config", "user.name", "Test"]);
        run_git(src_path, &["config", "core.autocrlf", "false"]);
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

    fn mtime_secs(path: &Path) -> u64 {
        let ft = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(path).expect("metadata of exported file"),
        );
        u64::try_from(ft.unix_seconds()).expect("non-negative mtime")
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

    // ---- discover_artifacts ----

    #[test]
    fn discover_returns_top_level_artifact_dirs_sorted() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let m = matcher(&[], &[]);
        let artifacts = fixture
            .backend
            .discover_artifacts("src", &fixture.url, &fixture.commit, None, &m)
            .expect("discover walks the git tree");

        assert_eq!(
            artifacts,
            vec![
                "editor".to_string(),
                "linky".to_string(),
                "lint".to_string()
            ],
            "only top-level trees, sorted; root files and dotdirs excluded"
        );
    }

    #[test]
    fn discover_skips_dotdirs() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let m = matcher(&[], &[]);
        let artifacts = fixture
            .backend
            .discover_artifacts("src", &fixture.url, &fixture.commit, None, &m)
            .expect("discover succeeds");

        assert!(
            !artifacts.iter().any(|a| a == ".hidden"),
            "names starting with '.' must be skipped"
        );
    }

    #[test]
    fn discover_applies_artifact_level_include() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let m = matcher(&["editor"], &[]);
        let artifacts = fixture
            .backend
            .discover_artifacts("src", &fixture.url, &fixture.commit, None, &m)
            .expect("discover succeeds");

        assert_eq!(
            artifacts,
            vec!["editor".to_string()],
            "artifact-level include must filter discovered names"
        );
    }

    // ---- symlink-as-artifact at root ----

    #[test]
    fn discover_errors_on_symlink_as_artifact_at_root() {
        let fixture = build_fixture_with_root_symlink();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let m = matcher(&[ROOT_SYMLINK_NAME], &[]);
        let result =
            fixture
                .backend
                .discover_artifacts("src", &fixture.url, &fixture.commit, None, &m);

        let err = result
            .expect_err("v1: a symlink-as-artifact at root must error, not be silently dropped");
        assert!(
            !matches!(err, Error::NotImplemented(_)),
            "must be a real symlink-at-root rejection, not the unimplemented stub: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(ROOT_SYMLINK_NAME),
            "rejection must name the offending top-level entry {ROOT_SYMLINK_NAME}, got: {msg}"
        );
        assert!(
            msg.contains("symlink"),
            "rejection must identify a symlink-at-root rejection, got: {msg}"
        );
    }

    #[test]
    fn discover_errors_on_excluded_root_symlink_before_filtering() {
        let fixture = build_fixture_with_root_symlink();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let m = matcher(&[], &[ROOT_SYMLINK_NAME]);
        let result =
            fixture
                .backend
                .discover_artifacts("src", &fixture.url, &fixture.commit, None, &m);

        let err = result.expect_err(
            "v1: a root symlink must error unconditionally, even when the matcher would exclude it",
        );
        assert!(
            !matches!(err, Error::NotImplemented(_)),
            "must be a real symlink-at-root rejection, not the unimplemented stub: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(ROOT_SYMLINK_NAME),
            "the Link check must run before include/exclude filtering and name {ROOT_SYMLINK_NAME}, got: {msg}"
        );
        assert!(
            msg.contains("symlink"),
            "rejection must identify a symlink-at-root rejection, got: {msg}"
        );
    }

    // ---- export_artifact ----

    fn export_named(
        fixture: &ExportFixture,
        artifact: &str,
        staging: &Path,
        m: &PathMatcher,
        policy: &ExportPolicy,
    ) -> Result<ExportResult> {
        let req = ExportRequest {
            source: "src",
            url: &fixture.url,
            commit: &fixture.commit,
            root: None,
            artifact,
            matcher: m,
            policy,
            staging_dir: staging,
            commit_time: EXPORT_COMMIT_TIME,
        };
        fixture.backend.export_artifact(&req)
    }

    fn export_editor(
        fixture: &ExportFixture,
        staging: &Path,
        m: &PathMatcher,
        policy: &ExportPolicy,
    ) -> Result<ExportResult> {
        export_named(fixture, "editor", staging, m, policy)
    }

    #[test]
    fn export_materializes_files_with_exact_content() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &["**/*.bak"]);
        export_editor(&fixture, staging.path(), &m, &ExportPolicy::default())
            .expect("export succeeds");

        assert_eq!(
            std::fs::read(staging.path().join("init.lua")).expect("init.lua exists"),
            EDITOR_INIT_CONTENT
        );
        assert_eq!(
            std::fs::read(staging.path().join("lua/opts.lua")).expect("nested opts.lua exists"),
            EDITOR_OPTS_CONTENT
        );
    }

    #[test]
    fn export_excludes_bak_files_by_path_matcher() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &["**/*.bak"]);
        let result = export_editor(&fixture, staging.path(), &m, &ExportPolicy::default())
            .expect("export succeeds");

        assert!(
            !staging.path().join("notes.bak").exists(),
            "**/*.bak must exclude notes.bak from staging"
        );
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.path == Path::new("notes.bak")),
            "excluded file must not appear in ExportResult.files"
        );
    }

    #[test]
    fn export_result_lists_exported_files() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &["**/*.bak"]);
        let result = export_editor(&fixture, staging.path(), &m, &ExportPolicy::default())
            .expect("export succeeds");

        let mut listed: Vec<String> = result
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().replace('\\', "/"))
            .collect();
        listed.sort();

        assert!(
            listed.contains(&"init.lua".to_string()),
            "files must list init.lua, got {listed:?}"
        );
        assert!(
            listed.contains(&"lua/opts.lua".to_string()),
            "files must list nested lua/opts.lua, got {listed:?}"
        );
        assert!(
            listed.contains(&"bin/run.sh".to_string()),
            "files must list bin/run.sh, got {listed:?}"
        );
    }

    #[test]
    fn export_sets_mtime_to_commit_time() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &["**/*.bak"]);
        let result = export_editor(&fixture, staging.path(), &m, &ExportPolicy::default())
            .expect("export succeeds");

        assert!(!result.files.is_empty(), "expected exported files");
        for file in &result.files {
            let on_disk = staging.path().join(&file.path);
            assert_eq!(
                mtime_secs(&on_disk),
                EXPORT_COMMIT_TIME,
                "exported {} mtime must equal commit_time",
                file.path.display()
            );
            assert_eq!(
                file.mtime, EXPORT_COMMIT_TIME,
                "ManifestFile.mtime must equal commit_time"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn export_preserves_executable_bit_by_default() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &["**/*.bak"]);
        export_editor(&fixture, staging.path(), &m, &ExportPolicy::default())
            .expect("export succeeds");

        let mode = std::fs::metadata(staging.path().join("bin/run.sh"))
            .expect("run.sh exists")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "preserve_executable default true: run.sh must have an exec bit, mode {mode:o}"
        );
    }

    // ---- in-artifact symlink policy ----

    #[test]
    fn export_rejects_symlink_when_policy_disallows() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &[]);
        let policy = ExportPolicy {
            allow_symlinks: false,
            ..ExportPolicy::default()
        };
        let err = export_named(&fixture, "linky", staging.path(), &m, &policy)
            .expect_err("linky/link is a symlink; allow_symlinks=false must error");

        assert!(
            !matches!(err, Error::NotImplemented(_)),
            "must be a real symlink-policy rejection, not the unimplemented stub: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(LINK_NAME),
            "rejection must name the offending symlink {LINK_NAME}, got: {msg}"
        );
        assert!(
            msg.contains("symlink"),
            "rejection must identify a symlink-policy rejection, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_materializes_symlink_when_allowed() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");
        let staging = TempDir::new().expect("staging dir");

        let m = matcher(&[], &[]);
        let policy = ExportPolicy {
            allow_symlinks: true,
            ..ExportPolicy::default()
        };
        export_named(&fixture, "linky", staging.path(), &m, &policy)
            .expect("export with symlinks allowed");

        let link = staging.path().join(LINK_NAME);
        let meta = std::fs::symlink_metadata(&link).expect("link entry exists");
        assert!(
            meta.file_type().is_symlink(),
            "allowed symlink must be materialized as a symlink, not dereferenced"
        );
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            Path::new(LINK_TARGET),
            "symlink target must be preserved verbatim"
        );
    }

    // ---- compute_digest ----

    #[test]
    fn compute_digest_is_blake3_prefixed_and_stable() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let m = matcher(&[], &[]);
        let first = fixture
            .backend
            .compute_digest("src", &fixture.url, &fixture.commit, None, &m)
            .expect("digest computes");
        let second = fixture
            .backend
            .compute_digest("src", &fixture.url, &fixture.commit, None, &m)
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
    fn compute_digest_frames_entries_so_content_cannot_bleed_into_next_path() {
        let mut bled_content = b"X".to_vec();
        bled_content.extend_from_slice(b"b");
        bled_content.extend_from_slice(FILE_TAG);
        bled_content.extend_from_slice(b"Y");

        let one_file = build_collision_fixture(&[("a", &bled_content)]);
        let two_files = build_collision_fixture(&[("a", b"X"), ("b", b"Y")]);
        one_file
            .backend
            .fetch("src", &one_file.url)
            .expect("fetch one-file tree");
        two_files
            .backend
            .fetch("src", &two_files.url)
            .expect("fetch two-file tree");

        assert_ne!(
            digest_of_art(&one_file),
            digest_of_art(&two_files),
            "distinct layouts whose naive path||tag||content streams are byte-identical \
             must hash differently; entries need length framing"
        );
    }

    #[test]
    fn compute_digest_reflects_matched_tree_not_matcher_config() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let digest = |m: &PathMatcher| {
            fixture
                .backend
                .compute_digest("src", &fixture.url, &fixture.commit, None, m)
                .expect("digest computes")
        };

        let no_exclude = digest(&matcher(&[], &[]));
        let exclude_nothing = digest(&matcher(&[], &["**/*.nonexistent"]));
        let exclude_lua = digest(&matcher(&[], &["**/*.lua"]));

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
}
