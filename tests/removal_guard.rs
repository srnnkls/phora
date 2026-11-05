#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

mod common;

use common::{run_git, run_phora, run_phora_ok};

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
        run_git(&source, &["init", "-b", "main", "--template="]);
        run_git(&source, &["add", "skills", "other"]);
        run_git(&source, &["commit", "-m", "fixture"]);
        let fixture = Self {
            _dir: dir,
            root,
            source,
            destination,
        };
        fixture.write_config(deploy, true);
        run_phora_ok(&fixture.root, &["sync", "--no-hooks"]);
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

    let stderr = String::from_utf8_lossy(
        &run_phora(&fixture.root, &["sync", "--prune", "--no-hooks"]).stderr,
    )
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

    let stderr = String::from_utf8_lossy(
        &run_phora(&fixture.root, &["sync", "--prune", "--no-hooks"]).stderr,
    )
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

    let stderr = String::from_utf8_lossy(
        &run_phora(&fixture.root, &["sync", "--prune", "--no-hooks"]).stderr,
    )
    .into_owned();
    assert_eq!(
        fs::read(&artifact).expect("the modified copy survives the prune"),
        b"edited\n"
    );
    assert_kept_message(&stderr);

    run_phora_ok(&fixture.root, &["sync", "--prune", "--force", "--no-hooks"]);
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
        let stderr = run_phora_ok(&fixture.root, &["sync", "--prune", "--no-hooks"]);
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
