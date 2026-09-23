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

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
    fs::write(path, contents).expect("write fixture file");
}

#[test]
fn one_source_splits_into_per_target_slices_by_subtree_rename() {
    let fixture = tempfile::tempdir().expect("fixture");
    let root = fixture.path();
    write(&root.join(".henia/claude/CLAUDE.md"), "claude");
    write(&root.join(".henia/claude/skills/a/SKILL.md"), "skill");
    write(&root.join(".henia/codex/AGENTS.md"), "codex");
    write(
        &root.join("phora.toml"),
        r#"
[paths]
cache = "cache"
state = "state"
[sources.henia]
path = "./.henia"
deploy = "link"
[targets.claude]
path = "claude-home"
sources.henia = { take = [{ "claude/" = "." }] }
[targets.codex]
path = "codex-home"
sources.henia = { take = [{ "codex/" = "agents/" }], collapse = false }
"#,
    );

    let result = phora(root, &["sync"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let read = |p: &str| fs::read_to_string(root.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
    assert_eq!(read("claude-home/CLAUDE.md"), "claude");
    assert_eq!(read("claude-home/skills/a/SKILL.md"), "skill");
    assert_eq!(read("codex-home/agents/AGENTS.md"), "codex");
    assert!(!root.join("claude-home/claude").exists());
    assert!(!root.join("claude-home/AGENTS.md").exists());
    assert!(!root.join("codex-home/agents/CLAUDE.md").exists());

    write(&root.join(".henia/claude/skills/a/SKILL.md"), "edited");
    assert_eq!(
        read("claude-home/skills/a/SKILL.md"),
        "edited",
        "a linked re-rooted leaf follows its source"
    );
}
