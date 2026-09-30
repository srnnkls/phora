//! The content store: each deployed file's bytes, kept once under `<cache>/content` and
//! reflinked into staging.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::Result;
use crate::sync::state::{FileStateStore, StateStore};

/// Objects younger than this survive a prune: a running sync may not have recorded them yet.
const PRUNE_GRACE: Duration = Duration::from_hours(1);
/// A sync prunes the store once the previous prune is older than this.
const AUTO_PRUNE_INTERVAL: Duration = Duration::from_hours(7 * 24);
const PRUNE_MARKER: &str = ".last-prune";
const EXECUTABLE_SUFFIX: &str = ".x";

pub(crate) fn object_path(store: &Path, blake3: &str, executable: bool) -> PathBuf {
    let name = if executable {
        format!("{blake3}{EXECUTABLE_SUFFIX}")
    } else {
        blake3.to_owned()
    };
    store.join(&blake3[..2]).join(name)
}

#[derive(Debug, Default)]
pub struct PruneReport {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
}

/// Removes store objects no registry under `projects` references.
///
/// # Errors
/// Unreadable registries or store entries.
pub fn prune(store: &Path, projects: &Path, dry_run: bool) -> Result<PruneReport> {
    let live = live_hashes(projects)?;
    let mut report = PruneReport::default();
    let shards = match std::fs::read_dir(store) {
        Ok(shards) => shards,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e.into()),
    };
    for shard in shards {
        let shard = shard?;
        if !shard.file_type()?.is_dir() {
            continue;
        }
        let shard = shard.path();
        for entry in std::fs::read_dir(&shard)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let referenced = !name.starts_with('.')
                && live.contains(name.strip_suffix(EXECUTABLE_SUFFIX).unwrap_or(&name));
            if referenced || age(meta.modified()?) < PRUNE_GRACE {
                continue;
            }
            let path = entry.path();
            if !dry_run {
                std::fs::remove_file(&path)?;
            }
            report.bytes += meta.len();
            report.removed.push(path);
        }
        if !dry_run {
            let _ = std::fs::remove_dir(&shard);
        }
    }
    report.removed.sort();
    Ok(report)
}

/// Prunes when the previous prune is older than a week; `None` when it was skipped.
///
/// # Errors
/// As [`prune`], plus a marker that cannot be written.
pub fn auto_prune(store: &Path, projects: &Path) -> Result<Option<PruneReport>> {
    let marker = store.join(PRUNE_MARKER);
    let recent = std::fs::metadata(&marker)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| age(modified) < AUTO_PRUNE_INTERVAL);
    if recent || !store.is_dir() {
        return Ok(None);
    }
    let report = prune(store, projects, false)?;
    std::fs::File::create(&marker)?;
    Ok(Some(report))
}

fn live_hashes(projects: &Path) -> Result<BTreeSet<String>> {
    let mut live = BTreeSet::new();
    let registries = match std::fs::read_dir(projects) {
        Ok(registries) => registries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(live),
        Err(e) => return Err(e.into()),
    };
    for registry in registries {
        let registry = registry?;
        if !registry.file_type()?.is_dir() {
            continue;
        }
        for record in FileStateStore::open(registry.path())?.all_artifacts()? {
            live.extend(record.files.into_iter().map(|file| file.blake3));
        }
    }
    Ok(live)
}

fn age(time: SystemTime) -> Duration {
    time.elapsed().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::sync::state::ArtifactRecord;

    const LIVE: &str = "aa11111111111111111111111111111111111111111111111111111111111111";
    const DEAD: &str = "bb22222222222222222222222222222222222222222222222222222222222222";
    const FRESH: &str = "cc33333333333333333333333333333333333333333333333333333333333333";

    struct Fixture {
        _root: TempDir,
        store: PathBuf,
        projects: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = TempDir::new().expect("fixture root");
        let store = root.path().join("content");
        let projects = root.path().join("projects");
        let record: ArtifactRecord = toml::from_str(&format!(
            r#"
version = 1
commit = "def456789abc123"
digest = "blake3:d4e5f6"
projected_at = "2026-01-31T12:34:56Z"
layout = "flat"
allow_symlinks = false
preserve_executable = true
files = [{{ path = "init.lua", size = 4, mtime = 0, blake3 = "{LIVE}" }}]

[key]
target = "editor"
source = "dotfiles"
artifact = "init.lua"
"#
        ))
        .expect("record parses");
        FileStateStore::open(projects.join("project-a"))
            .expect("open registry")
            .put_artifact(&record)
            .expect("record written");
        let stale = filetime::FileTime::from_unix_time(0, 0);
        for (shard, name, old) in [
            ("aa", format!("{LIVE}.x"), true),
            ("bb", DEAD.to_owned(), true),
            ("cc", FRESH.to_owned(), false),
            ("bb", format!(".{DEAD}.123.0"), true),
        ] {
            let path = store.join(shard).join(&name);
            std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
            std::fs::write(&path, b"data").expect("object written");
            if old {
                filetime::set_file_mtime(&path, stale).expect("age object");
            }
        }
        Fixture {
            _root: root,
            store,
            projects,
        }
    }

    #[test]
    fn prune_removes_unreferenced_objects_past_the_grace_period() {
        let fx = fixture();

        let report = prune(&fx.store, &fx.projects, false).expect("prune succeeds");

        let mut expected = vec![
            fx.store.join("bb").join(DEAD),
            fx.store.join("bb").join(format!(".{DEAD}.123.0")),
        ];
        expected.sort();
        assert_eq!(report.removed, expected);
        assert_eq!(report.bytes, 8);
        assert!(!fx.store.join("bb").exists(), "an emptied shard is removed");
        assert!(
            object_path(&fx.store, LIVE, true).exists(),
            "a recorded hash keeps its object"
        );
        assert!(
            object_path(&fx.store, FRESH, false).exists(),
            "an object inside the grace period survives"
        );
    }

    #[test]
    fn dry_run_reports_without_removing() {
        let fx = fixture();

        let report = prune(&fx.store, &fx.projects, true).expect("dry run succeeds");

        assert_eq!(report.removed.len(), 2);
        assert!(report.removed.iter().all(|path| path.exists()));
    }

    #[test]
    fn auto_prune_runs_once_per_interval() {
        let fx = fixture();

        let first = auto_prune(&fx.store, &fx.projects).expect("first auto prune");
        std::fs::write(fx.store.join("cc").join(DEAD), b"data").expect("new object");
        filetime::set_file_mtime(
            fx.store.join("cc").join(DEAD),
            filetime::FileTime::from_unix_time(0, 0),
        )
        .expect("age object");
        let second = auto_prune(&fx.store, &fx.projects).expect("second auto prune");

        assert_eq!(first.expect("first run prunes").removed.len(), 2);
        assert!(second.is_none(), "a prune inside the interval is skipped");
    }
}
