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

/// Runs fixture git in `directory` with an isolated config and a fixed identity.
pub fn run_git(directory: &Path, args: &[&str]) {
    assert_sandboxed(directory);
    let output = std::process::Command::new("git")
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs the phora binary in `directory`, with home, cache, and state under it.
pub fn run_phora(directory: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_phora"))
        .current_dir(directory)
        .args(args)
        .env("HOME", directory.join("home"))
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .output()
        .expect("run phora")
}

/// Runs phora like [`run_phora`], asserts success, and returns its stderr.
pub fn run_phora_ok(directory: &Path, args: &[&str]) -> String {
    let output = run_phora(directory, args);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "phora {args:?} failed:\n{stderr}");
    stderr
}
