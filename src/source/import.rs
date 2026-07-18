use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gix::object::tree::EntryKind;

use crate::config::Refspec;
use crate::kernel::{Digest, SourceName, safe_component};

use super::cache::{MirrorStaging, lock_mirror, mirror_path};
use super::{GitBackend, Result, SourceBackend, SourceError};

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
    digests: BTreeMap<SourceName, Digest>,
}

impl HttpBackend {
    #[must_use]
    pub fn new(git_dir: PathBuf, digests: BTreeMap<SourceName, Digest>) -> Self {
        let git = GitBackend::new(git_dir.clone());
        Self {
            git_dir,
            git,
            digests,
        }
    }
}

impl SourceBackend for HttpBackend {
    fn fetch(&self, source: &SourceName, url: &str) -> Result<()> {
        std::fs::create_dir_all(&self.git_dir)
            .map_err(|e| SourceError::Source(format!("source {source}: create git dir: {e}")))?;
        let temp = TempDownload::create(&self.git_dir);

        super::http::download(url, &temp.path)?;

        if let Some(expected) = self.digests.get(source) {
            let bytes = std::fs::read(&temp.path)
                .map_err(|e| SourceError::Source(format!("source {source}: read download: {e}")))?;
            super::http::verify_digest(&bytes, expected)
                .map_err(|e| SourceError::Source(format!("source {source}: {e}")))?;
        }

        let entries = super::archive::extract(&temp.path, url)?;
        let _lock = lock_mirror(&self.git_dir, source, url)?;
        import_tree(&self.git_dir, url, &entries)?;
        Ok(())
    }

    fn mirror_ready(&self, url: &str) -> bool {
        self.git.mirror_ready(url)
    }

    /// Resolve ignores the refspec: url sources live at refs/heads/phora.
    fn resolve(&self, source: &SourceName, url: &str, _refspec: &Refspec) -> Result<String> {
        let mirror = mirror_path(&self.git_dir, url);
        let repo = gix::open(&mirror)
            .map_err(|e| SourceError::Source(format!("open mirror {source}: {e}")))?;
        let commit = repo
            .find_reference(IMPORT_REF)
            .map_err(|e| SourceError::Source(format!("{IMPORT_REF} in {source}: {e}")))?
            .peel_to_commit()
            .map_err(|e| SourceError::Source(format!("peel {IMPORT_REF} in {source}: {e}")))?;
        Ok(commit.id().to_hex().to_string())
    }

    fn commit_time(&self, source: &SourceName, url: &str, commit: &str) -> Result<u64> {
        self.git.commit_time(source, url, commit)
    }

    fn list_source_leaves(
        &self,
        source: &SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
    ) -> Result<Vec<String>> {
        self.git.list_source_leaves(source, url, commit, root)
    }

    fn compute_digest(
        &self,
        source: &SourceName,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        include: &[String],
        exclude: &[String],
    ) -> Result<String> {
        self.git
            .compute_digest(source, url, commit, root, include, exclude)
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

fn to_gix_entry_kind(kind: super::archive::EntryKind) -> EntryKind {
    match kind {
        super::archive::EntryKind::Blob => EntryKind::Blob,
        super::archive::EntryKind::BlobExecutable => EntryKind::BlobExecutable,
        super::archive::EntryKind::Link => EntryKind::Link,
    }
}

/// Writes `entries` as a synthetic commit on `refs/heads/phora` in the bare mirror for
/// `url` (created if absent), returning the commit id as hex. The id is content-addressed:
/// fixed identity/time/message + no parents + git-sorted trees ⇒ identical content, identical id.
pub(super) fn import_tree(
    git_dir: &Path,
    url: &str,
    entries: &[super::archive::ExtractedEntry],
) -> Result<String> {
    let mirror = mirror_path(git_dir, url);

    if mirror.exists() {
        let repo = gix::open(&mirror)
            .map_err(|e| SourceError::Source(format!("open mirror {url}: {e}")))?;
        return write_import(&repo, url, entries);
    }

    std::fs::create_dir_all(git_dir)
        .map_err(|e| SourceError::Source(format!("create git dir for {url}: {e}")))?;
    let staging = MirrorStaging::create(git_dir, url);
    let commit = {
        let repo = gix::init_bare(&staging.path)
            .map_err(|e| SourceError::Source(format!("init mirror {url}: {e}")))?;
        write_import(&repo, url, entries)?
    };
    staging.commit_to(&mirror, url)?;
    Ok(commit)
}

fn write_import(
    repo: &gix::Repository,
    url: &str,
    entries: &[super::archive::ExtractedEntry],
) -> Result<String> {
    let mut root = ImportDir::default();
    for entry in entries {
        let oid = repo
            .write_blob(&entry.data)
            .map_err(|e| SourceError::Source(format!("write blob for {url}: {e}")))?
            .detach();
        root.insert(&entry.path, to_gix_entry_kind(entry.kind), oid)?;
    }

    let root_oid = write_import_tree(repo, &root, url)?;

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
        .map_err(|e| SourceError::Source(format!("write import commit for {url}: {e}")))?
        .detach();

    // PreviousValue::Any: create the ref if absent, force-update it on re-import.
    repo.reference(
        IMPORT_REF,
        commit_id,
        gix::refs::transaction::PreviousValue::Any,
        IMPORT_MESSAGE,
    )
    .map_err(|e| SourceError::Source(format!("update {IMPORT_REF} for {url}: {e}")))?;

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
                SourceError::Source(format!("non-utf8 import path: {}", path.display()))
            })?;
            components.push(safe_component(name)?.to_string());
        }
        let Some((leaf, dirs)) = components.split_last() else {
            return Err(SourceError::Source("empty import path".to_owned()));
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
                    return Err(SourceError::Source(format!(
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
                ImportNode::Dir(_) => Err(SourceError::Source(format!(
                    "import path collides file and directory at {leaf:?}"
                ))),
                ImportNode::Leaf { .. } => Err(SourceError::Source(format!(
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
        .map_err(|e| SourceError::Source(format!("write tree for {url}: {e}")))?
        .detach())
}
