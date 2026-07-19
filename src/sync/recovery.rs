//! Recovery and rollback for interrupted target-side artifact swaps.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::store::Registry;

use super::journal::Journal;

pub(super) fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub(super) fn rollback_swap(dst: &Path, backup: Option<&Path>) -> Result<()> {
    remove_path(dst)
        .map_err(|e| Error::Projection(format!("rollback remove {}: {e}", dst.display())))?;
    if let Some(backup) = backup {
        std::fs::rename(backup, dst).map_err(|e| {
            Error::Projection(format!(
                "rollback restore {} -> {}: {e}",
                backup.display(),
                dst.display()
            ))
        })?;
    }
    Ok(())
}

pub(super) fn backup_path(staging_base: &Path, dst: &Path) -> PathBuf {
    let leaf = dst.file_name().map_or_else(
        || "artifact".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    staging_base.join(format!(".phora-backup-{leaf}"))
}

/// Startup reconciliation: replay `journal`, then remove any orphaned
/// `<target_parent>/.phora-stage*` left by a crash.
pub fn recovery_sweep(
    target_parent: &Path,
    journal: &Journal,
    registry: &dyn Registry,
) -> Result<()> {
    let entries = journal.entries()?;
    if journal.refuses_writes() && !entries.is_empty() {
        return Err(journal.readonly_error());
    }
    for entry in entries {
        if entry.swap_completed {
            registry.put(&entry.record)?;
        } else {
            let backup = backup_path(&entry.staging_base, &entry.dst);
            if backup
                .try_exists()
                .map_err(|e| Error::Projection(format!("stat backup {}: {e}", backup.display())))?
            {
                std::fs::rename(&backup, &entry.dst).map_err(|e| {
                    Error::Projection(format!(
                        "restore backup {} -> {}: {e}",
                        backup.display(),
                        entry.dst.display()
                    ))
                })?;
            }
            remove_path(&entry.staging).map_err(|e| {
                Error::Projection(format!("discard staging {}: {e}", entry.staging.display()))
            })?;
        }
        journal.remove(&entry.dst)?;
    }
    remove_orphaned_staging(target_parent)
}

fn remove_orphaned_staging(target_parent: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(target_parent) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(Error::Projection(format!(
                "read dir {}: {e}",
                target_parent.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::Projection(format!("read entry in {}: {e}", target_parent.display()))
        })?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".phora-stage")
        {
            remove_path(&entry.path()).map_err(|e| {
                Error::Projection(format!("remove orphan {}: {e}", entry.path().display()))
            })?;
        }
    }
    Ok(())
}
