use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phora::store::{
    ArtifactKey, EjectedEntry, FileRegistry, HookState, ManifestFile, RegistryRecord,
    StateLockGuard, StoreError,
};
use phora::sync::inspect::inspect;
use phora::sync::model::{ManagedCondition, ObservedArtifact};
use phora::sync::state::StateStore;
use tempfile::TempDir;

const SOURCE: &str = "company-configs";
const COMMIT: &str = "abc123def456";
const ARTIFACT: &str = "snippets";
const TARGET: &str = "vscode";

fn store() -> (TempDir, FileRegistry) {
    let dir = TempDir::new().expect("temp state root");
    let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open store");
    (dir, reg)
}

fn key() -> ArtifactKey {
    ArtifactKey {
        target: TARGET.to_owned(),
        source: SOURCE.to_owned(),
        artifact: ARTIFACT.to_owned(),
    }
}

fn mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .expect("metadata")
        .modified()
        .expect("modified")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs()
}

fn deploy_and_record(target: &Path, files: &[(&str, &[u8])]) -> RegistryRecord {
    let mut manifest = Vec::new();
    for (rel, contents) in files {
        let path = target.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(&path, contents).expect("write artifact file");
        manifest.push(ManifestFile {
            path: PathBuf::from(rel),
            size: contents.len() as u64,
            mtime: mtime_secs(&path),
            blake3: blake3::hash(contents).to_hex().to_string(),
        });
    }
    RegistryRecord {
        version: 1,
        key: key(),
        source: SOURCE.to_owned(),
        commit: COMMIT.to_owned(),
        digest: "blake3:d4e5f6".to_owned(),
        projected_at: "2026-01-31T12:34:56Z".to_owned(),
        layout: "flat".to_owned(),
        kind: phora::store::RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: manifest,
        linked: false,
        vars_digest: None,
        deploy_root: None,
        layout_separator: None,
    }
}

fn ejected(source: &str, artifact: &str) -> EjectedEntry {
    EjectedEntry {
        source: source.to_owned(),
        artifact: artifact.to_owned(),
        ejected_at: "2026-01-31T14:00:00Z".to_owned(),
    }
}

fn observe(
    store: &dyn StateStore,
    target_path: &Path,
    key: &ArtifactKey,
    expected_source: &str,
    expected_commit: &str,
    ejections: &[EjectedEntry],
) -> ObservedArtifact<RegistryRecord> {
    inspect(
        target_path,
        expected_source,
        expected_commit,
        ejections,
        store,
        key,
        None,
    )
    .expect("inspect must not error on a well-formed fixture")
}

struct OneShotStore {
    record: RegistryRecord,
    artifact_reads: Cell<usize>,
}

impl OneShotStore {
    fn new(record: RegistryRecord) -> Self {
        Self {
            record,
            artifact_reads: Cell::new(0),
        }
    }
}

impl StateStore for OneShotStore {
    fn artifact(&self, key: &ArtifactKey) -> Result<Option<RegistryRecord>, StoreError> {
        assert_eq!(key, &self.record.key, "inspect must request the seeded key");
        let reads = self.artifact_reads.get();
        self.artifact_reads.set(reads + 1);
        if reads == 0 {
            Ok(Some(self.record.clone()))
        } else {
            Err(StoreError::Registry(
                "artifact record was read more than once".to_owned(),
            ))
        }
    }

    fn put_artifact(&self, _record: &RegistryRecord) -> Result<(), StoreError> {
        unreachable!("inspection must not write an artifact")
    }

    fn remove_artifact(&self, _key: &ArtifactKey) -> Result<(), StoreError> {
        unreachable!("inspection must not remove an artifact")
    }

    fn target_artifacts(&self, _target: &str) -> Result<Vec<RegistryRecord>, StoreError> {
        unreachable!("a directly managed artifact must not fall back to a target-wide read")
    }

    fn all_artifacts(&self) -> Result<Vec<RegistryRecord>, StoreError> {
        unreachable!("inspection must not list every artifact")
    }

    fn ejections(&self, _target: &str) -> Result<Vec<EjectedEntry>, StoreError> {
        unreachable!("inspection receives ejections from its caller")
    }

    fn save_ejections(&self, _target: &str, _entries: &[EjectedEntry]) -> Result<(), StoreError> {
        unreachable!("inspection must not save ejections")
    }

    fn hook_state(&self, _target: &str) -> Result<Vec<HookState>, StoreError> {
        unreachable!("inspection must not read hook state")
    }

    fn record_hook_success(
        &self,
        _target: &str,
        _hook_id: &str,
        _digest_set: &BTreeSet<String>,
    ) -> Result<(), StoreError> {
        unreachable!("inspection must not record hook state")
    }

    fn acquire_lock(&self) -> Result<StateLockGuard, StoreError> {
        unreachable!("inspection must not acquire the project lock")
    }

    fn journal_root(&self) -> PathBuf {
        unreachable!("inspection must not inspect the journal root")
    }
}

#[test]
fn inspect_reads_an_absent_unejected_target_as_missing() {
    let (_dir, reg) = store();
    let target = TempDir::new().expect("target dir");
    let missing = target.path().join("never-deployed");

    let observed = observe(&reg, &missing, &key(), SOURCE, COMMIT, &[]);

    assert!(
        matches!(observed, ObservedArtifact::Missing),
        "an absent, un-ejected target must observe as ObservedArtifact::Missing, got {observed:?}"
    );
}

#[test]
fn inspect_reads_an_ejected_artifact_as_ejected_even_when_present() {
    let (_dir, reg) = store();
    let target = TempDir::new().expect("target dir");
    let record = deploy_and_record(target.path(), &[("a.json", b"{}")]);
    reg.put_artifact(&record).expect("seed record");

    let observed = observe(
        &reg,
        target.path(),
        &key(),
        SOURCE,
        COMMIT,
        &[ejected(SOURCE, ARTIFACT)],
    );

    assert!(
        matches!(observed, ObservedArtifact::Ejected),
        "an ejected artifact observes as ObservedArtifact::Ejected even over a present matching \
         record, got {observed:?}"
    );
}

#[test]
fn inspect_reads_a_clean_managed_deployment_as_managed_clean() {
    let (_dir, reg) = store();
    let target = TempDir::new().expect("target dir");
    let record = deploy_and_record(target.path(), &[("a.json", b"{}")]);
    reg.put_artifact(&record).expect("seed record");

    let observed = observe(&reg, target.path(), &key(), SOURCE, COMMIT, &[]);

    let ObservedArtifact::Managed(managed) = observed else {
        panic!(
            "a matching in-sync deployment must observe as ObservedArtifact::Managed, got {observed:?}"
        );
    };
    assert!(
        matches!(managed.condition, ManagedCondition::Clean),
        "a byte-for-byte in-sync managed artifact carries ManagedCondition::Clean, got {:?}",
        managed.condition
    );
}

#[test]
fn inspect_reads_a_directly_managed_record_exactly_once() {
    let target = TempDir::new().expect("target dir");
    let record = deploy_and_record(target.path(), &[("a.json", b"{}")]);
    let store = OneShotStore::new(record.clone());

    let observed = inspect(
        target.path(),
        SOURCE,
        COMMIT,
        &[],
        &store,
        &record.key,
        None,
    )
    .expect("a directly managed artifact must not reread its one-shot record");

    let ObservedArtifact::Managed(managed) = observed else {
        panic!("the seeded artifact must remain managed, got {observed:?}");
    };
    assert_eq!(
        managed.record, record,
        "inspection must return the original record obtained from its single artifact read"
    );
    assert_eq!(
        store.artifact_reads.get(),
        1,
        "one observation of a directly managed artifact performs exactly one artifact(key) read"
    );
}

#[test]
fn inspect_reads_an_unrecorded_present_target_as_foreign() {
    let (_dir, reg) = store();
    let target = TempDir::new().expect("target dir");
    std::fs::write(target.path().join("a.json"), b"{}").expect("write present file");

    let observed = observe(&reg, target.path(), &key(), SOURCE, COMMIT, &[]);

    assert!(
        matches!(observed, ObservedArtifact::Foreign(..)),
        "a present target with no registry record observes as ObservedArtifact::Foreign, got \
         {observed:?}"
    );
}
