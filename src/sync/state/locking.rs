use std::path::Path;

use super::file::{FileStateStore, StateError};

type Result<T> = std::result::Result<T, StateError>;

/// RAII guard holding an exclusive OS lock on `state.lock`; released on drop.
#[derive(Debug)]
pub struct StateLock {
    _file: std::fs::File,
}

// flock OFD is inherited by a forked `git` until it execs: the flock-holding test takes write, forkers take read.
#[cfg(test)]
pub static STATE_LOCK_SERIAL: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[cfg(test)]
pub fn guard_git_fork() -> std::sync::RwLockReadGuard<'static, ()> {
    STATE_LOCK_SERIAL
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Refuse fixture git outside the temp sandbox, so it can never write a real repo.
///
/// # Panics
///
/// Panics when `cwd` resolves outside the process temporary directory.
#[cfg(test)]
pub fn assert_git_sandboxed(cwd: &Path) {
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

impl FileStateStore {
    pub(super) fn acquire_state_lock(&self) -> Result<StateLock> {
        let locks_dir = &self.journal_root;
        if let Err(e) = std::fs::create_dir_all(locks_dir) {
            return Err(self.lock_io_error("create locks dir", locks_dir, &e));
        }
        let lock_path = locks_dir.join("state.lock");
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(e) => return Err(self.lock_io_error("open lock file", &lock_path, &e)),
        };
        match file.try_lock() {
            Ok(()) => Ok(StateLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(StateError::Lock(
                "another phora process is running for this project (state.lock held)".to_owned(),
            )),
            Err(std::fs::TryLockError::Error(e)) => Err(StateError::StateStore(format!(
                "acquire lock on {}: {e}",
                lock_path.display()
            ))),
        }
    }

    fn lock_io_error(&self, what: &str, path: &Path, e: &std::io::Error) -> StateError {
        classify_io_error(&self.state_root, what, path, e)
    }

    /// One-line advisory when the state root sits on a network filesystem whose
    /// `state.lock` flock is unreliable across hosts; `None` on local storage.
    #[must_use]
    pub fn lock_advisory(&self) -> Option<String> {
        statfs_fstype(&self.state_root)
            .as_deref()
            .and_then(network_lock_advisory)
    }
}

fn is_network_fstype(fstype: &str) -> bool {
    const NETWORK_FSTYPES: &[&str] = &[
        "nfs", "nfs4", "smbfs", "smb", "smb2", "cifs", "afpfs", "webdav",
    ];
    NETWORK_FSTYPES.contains(&fstype.trim().to_ascii_lowercase().as_str())
}

// Best-effort warning only: detecting the mount never builds a cross-host lock (scope constraint).
pub(super) fn network_lock_advisory(fstype: &str) -> Option<String> {
    is_network_fstype(fstype).then(|| {
        format!(
            "phora: state root is on a network filesystem ({fstype}); the state.lock is \
             advisory over NFS/SMB and may not block concurrent syncs from other machines"
        )
    })
}

#[cfg(test)]
pub(super) fn is_network_fs(path: &Path) -> bool {
    statfs_fstype(path).is_some_and(|fstype| is_network_fstype(&fstype))
}

#[cfg(target_os = "macos")]
fn statfs_fstype(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated path; the kernel initializes
    // `buf` on success (rc == 0) before we read it.
    let buf = unsafe {
        if libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) != 0 {
            return None;
        }
        buf.assume_init()
    };
    let bytes = buf.f_fstypename.map(i8::cast_unsigned);
    let name = std::ffi::CStr::from_bytes_until_nul(&bytes).ok()?;
    Some(name.to_string_lossy().into_owned())
}

// Linux statfs yields a numeric magic, not a name; the fstype string lives in mountinfo instead.
#[cfg(target_os = "linux")]
fn statfs_fstype(path: &Path) -> Option<String> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        let (pre, post) = line.split_once(" - ")?;
        let mount_point = pre.split_whitespace().nth(4).map(unescape_octal)?;
        let fstype = post.split_whitespace().next()?;
        if target.starts_with(&mount_point) {
            let depth = Path::new(&mount_point).components().count();
            if best.as_ref().is_none_or(|(d, _)| depth >= *d) {
                best = Some((depth, fstype.to_owned()));
            }
        }
    }
    best.map(|(_, fstype)| fstype)
}

#[cfg(target_os = "linux")]
pub(super) fn unescape_octal(field: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let digits: String = chars.by_ref().take(3).collect();
            if let Ok(byte) = u8::from_str_radix(&digits, 8) {
                out.push(byte);
                continue;
            }
            out.push(b'\\');
            out.extend_from_slice(digits.as_bytes());
        } else {
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn statfs_fstype(_path: &Path) -> Option<String> {
    None
}

fn classify_io_error(root: &Path, what: &str, path: &Path, e: &std::io::Error) -> StateError {
    if matches!(
        e.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    ) {
        StateError::ReadOnly(format!(
            "state root {} is read-only ({what} {}: {e})",
            root.display(),
            path.display()
        ))
    } else {
        StateError::StateStore(format!("{what} {}: {e}", path.display()))
    }
}
