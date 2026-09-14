use std::collections::BTreeSet;
use std::path::PathBuf;

use phora::sync::state::StateStore;
use phora::sync::state::{
    ArtifactKey, ArtifactRecord, Ejection, FileStateStore, HookState, StateError, StateLock,
};
use tempfile::TempDir;

fn store() -> (TempDir, FileStateStore) {
    let dir = TempDir::new().expect("temp state root");
    let reg = FileStateStore::open(dir.path().to_path_buf()).expect("open store");
    (dir, reg)
}

fn record(target: &str, source: &str, artifact: &str) -> ArtifactRecord {
    ArtifactRecord {
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
        kind: phora::sync::state::RecordKind::Dir,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![],
        linked: false,
        history: false,
        worktree_admin_id: None,
        mirror_key: None,
        cache_git_root: None,
        vars_digest: None,
        deploy_root: None,
        layout_separator: None,
    }
}

fn digest_set(digests: &[&str]) -> BTreeSet<String> {
    digests.iter().map(|d| (*d).to_owned()).collect()
}

fn as_store(reg: &FileStateStore) -> &dyn StateStore {
    reg
}

fn sorted_keys(records: &[ArtifactRecord]) -> Vec<(String, String, String)> {
    let mut keys: Vec<(String, String, String)> = records
        .iter()
        .map(|r| {
            (
                r.key.target.clone(),
                r.key.source.clone(),
                r.key.artifact.clone(),
            )
        })
        .collect();
    keys.sort();
    keys
}

fn key(target: &str, source: &str, artifact: &str) -> (String, String, String) {
    (target.to_owned(), source.to_owned(), artifact.to_owned())
}

#[test]
fn file_registry_is_usable_as_a_state_store_trait_object() {
    let (_dir, reg) = store();
    let store: &dyn StateStore = as_store(&reg);
    let rec = record("vscode", "company-configs", "snippets");

    store
        .put_artifact(&rec)
        .expect("put through the trait object");
    let got = store
        .artifact(&rec.key)
        .expect("get through the trait object")
        .expect("record present");

    assert_eq!(got, rec, "a &dyn StateStore round-trips a record put→get");
}

#[test]
fn state_store_put_get_remove_round_trips_through_the_trait() {
    let (_dir, reg) = store();
    let store = as_store(&reg);
    let rec = record("vscode", "company-configs", "snippets");

    assert!(
        store.artifact(&rec.key).expect("absent get").is_none(),
        "an absent key yields Ok(None) through the trait"
    );
    store.put_artifact(&rec).expect("put");
    assert_eq!(
        store.artifact(&rec.key).expect("get").expect("present"),
        rec,
        "put then get returns the exact record"
    );
    store.remove_artifact(&rec.key).expect("remove");
    assert!(
        store
            .artifact(&rec.key)
            .expect("get after remove")
            .is_none(),
        "a removed record is gone"
    );
}

#[test]
fn state_store_lists_per_target_and_across_all_targets() {
    let (_dir, reg) = store();
    let store = as_store(&reg);
    store
        .put_artifact(&record("vscode", "company-configs", "snippets"))
        .expect("put a");
    store
        .put_artifact(&record("vscode", "dotfiles", "settings"))
        .expect("put b");
    store
        .put_artifact(&record("nvim", "dotfiles", "init"))
        .expect("put c");

    let vscode = store.target_artifacts("vscode").expect("list vscode");
    assert_eq!(
        sorted_keys(&vscode),
        vec![
            key("vscode", "company-configs", "snippets"),
            key("vscode", "dotfiles", "settings"),
        ],
        "target_artifacts returns exactly the vscode records — no other target's, no duplicates"
    );

    let all = store.all_artifacts().expect("list all");
    assert_eq!(
        sorted_keys(&all),
        vec![
            key("nvim", "dotfiles", "init"),
            key("vscode", "company-configs", "snippets"),
            key("vscode", "dotfiles", "settings"),
        ],
        "all_artifacts returns exactly every stored record across targets"
    );
}

#[test]
fn state_store_round_trips_ejections() {
    let (_dir, reg) = store();
    let store = as_store(&reg);
    let entries = vec![Ejection {
        source: "company-configs".to_owned(),
        artifact: "snippets".to_owned(),
        ejected_at: "2026-01-31T14:00:00Z".to_owned(),
    }];

    assert!(
        store
            .ejections("vscode")
            .expect("empty ejections")
            .is_empty(),
        "no meta yields an empty ejection list"
    );
    store
        .save_ejections("vscode", &entries)
        .expect("save ejections");
    assert_eq!(
        store.ejections("vscode").expect("load ejections"),
        entries,
        "save then load returns every ejection field-for-field"
    );
}

#[test]
fn state_store_records_and_reads_hook_state() {
    let (_dir, reg) = store();
    let store = as_store(&reg);

    assert!(
        store
            .hook_state("vscode")
            .expect("empty hook state")
            .is_empty(),
        "an unrun target has empty hook state"
    );
    store
        .record_hook_success("vscode", "vscode#0", &digest_set(&["blake3:aaa"]))
        .expect("record first success");
    let loaded = store.hook_state("vscode").expect("load hook state");
    assert_eq!(
        loaded,
        vec![HookState {
            hook_id: "vscode#0".to_owned(),
            last_success: digest_set(&["blake3:aaa"]),
        }],
        "the recorded hook success is read back"
    );
}

#[test]
fn state_store_acquires_and_releases_the_project_lock() {
    let dir = TempDir::new().expect("temp state root");
    let first = FileStateStore::open(dir.path().to_path_buf()).expect("open first");
    let second = FileStateStore::open(dir.path().to_path_buf()).expect("open second");

    {
        let _held = as_store(&first)
            .acquire_lock()
            .expect("first acquires the lock through the trait");
        assert!(
            as_store(&second).acquire_lock().is_err(),
            "a second acquire fails while the first guard is held"
        );
    }
    assert!(
        as_store(&second).acquire_lock().is_ok(),
        "dropping the guard releases the lock so another instance can acquire it"
    );
}

#[test]
fn state_store_journal_root_is_the_locks_dir_under_the_state_root() {
    let (dir, reg) = store();
    let store = as_store(&reg);

    let root = store.journal_root();
    assert_eq!(
        root,
        dir.path().join("locks"),
        "journal_root must be exactly <state_root>/locks — the directory holding the deploy \
         journal and state.lock"
    );
}

struct InMemoryStore {
    records: std::sync::Mutex<Vec<ArtifactRecord>>,
}

impl InMemoryStore {
    fn new() -> Self {
        Self {
            records: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl StateStore for InMemoryStore {
    fn artifact(&self, key: &ArtifactKey) -> Result<Option<ArtifactRecord>, StateError> {
        Ok(self
            .records
            .lock()
            .expect("lock")
            .iter()
            .find(|r| &r.key == key)
            .cloned())
    }

    fn put_artifact(&self, record: &ArtifactRecord) -> Result<(), StateError> {
        let mut recs = self.records.lock().expect("lock");
        recs.retain(|r| r.key != record.key);
        recs.push(record.clone());
        Ok(())
    }

    fn remove_artifact(&self, key: &ArtifactKey) -> Result<(), StateError> {
        self.records.lock().expect("lock").retain(|r| &r.key != key);
        Ok(())
    }

    fn target_artifacts(&self, target: &str) -> Result<Vec<ArtifactRecord>, StateError> {
        Ok(self
            .records
            .lock()
            .expect("lock")
            .iter()
            .filter(|r| r.key.target == target)
            .cloned()
            .collect())
    }

    fn all_artifacts(&self) -> Result<Vec<ArtifactRecord>, StateError> {
        Ok(self.records.lock().expect("lock").clone())
    }

    fn ejections(&self, _target: &str) -> Result<Vec<Ejection>, StateError> {
        Ok(Vec::new())
    }

    fn save_ejections(&self, _target: &str, _entries: &[Ejection]) -> Result<(), StateError> {
        unimplemented!("the reconcile suite does not exercise fake ejection writes")
    }

    fn hook_state(&self, _target: &str) -> Result<Vec<HookState>, StateError> {
        Ok(Vec::new())
    }

    fn record_hook_success(
        &self,
        _target: &str,
        _hook_id: &str,
        _digest_set: &BTreeSet<String>,
    ) -> Result<(), StateError> {
        unimplemented!("the reconcile suite does not exercise fake hook writes")
    }

    fn acquire_lock(&self) -> Result<StateLock, StateError> {
        unimplemented!("the reconcile suite does not exercise fake lock acquisition")
    }

    fn journal_root(&self) -> PathBuf {
        unimplemented!("the reconcile suite does not exercise the fake journal root")
    }
}

#[test]
fn in_memory_fake_is_reachable_through_the_trait_object() {
    let fake = InMemoryStore::new();
    let store: &dyn StateStore = &fake;
    let rec = record("home", "dotfiles", "init");

    store.put_artifact(&rec).expect("fake put");
    assert_eq!(
        store
            .artifact(&rec.key)
            .expect("fake get")
            .expect("present"),
        rec,
        "the in-memory fake round-trips a record through &dyn StateStore"
    );
    assert_eq!(
        store.all_artifacts().expect("fake list all"),
        vec![rec],
        "the fake lists what it stored"
    );
}

#[test]
fn state_store_matches_the_legacy_registry_for_shared_reads() {
    use phora::sync::state::StateStore;

    let (_dir, reg) = store();
    reg.put_artifact(&record("vscode", "company-configs", "snippets"))
        .expect("seed via legacy trait");
    reg.put_artifact(&record("nvim", "dotfiles", "init"))
        .expect("seed via legacy trait");

    let via_store = as_store(&reg).all_artifacts().expect("store list");
    let via_registry = StateStore::all_artifacts(&reg).expect("registry list");
    assert_eq!(
        via_store, via_registry,
        "reading through StateStore must equal reading the same data through the legacy StateStore"
    );
}
