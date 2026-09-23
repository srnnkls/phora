#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn phora(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .current_dir(root)
        .args(args)
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_STATE_HOME", root.join("state"))
        .output()
        .expect("run phora")
}

#[test]
fn directory_selects_configs_hooks_sources_targets_and_locks() {
    let fixture = tempfile::tempdir().expect("fixture");
    let root = fixture.path();
    let project = root.join("project with spaces");
    fs::create_dir(&project).expect("project");
    fs::write(root.join("phora.toml"), "invalid caller config").expect("caller config");
    fs::write(
        project.join("phora.toml"),
        r#"
[paths]
cache = "cache"
state = "state"
[hooks]
pre_sync = "mkdir -p .henia/claude && printf compiled > .henia/claude/SKILL.md"
post_sync = "test -f installed/SKILL.md && printf checked > smoke-result"
[sources.compiled]
path = ".henia/claude"
deploy = "link"
"#,
    )
    .expect("shared config");
    fs::write(
        project.join("phora.local.toml"),
        r#"
[targets.claude]
path = "installed"
sources.compiled = { collapse = false }
"#,
    )
    .expect("local config");
    let result = phora(root, &["-C", "project with spaces", "sync"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read(project.join("installed/SKILL.md")).expect("deployed skill"),
        b"compiled"
    );
    assert_eq!(
        fs::read(project.join("smoke-result")).expect("post-sync result"),
        b"checked"
    );
    assert!(project.join("phora.lock").is_file());
    assert!(!root.join("phora.lock").exists());
    assert!(!root.join(".henia").exists());
    let replay = phora(
        root,
        &[
            "sync",
            "--directory",
            "project with spaces",
            "--frozen",
            "--no-hooks",
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
}

#[test]
fn missing_directory_fails_before_reading_caller_config() {
    let fixture = tempfile::tempdir().expect("fixture");
    fs::write(
        fixture.path().join("phora.toml"),
        "[hooks]\npre_sync = 'touch unexpected'\n",
    )
    .expect("config");
    let result = phora(fixture.path(), &["-C", "missing-project", "sync"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing-project"));
    assert!(!fixture.path().join("unexpected").exists());
    assert!(!fixture.path().join("phora.lock").exists());
}
