use std::path::Path;

/// HEAD of a local working-tree repo; a non-repo or unborn HEAD yields the
/// `"link"` sentinel, since link mode must tolerate a plain directory.
pub fn read_local_head(path: &str) -> crate::error::Result<String> {
    let Ok(repo) = gix::open(path) else {
        return Ok("link".to_owned());
    };
    match repo.head_id() {
        Ok(id) => Ok(id.to_hex().to_string()),
        Err(_) => Ok("link".to_owned()),
    }
}

/// True when `git` is a local filesystem path (absolute or existing), not a scheme/scp-style URL.
#[must_use]
pub fn is_local_path(git: &str) -> bool {
    if git.contains("://") {
        return false;
    }
    if matches!(git.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic()) {
        return true;
    }
    let first_slash = git.find('/');
    if let Some(colon) = git.find(':')
        && first_slash.is_none_or(|slash| colon < slash)
    {
        return false;
    }
    let path = Path::new(git);
    path.is_absolute() || path.exists()
}
