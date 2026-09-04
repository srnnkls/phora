use std::fs::File;
use std::io;

/// Rust std maps `sync_all` to `F_FULLFSYNC` on Apple targets; write-then-rename
/// only needs the ordering `F_BARRIERFSYNC` gives.
#[cfg(target_os = "macos")]
pub(crate) fn fsync_barrier(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    // SAFETY: `file` keeps its descriptor open for the duration of the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == 0 {
        return Ok(());
    }
    file.sync_all()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn fsync_barrier(file: &File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::fsync_barrier;

    #[test]
    fn barrier_succeeds_on_a_written_file() {
        use std::io::Write as _;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let mut file = std::fs::File::create(dir.path().join("record.tmp")).expect("create");
        file.write_all(b"record").expect("write");
        fsync_barrier(&file).expect("barrier");
    }
}
