use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::sync::model::{ManagedArtifact, ManagedCondition, ObservedArtifact, ScannedFile};
use crate::sync::scan::{link_target_bytes, mtime_secs, scan_dir_soft};
use crate::sync::state::{
    ArtifactKey, ArtifactRecord, Ejection, ManifestEntryKind, ManifestFile, StateStore,
};

#[derive(Debug)]
pub enum ArtifactState {
    Clean,
    /// Managed artifact whose lock advanced past the deployed commit; redeploys without `--force`.
    Outdated,
    Modified {
        changed: Vec<PathBuf>,
    },
    Foreign,
    Missing,
    Ejected,
    Linked,
    /// Clean-like state carrying refreshed per-file metadata.
    Revalidated {
        fresh: Vec<ScannedFile>,
    },
}

struct ArtifactClassification {
    state: ArtifactState,
    record: Option<ArtifactRecord>,
}

impl ArtifactClassification {
    fn unmanaged(state: ArtifactState) -> Self {
        Self {
            state,
            record: None,
        }
    }

    fn managed(state: ArtifactState, record: ArtifactRecord) -> Self {
        Self {
            state,
            record: Some(record),
        }
    }
}

enum RecordLookup {
    Managed(ArtifactRecord),
    Classified(ArtifactClassification),
}

#[expect(
    clippy::too_many_arguments,
    reason = "drift inputs are independent scalars; bundling them into a struct would only relocate the arity"
)]
pub fn check_artifact_state(
    target_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    ejected: &[Ejection],
    artifact_name: &str,
    store: &dyn StateStore,
    key: &ArtifactKey,
    expected_vars_digest: Option<&str>,
) -> Result<ArtifactState> {
    Ok(classify_artifact_state(
        target_path,
        expected_source,
        expected_commit,
        ejected,
        artifact_name,
        store,
        key,
        expected_vars_digest,
    )?
    .state)
}

#[expect(
    clippy::too_many_arguments,
    reason = "drift inputs are independent scalars; bundling them into a struct would only relocate the arity"
)]
fn classify_artifact_state(
    target_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    ejected: &[Ejection],
    artifact_name: &str,
    store: &dyn StateStore,
    key: &ArtifactKey,
    expected_vars_digest: Option<&str>,
) -> Result<ArtifactClassification> {
    let is_ejected = ejected.iter().any(|e| {
        e.source == expected_source
            && (e.artifact == artifact_name
                || artifact_name.starts_with(&format!("{}/", e.artifact))
                || e.artifact.starts_with(&format!("{artifact_name}/")))
    });
    if is_ejected {
        return Ok(ArtifactClassification::unmanaged(ArtifactState::Ejected));
    }

    match target_path.try_exists() {
        Ok(false) => {
            return Ok(ArtifactClassification::unmanaged(ArtifactState::Missing));
        }
        Ok(true) => {}
        Err(e) => {
            return Err(Error::Projection(format!(
                "stat {}: {e}",
                target_path.display()
            )));
        }
    }

    if std::fs::symlink_metadata(target_path)
        .is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
    {
        return check_file_artifact_state(
            target_path,
            expected_source,
            expected_commit,
            store,
            key,
            expected_vars_digest,
        );
    }

    let record = match artifact_record(store, key, expected_source)? {
        RecordLookup::Managed(record) => record,
        RecordLookup::Classified(classification) => return Ok(classification),
    };

    let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
    let mut fresh: Vec<ScannedFile> = Vec::new();

    for mf in &record.files {
        let file_path = target_path.join(&mf.path);
        let meta = match std::fs::symlink_metadata(&file_path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                changed.insert(mf.path.clone());
                continue;
            }
            Err(e) => {
                return Err(Error::Projection(format!(
                    "stat {}: {e}",
                    file_path.display()
                )));
            }
        };
        match mf.kind {
            ManifestEntryKind::File => {
                if !meta.is_file() {
                    changed.insert(mf.path.clone());
                    continue;
                }
                if meta.len() != mf.size || mtime_secs(&meta, &file_path)? != mf.mtime {
                    match revalidate_file(&file_path, &meta, mf)? {
                        Some(scanned) => fresh.push(scanned),
                        None => {
                            changed.insert(mf.path.clone());
                        }
                    }
                }
            }
            ManifestEntryKind::Link => {
                if !meta.file_type().is_symlink() || !link_matches(&file_path, mf)? {
                    changed.insert(mf.path.clone());
                }
            }
        }
    }

    let scan = scan_dir_soft(target_path)?;
    let known: HashSet<&PathBuf> = record.files.iter().map(|f| &f.path).collect();
    for cf in &scan.files {
        if !known.contains(&cf.path) && (!record.history || cf.path != Path::new(".git")) {
            changed.insert(cf.path.clone());
        }
    }
    if record.history || !record.allow_symlinks {
        for path in scan.symlinks {
            if !known.contains(&path) {
                changed.insert(path);
            }
        }
    }

    let state = classify_drift(
        &record,
        changed.into_iter().collect(),
        fresh,
        expected_commit,
        expected_vars_digest,
    );
    Ok(ArtifactClassification::managed(state, record))
}

/// Drift check when the target IS a single renamed FILE, not a directory of recorded files.
fn check_file_artifact_state(
    file_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    store: &dyn StateStore,
    key: &ArtifactKey,
    expected_vars_digest: Option<&str>,
) -> Result<ArtifactClassification> {
    let record = match artifact_record(store, key, expected_source)? {
        RecordLookup::Managed(record) => record,
        RecordLookup::Classified(classification) => return Ok(classification),
    };

    let meta = std::fs::symlink_metadata(file_path)
        .map_err(|e| Error::Projection(format!("stat {}: {e}", file_path.display())))?;
    let (changed, fresh) = match record.files.first() {
        Some(mf) => match mf.kind {
            ManifestEntryKind::File if !meta.is_file() => (vec![file_path.to_path_buf()], vec![]),
            ManifestEntryKind::File
                if meta.len() != mf.size || mtime_secs(&meta, file_path)? != mf.mtime =>
            {
                match revalidate_file(file_path, &meta, mf)? {
                    Some(scanned) => (vec![], vec![scanned]),
                    None => (vec![file_path.to_path_buf()], vec![]),
                }
            }
            ManifestEntryKind::Link
                if !meta.file_type().is_symlink() || !link_matches(file_path, mf)? =>
            {
                (vec![file_path.to_path_buf()], vec![])
            }
            ManifestEntryKind::File | ManifestEntryKind::Link => (vec![], vec![]),
        },
        None => (vec![file_path.to_path_buf()], vec![]),
    };

    let state = classify_drift(
        &record,
        changed,
        fresh,
        expected_commit,
        expected_vars_digest,
    );
    Ok(ArtifactClassification::managed(state, record))
}

fn artifact_record(
    store: &dyn StateStore,
    key: &ArtifactKey,
    expected_source: &str,
) -> Result<RecordLookup> {
    let Some(record) = store.artifact(key)? else {
        if let Some(record) = managed_under_sibling_shape(store, key, expected_source)? {
            return Ok(RecordLookup::Classified(ArtifactClassification::managed(
                ArtifactState::Outdated,
                record,
            )));
        }
        return Ok(RecordLookup::Classified(ArtifactClassification::unmanaged(
            ArtifactState::Foreign,
        )));
    };
    if record.linked {
        return Ok(RecordLookup::Classified(ArtifactClassification::managed(
            ArtifactState::Linked,
            record,
        )));
    }
    if record.key.source != expected_source {
        return Ok(RecordLookup::Classified(ArtifactClassification::unmanaged(
            ArtifactState::Foreign,
        )));
    }
    Ok(RecordLookup::Managed(record))
}

/// Finds `expected_source` under the collapsed-dir/per-leaf counterpart of `key`.
fn managed_under_sibling_shape(
    store: &dyn StateStore,
    key: &ArtifactKey,
    expected_source: &str,
) -> Result<Option<ArtifactRecord>> {
    let under = |child: &str, parent: &str| {
        child
            .strip_prefix(parent)
            .is_some_and(|r| r.starts_with('/'))
    };
    Ok(store
        .target_artifacts(&key.target)?
        .into_iter()
        .find(|record| {
            record.key.source == expected_source
                && (under(&record.key.artifact, &key.artifact)
                    || under(&key.artifact, &record.key.artifact))
        }))
}

fn classify_drift(
    record: &ArtifactRecord,
    changed: Vec<PathBuf>,
    fresh: Vec<ScannedFile>,
    expected_commit: &str,
    expected_vars_digest: Option<&str>,
) -> ArtifactState {
    if !changed.is_empty() {
        return ArtifactState::Modified { changed };
    }
    let commit_advanced = record.commit != expected_commit;
    let vars_changed =
        record.vars_digest.is_some() && record.vars_digest.as_deref() != expected_vars_digest;
    if commit_advanced || vars_changed {
        return ArtifactState::Outdated;
    }
    if !fresh.is_empty() {
        return ArtifactState::Revalidated { fresh };
    }
    ArtifactState::Clean
}

fn link_matches(path: &Path, mf: &ManifestFile) -> Result<bool> {
    let bytes = link_target_bytes(path)
        .map_err(|e| Error::Projection(format!("read link {}: {e}", path.display())))?;
    Ok(bytes.len() as u64 == mf.size && blake3::hash(&bytes).to_hex().as_str() == mf.blake3)
}

/// `None` declines the refresh on any uncertainty (read error, re-stat error, mid-flight
/// change, hash mismatch); `Some` is a revalidated stat whose bytes matched `mf.blake3`.
fn revalidate_file(
    file_path: &Path,
    meta: &std::fs::Metadata,
    mf: &ManifestFile,
) -> Result<Option<ScannedFile>> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let Ok(mut file) = std::fs::File::open(file_path) else {
        return Ok(None);
    };
    let Ok(pre) = file.metadata() else {
        return Ok(None);
    };
    if !pre.is_file() {
        return Ok(None);
    }
    // Closes the path-resolution TOCTOU: the opened fd must be the same inode the caller's
    // no-follow pre-stat saw, else a mid-validation path swap could mask content drift.
    if pre.ino() != meta.ino() || pre.dev() != meta.dev() {
        return Ok(None);
    }

    let mut content = Vec::new();
    if file.read_to_end(&mut content).is_err() {
        return Ok(None);
    }

    let Ok(post) = file.metadata() else {
        return Ok(None);
    };
    let size = post.len();
    let mtime = mtime_secs(&post, file_path)?;
    // The held inode's size/mtime moved across the read: an in-place mid-validation change.
    if size != pre.len() || mtime != mtime_secs(&pre, file_path)? {
        return Ok(None);
    }

    if blake3::hash(&content).to_hex().to_string() != mf.blake3 {
        return Ok(None);
    }

    Ok(Some(ScannedFile {
        path: mf.path.clone(),
        size,
        mtime,
    }))
}

pub fn inspect(
    target_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    ejected: &[Ejection],
    store: &dyn StateStore,
    key: &ArtifactKey,
    expected_vars_digest: Option<&str>,
) -> Result<ObservedArtifact<ArtifactRecord>> {
    let classification = classify_artifact_state(
        target_path,
        expected_source,
        expected_commit,
        ejected,
        &key.artifact,
        store,
        key,
        expected_vars_digest,
    )?;
    let ArtifactClassification { state, record } = classification;
    match state {
        ArtifactState::Missing => Ok(ObservedArtifact::Missing),
        ArtifactState::Foreign => Ok(ObservedArtifact::Foreign(target_path.to_path_buf())),
        ArtifactState::Ejected => Ok(ObservedArtifact::Ejected),
        ArtifactState::Clean => managed_observation(record, key, ManagedCondition::Clean),
        ArtifactState::Outdated => managed_observation(record, key, ManagedCondition::Outdated),
        ArtifactState::Modified { changed } => {
            managed_observation(record, key, ManagedCondition::Modified { changed })
        }
        ArtifactState::Linked => managed_observation(record, key, ManagedCondition::Linked),
        ArtifactState::Revalidated { fresh } => managed_observation(
            record,
            key,
            ManagedCondition::MetadataChangedButContentClean { refreshed: fresh },
        ),
    }
}

fn managed_observation(
    record: Option<ArtifactRecord>,
    key: &ArtifactKey,
    condition: ManagedCondition,
) -> Result<ObservedArtifact<ArtifactRecord>> {
    let record = record.ok_or_else(|| {
        Error::Projection(format!(
            "managed record for {} vanished mid-observation",
            key.artifact
        ))
    })?;
    Ok(ObservedArtifact::Managed(ManagedArtifact {
        record,
        condition,
        overlay_stale: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::state::FileStateStore;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn read_mtime_secs(path: &Path) -> u64 {
        std::fs::metadata(path)
            .expect("metadata")
            .modified()
            .expect("modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after epoch")
            .as_secs()
    }

    fn set_mtime(path: &Path, secs: u64) {
        filetime::set_file_mtime(
            path,
            filetime::FileTime::from_unix_time(secs.cast_signed(), 0),
        )
        .expect("set mtime");
    }

    const SOURCE: &str = "company-configs";
    const COMMIT: &str = "abc123def456";
    const ARTIFACT: &str = "snippets";
    const TARGET: &str = "vscode";

    fn key() -> ArtifactKey {
        ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: ARTIFACT.to_owned(),
        }
    }

    fn registry() -> (TempDir, FileStateStore) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileStateStore::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    /// Record's `ManifestFile` entries carry on-disk size+mtime, so the result reads as Clean.
    fn deploy_and_record(
        target: &Path,
        files: &[(&str, &[u8])],
        allow_symlinks: bool,
    ) -> ArtifactRecord {
        let mut manifest = Vec::new();
        for (rel, contents) in files {
            let path = target.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::write(&path, contents).expect("write artifact file");
            manifest.push(ManifestFile {
                path: PathBuf::from(rel),
                kind: crate::sync::state::ManifestEntryKind::File,
                size: contents.len() as u64,
                mtime: read_mtime_secs(&path),
                blake3: blake3::hash(contents).to_hex().to_string(),
            });
        }
        ArtifactRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::sync::state::RecordKind::Dir,
            allow_symlinks,
            preserve_executable: true,
            files: manifest,
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

    fn deploy_and_record_file(file_path: &Path, contents: &[u8]) -> ArtifactRecord {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(file_path, contents).expect("write file artifact");
        let leaf = file_path.file_name().expect("file leaf");
        let mf = ManifestFile {
            path: PathBuf::from(leaf),
            kind: crate::sync::state::ManifestEntryKind::File,
            size: contents.len() as u64,
            mtime: read_mtime_secs(file_path),
            blake3: blake3::hash(contents).to_hex().to_string(),
        };
        ArtifactRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::sync::state::RecordKind::File,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![mf],
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

    fn ejected(source: &str, artifact: &str) -> Ejection {
        Ejection {
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        }
    }

    fn state(target: &Path, ejected: &[Ejection], reg: &FileStateStore) -> ArtifactState {
        check_artifact_state(target, SOURCE, COMMIT, ejected, ARTIFACT, reg, &key(), None)
            .expect("check_artifact_state")
    }

    #[test]
    fn ejected_beats_missing_target() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let missing = target.path().join("never-deployed");

        let st = state(&missing, &[ejected(SOURCE, ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Ejected),
            "an ejected artifact stays Ejected even when its target dir is absent, got {st:?}"
        );
    }

    #[test]
    fn ejected_beats_existing_clean_deployment() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");

        let st = state(target.path(), &[ejected(SOURCE, ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Ejected),
            "ejected beats everything, even a present matching record, got {st:?}"
        );
    }

    #[test]
    fn not_ejected_when_artifact_matches_but_source_differs() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");

        let st = state(target.path(), &[ejected("other-source", ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "an ejected entry whose artifact matches but whose source differs from expected_source \
             must not eject this artifact: ejection keys on (artifact, source), got {st:?}"
        );
    }

    #[test]
    fn a_dir_eject_blocks_redeploy_of_a_leaf_under_it() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let leaf = target.path().join("editor").join("a.md");
        std::fs::create_dir_all(leaf.parent().expect("leaf parent")).expect("mkdir leaf parent");
        std::fs::write(&leaf, b"alpha\n").expect("write leaf");

        let leaf_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "editor/a.md".to_owned(),
        };
        let st = check_artifact_state(
            &leaf,
            SOURCE,
            COMMIT,
            &[ejected(SOURCE, "editor")],
            "editor/a.md",
            &reg,
            &leaf_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Ejected),
            "a dir-ejected `editor` must block redeploy of the leaf `editor/a.md` that falls under \
             it — the user took over the whole `editor` path; got {st:?}"
        );
    }

    #[test]
    fn a_leaf_eject_blocks_redeploy_of_a_collapsed_dir_over_it() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let dir = target.path().join("editor");
        std::fs::create_dir_all(&dir).expect("mkdir dir");

        let dir_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "editor".to_owned(),
        };
        let st = check_artifact_state(
            &dir,
            SOURCE,
            COMMIT,
            &[ejected(SOURCE, "editor/a.md")],
            "editor",
            &reg,
            &dir_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Ejected),
            "a leaf-ejected `editor/a.md` must block redeploy of a collapsed `editor` dir that \
             would overwrite it; got {st:?}"
        );
    }

    #[test]
    fn missing_when_target_absent_and_not_ejected() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let missing = target.path().join("not-here");

        let st = state(&missing, &[], &reg);

        assert!(
            matches!(st, ArtifactState::Missing),
            "absent target + not ejected => Missing, got {st:?}"
        );
    }

    #[test]
    fn foreign_when_target_exists_but_no_record() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        std::fs::write(target.path().join("a.json"), b"{}").expect("write file");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Foreign),
            "existing target with no registry record => Foreign, got {st:?}"
        );
    }

    fn sibling_record(
        artifact: &str,
        kind: crate::sync::state::RecordKind,
        linked: bool,
    ) -> ArtifactRecord {
        ArtifactRecord {
            version: 1,
            key: ArtifactKey {
                target: TARGET.to_owned(),
                source: SOURCE.to_owned(),
                artifact: artifact.to_owned(),
            },
            source: SOURCE.to_owned(),
            commit: if linked {
                "link".to_owned()
            } else {
                COMMIT.to_owned()
            },
            digest: "link:".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            kind,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked,
            history: false,
            worktree_admin_id: None,
            mirror_key: None,
            cache_git_root: None,
            vars_digest: None,
            deploy_root: None,
            layout_separator: None,
        }
    }

    #[test]
    fn collapsed_dir_key_over_recorded_per_leaf_reads_outdated_not_foreign() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let dir = target.path().join("snippets");
        std::fs::create_dir_all(&dir).expect("mkdir dir");

        reg.put_artifact(&sibling_record(
            "snippets/a.json",
            crate::sync::state::RecordKind::File,
            true,
        ))
        .expect("put per-leaf record");

        let dir_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "snippets".to_owned(),
        };
        let st = check_artifact_state(&dir, SOURCE, COMMIT, &[], "snippets", &reg, &dir_key, None)
            .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "a plan that collapsed `snippets` this run while the prior run recorded it per-leaf \
             (`snippets/a.json`) must redeploy under the new key, not read it as Foreign; got {st:?}"
        );
    }

    #[test]
    fn per_leaf_key_under_recorded_collapsed_dir_reads_outdated_not_foreign() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let leaf = target.path().join("snippets").join("a.json");
        std::fs::create_dir_all(leaf.parent().expect("leaf parent")).expect("mkdir");
        std::fs::write(&leaf, b"{}").expect("write leaf");

        reg.put_artifact(&sibling_record(
            "snippets",
            crate::sync::state::RecordKind::Dir,
            true,
        ))
        .expect("put collapsed record");

        let leaf_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: SOURCE.to_owned(),
            artifact: "snippets/a.json".to_owned(),
        };
        let st = check_artifact_state(
            &leaf,
            SOURCE,
            COMMIT,
            &[],
            "snippets/a.json",
            &reg,
            &leaf_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "a plan that split `snippets` into per-leaf this run while the prior run recorded the \
             collapsed dir must redeploy under the new key, not read it as Foreign; got {st:?}"
        );
    }

    #[test]
    fn unrelated_source_sibling_stays_foreign() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let dir = target.path().join("snippets");
        std::fs::create_dir_all(&dir).expect("mkdir dir");

        reg.put_artifact(&sibling_record(
            "snippets/a.json",
            crate::sync::state::RecordKind::File,
            true,
        ))
        .expect("put per-leaf record");

        let dir_key = ArtifactKey {
            target: TARGET.to_owned(),
            source: "other-source".to_owned(),
            artifact: "snippets".to_owned(),
        };
        let st = check_artifact_state(
            &dir,
            "other-source",
            COMMIT,
            &[],
            "snippets",
            &reg,
            &dir_key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Foreign),
            "a sibling-shaped record from a DIFFERENT source is not this artifact under another \
             shape; the dir stays Foreign, got {st:?}"
        );
    }

    #[test]
    fn foreign_when_record_source_differs() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            "other-source",
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Foreign),
            "record is findable under key (source={SOURCE}) yet expected_source is other-source: \
             record.key.source != expected_source => Foreign, got {st:?}"
        );
    }

    #[test]
    fn outdated_when_commit_advanced_but_files_clean() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            "other-commit",
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "same source, lock advanced past the deployed commit, on-disk files still match the \
             record => Outdated (a managed artifact to redeploy), not Foreign, got {st:?}"
        );
    }

    #[test]
    fn modified_when_commit_advanced_and_files_diverge() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        std::fs::write(target.path().join("a.json"), b"locally edited").expect("tamper file");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            "other-commit",
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            None,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Modified { .. }),
            "a user edit must read as Modified even when the lock also advanced, so the redeploy \
             warns instead of silently clobbering local changes, got {st:?}"
        );
    }

    // ── linked artifacts (DLD-005) ─────────────────────────────────

    /// A linked record: no manifest files, sentinel commit/digest, `linked = true`.
    fn linked_record() -> ArtifactRecord {
        ArtifactRecord {
            version: 1,
            key: key(),
            source: SOURCE.to_owned(),
            commit: "link".to_owned(),
            digest: "link:".to_owned(),
            projected_at: "2026-06-08T12:00:00Z".to_owned(),
            layout: "flat".to_owned(),
            kind: crate::sync::state::RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: true,
            history: false,
            worktree_admin_id: None,
            mirror_key: None,
            cache_git_root: None,
            vars_digest: None,
            deploy_root: None,
            layout_separator: None,
        }
    }

    /// A symlink deployed at the key, pointing at a live directory, with a linked record
    /// whose sentinel commit deliberately mismatches `expected_commit`. The linked
    /// short-circuit must fire BEFORE the commit/source Foreign check, yielding Linked.
    #[test]
    fn linked_record_reads_linked_not_foreign_despite_commit_mismatch() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let live = parent.path().join("worktree-artifact");
        std::fs::create_dir_all(&live).expect("mkdir live target");
        std::fs::write(live.join("snippets.json"), b"{}").expect("write live file");
        let dst = parent.path().join("deployed-link");
        symlink(&live, &dst).expect("deploy symlink");

        reg.put_artifact(&linked_record())
            .expect("put linked record");

        let st = check_artifact_state(&dst, SOURCE, COMMIT, &[], ARTIFACT, &reg, &key(), None)
            .expect("check_artifact_state on a linked symlink");

        assert!(
            matches!(st, ArtifactState::Linked),
            "a linked record must short-circuit to Linked BEFORE the commit-mismatch Foreign \
             check (sentinel commit `link` != expected {COMMIT}), got {st:?}"
        );
    }

    /// Even when the symlink target's content diverges from anything recorded, a linked
    /// artifact must never be reported Modified — it is quarantined from per-file drift.
    #[test]
    fn linked_record_never_reads_modified_when_target_content_differs() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let live = parent.path().join("worktree-artifact");
        std::fs::create_dir_all(&live).expect("mkdir live target");
        std::fs::write(live.join("anything.json"), b"locally edited content")
            .expect("write divergent file");
        let dst = parent.path().join("deployed-link");
        symlink(&live, &dst).expect("deploy symlink");

        reg.put_artifact(&linked_record())
            .expect("put linked record");

        let st = state(&dst, &[], &reg);

        assert!(
            matches!(st, ArtifactState::Linked),
            "a linked record is quarantined from per-file drift; it must read Linked, never \
             Modified, even when the live target content changes, got {st:?}"
        );
    }

    /// A dangling linked symlink (its target deleted): `try_exists` follows the link and
    /// returns Ok(false), so the state is Missing — and the call must not crash.
    #[test]
    fn dangling_linked_symlink_reads_missing_without_crashing() {
        let (_state_dir, reg) = registry();
        let parent = TempDir::new().expect("target parent");
        let gone = parent.path().join("deleted-worktree-artifact");
        let dst = parent.path().join("deployed-link");
        symlink(&gone, &dst).expect("deploy dangling symlink");
        assert!(
            !gone.exists(),
            "premise: the symlink target must be absent so the link dangles"
        );

        reg.put_artifact(&linked_record())
            .expect("put linked record");

        let st = state(&dst, &[], &reg);

        assert!(
            matches!(st, ArtifactState::Missing),
            "a dangling linked symlink follows to a non-existent target => Missing (redeploy), \
             and must not error, got {st:?}"
        );
    }

    #[test]
    fn clean_when_disk_matches_record() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        reg.put_artifact(&record).expect("put record");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "every recorded file present with matching size+mtime, no extras => Clean, got {st:?}"
        );
    }

    #[test]
    fn modified_when_recorded_file_size_changed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        let edited = target.path().join("a.json");
        std::fs::write(&edited, b"{\"changed\": true}").expect("rewrite file");
        set_mtime(&edited, record.files[0].mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("size change must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the size-changed file must appear in `changed`, got {changed:?}"
        );
    }

    /// Reframes the former `modified_when_recorded_file_mtime_changed` bug pin.
    #[test]
    fn revalidated_when_only_mtime_changed_but_bytes_identical() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        let new_mtime = record.files[0].mtime + 999;
        set_mtime(&target.path().join("a.json"), new_mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "a touched-but-byte-identical file must escalate to its recorded blake3, match, \
                 and reclassify from the false-positive Modified to Revalidated, got {st:?}"
            );
        };
        let entry = fresh
            .iter()
            .find(|f| f.path == *Path::new("a.json"))
            .unwrap_or_else(|| {
                panic!("Revalidated must carry fresh stat for the revalidated file, got {fresh:?}")
            });
        assert_eq!(
            entry.mtime, new_mtime,
            "fresh stat must carry the NEW on-disk mtime so the refresh returns it to the fast path"
        );
        assert_eq!(
            entry.size, record.files[0].size,
            "the byte-identical file's size is unchanged and must be carried through as recorded"
        );
    }

    #[test]
    fn modified_when_bytes_differ_at_same_size_with_bumped_mtime() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        let edited = target.path().join("a.json");
        std::fs::write(&edited, b"[]").expect("rewrite to same-length different content");
        set_mtime(&edited, record.files[0].mtime + 5);
        assert_eq!(
            std::fs::metadata(&edited).expect("edited meta").len(),
            record.files[0].size,
            "premise: the edit preserves byte length so only mtime + content diverge"
        );

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!(
                "a same-size content change must read Modified once the mtime drift forces a \
                 blake3 escalation that mismatches the recorded hash, got {st:?}"
            );
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the genuinely edited file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn revalidated_when_all_stat_divergent_files_are_byte_identical() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        reg.put_artifact(&record).expect("put record");
        let a_mtime = record.files[0].mtime + 100;
        let b_mtime = record.files[1].mtime + 200;
        set_mtime(&target.path().join("a.json"), a_mtime);
        set_mtime(&target.path().join("b.txt"), b_mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "when every stat-divergent file hash-matches its record the artifact must be \
                 Revalidated, not Modified, got {st:?}"
            );
        };
        let a = fresh
            .iter()
            .find(|f| f.path == *Path::new("a.json"))
            .unwrap_or_else(|| panic!("fresh must include a.json, got {fresh:?}"));
        let b = fresh
            .iter()
            .find(|f| f.path == *Path::new("b.txt"))
            .unwrap_or_else(|| panic!("fresh must include b.txt, got {fresh:?}"));
        assert_eq!(
            a.mtime, a_mtime,
            "a.json fresh stat must carry its new mtime"
        );
        assert_eq!(
            b.mtime, b_mtime,
            "b.txt fresh stat must carry its new mtime"
        );
    }

    /// Partial-drift rule (DGI-D4): one real edit collapses the artifact to Modified and the
    /// touched sibling's revalidation is discarded, never half-persisted.
    #[test]
    fn modified_no_refresh_when_one_file_edited_and_sibling_only_touched() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        reg.put_artifact(&record).expect("put record");
        let a = target.path().join("a.json");
        std::fs::write(&a, b"[]").expect("same-length edit of a.json");
        set_mtime(&a, record.files[0].mtime + 13);
        assert_eq!(
            std::fs::metadata(&a).expect("a meta").len(),
            record.files[0].size,
            "premise: a.json keeps its recorded byte length so the size gate passes and only the \
             hash-miss can catch the edit"
        );
        set_mtime(&target.path().join("b.txt"), record.files[1].mtime + 777);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!(
                "any genuinely changed file must collapse the artifact to Modified — the touched \
                 sibling's revalidation must be discarded, never surfaced as Revalidated, got {st:?}"
            );
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the same-size, hash-mismatched file must appear in `changed`, got {changed:?}"
        );
        let after = reg
            .artifact(&key())
            .expect("get after classify")
            .expect("record still present");
        assert_eq!(
            after, record,
            "a Modified artifact must never half-persist a refreshed stat for its touched-but-\
             identical sibling: the stored record stays byte-for-byte unchanged (DGI-D4)"
        );
    }

    /// Hot path (gestalt step 1): a size+mtime stat-match is classified without reading or
    /// hashing the file. A poisoned recorded blake3 over an untouched file stays invisible.
    #[test]
    fn hot_path_never_hashes_a_file_whose_size_and_mtime_match() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let mut record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        record.files[0].blake3 = "0".repeat(64);
        reg.put_artifact(&record)
            .expect("put record with poisoned blake3");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "a size+mtime stat-match must classify Clean without hashing; the poisoned blake3 must \
             stay invisible on the hot path, got {st:?}"
        );
    }

    /// Hot path holds per-file: a poisoned-but-stat-matching A is never hashed while a
    /// touched-identical sibling B escalates, so the artifact reads `Revalidated { fresh: [B] }`.
    #[test]
    fn hot_path_skips_stat_matching_file_while_sibling_escalates() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let mut record = deploy_and_record(
            target.path(),
            &[("a.json", b"{}"), ("b.txt", b"hello")],
            false,
        );
        record.files[0].blake3 = "0".repeat(64);
        reg.put_artifact(&record)
            .expect("put record with poisoned A blake3");
        let b_mtime = record.files[1].mtime + 555;
        set_mtime(&target.path().join("b.txt"), b_mtime);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "A is a stat-match and must never be hashed, so its poisoned blake3 stays invisible \
                 while B escalates and revalidates => Revalidated, got {st:?}"
            );
        };
        assert_eq!(
            fresh.len(),
            1,
            "only the stat-divergent sibling B may be refreshed; the stat-matching A must not \
             appear in `fresh`, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].path,
            PathBuf::from("b.txt"),
            "the single fresh entry must be B, never the stat-matching A, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].mtime, b_mtime,
            "B's fresh stat must carry its new mtime, got {fresh:?}"
        );
    }

    #[test]
    fn classification_of_revalidation_does_not_persist_or_mutate_the_record() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        let recorded_blake3 = record.files[0].blake3.clone();
        reg.put_artifact(&record).expect("put record");
        set_mtime(&target.path().join("a.json"), record.files[0].mtime + 42);

        let st = state(target.path(), &[], &reg);
        assert!(
            matches!(st, ArtifactState::Revalidated { .. }),
            "premise: the touched-identical file revalidates, got {st:?}"
        );

        let after = reg
            .artifact(&key())
            .expect("get after classify")
            .expect("record still present");
        assert_eq!(
            after.files[0].blake3, recorded_blake3,
            "classify must never rewrite mf.blake3 across a revalidation"
        );
        assert_eq!(
            after, record,
            "the read-only classify path must persist NOTHING: the stored record (including the \
             stale size/mtime) must be byte-for-byte unchanged"
        );
    }

    #[test]
    fn file_kind_revalidates_when_touched_but_bytes_identical() {
        let (_state_dir, reg) = registry();
        let dir = TempDir::new().expect("target dir");
        let file = dir.path().join("config.json");
        let record = deploy_and_record_file(&file, b"{}");
        reg.put_artifact(&record).expect("put record");
        let new_mtime = record.files[0].mtime + 314;
        set_mtime(&file, new_mtime);

        let st = state(&file, &[], &reg);

        let ArtifactState::Revalidated { fresh } = st else {
            panic!(
                "the single-renamed-FILE path must escalate a touched-but-identical file to \
                 blake3 and read Revalidated, got {st:?}"
            );
        };
        assert_eq!(
            fresh.len(),
            1,
            "a single-file artifact must produce exactly one fresh entry, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].path, record.files[0].path,
            "the fresh entry must carry the recorded file's path, not the absolute target path or a \
             wrong leaf, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].size, record.files[0].size,
            "the byte-identical file's size is unchanged and must be carried through as recorded, \
             got {fresh:?}"
        );
        assert_eq!(
            fresh[0].mtime, new_mtime,
            "file-kind revalidation must carry the fresh on-disk mtime of the revalidated file, \
             got {fresh:?}"
        );
    }

    /// Fail-closed (DGI-D5): an unreadable stat-divergent file reads Modified, never Clean.
    #[cfg(unix)]
    #[test]
    fn modified_when_stat_divergent_file_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        let f = target.path().join("a.json");
        set_mtime(&f, record.files[0].mtime + 11);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

        // chmod 0o000 does not deny root: if the file still reads, perms are bypassed (running as
        // root, e.g. a CI container) and the fail-closed precondition cannot hold — skip.
        if std::fs::read(&f).is_ok() {
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644))
                .expect("restore perms");
            return;
        }

        let st = state(target.path(), &[], &reg);

        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms for tempdir cleanup");

        assert!(
            matches!(st, ArtifactState::Modified { .. }),
            "a permission/IO error reading a stat-divergent file during escalation must fail \
             closed to Modified, never silently Clean/Revalidated, got {st:?}"
        );
    }

    /// Collapsed/linked fallback (DGI-D6): empty `files[]` takes the stat-only short path.
    #[test]
    fn empty_files_record_reads_clean_without_escalation() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[], false);
        reg.put_artifact(&record).expect("put record");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "a record with empty files[] must take the existing stat-only short path and read \
             Clean, never hash or panic, got {st:?}"
        );
    }

    /// Collapsed/linked fallback (DGI-D6): a `linked = true` record short-circuits to Linked
    /// before any per-file escalation, even with on-disk content that would otherwise be hashed.
    #[test]
    fn linked_record_short_circuits_before_escalation() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        std::fs::write(
            target.path().join("a.json"),
            b"content that would be hashed",
        )
        .expect("write file under linked target");
        reg.put_artifact(&linked_record())
            .expect("put linked record");

        let st = state(target.path(), &[], &reg);

        assert!(
            matches!(st, ArtifactState::Linked),
            "a linked record must take the Linked short-circuit before per-file revalidation, \
             never hashing or panicking, got {st:?}"
        );
    }

    #[test]
    fn modified_when_recorded_file_deleted() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}"), ("b.txt", b"x")], false);
        reg.put_artifact(&record).expect("put record");
        std::fs::remove_file(target.path().join("b.txt")).expect("delete file");

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("deleted recorded file must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("b.txt")),
            "the deleted recorded file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn modified_when_extra_untracked_file_present() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        std::fs::write(target.path().join("extra.tmp"), b"stray").expect("write extra");

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("extra on-disk file must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("extra.tmp")),
            "the extra untracked file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn modified_when_symlink_present_and_disallowed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put_artifact(&record).expect("put record");
        symlink("a.json", target.path().join("link.json")).expect("create symlink");

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("disallowed on-disk symlink must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("link.json")),
            "with allow_symlinks=false, the symlink must appear in `changed`, got {changed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn modified_when_recorded_file_replaced_by_symlink() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], true);
        reg.put_artifact(&record).expect("put record");

        let recorded_path = target.path().join("a.json");
        let recorded_size = record.files[0].size;
        let recorded_mtime = record.files[0].mtime;

        let decoy_dir = TempDir::new().expect("decoy dir");
        let decoy = decoy_dir.path().join("decoy.json");
        let decoy_bytes = usize::try_from(recorded_size).expect("recorded size fits usize");
        std::fs::write(&decoy, vec![0u8; decoy_bytes]).expect("write decoy");
        set_mtime(&decoy, recorded_mtime);
        assert_eq!(
            std::fs::metadata(&decoy).expect("decoy meta").len(),
            recorded_size,
            "decoy must match the recorded file's size so a metadata-follows-symlink impl is fooled"
        );

        std::fs::remove_file(&recorded_path).expect("remove recorded regular file");
        symlink(&decoy, &recorded_path).expect("replace recorded file with symlink");

        let followed = std::fs::metadata(&recorded_path).expect("followed meta");
        assert_eq!(
            followed.len(),
            recorded_size,
            "following the symlink must yield the decoy's matching size (the trap)"
        );

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!(
                "a recorded REGULAR file replaced on disk by a symlink whose target matches \
                 size+mtime must yield Modified; a metadata-follows-symlink stat is fooled into \
                 Clean. got {st:?}"
            );
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the recorded path is now a symlink, not the original regular file, so it must appear \
             in `changed`, got {changed:?}"
        );
    }

    // ── per-artifact vars digest (TPH-010) ─────────────────────────

    /// A clean deployment whose record carries `vars_digest`; on-disk files match
    /// the record so only the vars-digest comparison can move the state.
    fn deploy_and_record_with_vars(
        target: &Path,
        files: &[(&str, &[u8])],
        vars_digest: Option<&str>,
    ) -> ArtifactRecord {
        let mut record = deploy_and_record(target, files, false);
        record.vars_digest = vars_digest.map(str::to_owned);
        record
    }

    #[test]
    fn outdated_when_vars_digest_differs_and_files_clean() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record =
            deploy_and_record_with_vars(target.path(), &[("a.json", b"{}")], Some("blake3:old"));
        reg.put_artifact(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            Some("blake3:new"),
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Outdated),
            "same source, same commit, on-disk files clean, but the templating vars changed since \
             deploy (record vars_digest blake3:old != expected blake3:new) => Outdated so the \
             artifact re-renders and redeploys without --force, got {st:?}"
        );
    }

    #[test]
    fn clean_when_vars_digest_matches_and_nothing_else_changed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record =
            deploy_and_record_with_vars(target.path(), &[("a.json", b"{}")], Some("blake3:same"));
        reg.put_artifact(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            Some("blake3:same"),
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Clean),
            "record vars_digest equals the expected current vars_digest and nothing else drifted \
             => Clean, no needless re-render, got {st:?}"
        );
    }

    #[test]
    fn clean_when_feature_free_record_has_no_vars_digest() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record_with_vars(target.path(), &[("a.json", b"{}")], None);
        reg.put_artifact(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
            Some("blake3:current-vars"),
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Clean),
            "a feature-free artifact (rendered no template, so vars_digest is None) must be \
             unaffected by a vars change: it stays Clean even when an expected vars_digest is \
             supplied (INV-8), got {st:?}"
        );
    }
}
