use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use gix::object::tree::EntryKind;

use super::{Commit, Path, Refspec, SourceName, safe_component};

use super::cache::{
    MirrorStaging, fetch_into_mirror, lock_mirror, mirror_path, mirror_path_for_key, open_mirror,
    reclone_mirror, sweep_orphan_staging,
};
use super::inventory::{populate_inventory, snapshot_commit};
use super::resolve::resolve_worktree;
use super::snapshot::{
    ResolvePolicy, ResolveRequest, ResolvedRevision, ResolvedSource, RevisionSpec, SnapshotId,
    SourceDirectoryEntry, SourceDirectoryEntryKind, SourceEntry, SourceIdentity, SourceLocation,
    SourceStore, SourceTimestamp, commit_from_hex,
};
use super::{
    MirrorKey, NormalizedUrl, Result, SourceEntryKind, SourceEntryMeta, SourceError,
    SourceInventory, SourcePath,
};

pub struct GitBackend {
    pub(super) git_dir: PathBuf,
}

impl GitBackend {
    #[must_use]
    pub fn new(git_dir: PathBuf) -> Self {
        Self { git_dir }
    }

    pub(super) fn mirror_path(&self, url: &str) -> PathBuf {
        mirror_path(&self.git_dir, url)
    }

    /// Reads a remote's root `phora.toml` at `refspec`, reusing an existing mirror
    /// when present and otherwise fetching one. Offline if the mirror is cached.
    ///
    /// # Errors
    /// - the remote cannot be fetched, the ref cannot be resolved, or no `phora.toml` exists.
    pub fn fetch_root_manifest(
        &self,
        source: &SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> Result<Vec<u8>> {
        if self.mirror_path(url).exists() {
            let commit = self.resolve_commit(source, url, refspec)?;
            return self.read_cached_file(source, url, &commit, Path::new("phora.toml"));
        }
        self.shallow_read_root_manifest(source, url, refspec)
    }

    /// Every path whose blob differs between `from_commit` and `to_commit` (added, removed, or
    /// modified), read from `url`'s mirror. Backs the `phora trust` inspect-before-trust diff: both
    /// commits must already be in the mirror (a full `phora sync` clone holds them).
    ///
    /// # Errors
    /// - the mirror is missing, either commit cannot be resolved, or a tree cannot be walked.
    pub fn file_diff_between(
        &self,
        source: &SourceName,
        url: &str,
        from_commit: &str,
        to_commit: &str,
    ) -> Result<Vec<String>> {
        let mirror = self.mirror_path(url);
        let repo = gix::open(&mirror)
            .map_err(|e| SourceError::Source(format!("open mirror {source}: {e}")))?;
        let from = tree_blobs(&repo, source, from_commit)?;
        let to = tree_blobs(&repo, source, to_commit)?;
        let mut changed: BTreeSet<String> = BTreeSet::new();
        for (path, id) in &to {
            if from.get(path) != Some(id) {
                changed.insert(path.clone());
            }
        }
        for path in from.keys() {
            if !to.contains_key(path) {
                changed.insert(path.clone());
            }
        }
        Ok(changed.into_iter().collect())
    }

    /// Reads a remote's root `phora.toml` via a `--depth=1` shallow clone into an
    /// ephemeral staging dir, leaving the persistent mirror cache untouched.
    ///
    /// # Errors
    /// - the shallow clone fails, the ref cannot be resolved, or no `phora.toml` exists.
    fn shallow_read_root_manifest(
        &self,
        source: &SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> Result<Vec<u8>> {
        let depth = std::num::NonZeroU32::new(1).expect("1 is non-zero");
        std::fs::create_dir_all(&self.git_dir)
            .map_err(|e| SourceError::Source(format!("source {source}: create git dir: {e}")))?;
        let staging = MirrorStaging::create(&self.git_dir, url);

        let mut prepare = gix::prepare_clone_bare(url, &staging.path)
            .map_err(|e| SourceError::Source(format!("prepare shallow clone {source}: {e}")))?
            .with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(depth));
        if let Some(refname) = shallow_ref_name(refspec) {
            prepare = prepare.with_ref_name(Some(refname.as_str())).map_err(|e| {
                SourceError::Source(format!("shallow ref {refname} for {source}: {e}"))
            })?;
        }
        let (repo, _) = prepare
            .fetch_only(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
            .map_err(|e| SourceError::Source(format!("shallow clone {source}: {e}")))?;

        let commit = resolve_in(&repo, source, refspec)?;
        read_blob_at(&repo, source, &commit, Path::new("phora.toml"))
    }

    pub(super) fn refresh_mirror(&self, source: &SourceName, url: &str) -> Result<()> {
        let _lock = lock_mirror(&self.git_dir, source, url)?;
        sweep_orphan_staging(&self.git_dir, url);
        let mirror = self.mirror_path(url);

        if let Some(repo) = open_mirror(source, &mirror)?
            && fetch_into_mirror(source, &repo).is_ok()
        {
            return Ok(());
        }
        reclone_mirror(&self.git_dir, source, url, &mirror)
    }

    pub(super) fn resolve_commit(
        &self,
        source: &SourceName,
        url: &str,
        refspec: &Refspec,
    ) -> Result<String> {
        let mirror = self.mirror_path(url);
        let repo = gix::open(&mirror)
            .map_err(|e| SourceError::Source(format!("open mirror {source}: {e}")))?;
        resolve_in(&repo, source, refspec)
    }

    pub(super) fn authored_at(&self, source: &SourceName, url: &str, commit: &str) -> Result<u64> {
        let mirror = self.mirror_path(url);
        let repo = gix::open(&mirror)
            .map_err(|e| SourceError::Source(format!("open mirror {source}: {e}")))?;
        let oid = gix::ObjectId::from_hex(commit.as_bytes())
            .map_err(|e| SourceError::Source(format!("parse commit {commit} in {source}: {e}")))?;
        let commit_obj = repo
            .find_commit(oid)
            .map_err(|e| SourceError::Source(format!("commit {commit} in {source}: {e}")))?;
        let seconds = commit_obj
            .author()
            .map_err(|e| SourceError::Source(format!("author of {commit} in {source}: {e}")))?
            .time()
            .map_err(|e| SourceError::Source(format!("author time of {commit} in {source}: {e}")))?
            .seconds;
        u64::try_from(seconds)
            .map_err(|e| SourceError::Source(format!("author time of {commit} in {source}: {e}")))
    }

    fn read_cached_file(
        &self,
        source: &SourceName,
        url: &str,
        commit: &str,
        path: &Path,
    ) -> Result<Vec<u8>> {
        let mirror = self.mirror_path(url);
        let repo = gix::open(&mirror)
            .map_err(|e| SourceError::Source(format!("open mirror {source}: {e}")))?;
        read_blob_at(&repo, source, commit, path)
    }
}

fn shallow_ref_name(refspec: &Refspec) -> Option<String> {
    match refspec {
        Refspec::Branch(name) => Some(format!("refs/heads/{name}")),
        Refspec::Tag(name) => Some(format!("refs/tags/{name}")),
        Refspec::Rev(_) | Refspec::Default | Refspec::None => None,
    }
}

fn resolve_in(repo: &gix::Repository, source: &SourceName, refspec: &Refspec) -> Result<String> {
    let commit = match refspec {
        Refspec::Branch(name) => repo
            .find_reference(&format!("refs/heads/{name}"))
            .map_err(|e| SourceError::Source(format!("branch {name} in {source}: {e}")))?
            .peel_to_commit()
            .map_err(|e| SourceError::Source(format!("peel branch {name} in {source}: {e}")))?,
        Refspec::Tag(name) => repo
            .find_reference(&format!("refs/tags/{name}"))
            .map_err(|e| SourceError::Source(format!("tag {name} in {source}: {e}")))?
            .peel_to_commit()
            .map_err(|e| SourceError::Source(format!("peel tag {name} in {source}: {e}")))?,
        Refspec::Rev(rev) => {
            let commit: Commit = rev
                .parse()
                .map_err(|e| SourceError::Source(format!("parse rev {rev} in {source}: {e}")))?;
            let oid = gix::ObjectId::from_hex(commit.as_str().as_bytes())
                .map_err(|e| SourceError::Source(format!("parse rev {rev} in {source}: {e}")))?;
            repo.find_commit(oid)
                .map_err(|e| SourceError::Source(format!("rev {rev} in {source}: {e}")))?
        }
        Refspec::Default => repo
            .head_commit()
            .map_err(|e| SourceError::Source(format!("default branch (HEAD) in {source}: {e}")))?,
        Refspec::None => {
            return Err(SourceError::Source(format!(
                "source {source}: git backend cannot resolve a url source's empty refspec"
            )));
        }
    };
    Ok(commit.id().to_hex().to_string())
}

fn tree_blobs(
    repo: &gix::Repository,
    source: &SourceName,
    commit: &str,
) -> Result<BTreeMap<String, gix::ObjectId>> {
    let oid = gix::ObjectId::from_hex(commit.as_bytes())
        .map_err(|e| SourceError::Source(format!("parse commit {commit} in {source}: {e}")))?;
    let tree = repo
        .find_commit(oid)
        .map_err(|e| SourceError::Source(format!("commit {commit} in {source}: {e}")))?
        .tree()
        .map_err(|e| SourceError::Source(format!("tree of {commit} in {source}: {e}")))?;
    let mut blobs = BTreeMap::new();
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(|e| SourceError::Source(format!("walk tree of {commit} in {source}: {e}")))?;
    for entry in recorder.records {
        if entry.mode.is_blob() {
            blobs.insert(entry.filepath.to_string(), entry.oid);
        }
    }
    Ok(blobs)
}

fn read_blob_at(
    repo: &gix::Repository,
    source: &SourceName,
    commit: &str,
    path: &Path,
) -> Result<Vec<u8>> {
    let oid = gix::ObjectId::from_hex(commit.as_bytes())
        .map_err(|e| SourceError::Source(format!("parse commit {commit} in {source}: {e}")))?;
    let tree = repo
        .find_commit(oid)
        .map_err(|e| SourceError::Source(format!("commit {commit} in {source}: {e}")))?
        .tree()
        .map_err(|e| SourceError::Source(format!("tree of {commit} in {source}: {e}")))?;
    let display = path.display();
    let entry = tree
        .lookup_entry_by_path(path)
        .map_err(|e| SourceError::Source(format!("read {display} at {commit}: {e}")))?
        .ok_or_else(|| SourceError::FileAbsent {
            source_name: source.to_string(),
            commit: commit.to_owned(),
            path: path.to_owned(),
        })?;
    if !entry.mode().is_blob() {
        return Err(SourceError::Source(format!(
            "{display} at {commit} in {source} is not a regular file"
        )));
    }
    let object = entry
        .object()
        .map_err(|e| SourceError::Source(format!("read {display} at {commit}: {e}")))?;
    Ok(object.data.clone())
}

impl GitBackend {
    /// Every blob leaf under `tree`, breadth-first into `(forward-slashed root-relative
    /// path, frame tag, oid)`. The tag distinguishes exec/link/file so the digest frames
    /// match the historical tree-walk byte stream.
    fn collect_digest_leaves(
        repo: &gix::Repository,
        source: &str,
        tree: &gix::Tree<'_>,
        rel_path: &Path,
        leaves: &mut Vec<(String, &'static [u8], gix::ObjectId)>,
    ) -> Result<()> {
        for entry in tree.iter() {
            let entry = entry
                .map_err(|e| SourceError::Source(format!("read tree entry in {source}: {e}")))?;
            let component = safe_component(&entry.filename().to_string())?.to_string();
            let entry_rel = rel_path.join(component);

            match entry.kind() {
                EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                    let tag: &[u8] = match entry.kind() {
                        EntryKind::BlobExecutable => b"\x00exec\x00",
                        EntryKind::Link => b"\x00link\x00",
                        _ => b"\x00file\x00",
                    };
                    leaves.push((
                        entry_rel.to_string_lossy().replace('\\', "/"),
                        tag,
                        entry.object_id(),
                    ));
                }
                EntryKind::Tree => {
                    let subtree = repo
                        .find_tree(entry.object_id())
                        .map_err(|e| SourceError::Source(format!("subtree in {source}: {e}")))?;
                    Self::collect_digest_leaves(repo, source, &subtree, &entry_rel, leaves)?;
                }
                EntryKind::Commit => {}
            }
        }
        Ok(())
    }

    fn open_snapshot(&self, snapshot: &SnapshotId) -> Result<gix::Repository> {
        let mirror = mirror_path_for_key(&self.git_dir, snapshot.mirror());
        gix::open(&mirror).map_err(|e| {
            SourceError::Source(format!(
                "open snapshot mirror {}: {e}",
                snapshot.mirror().as_str()
            ))
        })
    }

    fn commit_tree<'repo>(
        repo: &'repo gix::Repository,
        source: &str,
        commit: &str,
    ) -> Result<gix::Tree<'repo>> {
        let oid = gix::ObjectId::from_hex(commit.as_bytes())
            .map_err(|e| SourceError::Source(format!("parse commit {commit} in {source}: {e}")))?;
        repo.find_commit(oid)
            .map_err(|e| SourceError::Source(format!("commit {commit} in {source}: {e}")))?
            .tree()
            .map_err(|e| SourceError::Source(format!("tree of {commit} in {source}: {e}")))
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
                        SourceError::Source(format!("lookup root {} in {source}: {e}", r.display()))
                    })?
                    .ok_or_else(|| SourceError::RootNotFound {
                        root: r.to_path_buf(),
                    })?;
                repo.find_tree(entry.object_id()).map_err(|e| {
                    SourceError::Source(format!("root tree {} in {source}: {e}", r.display()))
                })
            }
            None => Ok(tree),
        }
    }

    pub(super) fn find_blob_data(
        repo: &gix::Repository,
        source: &str,
        oid: gix::ObjectId,
    ) -> Result<Vec<u8>> {
        let blob = repo
            .find_blob(oid)
            .map_err(|e| SourceError::Source(format!("blob {oid} in {source}: {e}")))?;
        Ok(blob.data.clone())
    }
}

fn kind_of_tag(tag: &[u8]) -> SourceEntryKind {
    match tag {
        b"\x00exec\x00" => SourceEntryKind::Executable,
        b"\x00link\x00" => SourceEntryKind::Symlink,
        _ => SourceEntryKind::File,
    }
}

fn kind_of_entry(kind: EntryKind) -> Option<SourceEntryKind> {
    match kind {
        EntryKind::Blob => Some(SourceEntryKind::File),
        EntryKind::BlobExecutable => Some(SourceEntryKind::Executable),
        EntryKind::Link => Some(SourceEntryKind::Symlink),
        EntryKind::Tree | EntryKind::Commit => None,
    }
}

impl SourceStore for GitBackend {
    fn resolve(&self, request: &ResolveRequest, policy: ResolvePolicy) -> Result<ResolvedSource> {
        match &request.location {
            SourceLocation::Git { url } => {
                if policy == ResolvePolicy::Refresh {
                    self.refresh_mirror(&request.name, url)?;
                }
                let refspec = legacy_refspec(&request.revision);
                let commit = self.resolve_commit(&request.name, url, &refspec)?;
                let authored_at = self.authored_at(&request.name, url, &commit)?;
                let commit = commit_from_hex(&commit)?;
                let normalized = NormalizedUrl::parse(url);
                Ok(ResolvedSource {
                    name: request.name.clone(),
                    snapshot: SnapshotId::Git {
                        mirror: MirrorKey::from_url(&normalized),
                        commit: commit.clone(),
                    },
                    revision: ResolvedRevision::Commit(commit),
                    authored_at: SourceTimestamp::from_unix_seconds(authored_at),
                    normalized_location: SourceIdentity::Git(normalized),
                })
            }
            SourceLocation::Worktree { root } => {
                if !matches!(request.revision, RevisionSpec::Default | RevisionSpec::None) {
                    return Err(SourceError::Source(format!(
                        "worktree source {} cannot select a git revision",
                        request.name
                    )));
                }
                resolve_worktree(&self.git_dir, &request.name, root)
            }
            SourceLocation::Url { .. } => Err(SourceError::Source(format!(
                "url source {} requires the url source adapter",
                request.name
            ))),
        }
    }

    fn inventory(
        &self,
        snapshot: &SnapshotId,
        root: Option<&SourcePath>,
    ) -> Result<SourceInventory> {
        let name = snapshot.mirror().as_str();
        let commit = snapshot_commit(snapshot).as_str();
        let repo = self.open_snapshot(snapshot)?;
        let tree = Self::subtree_at_root(
            &repo,
            name,
            commit,
            root.map(|path| Path::new(path.as_str())),
        )?;
        let mut leaves = Vec::new();
        Self::collect_digest_leaves(&repo, name, &tree, Path::new(""), &mut leaves)?;
        populate_inventory(
            leaves
                .into_iter()
                .map(|(path, tag, _)| (path, kind_of_tag(tag))),
        )
    }

    fn read(&self, snapshot: &SnapshotId, path: &SourcePath) -> Result<SourceEntry> {
        let name = snapshot.mirror().as_str();
        let commit = snapshot_commit(snapshot).as_str();
        let repo = self.open_snapshot(snapshot)?;
        let tree = Self::commit_tree(&repo, name, commit)?;
        let entry = tree
            .lookup_entry_by_path(Path::new(path.as_str()))
            .map_err(|e| SourceError::Source(format!("read {path} at {commit} in {name}: {e}")))?
            .ok_or_else(|| SourceError::FileAbsent {
                source_name: name.to_owned(),
                commit: commit.to_owned(),
                path: PathBuf::from(path.as_str()),
            })?;
        let kind =
            kind_of_entry(entry.mode().kind()).ok_or_else(|| SourceError::MappedKeyNotALeaf {
                key: PathBuf::from(path.as_str()),
            })?;
        let bytes = Self::find_blob_data(&repo, name, entry.object_id())?;
        Ok(SourceEntry {
            meta: SourceEntryMeta {
                path: path.clone(),
                kind,
            },
            bytes,
        })
    }

    fn list_directory(
        &self,
        snapshot: &SnapshotId,
        path: Option<&SourcePath>,
    ) -> Result<Vec<SourceDirectoryEntry>> {
        let name = snapshot.mirror().as_str();
        let commit = snapshot_commit(snapshot).as_str();
        let repo = self.open_snapshot(snapshot)?;
        let subtree = Self::subtree_at_root(
            &repo,
            name,
            commit,
            path.map(|path| Path::new(path.as_str())),
        )?;
        let mut entries = Vec::new();
        for entry in subtree.iter() {
            let entry = entry
                .map_err(|e| SourceError::Source(format!("read tree entry in {name}: {e}")))?;
            let filename = entry.filename().to_string();
            let component = safe_component(&filename)?;
            let child = path.map_or_else(
                || component.to_owned(),
                |parent| format!("{}/{component}", parent.as_str()),
            );
            let kind = match entry.kind() {
                EntryKind::Blob => SourceDirectoryEntryKind::File,
                EntryKind::BlobExecutable => SourceDirectoryEntryKind::Executable,
                EntryKind::Link => SourceDirectoryEntryKind::Symlink,
                EntryKind::Tree => SourceDirectoryEntryKind::Directory,
                EntryKind::Commit => {
                    return Err(SourceError::Source(format!(
                        "gitlink {child} in {name} is not a supported source entry"
                    )));
                }
            };
            entries.push(SourceDirectoryEntry {
                path: SourcePath::new(&child)?,
                kind,
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }
}

fn legacy_refspec(revision: &RevisionSpec) -> Refspec {
    match revision {
        RevisionSpec::Branch(name) => Refspec::Branch(name.clone()),
        RevisionSpec::Tag(name) => Refspec::Tag(name.clone()),
        RevisionSpec::Commit(commit) => Refspec::Rev(commit.to_string()),
        RevisionSpec::Default => Refspec::Default,
        RevisionSpec::None => Refspec::None,
    }
}
