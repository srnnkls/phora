#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

mod common;

use common::{run_git, run_phora, run_phora_ok};

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Project {
    /// Two local sources, `up` and `keep`, synced into target `mako`; `up` with `history` when asked.
    fn synced(history: bool) -> Self {
        let dir = tempfile::tempdir().expect("project directory");
        let root = dir.path().join("proj");
        fs::create_dir_all(&root).expect("project root");
        fs::write(root.join("phora.toml"), "version = 1\n").expect("config");
        for name in ["up", "keep"] {
            let source = dir.path().join(name);
            fs::create_dir_all(source.join(name)).expect("source tree");
            fs::write(source.join(name).join("a.txt"), format!("{name}\n")).expect("source file");
            run_git(&source, &["init", "-b", "main", "--template="]);
            run_git(&source, &["add", name]);
            run_git(&source, &["commit", "-m", "fixture"]);
            run_git(&source, &["tag", "v1"]);
        }
        let project = Self { _dir: dir, root };
        run_phora_ok(
            &project.root,
            &["target", "add", "--path", "resources/mako", "mako"],
        );
        for name in ["up", "keep"] {
            let source = project.source(name);
            let mut args = vec![
                "add",
                source.as_str(),
                "--as",
                name,
                "--tag",
                "v1",
                "--to",
                "mako",
            ];
            if history && name == "up" {
                args.push("--history");
            }
            args.push("-y");
            run_phora_ok(&project.root, &args);
        }
        run_phora_ok(&project.root, &["sync"]);
        assert!(project.deployed("up").exists(), "premise: up is deployed");
        project
    }

    fn source(&self, name: &str) -> String {
        self.root
            .parent()
            .expect("project parent")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn deployed(&self, name: &str) -> PathBuf {
        self.root.join("resources/mako").join(name)
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&run_phora(&self.root, args).stdout).into_owned()
    }

    fn registered_worktrees(&self) -> String {
        let mirrors = self.root.join("cache/phora/git");
        fs::read_dir(&mirrors)
            .expect("mirror directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "git"))
            .map(|mirror| worktree_list(&mirror))
            .collect()
    }

    fn remove_from_config(&self, name: &str) {
        let config = fs::read_to_string(self.root.join("phora.toml")).expect("config");
        let binding = format!("{name} = ");
        let mut kept = config
            .lines()
            .filter(|line| !line.starts_with(&binding))
            .collect::<Vec<_>>()
            .join("\n");
        kept.push('\n');
        assert_ne!(kept, config, "premise: the config binds {name} on one line");
        fs::write(self.root.join("phora.toml"), kept).expect("config");
    }
}

fn worktree_list(mirror: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["-C"])
        .arg(mirror)
        .args(["worktree", "list"])
        .output()
        .expect("git worktree list");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_only_keep_remains(project: &Project) {
    assert!(
        !project.deployed("up").exists(),
        "rm removes the deployed files"
    );
    assert!(
        project.deployed("keep").exists(),
        "rm leaves other sources deployed"
    );
    let listed = project.stdout(&["where"]);
    assert!(
        !listed.contains("up/"),
        "rm drops the record; where:\n{listed}"
    );
    assert!(
        listed.contains("keep/"),
        "rm keeps other records; where:\n{listed}"
    );
}

#[test]
fn rm_removes_a_history_deployment_its_record_and_its_worktree() {
    let project = Project::synced(true);
    let deployed = project.deployed("up");
    assert!(
        project
            .registered_worktrees()
            .contains(deployed.to_str().expect("path")),
        "premise: the history deployment is a registered worktree"
    );

    run_phora_ok(&project.root, &["rm", "up"]);

    assert_only_keep_remains(&project);
    assert!(
        !project
            .registered_worktrees()
            .contains(deployed.to_str().expect("path")),
        "rm unregisters the worktree; mirrors:\n{}",
        project.registered_worktrees()
    );
}

#[test]
fn rm_removes_a_copied_deployment_and_its_record() {
    let project = Project::synced(false);

    run_phora_ok(&project.root, &["rm", "up"]);

    assert_only_keep_remains(&project);
}

#[test]
fn rm_keeps_a_locally_modified_deployment() {
    let project = Project::synced(false);
    let edited = project.deployed("up").join("a.txt");
    fs::write(&edited, "mine\n").expect("edit the deployed copy");

    let stderr = run_phora_ok(&project.root, &["rm", "up"]);

    assert!(
        stderr.contains("phora: keeping up:up"),
        "the edit is reported; got:\n{stderr}"
    );
    assert_eq!(fs::read_to_string(&edited).expect("kept"), "mine\n");
    let orphans = project.stdout(&["list", "--orphans"]);
    assert!(
        orphans.contains("up/up"),
        "the kept record is an orphan; got:\n{orphans}"
    );
}

#[test]
fn sync_reports_the_deployment_of_a_source_removed_from_config() {
    let project = Project::synced(false);
    project.remove_from_config("up");

    let stderr = run_phora_ok(&project.root, &["sync"]);

    assert!(
        stderr.contains("1 orphaned record(s) no longer in config"),
        "a plain sync names the orphan count; got:\n{stderr}"
    );
    assert!(project.stdout(&["list", "--orphans"]).contains("up/up"));
    assert!(
        project.deployed("up").exists(),
        "a plain sync deletes nothing"
    );
}

#[test]
fn rebuild_registry_forgets_a_removed_source_whose_files_are_gone() {
    let project = Project::synced(true);
    project.remove_from_config("up");
    fs::rename(project.deployed("up"), project.root.join("trashed-up")).expect("move away");

    let rebuilt = project.stdout(&["rebuild-registry"]);

    assert!(rebuilt.contains("forgot [up/up]"), "got:\n{rebuilt}");
    assert!(!project.stdout(&["where"]).contains("up/"));
    assert!(
        project
            .stdout(&["list", "--orphans"])
            .contains("No orphaned records.")
    );
}
