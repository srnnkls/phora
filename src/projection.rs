//! Deployment: drift detection, copy/scan, atomic directory swap.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::error::{Error, Result};
use crate::registry::{ArtifactKey, EjectedEntry, Registry, RegistryRecord, ScannedFile};

#[derive(Debug)]
pub enum ArtifactState {
    Clean,
    Modified { changed: Vec<PathBuf> },
    Foreign,
    Missing,
    Ejected,
}

#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    /// Relative paths of symlinks encountered (excluded from `files`).
    pub symlinks: Vec<PathBuf>,
}

pub fn check_artifact_state(
    target_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    ejected: &[EjectedEntry],
    artifact_name: &str,
    registry: &dyn Registry,
    key: &ArtifactKey,
) -> Result<ArtifactState> {
    let is_ejected = ejected
        .iter()
        .any(|e| e.artifact == artifact_name && e.source == expected_source);
    if is_ejected {
        return Ok(ArtifactState::Ejected);
    }

    match target_path.try_exists() {
        Ok(false) => return Ok(ArtifactState::Missing),
        Ok(true) => {}
        Err(e) => {
            return Err(Error::Projection(format!(
                "stat {}: {e}",
                target_path.display()
            )));
        }
    }

    let Some(record) = registry.get(key)? else {
        return Ok(ArtifactState::Foreign);
    };

    if record.key.source != expected_source || record.commit != expected_commit {
        return Ok(ArtifactState::Foreign);
    }

    let mut changed: BTreeSet<PathBuf> = BTreeSet::new();

    for mf in &record.files {
        let file_path = target_path.join(&mf.path);
        // No-follow stat: a recorded regular file swapped for a symlink is drift.
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
        if !meta.is_file() {
            changed.insert(mf.path.clone());
            continue;
        }
        if meta.len() != mf.size || mtime_secs(&meta, &file_path)? != mf.mtime {
            changed.insert(mf.path.clone());
        }
    }

    let scan = scan_dir_soft(target_path)?;
    let known: HashSet<&PathBuf> = record.files.iter().map(|f| &f.path).collect();
    for cf in &scan.files {
        if !known.contains(&cf.path) {
            changed.insert(cf.path.clone());
        }
    }
    if !record.allow_symlinks {
        changed.extend(scan.symlinks);
    }

    if changed.is_empty() {
        Ok(ArtifactState::Clean)
    } else {
        Ok(ArtifactState::Modified {
            changed: changed.into_iter().collect(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum ScanMode {
    Strict,
    Soft,
}

/// Soft scan: never errors on symlinks, reports them for "treat as Modified".
pub fn scan_dir_soft(dir: &Path) -> Result<ScanResult> {
    scan_dir(dir, true, ScanMode::Soft)
}

/// Strict scan (write path): errors on a disallowed symlink.
pub fn scan_dir_strict(dir: &Path, allow_symlinks: bool) -> Result<ScanResult> {
    scan_dir(dir, allow_symlinks, ScanMode::Strict)
}

fn scan_dir(dir: &Path, allow_symlinks: bool, mode: ScanMode) -> Result<ScanResult> {
    let mut files = Vec::new();
    let mut symlinks = Vec::new();

    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::Projection(format!("walk {}: {e}", dir.display())))?;
        let ft = entry.file_type();
        let rel = relative(entry.path(), dir)?;

        if ft.is_symlink() {
            match mode {
                ScanMode::Strict if !allow_symlinks => {
                    return Err(Error::SymlinkNotAllowed { path: rel });
                }
                ScanMode::Strict => {}
                ScanMode::Soft => symlinks.push(rel),
            }
            continue;
        }

        if !ft.is_file() {
            continue;
        }

        let meta = entry
            .metadata()
            .map_err(|e| Error::Projection(format!("stat {}: {e}", entry.path().display())))?;
        files.push(ScannedFile {
            path: rel,
            size: meta.len(),
            mtime: mtime_secs(&meta, entry.path())?,
        });
    }

    Ok(ScanResult { files, symlinks })
}

fn relative(path: &Path, base: &Path) -> Result<PathBuf> {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .map_err(|e| Error::Projection(format!("strip prefix {}: {e}", path.display())))
}

fn mtime_secs(meta: &std::fs::Metadata, path: &Path) -> Result<u64> {
    meta.modified()
        .map_err(|e| Error::Projection(format!("mtime {}: {e}", path.display())))?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| Error::Projection(format!("mtime before epoch {}: {e}", path.display())))
}

/// Copy a file from staging to target, preferring reflink, preserving mtime.
pub fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if reflink_copy::reflink(src, dst).is_err() {
        std::fs::copy(src, dst).map_err(|e| {
            Error::Projection(format!("copy {} -> {}: {e}", src.display(), dst.display()))
        })?;
    }
    copy_mtime(src, dst)
}

fn copy_mtime(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::metadata(src)
        .map_err(|e| Error::Projection(format!("stat {}: {e}", src.display())))?;
    let mtime = filetime::FileTime::from_last_modification_time(&meta);
    filetime::set_file_mtime(dst, mtime)
        .map_err(|e| Error::Projection(format!("set mtime {}: {e}", dst.display())))
}

pub fn copy_tree(_src: &Path, _dst: &Path, _allow_symlinks: bool) -> Result<()> {
    Err(Error::NotImplemented("copy_tree"))
}

/// Atomic swap of staging into the destination, then persist the registry record.
pub fn deploy_artifact(
    _staging_base: &Path,
    _staging: &Path,
    _dst: &Path,
    _record: RegistryRecord,
    _registry: &dyn Registry,
) -> Result<()> {
    Err(Error::NotImplemented("deploy_artifact"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{FileRegistry, ManifestFile};
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn mtime_secs(path: &Path) -> u64 {
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

    // copy_file

    #[test]
    fn copy_file_reproduces_source_content() {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"reflink-or-copy payload").expect("write src");

        copy_file(&src, &dst).expect("copy_file");

        assert_eq!(
            std::fs::read(&dst).expect("read dst"),
            b"reflink-or-copy payload",
            "dst content must be byte-identical to src regardless of reflink vs plain copy"
        );
    }

    #[test]
    fn copy_file_preserves_source_mtime() {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"payload").expect("write src");
        let known = 1_700_000_000u64;
        set_mtime(&src, known);

        copy_file(&src, &dst).expect("copy_file");

        assert_eq!(
            mtime_secs(&dst),
            known,
            "copy_file must explicitly carry the src mtime onto dst (reflink does not copy mtime)"
        );
    }

    // scan_dir_soft / scan_dir_strict

    #[test]
    fn soft_scan_reports_regular_files_with_relative_path() {
        let dir = TempDir::new().expect("tempdir");
        let file = dir.path().join("settings.json");
        std::fs::write(&file, b"{}").expect("write file");
        let known_mtime = 1_700_000_123u64;
        set_mtime(&file, known_mtime);

        let scan = scan_dir_soft(dir.path()).expect("soft scan must not error");

        let scanned = scan
            .files
            .iter()
            .find(|f| f.path == *Path::new("settings.json"))
            .unwrap_or_else(|| {
                panic!(
                    "soft scan must list the regular file by its path relative to the scanned dir, \
                     got {:?}",
                    scan.files
                )
            });
        assert_eq!(
            scanned.size, 2,
            "scanned file size must be the on-disk byte length"
        );
        assert_eq!(
            scanned.mtime, known_mtime,
            "scanned file mtime must be the on-disk mtime in whole seconds since epoch"
        );
    }

    #[test]
    fn soft_scan_reports_symlink_without_error_and_excludes_it_from_files() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"hi").expect("write real");
        symlink("real.txt", dir.path().join("link.txt")).expect("create symlink");

        let scan = scan_dir_soft(dir.path()).expect("soft scan must never error on symlinks");

        assert!(
            scan.symlinks.contains(&PathBuf::from("link.txt")),
            "soft scan must report the symlink in `symlinks`, got {:?}",
            scan.symlinks
        );
        assert!(
            !scan.files.iter().any(|f| f.path == *Path::new("link.txt")),
            "symlink must be excluded from `files`, got {:?}",
            scan.files
        );
        assert!(
            scan.files.iter().any(|f| f.path == *Path::new("real.txt")),
            "the regular file must still appear in `files`, got {:?}",
            scan.files
        );
    }

    #[test]
    fn strict_scan_errors_on_disallowed_symlink() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"hi").expect("write real");
        symlink("real.txt", dir.path().join("link.txt")).expect("create symlink");

        let err = scan_dir_strict(dir.path(), false)
            .expect_err("strict scan with symlinks disallowed must return Err on a symlink");

        let Error::SymlinkNotAllowed { path } = err else {
            panic!("strict scan must reject the disallowed symlink specifically, got {err:?}");
        };
        assert!(
            path.ends_with("link.txt"),
            "the error must name the offending symlink (link.txt), got {}",
            path.display()
        );
    }

    // check_artifact_state

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

    fn registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    /// Record's `ManifestFile` entries carry on-disk size+mtime, so the result reads as Clean.
    fn deploy_and_record(
        target: &Path,
        files: &[(&str, &[u8])],
        allow_symlinks: bool,
    ) -> RegistryRecord {
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
                blake3: "blake3:deadbeef".to_owned(),
            });
        }
        RegistryRecord {
            version: 1,
            key: key(),
            commit: COMMIT.to_owned(),
            digest: "blake3:d4e5f6".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks,
            preserve_executable: true,
            files: manifest,
        }
    }

    fn ejected(source: &str, artifact: &str) -> EjectedEntry {
        EjectedEntry {
            source: source.to_owned(),
            artifact: artifact.to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        }
    }

    fn state(target: &Path, ejected: &[EjectedEntry], reg: &FileRegistry) -> ArtifactState {
        check_artifact_state(target, SOURCE, COMMIT, ejected, ARTIFACT, reg, &key())
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
        reg.put(&record).expect("put record");

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
        reg.put(&record).expect("put record");

        let st = state(target.path(), &[ejected("other-source", ARTIFACT)], &reg);

        assert!(
            matches!(st, ArtifactState::Clean),
            "an ejected entry whose artifact matches but whose source differs from expected_source \
             must not eject this artifact: ejection keys on (artifact, source), got {st:?}"
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

    #[test]
    fn foreign_when_record_source_differs() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            "other-source",
            COMMIT,
            &[],
            ARTIFACT,
            &reg,
            &record.key,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Foreign),
            "record is findable under key (source={SOURCE}) yet expected_source is other-source: \
             record.key.source != expected_source => Foreign, got {st:?}"
        );
    }

    #[test]
    fn foreign_when_record_commit_differs() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");

        let st = check_artifact_state(
            target.path(),
            SOURCE,
            "other-commit",
            &[],
            ARTIFACT,
            &reg,
            &record.key,
        )
        .expect("check_artifact_state");

        assert!(
            matches!(st, ArtifactState::Foreign),
            "record is findable under key yet expected_commit is other-commit: \
             record.commit ({COMMIT}) != expected_commit => Foreign, got {st:?}"
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
        reg.put(&record).expect("put record");

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
        reg.put(&record).expect("put record");
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

    #[test]
    fn modified_when_recorded_file_mtime_changed() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}")], false);
        reg.put(&record).expect("put record");
        set_mtime(&target.path().join("a.json"), record.files[0].mtime + 999);

        let st = state(target.path(), &[], &reg);

        let ArtifactState::Modified { changed } = st else {
            panic!("mtime change must yield Modified, got {st:?}");
        };
        assert!(
            changed.contains(&PathBuf::from("a.json")),
            "the mtime-changed file must appear in `changed`, got {changed:?}"
        );
    }

    #[test]
    fn modified_when_recorded_file_deleted() {
        let (_state_dir, reg) = registry();
        let target = TempDir::new().expect("target dir");
        let record = deploy_and_record(target.path(), &[("a.json", b"{}"), ("b.txt", b"x")], false);
        reg.put(&record).expect("put record");
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
        reg.put(&record).expect("put record");
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
        reg.put(&record).expect("put record");
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
        reg.put(&record).expect("put record");

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
}
