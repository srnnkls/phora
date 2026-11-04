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

fn run(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .current_dir(directory)
        .args(args)
        .env("HOME", directory.join("home"))
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .output()
        .expect("run phora")
}

fn succeeds(directory: &Path, args: &[&str]) -> String {
    let output = run(directory, args);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "phora {args:?} failed:\n{stderr}");
    stderr
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

impl Fixture {
    fn new(deploy: &str) -> Self {
        let dir = tempfile::tempdir().expect("fixture directory");
        let root = dir.path().to_path_buf();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("skills/keep")).expect("skill tree");
        fs::create_dir_all(source.join("other")).expect("other tree");
        fs::create_dir_all(&destination).expect("destination directory");
        fs::write(source.join("skills/keep/SKILL.md"), b"keep\n").expect("skill");
        fs::write(source.join("other/notes.md"), b"other\n").expect("other leaf");
        git(&source, &["init", "-b", "main", "--template="]);
        git(&source, &["add", "skills", "other"]);
        git(&source, &["commit", "-m", "fixture"]);
        let fixture = Self {
            _dir: dir,
            root,
            source,
            destination,
        };
        fixture.write_config(deploy, true);
        succeeds(&fixture.root, &["sync", "--no-hooks"]);
        assert!(
            fs::symlink_metadata(fixture.artifact()).is_ok(),
            "premise: the skill is deployed"
        );
        fixture
    }

    fn write_config(&self, deploy: &str, with_target: bool) {
        let target = if with_target {
            format!(
                "[targets.dest]\npath = {:?}\nsources = {{ tropos = {{ collapse = false }} }}\n",
                self.destination.to_str().expect("destination path"),
            )
        } else {
            String::new()
        };
        fs::write(
            self.root.join("phora.toml"),
            format!(
                "version = 1\n[paths]\ncache = \"cache\"\nstate = \"state\"\n\
                 [sources.tropos]\npath = {:?}\nbranch = \"main\"\ndeploy = {deploy:?}\n{target}",
                self.source.to_str().expect("source path"),
            ),
        )
        .expect("config");
    }

    fn artifact(&self) -> PathBuf {
        self.destination.join("skills/keep/SKILL.md")
    }

    fn remove_target(&self, deploy: &str) {
        self.write_config(deploy, false);
    }
}

fn assert_kept_message(stderr: &str) {
    assert!(
        stderr.contains("phora: keeping tropos:skills/keep/SKILL.md")
            && stderr.contains("use --force to delete it"),
        "the kept artifact must be reported with its remedy; got:\n{stderr}"
    );
}

#[test]
fn prune_after_target_removal_keeps_a_directory_that_replaced_a_link() {
    let fixture = Fixture::new("link");
    let artifact = fixture.artifact();
    fs::remove_file(&artifact).expect("unlink the managed link");
    fs::create_dir(&artifact).expect("user directory over the link");
    fs::write(artifact.join("mine.md"), b"mine\n").expect("user file");
    fixture.remove_target("link");

    let stderr =
        String::from_utf8_lossy(&run(&fixture.root, &["sync", "--prune", "--no-hooks"]).stderr)
            .into_owned();

    assert_eq!(
        fs::read(artifact.join("mine.md")).expect("the user directory survives the prune"),
        b"mine\n"
    );
    assert_kept_message(&stderr);
}

#[test]
fn prune_keeps_a_link_that_now_points_elsewhere() {
    let fixture = Fixture::new("link");
    let artifact = fixture.artifact();
    let elsewhere = fixture.root.join("elsewhere.md");
    fs::write(&elsewhere, b"elsewhere\n").expect("user link target");
    fs::remove_file(&artifact).expect("unlink the managed link");
    std::os::unix::fs::symlink(&elsewhere, &artifact).expect("re-point the link");
    fixture.remove_target("link");

    let stderr =
        String::from_utf8_lossy(&run(&fixture.root, &["sync", "--prune", "--no-hooks"]).stderr)
            .into_owned();

    assert_eq!(
        fs::read_link(&artifact).expect("the re-pointed link survives the prune"),
        elsewhere
    );
    assert_kept_message(&stderr);
}

#[test]
fn prune_keeps_a_modified_copy_until_forced() {
    let fixture = Fixture::new("copy");
    let artifact = fixture.artifact();
    fs::write(&artifact, b"edited\n").expect("edit the deployed copy");
    fixture.remove_target("copy");

    let stderr =
        String::from_utf8_lossy(&run(&fixture.root, &["sync", "--prune", "--no-hooks"]).stderr)
            .into_owned();
    assert_eq!(
        fs::read(&artifact).expect("the modified copy survives the prune"),
        b"edited\n"
    );
    assert_kept_message(&stderr);

    succeeds(&fixture.root, &["sync", "--prune", "--force", "--no-hooks"]);
    assert!(
        fs::symlink_metadata(&artifact).is_err(),
        "--force must delete the modified copy"
    );
}

#[test]
fn prune_still_removes_a_clean_copy_and_link() {
    for deploy in ["copy", "link"] {
        let fixture = Fixture::new(deploy);
        fixture.remove_target(deploy);
        let stderr = succeeds(&fixture.root, &["sync", "--prune", "--no-hooks"]);
        assert!(
            fs::symlink_metadata(fixture.artifact()).is_err(),
            "{deploy}: an unchanged artifact is still pruned; stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("phora: keeping"),
            "{deploy}: nothing is kept:\n{stderr}"
        );
    }
}
