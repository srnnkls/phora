#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

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

fn phora(directory: &Path, args: &[&str]) {
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
}

fn assert_prunes_dangling_link(remove_target: bool) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let root = fixture.path();
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).expect("source directory");
    fs::create_dir_all(&destination).expect("destination directory");
    fs::write(source.join("tool"), b"managed\n").expect("source artifact");
    fs::write(source.join("keep"), b"source sentinel\n").expect("source sentinel");
    fs::write(destination.join("unmanaged"), b"user data\n").expect("unmanaged file");
    git(&source, &["init", "-b", "main", "--template="]);
    git(&source, &["add", "tool", "keep"]);
    git(&source, &["commit", "-m", "fixture"]);

    let base = format!(
        "version = 1\n[paths]\ncache = \"cache\"\nstate = \"state\"\n\
         [sources.live]\npath = {:?}\nbranch = \"main\"\ndeploy = \"link\"\ninclude = [\"tool\"]\n",
        source.to_str().expect("source path")
    );
    let target = format!(
        "[targets.dest]\npath = {:?}\nsources = [\"live\"]\n",
        destination.to_str().expect("destination path")
    );
    fs::write(root.join("phora.toml"), format!("{base}{target}")).expect("initial config");
    phora(root, &["sync", "--no-hooks"]);
    let link = destination.join("tool");
    assert!(
        fs::symlink_metadata(&link)
            .expect("deployed link")
            .is_symlink()
    );

    fs::remove_file(source.join("tool")).expect("remove link target");
    let remaining = if remove_target {
        base
    } else {
        format!("{base}{}", target.replace("[\"live\"]", "[]"))
    };
    fs::write(root.join("phora.toml"), remaining).expect("remove binding or target");
    phora(root, &["sync", "--no-hooks", "--prune"]);

    let error = fs::symlink_metadata(&link).expect_err("pruning must unlink the dangling entry");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        fs::read(source.join("keep")).expect("source sentinel survives"),
        b"source sentinel\n"
    );
    assert_eq!(
        fs::read(destination.join("unmanaged")).expect("unmanaged file survives"),
        b"user data\n"
    );
}

#[test]
fn prune_unbound_source_removes_dangling_link() {
    assert_prunes_dangling_link(false);
}

#[test]
fn prune_removed_target_removes_dangling_link() {
    assert_prunes_dangling_link(true);
}
