use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::error::{Error, Result};
use crate::sync::model::ScannedFile;

#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    /// Relative paths of symlinks encountered (excluded from `files`).
    pub symlinks: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
enum ScanMode {
    Strict,
    Soft,
}

/// Soft scan: never errors on symlinks, reports them for "treat as Modified".
pub(crate) fn scan_dir_soft(dir: &Path) -> Result<ScanResult> {
    scan_dir(dir, true, ScanMode::Soft)
}

/// Strict scan (write path): errors on a disallowed symlink.
pub(crate) fn scan_dir_strict(dir: &Path, allow_symlinks: bool) -> Result<ScanResult> {
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
                ScanMode::Strict | ScanMode::Soft => symlinks.push(rel),
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

pub(crate) fn mtime_secs(meta: &std::fs::Metadata, path: &Path) -> Result<u64> {
    meta.modified()
        .map_err(|e| Error::Projection(format!("mtime {}: {e}", path.display())))?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| Error::Projection(format!("mtime before epoch {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn set_mtime(path: &Path, secs: u64) {
        filetime::set_file_mtime(
            path,
            filetime::FileTime::from_unix_time(secs.cast_signed(), 0),
        )
        .expect("set mtime");
    }

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

    #[test]
    fn strict_scan_records_an_allowed_symlink_so_it_survives_the_copy_fallback() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"hi").expect("write real");
        symlink("real.txt", dir.path().join("link.txt")).expect("create symlink");

        let scan = scan_dir_strict(dir.path(), true).expect("strict scan with symlinks allowed");

        assert!(
            scan.symlinks.contains(&PathBuf::from("link.txt")),
            "an ALLOWED symlink must be recorded, not silently dropped, or copy_tree loses it on \
             the cross-device fallback; got {:?}",
            scan.symlinks
        );
        assert!(
            !scan.files.iter().any(|f| f.path == *Path::new("link.txt")),
            "the symlink must still be excluded from `files`, got {:?}",
            scan.files
        );
    }
}
