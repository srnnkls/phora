#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

fn git(directory: &Path, args: &[&str]) {
    common::assert_sandboxed(directory);
    let output = Command::new("git")
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

fn phora(directory: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_phora"))
        .current_dir(directory)
        .args(args)
        .env("HOME", directory.join("home"))
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .output()
        .expect("run phora");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

fn fixture(deploy: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("fixture directory");
    let root = dir.path().to_path_buf();
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(source.join("skills/drop/ref/deep")).expect("dropped skill tree");
    fs::create_dir_all(source.join("skills/keep")).expect("kept skill");
    fs::create_dir_all(&destination).expect("destination directory");
    fs::write(source.join("skills/drop/SKILL.md"), b"drop\n").expect("dropped skill");
    fs::write(source.join("skills/drop/ref/deep/a.md"), b"nested\n").expect("nested ref");
    fs::write(source.join("skills/keep/SKILL.md"), b"keep\n").expect("kept skill");
    git(&source, &["init", "-b", "main", "--template="]);
    git(&source, &["add", "skills"]);
    git(&source, &["commit", "-m", "fixture"]);
    write_config(&root, &source, &destination, deploy, "");
    phora(&root, &["sync", "--no-hooks"]);
    assert!(destination.join("skills/drop/ref/deep/a.md").exists());
    Fixture {
        _dir: dir,
        root,
        source,
        destination,
    }
}

fn write_config(root: &Path, source: &Path, destination: &Path, deploy: &str, take: &str) {
    fs::write(
        root.join("phora.toml"),
        format!(
            "version = 1\n[paths]\ncache = \"cache\"\nstate = \"state\"\n\
             [sources.tropos]\npath = {:?}\nbranch = \"main\"\ndeploy = {deploy:?}\n\
             [targets.dest]\npath = {:?}\nsources = {{ tropos = {{ {take}collapse = false }} }}\n",
            source.to_str().expect("source path"),
            destination.to_str().expect("destination path"),
        ),
    )
    .expect("config");
}

fn drop_upstream(fixture: &Fixture) {
    git(&fixture.source, &["rm", "-r", "-q", "skills/drop"]);
    git(&fixture.source, &["commit", "-m", "drop skill"]);
}

fn assert_dropped_tree_gone(fixture: &Fixture) {
    let dropped = fixture.destination.join("skills/drop");
    assert!(
        fs::symlink_metadata(&dropped).is_err(),
        "the dropped artifact's emptied directories must be removed: {}",
        dropped.display()
    );
    assert!(fixture.destination.join("skills/keep/SKILL.md").exists());
}

#[test]
fn fast_forward_drop_removes_emptied_artifact_directories() {
    let fixture = fixture("copy");
    drop_upstream(&fixture);
    phora(&fixture.root, &["update", "--fast-forward"]);
    assert_dropped_tree_gone(&fixture);
}

#[test]
fn fast_forward_drop_keeps_a_directory_holding_a_foreign_file_and_reports_it() {
    let fixture = fixture("copy");
    let foreign = fixture.destination.join("skills/drop/notes.txt");
    fs::write(&foreign, b"mine\n").expect("foreign file");
    drop_upstream(&fixture);
    let output = phora(&fixture.root, &["update", "--fast-forward"]);

    assert_eq!(
        fs::read(&foreign).expect("foreign file survives"),
        b"mine\n"
    );
    assert!(
        fs::symlink_metadata(fixture.destination.join("skills/drop/ref")).is_err(),
        "emptied subdirectories beside the foreign file are still removed"
    );
    assert!(!fixture.destination.join("skills/drop/SKILL.md").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let kept = fixture.destination.join("skills/drop");
    assert!(
        stderr.contains(&format!("kept directory {}", kept.display())),
        "the directory kept by a foreign file must be reported: {stderr}"
    );
    assert!(
        !stderr.contains(&format!(
            "kept directory {}:",
            fixture.destination.join("skills").display()
        )),
        "a parent holding live artifacts is not reported: {stderr}"
    );
}

#[test]
fn prune_removes_emptied_artifact_directories() {
    let fixture = fixture("copy");
    write_config(
        &fixture.root,
        &fixture.source,
        &fixture.destination,
        "copy",
        "take = [\"skills/keep/\"], ",
    );
    phora(&fixture.root, &["sync", "--no-hooks", "--prune"]);
    assert_dropped_tree_gone(&fixture);
}

#[test]
fn prune_of_linked_artifacts_removes_emptied_directories() {
    let fixture = fixture("link");
    assert!(
        fs::symlink_metadata(fixture.destination.join("skills/drop/ref/deep/a.md"))
            .expect("linked leaf")
            .is_symlink()
    );
    write_config(
        &fixture.root,
        &fixture.source,
        &fixture.destination,
        "link",
        "take = [\"skills/keep/\"], ",
    );
    phora(&fixture.root, &["sync", "--no-hooks", "--prune"]);
    assert_dropped_tree_gone(&fixture);
}
