#![allow(dead_code)]

use std::path::Path;

/// Refuse fixture git outside the temp sandbox, so it can never write a real repo.
pub fn assert_sandboxed(cwd: &Path) {
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
