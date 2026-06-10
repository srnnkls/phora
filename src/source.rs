//! Source port (`SourceBackend`) and its git adapter (`GitBackend`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gix::object::tree::EntryKind;

use crate::config::Refspec;
use crate::error::{Error, Result};
use crate::kernel::{Commit, Digest, Selection};
use crate::store::ManifestFile;

/// Re-exported beside the backends it composes; defined in `backend` to keep routing separate.
pub use crate::backend::RouterBackend;

/// gix clones origin as refs/remotes/origin/*; a mirror must update refs/heads/* directly.
const MIRROR_REFSPEC: &str = "+refs/heads/*:refs/heads/*";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Https,
    Ssh,
}

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
    pub selection: &'a Selection,
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
        selection: &Selection,
    ) -> Result<Vec<String>>;

    fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult>;

    /// Blake3 fingerprint of the matched subtree at the resolved commit — the
    /// selected source bytes, not the deploy/artifact set.
    ///
    /// Filters by path-level selection only (`Selection::selects_path`); the
    /// artifact dotfile opt-in does not apply, so a top-level `.config` is hashed
    /// under path rules regardless of that gate.
    ///
    /// No production consumer: lock reuse is decided by `lock::source_matches`,
    /// never this digest. Only test assertions read it.
    fn compute_digest(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &Selection,
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

/// `<MirrorKey>.git` under `git_dir`; the single source of mirror-directory layout.
pub(crate) fn mirror_path(git_dir: &Path, url: &str) -> PathBuf {
    let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
    git_dir.join(format!("{}.git", key.as_str()))
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
        mirror_path(&self.git_dir, url)
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
                let commit: Commit = rev
                    .parse()
                    .map_err(|e| Error::Source(format!("parse rev {rev} in {source}: {e}")))?;
                let oid = gix::ObjectId::from_hex(commit.as_str().as_bytes())
                    .map_err(|e| Error::Source(format!("parse rev {rev} in {source}: {e}")))?;
                repo.find_commit(oid)
                    .map_err(|e| Error::Source(format!("rev {rev} in {source}: {e}")))?
            }
            Refspec::None => {
                return Err(Error::Source(format!(
                    "source {source}: git backend cannot resolve a url source's empty refspec"
                )));
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
        selection: &Selection,
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

            let is_candidate = selection.selects_artifact(&name);
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
            selection: req.selection,
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
        selection: &Selection,
    ) -> Result<String> {
        let repo = self.open_mirror(source, url)?;
        let subtree = Self::subtree_at_root(&repo, source, commit, root)?;

        let mut hasher = blake3::Hasher::new();
        Self::hash_tree(
            &repo,
            source,
            &subtree,
            Path::new(""),
            selection,
            &mut hasher,
        )?;
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
        selection: &Selection,
        hasher: &mut blake3::Hasher,
    ) -> Result<()> {
        for entry in tree.iter() {
            let entry =
                entry.map_err(|e| Error::Source(format!("read tree entry in {source}: {e}")))?;
            let component = safe_component(&entry.filename().to_string())?.to_string();
            let entry_rel = rel_path.join(component);
            let is_dir = matches!(entry.kind(), EntryKind::Tree);

            if !selection.selects_path(&entry_rel, is_dir) {
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
                    Self::hash_tree(repo, source, &subtree, &entry_rel, selection, hasher)?;
                }
                EntryKind::Commit => {}
            }
        }
        Ok(())
    }
}

/// A download scratch file under `git_dir`, removed on drop.
struct TempDownload {
    path: PathBuf,
}

impl TempDownload {
    fn create(git_dir: &Path) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!(".phora-download-{}-{nonce}.tmp", std::process::id());
        Self {
            path: git_dir.join(name),
        }
    }
}

impl Drop for TempDownload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Url-source adapter: downloads + extracts + imports a synthetic mirror, then
/// reads it through an inner [`GitBackend`] over the same `git_dir`.
pub struct HttpBackend {
    git_dir: PathBuf,
    git: GitBackend,
    digests: BTreeMap<String, Digest>,
}

impl HttpBackend {
    #[must_use]
    pub fn new(git_dir: PathBuf, digests: BTreeMap<String, Digest>) -> Self {
        let git = GitBackend::new(git_dir.clone());
        Self {
            git_dir,
            git,
            digests,
        }
    }
}

impl SourceBackend for HttpBackend {
    fn fetch(&self, source: &str, url: &str) -> Result<()> {
        std::fs::create_dir_all(&self.git_dir)
            .map_err(|e| Error::Source(format!("source {source}: create git dir: {e}")))?;
        let temp = TempDownload::create(&self.git_dir);

        crate::http::download(url, &temp.path)?;

        if let Some(expected) = self.digests.get(source) {
            let bytes = std::fs::read(&temp.path)
                .map_err(|e| Error::Source(format!("source {source}: read download: {e}")))?;
            crate::http::verify_digest(&bytes, expected)
                .map_err(|e| Error::Source(format!("source {source}: {e}")))?;
        }

        let entries = crate::archive::extract(&temp.path, url)?;
        import_tree(&self.git_dir, url, &entries)?;
        Ok(())
    }

    /// Resolve ignores the refspec: url sources live at refs/heads/phora.
    fn resolve(&self, source: &str, url: &str, _refspec: &Refspec) -> Result<String> {
        let mirror = mirror_path(&self.git_dir, url);
        let repo =
            gix::open(&mirror).map_err(|e| Error::Source(format!("open mirror {source}: {e}")))?;
        let commit = repo
            .find_reference(IMPORT_REF)
            .map_err(|e| Error::Source(format!("{IMPORT_REF} in {source}: {e}")))?
            .peel_to_commit()
            .map_err(|e| Error::Source(format!("peel {IMPORT_REF} in {source}: {e}")))?;
        Ok(commit.id().to_hex().to_string())
    }

    fn commit_time(&self, source: &str, url: &str, commit: &str) -> Result<u64> {
        self.git.commit_time(source, url, commit)
    }

    fn discover_artifacts(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &Selection,
    ) -> Result<Vec<String>> {
        self.git
            .discover_artifacts(source, url, commit, root, selection)
    }

    fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult> {
        self.git.export_artifact(req)
    }

    fn compute_digest(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &Selection,
    ) -> Result<String> {
        self.git
            .compute_digest(source, url, commit, root, selection)
    }
}

struct ExportWalk<'a> {
    repo: &'a gix::Repository,
    source: &'a str,
    out_base: &'a Path,
    selection: &'a Selection,
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

            if !self.selection.selects_path(&entry_rel, is_dir) {
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

// Fixed identity/time/message: with no parents and git-sorted trees the commit id is a
// function of the imported content only — identical entries yield an identical commit id.
const IMPORT_NAME: &str = "phora";
const IMPORT_EMAIL: &str = "phora@localhost";
const IMPORT_MESSAGE: &str = "phora synthetic import";
// epoch+1, not epoch 0: HFS+/FAT32 clamp a 0 mtime on EXPORTED files, making clean checks
// report Modified; the commit id is pure content, so this never affects determinism.
const IMPORT_TIME_SECONDS: i64 = 1;
const IMPORT_REF: &str = "refs/heads/phora";

fn to_gix_entry_kind(kind: crate::archive::EntryKind) -> EntryKind {
    match kind {
        crate::archive::EntryKind::Blob => EntryKind::Blob,
        crate::archive::EntryKind::BlobExecutable => EntryKind::BlobExecutable,
        crate::archive::EntryKind::Link => EntryKind::Link,
    }
}

/// Writes `entries` as a synthetic commit on `refs/heads/phora` in the bare mirror for
/// `url` (created if absent), returning the commit id as hex. The id is content-addressed:
/// fixed identity/time/message + no parents + git-sorted trees ⇒ identical content, identical id.
pub(crate) fn import_tree(
    git_dir: &Path,
    url: &str,
    entries: &[crate::archive::ExtractedEntry],
) -> Result<String> {
    let mirror = mirror_path(git_dir, url);

    let repo = if mirror.exists() {
        gix::open(&mirror).map_err(|e| Error::Source(format!("open mirror {url}: {e}")))?
    } else {
        gix::init_bare(&mirror).map_err(|e| Error::Source(format!("init mirror {url}: {e}")))?
    };

    let mut root = ImportDir::default();
    for entry in entries {
        let oid = repo
            .write_blob(&entry.data)
            .map_err(|e| Error::Source(format!("write blob for {url}: {e}")))?
            .detach();
        root.insert(&entry.path, to_gix_entry_kind(entry.kind), oid)?;
    }

    let root_oid = write_import_tree(&repo, &root, url)?;

    let signature = gix::actor::Signature {
        name: IMPORT_NAME.into(),
        email: IMPORT_EMAIL.into(),
        time: gix::date::Time {
            seconds: IMPORT_TIME_SECONDS,
            offset: 0,
        },
    };
    let commit = gix::objs::Commit {
        tree: root_oid,
        parents: std::iter::empty::<gix::ObjectId>().collect(),
        author: signature.clone(),
        committer: signature,
        encoding: None,
        message: IMPORT_MESSAGE.into(),
        extra_headers: vec![],
    };
    let commit_id = repo
        .write_object(&commit)
        .map_err(|e| Error::Source(format!("write import commit for {url}: {e}")))?
        .detach();

    // PreviousValue::Any: create the ref if absent, force-update it on re-import.
    repo.reference(
        IMPORT_REF,
        commit_id,
        gix::refs::transaction::PreviousValue::Any,
        IMPORT_MESSAGE,
    )
    .map_err(|e| Error::Source(format!("update {IMPORT_REF} for {url}: {e}")))?;

    Ok(commit_id.to_hex().to_string())
}

/// Mutable in-memory directory while assembling the import; leaves carry a written
/// blob oid plus the git mode to encode, subdirectories nest further `ImportDir`s.
#[derive(Default)]
struct ImportDir {
    children: std::collections::BTreeMap<String, ImportNode>,
}

enum ImportNode {
    Leaf { kind: EntryKind, oid: gix::ObjectId },
    Dir(ImportDir),
}

impl ImportDir {
    fn insert(&mut self, path: &Path, kind: EntryKind, oid: gix::ObjectId) -> Result<()> {
        let mut components = Vec::new();
        for component in path.components() {
            let name = component.as_os_str().to_str().ok_or_else(|| {
                Error::Source(format!("non-utf8 import path: {}", path.display()))
            })?;
            components.push(safe_component(name)?.to_string());
        }
        let Some((leaf, dirs)) = components.split_last() else {
            return Err(Error::Source("empty import path".to_owned()));
        };

        let mut dir = self;
        for segment in dirs {
            let node = dir
                .children
                .entry(segment.clone())
                .or_insert_with(|| ImportNode::Dir(ImportDir::default()));
            match node {
                ImportNode::Dir(child) => dir = child,
                ImportNode::Leaf { .. } => {
                    return Err(Error::Source(format!(
                        "import path collides file and directory at {segment:?}"
                    )));
                }
            }
        }
        match dir.children.entry(leaf.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(ImportNode::Leaf { kind, oid });
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(existing) => match existing.get() {
                ImportNode::Dir(_) => Err(Error::Source(format!(
                    "import path collides file and directory at {leaf:?}"
                ))),
                ImportNode::Leaf { .. } => Err(Error::Source(format!(
                    "duplicate archive entry path: {leaf:?}"
                ))),
            },
        }
    }
}

fn write_import_tree(repo: &gix::Repository, dir: &ImportDir, url: &str) -> Result<gix::ObjectId> {
    let mut entries = Vec::with_capacity(dir.children.len());
    for (name, node) in &dir.children {
        let (mode, oid) = match node {
            ImportNode::Leaf { kind, oid } => ((*kind).into(), *oid),
            ImportNode::Dir(child) => {
                let child_oid = write_import_tree(repo, child, url)?;
                (EntryKind::Tree.into(), child_oid)
            }
        };
        entries.push(gix::objs::tree::Entry {
            mode,
            filename: name.as_str().into(),
            oid,
        });
    }
    // Git tree order treats directory names as suffixed with '/'; Entry's Ord encodes
    // exactly that, so sorting here makes the tree id input-order independent.
    entries.sort();

    let tree = gix::objs::Tree { entries };
    Ok(repo
        .write_object(&tree)
        .map_err(|e| Error::Source(format!("write tree for {url}: {e}")))?
        .detach())
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

/// Rejects any filename that is not a single inert path component, so a malicious git tree or
/// archive can never escape the staging dir when joined onto a path.
pub(crate) fn safe_component(name: &str) -> Result<&str> {
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

    fn matcher(include: &[&str], exclude: &[&str]) -> Selection {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        Selection::new(&inc, &exc).expect("patterns build into a selection")
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
        m: &Selection,
        policy: &ExportPolicy,
    ) -> Result<ExportResult> {
        let req = ExportRequest {
            source: "src",
            url: &fixture.url,
            commit: &fixture.commit,
            root: None,
            artifact,
            selection: m,
            policy,
            staging_dir: staging,
            commit_time: EXPORT_COMMIT_TIME,
        };
        fixture.backend.export_artifact(&req)
    }

    fn export_editor(
        fixture: &ExportFixture,
        staging: &Path,
        m: &Selection,
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

    // ---- import_tree (HTP-004): deterministic synthetic-commit import ----

    use crate::archive::{EntryKind as BlobKind, ExtractedEntry};

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

        let out = Command::new("git")
            .args([
                "--git-dir",
                mirror.to_str().expect("mirror path is utf8"),
                "fsck",
                "--strict",
            ])
            .output()
            .expect("git fsck runs");

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
            matches!(result, Err(Error::Source(_))),
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
            matches!(dir_after_file, Err(Error::Source(_))),
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
            matches!(file_after_dir, Err(Error::Source(_))),
            "a directory then a file at the same name must error"
        );
    }

    #[test]
    fn compute_digest_reflects_matched_tree_not_matcher_config() {
        let fixture = build_export_fixture();
        fixture.backend.fetch("src", &fixture.url).expect("fetch");

        let digest = |m: &Selection| {
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

    // ---- HTP-005 B/C: HttpBackend (url source) at the trait level ----

    mod http_backend {
        use std::collections::BTreeMap;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::path::Path;
        use std::time::Duration;

        use gix::object::tree::EntryKind;
        use tempfile::TempDir;

        use crate::config::Refspec;
        use crate::error::Error;
        use crate::kernel::{Digest, Selection};
        use crate::source::{ExportPolicy, ExportRequest, HttpBackend, SourceBackend, mirror_path};

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

        fn empty_matcher() -> Selection {
            Selection::new(&[], &[]).expect("empty selection builds")
        }

        #[test]
        fn fetch_then_resolve_ignores_refspec_and_reads_phora_head() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

            backend
                .fetch("pkg", &url)
                .expect("fetch downloads, extracts, and imports a tree");

            let resolved = backend
                .resolve("pkg", &url, &Refspec::Branch("main".into()))
                .expect("resolve must read refs/heads/phora, ignoring the bogus Branch(main)");

            assert_eq!(resolved.len(), 40, "resolve returns a 40-hex commit id");
            assert!(
                resolved.chars().all(|c| c.is_ascii_hexdigit()),
                "resolve returns a hex commit id, got: {resolved}"
            );

            let none_resolved = backend
                .resolve("pkg", &url, &Refspec::None)
                .expect("resolve with Refspec::None must also read the synthetic phora head");
            assert_eq!(
                none_resolved, resolved,
                "resolve must yield the same synthetic commit regardless of the passed refspec, \
                 proving it ignores the refspec and reads refs/heads/phora"
            );
        }

        #[test]
        fn commit_time_of_synthetic_commit_is_epoch_plus_one() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

            backend.fetch("pkg", &url).expect("fetch");
            let commit = backend
                .resolve("pkg", &url, &Refspec::None)
                .expect("resolve synthetic head");

            let time = backend
                .commit_time("pkg", &url, &commit)
                .expect("commit_time of synthetic commit");
            assert_eq!(
                time, 1,
                "the synthetic import commit's author time is epoch+1 (==1)"
            );
        }

        #[test]
        fn discover_and_export_yield_stripped_files_with_exec_bit() {
            let server = TarServer::spawn(build_pkg_tar_gz());
            let url = server.url();
            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

            backend.fetch("pkg", &url).expect("fetch");
            let commit = backend
                .resolve("pkg", &url, &Refspec::None)
                .expect("resolve synthetic head");
            let m = empty_matcher();

            let artifacts = backend
                .discover_artifacts("pkg", &url, &commit, None, &m)
                .expect("discover artifacts over the synthetic tree");
            assert_eq!(
                artifacts,
                vec!["bin".to_string()],
                "after pkg-1.0/ strip the only top-level directory artifact is `bin`; \
                 `hello.txt` is a root file, not an artifact dir"
            );

            let staging = TempDir::new().expect("staging tempdir");
            let policy = ExportPolicy {
                allow_symlinks: false,
                allow_submodules: false,
                preserve_executable: true,
            };
            let export = backend
                .export_artifact(&ExportRequest {
                    source: "pkg",
                    url: &url,
                    commit: &commit,
                    root: None,
                    artifact: "bin",
                    selection: &m,
                    policy: &policy,
                    staging_dir: staging.path(),
                    commit_time: 1,
                })
                .expect("export the `bin` artifact");

            let run = export
                .files
                .iter()
                .find(|f| f.path == Path::new("run.sh"))
                .expect("export must contain run.sh under the bin artifact");
            let run_on_disk = staging.path().join("run.sh");
            assert_eq!(
                std::fs::read(&run_on_disk).expect("read exported run.sh"),
                RUN_BODY,
                "exported run.sh content must equal the served file bytes"
            );
            assert_eq!(
                run.size,
                RUN_BODY.len() as u64,
                "manifest size must match the run.sh body length"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&run_on_disk)
                    .expect("stat exported run.sh")
                    .permissions()
                    .mode();
                assert!(
                    mode & 0o111 != 0,
                    "the 0o755 archive entry must export with the executable bit set, got mode {mode:o}"
                );
            }

            // hello.txt is a root file: discover skips it and export never touches it,
            // so its import fidelity is only provable straight from the git tree.
            let mirror = mirror_path(git_dir.path(), &url);
            let repo = gix::open(&mirror).expect("open synthetic mirror");
            let oid = gix::ObjectId::from_hex(commit.as_bytes()).expect("commit hex");
            let hello = repo
                .find_commit(oid)
                .expect("find synthetic commit")
                .tree()
                .expect("commit tree")
                .lookup_entry_by_path(Path::new("hello.txt"))
                .expect("lookup hello.txt")
                .expect("hello.txt present at the stripped tree root");
            let blob = repo.find_blob(hello.object_id()).expect("hello.txt blob");
            assert_eq!(
                blob.data, HELLO_BODY,
                "the stripped root file hello.txt must import with its served bytes (`hi`)"
            );
        }

        #[test]
        fn matching_digest_lets_fetch_succeed() {
            let tar_gz = build_pkg_tar_gz();
            let server = TarServer::spawn(tar_gz.clone());
            let url = server.url();

            let mut digests = BTreeMap::new();
            digests.insert("pkg".to_string(), Digest::sha256(sha256_of(&tar_gz)));

            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), digests);

            backend
                .fetch("pkg", &url)
                .expect("a matching configured digest must let fetch succeed");

            backend
                .resolve("pkg", &url, &Refspec::None)
                .expect("a verified fetch must create refs/heads/phora");
        }

        #[test]
        fn mismatched_digest_errors_before_import_naming_source() {
            let tar_gz = build_pkg_tar_gz();
            let server = TarServer::spawn(tar_gz);
            let url = server.url();

            let mut digests = BTreeMap::new();
            digests.insert("pkg".to_string(), Digest::sha256([0u8; 32]));

            let git_dir = TempDir::new().expect("git_dir tempdir");
            let backend = HttpBackend::new(git_dir.path().to_path_buf(), digests);

            let err = backend
                .fetch("pkg", &url)
                .expect_err("a non-matching configured digest must fail fetch");
            match err {
                Error::Source(msg) => assert!(
                    msg.contains("pkg"),
                    "the digest-mismatch error must name the source `pkg`, got: {msg}"
                ),
                other => panic!("expected Error::Source on digest mismatch, got: {other:?}"),
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
                backend.resolve("pkg", &url, &Refspec::None).is_err(),
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

            backend.fetch("pkg", &url).expect("fetch");
            let commit = backend
                .resolve("pkg", &url, &Refspec::None)
                .expect("resolve synthetic head");

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
    }
}
