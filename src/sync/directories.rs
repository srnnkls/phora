//! Skip enumeration of directories whose entries have not changed since staging.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::scan::scan_dir_soft;
use super::state::{ArtifactRecord, DirectoryStamp};
use crate::error::{Error, Result};

/// Capture before enumeration: an entry added during the read must invalidate
/// the saved timestamp. Never cache a tree containing unrecorded regular files
/// or disallowed symlinks, since skipping it would hide existing drift.
pub(super) fn snapshot(
    root: &Path,
    record: &ArtifactRecord,
) -> Result<Option<Vec<DirectoryStamp>>> {
    let known: HashSet<_> = record.files.iter().map(|f| f.path.as_path()).collect();
    let mut stamps = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let path = if rel.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let meta = std::fs::symlink_metadata(&path).map_err(|e| io_error("stat", &path, &e))?;
        if !meta.is_dir() {
            return Ok(None);
        }
        stamps.push(DirectoryStamp::new(rel.clone(), &meta));
        for entry in std::fs::read_dir(&path).map_err(|e| io_error("read dir", &path, &e))? {
            let entry = entry.map_err(|e| io_error("read entry", &path, &e))?;
            let child = rel.join(entry.file_name());
            let kind = entry
                .file_type()
                .map_err(|e| io_error("file type", &entry.path(), &e))?;
            if kind.is_dir() {
                pending.push(child);
            } else if (kind.is_file() && !known.contains(child.as_path()))
                || (kind.is_symlink() && !record.allow_symlinks)
            {
                return Ok(None);
            }
        }
    }
    stamps.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Some(stamps))
}

pub(super) fn foreign_paths(root: &Path, record: &ArtifactRecord) -> Result<BTreeSet<PathBuf>> {
    foreign_paths_with(root, record, |_| {})
}

fn foreign_paths_with(
    root: &Path,
    record: &ArtifactRecord,
    mut enumerated: impl FnMut(&Path),
) -> Result<BTreeSet<PathBuf>> {
    let known: HashSet<_> = record.files.iter().map(|f| f.path.as_path()).collect();
    let Some(stamps) = &record.directories else {
        return full_scan(root, &known, record.allow_symlinks);
    };
    let by_path: HashMap<_, _> = stamps.iter().map(|s| (s.path.as_path(), s)).collect();
    let mut children: HashMap<&Path, Vec<&Path>> = HashMap::new();
    for stamp in stamps {
        if let Some(parent) = stamp.path.parent() {
            children.entry(parent).or_default().push(&stamp.path);
        }
    }
    let mut changed = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let path = if rel.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_error("stat", &path, &e)),
        };
        if rel.as_os_str().is_empty() && meta.is_symlink() {
            // WalkDir follows a root symlink even with follow_links disabled.
            // Preserve that legacy behavior, including foreign descendants.
            return full_scan(root, &known, record.allow_symlinks);
        }
        if !meta.is_dir() {
            if (meta.is_file() && !known.contains(rel.as_path()))
                || (meta.is_symlink() && !record.allow_symlinks)
            {
                changed.insert(rel);
            }
            continue;
        }
        let current = DirectoryStamp::new(rel.clone(), &meta);
        if by_path
            .get(rel.as_path())
            .is_some_and(|old| **old == current)
        {
            if let Some(dirs) = children.get(rel.as_path()) {
                pending.extend(dirs.iter().map(|p| p.to_path_buf()));
            }
            continue;
        }
        enumerated(&rel);
        for entry in std::fs::read_dir(&path).map_err(|e| io_error("read dir", &path, &e))? {
            let entry = entry.map_err(|e| io_error("read entry", &path, &e))?;
            let child = rel.join(entry.file_name());
            let kind = entry
                .file_type()
                .map_err(|e| io_error("file type", &entry.path(), &e))?;
            if kind.is_dir() {
                pending.push(child);
            } else if (kind.is_file() && !known.contains(child.as_path()))
                || (kind.is_symlink() && !record.allow_symlinks)
            {
                changed.insert(child);
            }
        }
    }
    Ok(changed)
}

fn full_scan(
    root: &Path,
    known: &HashSet<&Path>,
    allow_symlinks: bool,
) -> Result<BTreeSet<PathBuf>> {
    let scan = scan_dir_soft(root)?;
    let mut changed: BTreeSet<_> = scan
        .files
        .into_iter()
        .filter(|f| !known.contains(f.path.as_path()))
        .map(|f| f.path)
        .collect();
    if !allow_symlinks {
        changed.extend(scan.symlinks);
    }
    Ok(changed)
}

fn io_error(operation: &str, path: &Path, error: &std::io::Error) -> Error {
    Error::Projection(format!("{operation} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::state::{ArtifactKey, ManifestFile, NewArtifactRecord, RecordKind};

    fn record(root: &Path, paths: &[&str]) -> ArtifactRecord {
        let files = paths
            .iter()
            .map(|rel| {
                let path = root.join(rel);
                std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
                std::fs::write(&path, b"original").expect("write");
                ManifestFile {
                    path: PathBuf::from(rel),
                    size: 8,
                    mtime: super::super::scan::mtime_secs(
                        &std::fs::metadata(&path).expect("stat"),
                        &path,
                    )
                    .expect("mtime"),
                    blake3: blake3::hash(b"original").to_hex().to_string(),
                }
            })
            .collect();
        let mut record = ArtifactRecord::projected(NewArtifactRecord {
            key: ArtifactKey {
                target: "target".into(),
                source: "source".into(),
                artifact: "artifact".into(),
            },
            underlying_source: "source",
            commit: "commit",
            digest: "digest".into(),
            layout: "flat".into(),
            kind: RecordKind::Dir,
            allow_symlinks: false,
            preserve_executable: true,
            files,
            vars_digest: None,
            deploy_root: None,
            layout_separator: None,
        });
        record.directories = snapshot(root, &record).expect("snapshot");
        record
    }

    #[test]
    fn unchanged_large_tree_never_enumerates_entries_but_file_drift_is_checked() {
        use crate::sync::state::{FileStateStore, StateStore};
        let root = tempfile::tempdir().expect("root");
        let paths: Vec<_> = (0..2048).map(|i| format!("nested/file-{i}")).collect();
        let record = record(
            root.path(),
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let mut reads = Vec::new();
        assert!(
            foreign_paths_with(root.path(), &record, |p| reads.push(p.to_path_buf()))
                .expect("check")
                .is_empty()
        );
        assert!(
            reads.is_empty(),
            "unchanged directories must not be enumerated"
        );
        std::fs::write(root.path().join("nested/file-0"), b"changed length").expect("edit");
        let state = tempfile::tempdir().expect("state");
        let store = FileStateStore::open(state.path().to_path_buf()).expect("store");
        store.put_artifact(&record).expect("put");
        assert_eq!(
            store.artifact(&record.key).expect("load"),
            Some(record.clone())
        );
        let result = super::super::inspect::check_artifact_state(
            root.path(),
            "source",
            "commit",
            &[],
            "artifact",
            &store,
            &record.key,
            None,
        )
        .expect("inspect");
        assert!(
            matches!(result, super::super::inspect::ArtifactState::Modified { changed } if changed == vec![PathBuf::from("nested/file-0")])
        );
    }

    #[test]
    fn nested_addition_reads_only_changed_directory_and_new_subtree() {
        let root = tempfile::tempdir().expect("root");
        let mut record = record(root.path(), &["stable/file", "changed/file"]);
        let dir = root.path().join("changed");
        filetime::set_file_mtime(&dir, filetime::FileTime::from_unix_time(100, 1)).expect("mtime");
        record.directories = snapshot(root.path(), &record).expect("snapshot");
        std::fs::create_dir_all(dir.join("new/deep")).expect("mkdir");
        std::fs::write(dir.join("new/deep/foreign"), b"extra").expect("write");
        // Same second, different nanosecond: seconds-only comparison misses this.
        filetime::set_file_mtime(&dir, filetime::FileTime::from_unix_time(100, 2)).expect("mtime");
        let mut reads = BTreeSet::new();
        let changed = foreign_paths_with(root.path(), &record, |p| {
            reads.insert(p.to_path_buf());
        })
        .expect("check");
        assert_eq!(
            changed,
            BTreeSet::from([PathBuf::from("changed/new/deep/foreign")])
        );
        assert_eq!(
            reads,
            ["changed", "changed/new", "changed/new/deep"]
                .map(PathBuf::from)
                .into()
        );
    }

    #[test]
    fn replacement_with_same_timestamp_is_enumerated_and_symlinks_are_not_followed() {
        let root = tempfile::tempdir().expect("root");
        let mut record = record(root.path(), &["nested/file"]);
        let dir = root.path().join("nested");
        let mtime = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(&dir).expect("stat"),
        );
        let saved = tempfile::tempdir().expect("saved");
        std::fs::rename(&dir, saved.path().join("old")).expect("move");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::write(dir.join("extra"), b"foreign").expect("write");
        filetime::set_file_mtime(&dir, mtime).expect("mtime");
        assert_eq!(
            foreign_paths(root.path(), &record).expect("check"),
            [PathBuf::from("nested/extra")].into()
        );
        std::fs::remove_dir_all(&dir).expect("remove");
        std::os::unix::fs::symlink(saved.path(), &dir).expect("symlink");
        assert_eq!(
            foreign_paths(root.path(), &record).expect("check"),
            [PathBuf::from("nested")].into()
        );
        record.allow_symlinks = true;
        assert!(
            foreign_paths(root.path(), &record)
                .expect("allowed symlink")
                .is_empty()
        );
    }

    #[test]
    fn legacy_records_and_dirty_snapshots_do_not_hide_foreign_files() {
        let root = tempfile::tempdir().expect("root");
        let mut record = record(root.path(), &["file"]);
        record.directories = None;
        let encoded = toml::to_string(&record).expect("encode");
        assert!(!encoded.contains("directories"));
        let record: ArtifactRecord = toml::from_str(&encoded).expect("legacy decode");
        std::fs::write(root.path().join("extra"), b"foreign").expect("write");
        assert_eq!(
            foreign_paths(root.path(), &record).expect("check"),
            [PathBuf::from("extra")].into()
        );
        assert!(snapshot(root.path(), &record).expect("snapshot").is_none());
    }

    #[test]
    fn root_symlink_preserves_legacy_foreign_descendant_detection() {
        let root = tempfile::tempdir().expect("root");
        let mut record = record(root.path(), &["file"]);
        let links = tempfile::tempdir().expect("links");
        let link = links.path().join("artifact");
        std::os::unix::fs::symlink(root.path(), &link).expect("symlink");
        std::fs::write(root.path().join("foreign"), b"extra").expect("write");
        for allow in [false, true] {
            record.allow_symlinks = allow;
            let cached = foreign_paths(&link, &record).expect("cached check");
            let mut legacy = record.clone();
            legacy.directories = None;
            assert_eq!(cached, foreign_paths(&link, &legacy).expect("legacy check"));
            assert!(cached.contains(Path::new("foreign")));
        }
    }
}
