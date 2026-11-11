#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

mod common;

use common::{run_git, run_phora, run_phora_ok};

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
}

impl Project {
    /// Source `a` with artifacts `a1` and `a2`, added to target `t` but not yet synced.
    fn added(deploy: &str) -> Self {
        let dir = tempfile::tempdir().expect("project directory");
        let base = dir.path().canonicalize().expect("canonical base");
        let source = base.join("sources/a");
        for artifact in ["a1", "a2"] {
            fs::create_dir_all(source.join(artifact)).expect("artifact");
            fs::write(source.join(artifact).join("f.txt"), format!("{artifact}\n")).expect("file");
        }
        run_git(&source, &["init", "-b", "main", "--template="]);
        run_git(&source, &["add", "a1", "a2"]);
        run_git(&source, &["commit", "-m", "r0"]);
        let root = base.join("proj");
        fs::create_dir_all(&root).expect("project root");
        fs::write(root.join("phora.toml"), "version = 1\n").expect("config");
        run_phora_ok(&root, &["target", "add", "--path", "dest", "t"]);
        let path = source.to_str().expect("source path");
        run_phora_ok(&root, &["add", path, "--branch", "main", "--to", "t", "-y"]);
        let config = fs::read_to_string(root.join("phora.toml")).expect("config");
        fs::write(
            root.join("phora.toml"),
            config.replace(
                "[sources.a]\n",
                &format!("[sources.a]\ndeploy = {deploy:?}\n"),
            ),
        )
        .expect("config");
        Self {
            _dir: dir,
            root,
            source,
        }
    }

    fn narrow_to_a1(&self) {
        let config = fs::read_to_string(self.root.join("phora.toml")).expect("config");
        fs::write(
            self.root.join("phora.toml"),
            config.replace("[sources.a]\n", "[sources.a]\ninclude = [\"a1/**\"]\n"),
        )
        .expect("config");
    }
}

#[test]
fn rebuild_registry_skips_a_source_no_sync_has_locked() {
    let project = Project::added("copy");
    fs::write(project.root.join("phora.lock"), "version = 1\n").expect("empty lock");

    let output = run_phora(&project.root, &["rebuild-registry"]);

    assert!(
        output.status.success(),
        "an unlocked source has deployed nothing to rebuild; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rebuild_registry_keeps_a_link_deployment_linked() {
    let project = Project::added("link");
    run_phora_ok(&project.root, &["sync"]);
    run_phora_ok(&project.root, &["rebuild-registry"]);
    project.narrow_to_a1();

    run_phora_ok(&project.root, &["sync"]);

    assert!(
        fs::symlink_metadata(project.root.join("dest/a2")).is_err(),
        "the rebuilt link record still mirrors its live source, so narrowing unlinks a2"
    );
    assert_eq!(
        fs::read_link(project.root.join("dest/a1")).expect("a1 stays linked"),
        project.source.join("a1")
    );
}

#[test]
fn rebuild_registry_keeps_a_local_edit_visible_to_removal() {
    let project = Project::added("copy");
    run_phora_ok(&project.root, &["sync"]);
    let edited = project.root.join("dest/a1/f.txt");
    fs::write(&edited, "mine\n").expect("local edit");
    run_phora_ok(&project.root, &["rebuild-registry"]);

    run_phora_ok(&project.root, &["rm", "a"]);

    assert_eq!(
        fs::read_to_string(&edited).expect("the edit survives rm"),
        "mine\n",
        "a rebuilt record must not vouch for an edited file"
    );
}
