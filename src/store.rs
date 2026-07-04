//! Registry port (`Registry`) and its file adapter (`FileRegistry`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors owned by the store context (`Registry` and its file adapter).
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("registry error: {0}")]
    Registry(String),

    #[error("lock error: {0}")]
    Lock(String),
}

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    File,
    #[default]
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactKey {
    pub target: String,
    pub source: String,
    pub artifact: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct RegistryRecord {
    pub version: u32,
    pub key: ArtifactKey,
    #[serde(default)]
    pub source: String,
    pub commit: String,
    pub digest: String,
    pub projected_at: String,
    pub layout: String,
    #[serde(default)]
    pub kind: RecordKind,
    pub allow_symlinks: bool,
    pub preserve_executable: bool,
    pub files: Vec<ManifestFile>,
    #[serde(default)]
    pub linked: bool,
    /// Digest of the full effective vars map at deploy time; `None` for feature-free artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars_digest: Option<String>,
    /// Deploy-time snapshot of the target's expanded absolute path; never re-expanded at read time. `None` on legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_root: Option<String>,
}

/// Borrowed inputs shared by every record-construction site (`deploy_one`, `rebuild_one`).
pub struct ProjectedRecord<'a> {
    pub key: ArtifactKey,
    pub underlying_source: &'a str,
    pub commit: &'a str,
    pub digest: String,
    pub layout: String,
    pub kind: RecordKind,
    pub allow_symlinks: bool,
    pub preserve_executable: bool,
    pub files: Vec<ManifestFile>,
    pub vars_digest: Option<String>,
    pub deploy_root: Option<String>,
}

impl RegistryRecord {
    /// Build a managed (`linked = false`) record, stamping `projected_at` to now.
    #[must_use]
    pub fn projected(p: ProjectedRecord<'_>) -> Self {
        Self {
            version: 1,
            key: p.key,
            source: p.underlying_source.to_owned(),
            commit: p.commit.to_owned(),
            digest: p.digest,
            projected_at: chrono::Utc::now().to_rfc3339(),
            layout: p.layout,
            kind: p.kind,
            allow_symlinks: p.allow_symlinks,
            preserve_executable: p.preserve_executable,
            files: p.files,
            linked: false,
            vars_digest: p.vars_digest,
            deploy_root: p.deploy_root,
        }
    }
}

/// Registry record file entry (carries the content hash used by `phora verify`).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub blake3: String,
}

/// Filesystem scan entry: stat metadata only, no content hash.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EjectedEntry {
    pub source: String,
    pub artifact: String,
    pub ejected_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookState {
    pub hook_id: String,
    pub last_success: std::collections::BTreeSet<String>,
}

pub use crate::kernel::Digest;

pub trait Registry {
    fn get(&self, key: &ArtifactKey) -> Result<Option<RegistryRecord>>;
    fn put(&self, record: &RegistryRecord) -> Result<()>;
    fn remove(&self, key: &ArtifactKey) -> Result<()>;
    fn list_target(&self, target: &str) -> Result<Vec<RegistryRecord>>;
    fn list_all(&self) -> Result<Vec<RegistryRecord>>;

    fn load_ejected(&self, target: &str) -> Result<Vec<EjectedEntry>>;
    fn save_ejected(&self, target: &str, ejected: &[EjectedEntry]) -> Result<()>;

    /// Per-hook last-success digest sets recorded for `target`.
    ///
    /// # Errors
    ///
    /// Returns an error if the target meta file cannot be read or parsed.
    fn load_hook_state(&self, target: &str) -> Result<Vec<HookState>>;

    /// Advance one hook's last-success set, leaving sibling hooks untouched.
    ///
    /// # Errors
    ///
    /// Returns an error if the target meta file cannot be read, parsed, or written.
    fn record_hook_success(
        &self,
        target: &str,
        hook_id: &str,
        digest_set: &std::collections::BTreeSet<String>,
    ) -> Result<()>;

    /// Directory holding the deploy journal and `state.lock`.
    fn locks_dir(&self) -> PathBuf;
}

/// `(target, source, artifact)` keys ejected across every target `records` span — lets readers tell a kept-but-ejected record from a managed one.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn ejected_index(
    registry: &dyn Registry,
    records: &[RegistryRecord],
) -> Result<std::collections::HashSet<(String, String, String)>> {
    let targets: std::collections::BTreeSet<&str> =
        records.iter().map(|r| r.key.target.as_str()).collect();
    let mut index = std::collections::HashSet::new();
    for target in targets {
        for entry in registry.load_ejected(target)? {
            index.insert((target.to_owned(), entry.source, entry.artifact));
        }
    }
    Ok(index)
}

pub struct FileRegistry {
    state_root: PathBuf,
}

/// RAII guard holding an exclusive OS lock on `state.lock`; released on drop.
#[derive(Debug)]
pub struct StateLockGuard {
    _file: std::fs::File,
}

// flock OFD is inherited by a forked `git` until it execs: the flock-holding test takes write, forkers take read.
#[cfg(test)]
pub(crate) static STATE_LOCK_SERIAL: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[cfg(test)]
pub(crate) fn guard_git_fork() -> std::sync::RwLockReadGuard<'static, ()> {
    STATE_LOCK_SERIAL
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Refuse fixture git outside the temp sandbox, so it can never write a real repo.
#[cfg(test)]
pub(crate) fn assert_git_sandboxed(cwd: &std::path::Path) {
    let sandbox = std::env::temp_dir();
    let sandbox = sandbox.canonicalize().unwrap_or(sandbox);
    let target = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    assert!(
        target.starts_with(&sandbox),
        "fixture git refused outside temp sandbox: {} not under {}",
        target.display(),
        sandbox.display(),
    );
}

const META_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct TargetMeta {
    version: u32,
    #[serde(default)]
    ejected: Vec<EjectedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hooks: Vec<HookState>,
}

impl Default for TargetMeta {
    fn default() -> Self {
        Self {
            version: META_VERSION,
            ejected: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

impl FileRegistry {
    pub fn open(state_root: PathBuf) -> Result<Self> {
        Ok(Self { state_root })
    }

    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    fn record_path(&self, key: &ArtifactKey) -> PathBuf {
        self.state_root
            .join("targets")
            .join(&key.target)
            .join("artifacts")
            .join(&key.source)
            .join(format!("{}.toml", key.artifact))
    }

    fn meta_path(&self, target: &str) -> PathBuf {
        self.state_root
            .join("targets")
            .join(target)
            .join("meta.toml")
    }

    fn read_meta(&self, target: &str) -> Result<TargetMeta> {
        let path = self.meta_path(target);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| StoreError::Registry(format!("parse meta {}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TargetMeta::default()),
            Err(e) => Err(StoreError::Registry(format!(
                "read meta {}: {e}",
                path.display()
            ))),
        }
    }

    fn write_meta(&self, target: &str, mut meta: TargetMeta) -> Result<()> {
        meta.version = META_VERSION;
        let serialized = toml::to_string(&meta)
            .map_err(|e| StoreError::Registry(format!("serialize meta: {e}")))?;
        atomic_write(&self.meta_path(target), &serialized)
    }

    /// Full-replace helper that can clobber sibling hooks; `record_hook_success` is the safe seam.
    ///
    /// # Errors
    ///
    /// Returns an error if the target meta file cannot be read, parsed, or written.
    #[cfg(test)]
    pub(crate) fn save_hook_state(&self, target: &str, hooks: &[HookState]) -> Result<()> {
        let mut meta = self.read_meta(target)?;
        meta.hooks = hooks.to_vec();
        self.write_meta(target, meta)
    }

    pub fn lock_exclusive(&self) -> Result<StateLockGuard> {
        let locks_dir = self.state_root.join("locks");
        std::fs::create_dir_all(&locks_dir).map_err(|e| {
            StoreError::Registry(format!("create locks dir {}: {e}", locks_dir.display()))
        })?;
        let lock_path = locks_dir.join("state.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                StoreError::Registry(format!("open lock file {}: {e}", lock_path.display()))
            })?;
        match file.try_lock() {
            Ok(()) => Ok(StateLockGuard { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::Lock(
                "another phora process is running for this project (state.lock held)".to_owned(),
            )),
            Err(std::fs::TryLockError::Error(e)) => Err(StoreError::Registry(format!(
                "acquire lock on {}: {e}",
                lock_path.display()
            ))),
        }
    }
}

fn read_record(path: &Path) -> Result<RegistryRecord> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| StoreError::Registry(format!("read record {}: {e}", path.display())))?;
    toml::from_str(&text)
        .map_err(|e| StoreError::Registry(format!("parse record {}: {e}", path.display())))
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Registry(format!("path has no parent: {}", path.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| StoreError::Registry(format!("create dir {}: {e}", parent.display())))?;
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        StoreError::Registry(format!("path has no file name: {}", path.display()))
    })?;
    let tmp = parent.join(format!(".{file_name}.tmp"));
    {
        use std::io::Write as _;
        let mut handle = std::fs::File::create(&tmp)
            .map_err(|e| StoreError::Registry(format!("create temp {}: {e}", tmp.display())))?;
        handle
            .write_all(contents.as_bytes())
            .map_err(|e| StoreError::Registry(format!("write temp {}: {e}", tmp.display())))?;
        handle
            .sync_all()
            .map_err(|e| StoreError::Registry(format!("fsync temp {}: {e}", tmp.display())))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StoreError::Registry(format!(
            "rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        ))
    })
}

fn collect_records(artifacts_dir: &Path, out: &mut Vec<RegistryRecord>) -> Result<()> {
    let source_dirs = match std::fs::read_dir(artifacts_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(StoreError::Registry(format!(
                "read dir {}: {e}",
                artifacts_dir.display()
            )));
        }
    };
    for source in source_dirs {
        let source = source.map_err(|e| {
            StoreError::Registry(format!("read entry in {}: {e}", artifacts_dir.display()))
        })?;
        if !source.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        collect_records_under(&source.path(), out)?;
    }
    Ok(())
}

fn collect_records_under(dir: &Path, out: &mut Vec<RegistryRecord>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| StoreError::Registry(format!("read dir {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| StoreError::Registry(format!("read entry in {}: {e}", dir.display())))?;
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_records_under(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            out.push(read_record(&path)?);
        }
    }
    Ok(())
}

fn sort_records(records: &mut [RegistryRecord]) {
    records.sort_by(|a, b| {
        (&a.key.target, &a.key.source, &a.key.artifact).cmp(&(
            &b.key.target,
            &b.key.source,
            &b.key.artifact,
        ))
    });
}

impl Registry for FileRegistry {
    fn get(&self, key: &ArtifactKey) -> Result<Option<RegistryRecord>> {
        let path = self.record_path(key);
        if path.exists() {
            Ok(Some(read_record(&path)?))
        } else {
            Ok(None)
        }
    }

    fn put(&self, record: &RegistryRecord) -> Result<()> {
        let path = self.record_path(&record.key);
        let serialized = toml::to_string(record)
            .map_err(|e| StoreError::Registry(format!("serialize record: {e}")))?;
        atomic_write(&path, &serialized)
    }

    fn remove(&self, key: &ArtifactKey) -> Result<()> {
        let path = self.record_path(key);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Registry(format!(
                "remove record {}: {e}",
                path.display()
            ))),
        }
    }

    fn list_target(&self, target: &str) -> Result<Vec<RegistryRecord>> {
        let artifacts_dir = self
            .state_root
            .join("targets")
            .join(target)
            .join("artifacts");
        let mut records = Vec::new();
        collect_records(&artifacts_dir, &mut records)?;
        sort_records(&mut records);
        Ok(records)
    }

    fn list_all(&self) -> Result<Vec<RegistryRecord>> {
        let targets_dir = self.state_root.join("targets");
        let target_dirs = match std::fs::read_dir(&targets_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(StoreError::Registry(format!(
                    "read dir {}: {e}",
                    targets_dir.display()
                )));
            }
        };
        let mut records = Vec::new();
        for target in target_dirs {
            let target = target.map_err(|e| {
                StoreError::Registry(format!("read entry in {}: {e}", targets_dir.display()))
            })?;
            if !target.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            collect_records(&target.path().join("artifacts"), &mut records)?;
        }
        sort_records(&mut records);
        Ok(records)
    }

    fn load_ejected(&self, target: &str) -> Result<Vec<EjectedEntry>> {
        Ok(self.read_meta(target)?.ejected)
    }

    fn save_ejected(&self, target: &str, ejected: &[EjectedEntry]) -> Result<()> {
        let mut meta = self.read_meta(target)?;
        meta.ejected = ejected.to_vec();
        self.write_meta(target, meta)
    }

    fn load_hook_state(&self, target: &str) -> Result<Vec<HookState>> {
        Ok(self.read_meta(target)?.hooks)
    }

    fn record_hook_success(
        &self,
        target: &str,
        hook_id: &str,
        digest_set: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let mut meta = self.read_meta(target)?;
        match meta.hooks.iter_mut().find(|h| h.hook_id == hook_id) {
            Some(existing) => existing.last_success.clone_from(digest_set),
            None => meta.hooks.push(HookState {
                hook_id: hook_id.to_owned(),
                last_success: digest_set.clone(),
            }),
        }
        self.write_meta(target, meta)
    }

    fn locks_dir(&self) -> PathBuf {
        self.state_root.join("locks")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ProjectId;
    use tempfile::TempDir;

    #[test]
    fn digest_requires_strict_sixty_four_hex_body() {
        use std::str::FromStr as _;
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(
            Digest::from_str(&format!("blake3:{hex}")).is_ok(),
            "a strict 64-hex blake3 body must parse"
        );
        assert!(
            Digest::from_str(&format!("sha256:{hex}")).is_ok(),
            "the unified Digest now also accepts sha256"
        );
        assert!(
            Digest::from_str("blake3:abc").is_err(),
            "a short body must be rejected (the unified Digest is strict)"
        );
        assert!(
            Digest::from_str("blake3:").is_err(),
            "an empty body must be rejected"
        );
    }

    // ── linked marker (DLD-005) ────────────────────────────────────

    /// A linked record carries no files and sentinel commit/digest; it must survive a
    /// TOML round-trip with `linked = true` intact and the empty `files` preserved.
    #[test]
    fn linked_record_round_trips_with_sentinels_and_empty_files() {
        let rec = RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: "vscode".to_owned(),
                source: "local-overlay".to_owned(),
                artifact: "snippets".to_owned(),
            },
            source: "local-overlay".to_owned(),
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: true,
            vars_digest: None,
            deploy_root: None,
        };

        let toml = toml::to_string(&rec).expect("serialize linked record");
        let back: RegistryRecord = toml::from_str(&toml).expect("deserialize linked record");

        assert_eq!(
            back, rec,
            "a linked record must round-trip field-for-field through TOML"
        );
        assert!(back.linked, "linked flag must survive serde");
        assert!(
            back.files.is_empty(),
            "a linked record carries no manifest files"
        );
        assert_eq!(back.commit, "link", "sentinel commit must round-trip");
        assert_eq!(back.digest, "link:", "sentinel digest must round-trip");
    }

    /// Records written before the linked marker existed have no `linked` key; serde
    /// `#[serde(default)]` must deserialize them with `linked = false` (back-compat).
    #[test]
    fn record_without_linked_field_deserializes_as_not_linked() {
        let legacy = r#"
version = 1
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "flat"
allow_symlinks = false
preserve_executable = true
files = []

[key]
target = "vscode"
source = "company-configs"
artifact = "snippets"
"#;

        let rec: RegistryRecord =
            toml::from_str(legacy).expect("legacy record without `linked` must still parse");

        assert!(
            !rec.linked,
            "a record predating the linked marker must default to linked = false"
        );
    }

    // ── underlying source field (PBR-004) ─────────────────────────

    /// An aliased binding keys `key.source` by IDENTITY (the `as`) but must record
    /// the UNDERLYING source name in the new `source` field. Both must survive a
    /// TOML round-trip independently so `rebuild-registry` can reconstruct provenance.
    #[test]
    fn record_round_trips_underlying_source_distinct_from_identity() {
        let rec = RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: "dest".to_owned(),
                source: "nvim".to_owned(),
                artifact: "init".to_owned(),
            },
            source: "dotfiles".to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "by-source".to_owned(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: false,
            vars_digest: None,
            deploy_root: None,
        };

        let toml = toml::to_string(&rec).expect("serialize aliased record");
        let back: RegistryRecord = toml::from_str(&toml).expect("deserialize aliased record");

        assert_eq!(
            back, rec,
            "an aliased record must round-trip field-for-field through TOML"
        );
        assert_eq!(
            back.key.source, "nvim",
            "key.source carries the binding IDENTITY (the `as`)"
        );
        assert_eq!(
            back.source, "dotfiles",
            "the new `source` field carries the UNDERLYING source name, distinct from identity"
        );
    }

    /// Records written before the `source` field existed have no `source` key; serde
    /// `#[serde(default)]` must deserialize them with an empty source (back-compat).
    #[test]
    fn record_without_source_field_deserializes_with_default_source() {
        let legacy = r#"
version = 1
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "flat"
allow_symlinks = false
preserve_executable = true
files = []

[key]
target = "vscode"
source = "company-configs"
artifact = "snippets"
"#;

        let rec: RegistryRecord =
            toml::from_str(legacy).expect("legacy record without `source` must still parse");

        assert_eq!(
            rec.source, "",
            "a record predating the underlying-source field must default to an empty `source`"
        );
    }

    // ── record kind: file vs dir (SMR-001) ─────────────────────────

    #[test]
    fn record_kind_defaults_to_dir() {
        assert_eq!(
            RecordKind::default(),
            RecordKind::Dir,
            "an unspecified record kind must default to a directory tree (the legacy shape)"
        );
    }

    #[test]
    fn record_without_kind_field_deserializes_as_dir() {
        let legacy = r#"
version = 1
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "flat"
allow_symlinks = false
preserve_executable = true
files = []

[key]
target = "vscode"
source = "company-configs"
artifact = "snippets"
"#;

        let rec: RegistryRecord =
            toml::from_str(legacy).expect("legacy record without `kind` must still parse");

        assert_eq!(
            rec.kind,
            RecordKind::Dir,
            "a record predating the kind field must default to RecordKind::Dir"
        );
    }

    #[test]
    fn file_kind_record_round_trips_as_lowercase_file() {
        let rec = RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: "dest".to_owned(),
                source: "agents-src".to_owned(),
                artifact: "CLAUDE.md".to_owned(),
            },
            source: "agents-src".to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: RecordKind::File,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: false,
            vars_digest: None,
            deploy_root: None,
        };

        let toml = toml::to_string(&rec).expect("serialize file-kind record");
        assert!(
            toml.contains("kind = \"file\""),
            "a file-kind record must serialize the lowercase tag `kind = \"file\"`, got:\n{toml}"
        );

        let back: RegistryRecord = toml::from_str(&toml).expect("deserialize file-kind record");
        assert_eq!(
            back.kind,
            RecordKind::File,
            "a file-kind record must round-trip its kind through serde"
        );
        assert_eq!(
            back, rec,
            "a file-kind record must round-trip field-for-field"
        );
    }

    #[test]
    fn dir_kind_record_serializes_as_lowercase_dir() {
        let rec = RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: "dest".to_owned(),
                source: "dotfiles".to_owned(),
                artifact: "nvim".to_owned(),
            },
            source: "dotfiles".to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "by-source".to_owned(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: false,
            vars_digest: None,
            deploy_root: None,
        };

        let toml = toml::to_string(&rec).expect("serialize dir-kind record");
        assert!(
            toml.contains("kind = \"dir\""),
            "a dir-kind record must serialize the lowercase tag `kind = \"dir\"`, got:\n{toml}"
        );

        let back: RegistryRecord = toml::from_str(&toml).expect("deserialize dir-kind record");
        assert_eq!(
            back.kind,
            RecordKind::Dir,
            "a dir-kind record must round-trip its kind through serde"
        );
    }

    #[test]
    fn projected_threads_file_kind_from_projected_record() {
        let projected = ProjectedRecord {
            key: ArtifactKey {
                target: "dest".to_owned(),
                source: "agents-src".to_owned(),
                artifact: "CLAUDE.md".to_owned(),
            },
            underlying_source: "agents-src",
            commit: "def456789abc123",
            digest: "blake3:d4e5f6".to_owned(),
            layout: "flat".to_owned(),
            kind: RecordKind::File,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            vars_digest: None,
            deploy_root: None,
        };

        let rec = RegistryRecord::projected(projected);

        assert_eq!(
            rec.kind,
            RecordKind::File,
            "projected() must carry the kind from the ProjectedRecord verbatim"
        );
    }

    // ── per-artifact vars digest (TPH-010) ─────────────────────────

    fn vars_digest_record(vars_digest: Option<&str>) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: "vscode".to_owned(),
                source: "company-configs".to_owned(),
                artifact: "snippets".to_owned(),
            },
            source: "company-configs".to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: false,
            vars_digest: vars_digest.map(str::to_owned),
            deploy_root: None,
        }
    }

    /// INV-8 (byte-stability): a feature-free record (rendered no template, so
    /// `vars_digest` is None) must serialize with NO `vars_digest` key, byte-identical
    /// to the pre-feature format.
    #[test]
    fn feature_free_record_serializes_without_vars_digest_key() {
        let rec = vars_digest_record(None);

        let toml = toml::to_string(&rec).expect("serialize feature-free record");

        assert!(
            !toml.contains("vars_digest"),
            "a record whose vars_digest is None must omit the key entirely (skip_serializing_if), \
             so feature-free records stay byte-identical to the pre-feature format (INV-8), got:\n{toml}"
        );

        let back: RegistryRecord = toml::from_str(&toml).expect("deserialize feature-free record");
        assert_eq!(
            back, rec,
            "a None vars_digest must round-trip field-for-field through TOML"
        );
        assert_eq!(
            back.vars_digest, None,
            "a record with no vars_digest key must deserialize back to None"
        );
    }

    /// A templated record carries a `vars_digest`; the key must serialize and round-trip.
    #[test]
    fn templated_record_round_trips_vars_digest() {
        let rec = vars_digest_record(Some("blake3:abc123"));

        let toml = toml::to_string(&rec).expect("serialize templated record");

        assert!(
            toml.contains("vars_digest"),
            "a record whose vars_digest is Some(..) must serialize the key, got:\n{toml}"
        );

        let back: RegistryRecord = toml::from_str(&toml).expect("deserialize templated record");
        assert_eq!(
            back, rec,
            "a Some(vars_digest) record must round-trip field-for-field through TOML"
        );
        assert_eq!(
            back.vars_digest.as_deref(),
            Some("blake3:abc123"),
            "the vars_digest value must survive serde"
        );
    }

    /// Records written before the vars-digest field existed have no `vars_digest` key;
    /// serde `#[serde(default)]` must deserialize them as None (back-compat).
    #[test]
    fn record_without_vars_digest_field_deserializes_as_none() {
        let legacy = r#"
version = 1
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "flat"
allow_symlinks = false
preserve_executable = true
files = []

[key]
target = "vscode"
source = "company-configs"
artifact = "snippets"
"#;

        let rec: RegistryRecord =
            toml::from_str(legacy).expect("legacy record without `vars_digest` must still parse");

        assert_eq!(
            rec.vars_digest, None,
            "a record predating the vars-digest field must default to None"
        );
    }

    // ── persisted deploy root (CLIFF-DEPLOYROOT-007) ───────────────

    /// A record must persist the target's expanded absolute deploy root so an
    /// orphaned record (its config `Target` gone) stays locatable; the field
    /// and its value must survive a TOML round-trip.
    #[test]
    fn record_persists_deploy_root_through_toml_round_trip() {
        let stored = r#"
version = 1
source = "nvim-src"
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "by-source"
allow_symlinks = false
preserve_executable = true
files = []
deploy_root = "/home/alice/.config/nvim"

[key]
target = "home"
source = "nvim"
artifact = "init"
"#;

        let rec: RegistryRecord =
            toml::from_str(stored).expect("a record carrying deploy_root must deserialize");
        let round_tripped = toml::to_string(&rec).expect("re-serialize record");

        assert!(
            round_tripped.contains("deploy_root"),
            "a record's persisted deploy_root must survive a TOML round-trip (the field is dropped \
             until CLIFF-DEPLOYROOT-007 adds it), got:\n{round_tripped}"
        );
        assert!(
            round_tripped.contains("/home/alice/.config/nvim"),
            "the expanded absolute deploy root value must survive the round-trip, got:\n{round_tripped}"
        );
    }

    #[test]
    fn deploy_root_snapshots_targets_expanded_path() {
        let target = crate::config::Config::parse(
            "version = 1\n\n[targets.home]\npath = \"~/.config/nvim\"\n",
        )
        .expect("minimal config parses")
        .targets
        .remove("home")
        .expect("target `home` present");
        let expanded = target.expanded_path();
        let expanded_str = expanded.to_string_lossy().into_owned();
        assert!(
            expanded.is_absolute() && !expanded_str.contains('~'),
            "premise: a `~/`-target must expand to an absolute tilde-free path (home resolved), got {expanded_str}"
        );
        assert_ne!(
            expanded_str, "~/.config/nvim",
            "premise: expanded_path must actually expand the tilde, not echo the raw config path"
        );

        let stored = format!(
            "version = 1\n\
             source = \"nvim-src\"\n\
             commit = \"def456789abc123\"\n\
             digest = \"blake3:d4e5f6\"\n\
             projected_at = \"2026-01-31T12:34:56Z\"\n\
             layout = \"by-source\"\n\
             allow_symlinks = false\n\
             preserve_executable = true\n\
             files = []\n\
             deploy_root = \"{expanded_str}\"\n\n\
             [key]\n\
             target = \"home\"\n\
             source = \"nvim\"\n\
             artifact = \"init\"\n"
        );

        let rec: RegistryRecord =
            toml::from_str(&stored).expect("record with expanded deploy_root must deserialize");
        let round_tripped = toml::to_string(&rec).expect("re-serialize record");
        let value: toml::Value =
            toml::from_str(&round_tripped).expect("re-parse serialized record as a table");

        let persisted = value
            .get("deploy_root")
            .and_then(toml::Value::as_str)
            .expect("re-serialized record must expose a deploy_root string");
        assert_eq!(
            persisted, expanded_str,
            "the persisted deploy root must equal the target's expanded absolute path (post tilde expansion)"
        );
    }

    /// Records written before the deploy-root field existed have no
    /// `deploy_root` key; adding the field must not break them (additive, no
    /// migration) — a legacy record must still deserialize cleanly.
    #[test]
    fn legacy_record_without_deploy_root_still_loads() {
        let legacy = r#"
version = 1
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "flat"
allow_symlinks = false
preserve_executable = true
files = []

[key]
target = "vscode"
source = "company-configs"
artifact = "snippets"
"#;

        let rec: RegistryRecord =
            toml::from_str(legacy).expect("legacy record without `deploy_root` must still parse");

        assert_eq!(
            rec.key.artifact, "snippets",
            "a record predating the deploy-root field must load with its other fields intact"
        );
    }

    fn record(target: &str, source: &str, artifact: &str) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: target.to_owned(),
                source: source.to_owned(),
                artifact: artifact.to_owned(),
            },
            source: source.to_owned(),
            commit: "def456789abc123".to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("python.json"),
                size: 12345,
                mtime: 1_738_329_296,
                blake3: "9e8d7c6b5a4f3e2d".to_owned(),
            }],
            linked: false,
            vars_digest: None,
            deploy_root: None,
        }
    }

    fn registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    // project_id

    #[test]
    fn project_id_is_blake3_of_canonical_root_first_sixteen_hex() {
        let dir = TempDir::new().expect("temp project root");
        let canonical = dir.path().canonicalize().expect("canonicalize root");
        let expected = blake3::hash(canonical.to_string_lossy().as_bytes());

        let id = ProjectId::for_path(dir.path()).expect("project id");

        assert_eq!(
            id.as_str(),
            &expected.to_hex()[..16],
            "project id must be the first 16 hex chars of blake3(canonical root)"
        );
    }

    #[test]
    fn project_id_is_deterministic_for_same_canonical_path() {
        let dir = TempDir::new().expect("temp project root");

        let first = ProjectId::for_path(dir.path()).expect("first id");
        let second = ProjectId::for_path(dir.path()).expect("second id");

        assert_eq!(first, second, "same canonical path => same project id");
    }

    // put / get / remove

    #[test]
    fn get_on_absent_key_is_none() {
        let (_dir, reg) = registry();
        let key = ArtifactKey {
            target: "vscode".to_owned(),
            source: "company-configs".to_owned(),
            artifact: "snippets".to_owned(),
        };

        let got = reg.get(&key).expect("get must not error on absent key");

        assert!(got.is_none(), "absent key yields Ok(None)");
    }

    #[test]
    fn put_then_get_round_trips_the_record() {
        let (_dir, reg) = registry();
        let rec = record("vscode", "company-configs", "snippets");

        reg.put(&rec).expect("put record");
        let got = reg
            .get(&rec.key)
            .expect("get record")
            .expect("record present");

        assert_eq!(got, rec, "get must return the exact record that was put");
    }

    #[test]
    fn put_writes_record_at_target_source_artifact_path() {
        let (dir, reg) = registry();
        let rec = record("vscode", "company-configs", "snippets");

        reg.put(&rec).expect("put record");

        let expected = dir
            .path()
            .join("targets")
            .join("vscode")
            .join("artifacts")
            .join("company-configs")
            .join("snippets.toml");
        assert!(
            expected.is_file(),
            "record must land at <root>/targets/<target>/artifacts/<source>/<artifact>.toml, expected {}",
            expected.display()
        );
        assert_eq!(
            reg.get(&rec.key)
                .expect("get record")
                .expect("record present"),
            rec,
            "the record written at that path must round-trip back through get"
        );
    }

    #[test]
    fn put_creates_missing_parent_directories() {
        let (dir, reg) = registry();
        assert!(
            !dir.path().join("targets").exists(),
            "targets dir absent before first put (premise)"
        );
        let rec = record("vscode", "company-configs", "snippets");

        reg.put(&rec).expect("put must create parent dirs");

        let expected = dir
            .path()
            .join("targets")
            .join("vscode")
            .join("artifacts")
            .join("company-configs")
            .join("snippets.toml");
        assert!(
            expected.is_file(),
            "put must create the full parent chain and write the file at {}",
            expected.display()
        );
        assert_eq!(
            reg.get(&rec.key)
                .expect("get record")
                .expect("record present"),
            rec,
            "record written into freshly created dirs must round-trip"
        );
    }

    #[test]
    fn remove_deletes_the_record() {
        let (_dir, reg) = registry();
        let rec = record("vscode", "company-configs", "snippets");
        reg.put(&rec).expect("put record");

        reg.remove(&rec.key).expect("remove record");

        assert!(
            reg.get(&rec.key).expect("get after remove").is_none(),
            "removed record must no longer be found"
        );
    }

    #[test]
    fn put_leaves_no_temp_file() {
        let (dir, reg) = registry();

        reg.put(&record("vscode", "company-configs", "snippets"))
            .expect("put record");

        let artifact_dir = dir
            .path()
            .join("targets")
            .join("vscode")
            .join("artifacts")
            .join("company-configs");
        let entries: Vec<PathBuf> = std::fs::read_dir(&artifact_dir)
            .expect("artifact dir must exist after put")
            .map(|e| e.expect("dir entry").path())
            .collect();

        assert_eq!(
            entries,
            vec![artifact_dir.join("snippets.toml")],
            "atomic write must leave only <artifact>.toml — no temp/.tmp leftover, found {entries:?}"
        );
    }

    // list_target / list_all

    fn contains_artifact(records: &[RegistryRecord], source: &str, artifact: &str) -> bool {
        records.iter().any(|r| {
            r.key.source == source && r.key.artifact == artifact && r.commit == "def456789abc123"
        })
    }

    #[test]
    fn list_target_returns_only_records_under_that_target() {
        let (_dir, reg) = registry();
        reg.put(&record("vscode", "company-configs", "snippets"))
            .expect("put a");
        reg.put(&record("vscode", "dotfiles", "settings"))
            .expect("put b");
        reg.put(&record("nvim", "dotfiles", "init")).expect("put c");

        let vscode = reg.list_target("vscode").expect("list vscode");

        assert_eq!(vscode.len(), 2, "two records under vscode");
        assert!(
            vscode.iter().all(|r| r.key.target == "vscode"),
            "list_target must not leak other targets"
        );
        assert!(
            contains_artifact(&vscode, "company-configs", "snippets"),
            "vscode list must contain company-configs/snippets"
        );
        assert!(
            contains_artifact(&vscode, "dotfiles", "settings"),
            "vscode list must contain dotfiles/settings"
        );
        assert!(
            !contains_artifact(&vscode, "dotfiles", "init"),
            "vscode list must exclude the nvim-only dotfiles/init record"
        );
    }

    #[test]
    fn list_target_on_absent_target_is_empty() {
        let (_dir, reg) = registry();

        let records = reg
            .list_target("never-projected")
            .expect("listing an absent target must not error");

        assert_eq!(records, vec![], "absent target dir => empty record list");
    }

    #[test]
    fn list_all_returns_records_across_all_targets() {
        let (_dir, reg) = registry();
        reg.put(&record("vscode", "company-configs", "snippets"))
            .expect("put vscode");
        reg.put(&record("nvim", "dotfiles", "init"))
            .expect("put nvim");

        let all = reg.list_all().expect("list all");

        assert_eq!(all.len(), 2, "records from every target appear");
        assert!(
            all.iter().any(|r| r.key.target == "vscode"
                && r.key.source == "company-configs"
                && r.key.artifact == "snippets"),
            "vscode company-configs/snippets present in list_all"
        );
        assert!(
            all.iter().any(|r| r.key.target == "nvim"
                && r.key.source == "dotfiles"
                && r.key.artifact == "init"),
            "nvim dotfiles/init present in list_all"
        );
    }

    #[test]
    fn list_target_orders_records_by_source_then_artifact() {
        let (_dir, reg) = registry();
        reg.put(&record("home", "second", "COPY.md"))
            .expect("put second");
        reg.put(&record("home", "dotfiles", "READER.md"))
            .expect("put dotfiles");

        let keys: Vec<(String, String)> = reg
            .list_target("home")
            .expect("list home")
            .into_iter()
            .map(|r| (r.key.source, r.key.artifact))
            .collect();

        assert_eq!(
            keys,
            vec![
                ("dotfiles".to_owned(), "READER.md".to_owned()),
                ("second".to_owned(), "COPY.md".to_owned()),
            ],
            "list_target must order by (source, artifact), independent of filesystem read_dir order"
        );
    }

    #[test]
    fn list_all_orders_records_by_target_then_source_then_artifact() {
        let (_dir, reg) = registry();
        reg.put(&record("nvim", "dotfiles", "init"))
            .expect("put nvim");
        reg.put(&record("home", "second", "COPY.md"))
            .expect("put home second");
        reg.put(&record("home", "dotfiles", "READER.md"))
            .expect("put home dotfiles");

        let keys: Vec<(String, String, String)> = reg
            .list_all()
            .expect("list all")
            .into_iter()
            .map(|r| (r.key.target, r.key.source, r.key.artifact))
            .collect();

        assert_eq!(
            keys,
            vec![
                (
                    "home".to_owned(),
                    "dotfiles".to_owned(),
                    "READER.md".to_owned()
                ),
                ("home".to_owned(), "second".to_owned(), "COPY.md".to_owned()),
                ("nvim".to_owned(), "dotfiles".to_owned(), "init".to_owned()),
            ],
            "list_all must order by (target, source, artifact), independent of read_dir order"
        );
    }

    // ejected meta

    #[test]
    fn load_ejected_on_missing_meta_is_empty() {
        let (_dir, reg) = registry();

        let ejected = reg
            .load_ejected("vscode")
            .expect("missing meta must not error");

        assert!(ejected.is_empty(), "no meta file => empty ejected list");
    }

    #[test]
    fn save_then_load_ejected_round_trips() {
        let (_dir, reg) = registry();
        let entries = vec![
            EjectedEntry {
                source: "company-configs".to_owned(),
                artifact: "snippets".to_owned(),
                ejected_at: "2026-01-31T14:00:00Z".to_owned(),
            },
            EjectedEntry {
                source: "dotfiles".to_owned(),
                artifact: "old-config".to_owned(),
                ejected_at: "2026-01-30T10:00:00Z".to_owned(),
            },
        ];

        reg.save_ejected("vscode", &entries).expect("save ejected");
        let loaded = reg.load_ejected("vscode").expect("load ejected");

        assert_eq!(
            loaded, entries,
            "load_ejected must return every saved entry, in order, field-for-field"
        );
    }

    #[test]
    fn save_ejected_writes_meta_at_target_path() {
        let (dir, reg) = registry();
        let entries = vec![EjectedEntry {
            source: "company-configs".to_owned(),
            artifact: "snippets".to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        }];

        reg.save_ejected("vscode", &entries).expect("save ejected");

        let expected = dir.path().join("targets").join("vscode").join("meta.toml");
        assert!(
            expected.is_file(),
            "ejected meta must land at <root>/targets/<target>/meta.toml, expected {}",
            expected.display()
        );
    }

    // state.lock

    #[test]
    fn lock_exclusive_succeeds_when_unheld() {
        let (_dir, reg) = registry();

        let guard = reg.lock_exclusive();

        assert!(guard.is_ok(), "first exclusive lock must succeed");
    }

    #[test]
    fn lock_exclusive_creates_lock_file_at_locks_state_lock() {
        let (dir, reg) = registry();

        let _held = reg.lock_exclusive().expect("first lock acquired");

        let expected = dir.path().join("locks").join("state.lock");
        assert!(
            expected.is_file(),
            "lock_exclusive must create <root>/locks/state.lock, expected {}",
            expected.display()
        );
    }

    #[test]
    fn second_exclusive_lock_fails_while_first_held() {
        let dir = TempDir::new().expect("temp state root");
        let first = FileRegistry::open(dir.path().to_path_buf()).expect("open first registry");
        let second = FileRegistry::open(dir.path().to_path_buf()).expect("open second registry");

        let _held = first
            .lock_exclusive()
            .expect("first instance acquires the lock");

        let blocked = second.lock_exclusive();

        let err = blocked.expect_err(
            "a second FileRegistry on the same state_root must fail to lock while the first holds it",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("phora") && (msg.contains("state.lock") || msg.contains("lock held")),
            "lock-held error must mention another phora process / state.lock, got: {msg}"
        );
    }

    #[test]
    fn lock_is_released_after_guard_dropped() {
        let _serial = STATE_LOCK_SERIAL
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = TempDir::new().expect("temp state root");
        let first = FileRegistry::open(dir.path().to_path_buf()).expect("open first registry");
        let second = FileRegistry::open(dir.path().to_path_buf()).expect("open second registry");

        {
            let _held = first
                .lock_exclusive()
                .expect("first instance acquires the lock");
        }

        let reacquired = second.lock_exclusive();

        assert!(
            reacquired.is_ok(),
            "dropping the guard must release the OS lock so another instance can acquire it, got: {:?}",
            reacquired.err()
        );
    }

    // ── per-hook last-success digest-set (TPH-002) ─────────────────

    fn digest_set(digests: &[&str]) -> std::collections::BTreeSet<String> {
        digests.iter().map(|d| (*d).to_owned()).collect()
    }

    /// A hook's last-success record is the set of artifact digests the target
    /// carried when that hook last exited 0; saving then loading must return it
    /// field-for-field (INV-4 state shape).
    #[test]
    fn save_then_load_hook_state_round_trips() {
        let (_dir, reg) = registry();
        let states = vec![HookState {
            hook_id: "vscode#0".to_owned(),
            last_success: digest_set(&["blake3:aaa", "blake3:bbb"]),
        }];

        reg.save_hook_state("vscode", &states)
            .expect("save hook state");
        let loaded = reg.load_hook_state("vscode").expect("load hook state");

        assert_eq!(
            loaded, states,
            "load_hook_state must return every saved hook record, field-for-field"
        );
    }

    /// A target that never ran a hook has no hook state; loading must not error
    /// and must yield an empty list (INV-8 absence, not a synthesized record).
    #[test]
    fn load_hook_state_on_missing_target_is_empty() {
        let (_dir, reg) = registry();

        let states = reg
            .load_hook_state("never-synced")
            .expect("missing hook state must not error");

        assert!(
            states.is_empty(),
            "a target with no recorded hook run yields an empty hook-state list"
        );
    }

    /// INV-4 success-only: an unrecorded sibling hook keeps its prior set and re-fires.
    #[test]
    fn record_hook_success_merges_and_leaves_other_hooks_untouched() {
        let (_dir, reg) = registry();
        reg.save_hook_state(
            "vscode",
            &[
                HookState {
                    hook_id: "vscode#0".to_owned(),
                    last_success: digest_set(&["blake3:aaa"]),
                },
                HookState {
                    hook_id: "vscode#1".to_owned(),
                    last_success: digest_set(&["blake3:bbb"]),
                },
            ],
        )
        .expect("seed two hook records");

        reg.record_hook_success("vscode", "vscode#0", &digest_set(&["blake3:ccc"]))
            .expect("record success only for the hook that exited 0");

        let loaded = reg.load_hook_state("vscode").expect("load hook state");
        let succeeded = loaded
            .iter()
            .find(|s| s.hook_id == "vscode#0")
            .expect("succeeded hook present");
        let untouched = loaded
            .iter()
            .find(|s| s.hook_id == "vscode#1")
            .expect("the hook that never succeeded must still be present");

        assert_eq!(
            succeeded.last_success,
            digest_set(&["blake3:ccc"]),
            "the succeeded hook's last-success set advances to the current digest-set"
        );
        assert_eq!(
            untouched.last_success,
            digest_set(&["blake3:bbb"]),
            "a hook not recorded keeps its prior set, so it re-fires next sync (INV-4)"
        );
    }

    #[test]
    fn record_hook_success_creates_record_for_first_success() {
        let (_dir, reg) = registry();

        reg.record_hook_success("vscode", "vscode#0", &digest_set(&["blake3:aaa"]))
            .expect("record first-ever success");

        let loaded = reg.load_hook_state("vscode").expect("load hook state");
        assert_eq!(
            loaded,
            vec![HookState {
                hook_id: "vscode#0".to_owned(),
                last_success: digest_set(&["blake3:aaa"]),
            }],
            "first success for a hook with no prior record creates exactly that record"
        );
    }

    /// INV-8: a target that records hook state and ejected entries side by side
    /// must round-trip both — hook state does not clobber the existing ejected
    /// meta, and vice versa.
    #[test]
    fn hook_state_and_ejected_meta_coexist_for_same_target() {
        let (_dir, reg) = registry();
        let ejected = vec![EjectedEntry {
            source: "company-configs".to_owned(),
            artifact: "snippets".to_owned(),
            ejected_at: "2026-06-13T10:00:00Z".to_owned(),
        }];
        let hooks = vec![HookState {
            hook_id: "vscode#0".to_owned(),
            last_success: digest_set(&["blake3:aaa"]),
        }];

        reg.save_ejected("vscode", &ejected).expect("save ejected");
        reg.save_hook_state("vscode", &hooks)
            .expect("save hook state");

        assert_eq!(
            reg.load_ejected("vscode").expect("load ejected"),
            ejected,
            "saving hook state must not clobber existing ejected entries"
        );
        assert_eq!(
            reg.load_hook_state("vscode").expect("load hook state"),
            hooks,
            "hook state must persist alongside ejected entries"
        );
    }

    /// INV-8 (byte-stability): a hook-free target writes meta identical to the
    /// pre-feature format — the new hook-state field is `skip_serializing_if`
    /// absent, so an ejected-only meta serializes byte-for-byte as today.
    #[test]
    fn hook_free_target_meta_serializes_byte_identical_to_pre_feature() {
        let (dir, reg) = registry();
        let ejected = vec![EjectedEntry {
            source: "company-configs".to_owned(),
            artifact: "snippets".to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        }];

        reg.save_ejected("vscode", &ejected).expect("save ejected");

        let meta_path = dir.path().join("targets").join("vscode").join("meta.toml");
        let written = std::fs::read_to_string(&meta_path).expect("read meta.toml");
        let expected = "\
version = 1

[[ejected]]
source = \"company-configs\"
artifact = \"snippets\"
ejected_at = \"2026-01-31T14:00:00Z\"
";
        assert_eq!(
            written, expected,
            "a hook-free target's meta.toml must serialize byte-identically to the pre-feature format (INV-8)"
        );
    }
}
