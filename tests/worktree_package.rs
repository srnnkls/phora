#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
mod common;

const PACKAGE_MANIFEST: &str = r#"
[sources.tropos]
path = "."
include = ["skills/**"]
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[targets.tropos]
path = "."
sources.tropos = { collapse = false }
[targets.loqui]
path = "skills/loqui/reference/loqui"
sources.loqui = { collapse = false }
"#;

const LOCAL: &str = r#"
[paths]
cache = "cache"
state = "state"
[targets.tropos]
phase = "prepare"
path = ".tropos"
imports = ["tropos"]
"#;

struct Fixture {
    root: TempDir,
    package: PathBuf,
    dependency: PathBuf,
    project: PathBuf,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn git_output(path: &Path, args: &[&str]) -> String {
    common::assert_sandboxed(path);
    let out = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 git output")
}

fn git(path: &Path, args: &[&str]) {
    git_output(path, args);
}

fn commit_all(path: &Path, message: &str) {
    git(path, &["add", "."]);
    git(path, &["commit", "-qm", message]);
}

impl Fixture {
    fn new(manifest: &str) -> Self {
        let root = tempfile::tempdir().expect("sandbox");
        let package = root.path().join("tropos");
        let dependency = root.path().join("loqui");
        let project = root.path().join("consumer");
        for path in [&package, &dependency, &project] {
            std::fs::create_dir_all(path).expect("mkdir");
        }
        for path in [&package, &dependency] {
            git(path, &["init", "-q", "-b", "main", "--template="]);
        }
        write(&dependency.join("languages/rust/README.md"), "Loqui v1\n");
        commit_all(&dependency, "dependency");
        write(&package.join("skills/code/SKILL.md"), "skill v1\n");
        write(&package.join("phora.toml"), manifest);
        commit_all(&package, "package");
        write(
            &root.path().join("gitconfig"),
            &format!(
                "[url {:?}]\n\tinsteadOf = https://example.invalid/loqui.git\n",
                dependency.display().to_string()
            ),
        );
        let fixture = Self {
            root,
            package,
            dependency,
            project,
        };
        fixture.configure(
            &format!(
                "[sources.tropos]\npath = {:?}\ntransitive = true\ndeploy = \"link\"\n",
                fixture.package.display().to_string()
            ),
            LOCAL,
        );
        fixture
    }

    fn configure(&self, base: &str, local: &str) {
        write(&self.project.join("phora.toml"), base);
        write(&self.project.join("phora.local.toml"), local);
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", self.root.path().join("home"))
            .env("GIT_CONFIG_GLOBAL", self.root.path().join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("phora")
    }

    fn succeeds(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "phora {args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.project.join(path))
            .unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    fn lock(&self) -> phora::lock::Lock {
        toml::from_str(&self.read("phora.lock")).expect("lock")
    }

    fn package_status(&self) -> String {
        git_output(
            &self.package,
            &["status", "--porcelain", "--untracked-files=all"],
        )
    }

    fn loqui_commit(&self) -> String {
        self.lock()
            .sources
            .iter()
            .find(|s| s.name.ends_with("%loqui"))
            .expect("the package's remote dependency is locked")
            .commit
            .clone()
    }
}

#[test]
fn linked_package_deploys_uncommitted_files_without_touching_its_worktree() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    write(
        &fixture.package.join("skills/code/SKILL.md"),
        "skill edit\n",
    );
    write(&fixture.package.join("skills/new/SKILL.md"), "untracked\n");
    let status_before = fixture.package_status();

    fixture.succeeds(&["sync"]);

    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "skill edit\n");
    assert_eq!(fixture.read(".tropos/skills/new/SKILL.md"), "untracked\n");
    assert_eq!(
        fixture.read(".tropos/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui v1\n"
    );
    assert_eq!(
        fixture.package_status(),
        status_before,
        "sync must not write into the linked package's worktree"
    );
    let lock = fixture.lock();
    let package = lock.find_source("tropos").expect("package pin");
    assert_eq!(package.resolved, "link");
    assert_eq!(package.digest, "link:");

    write(&fixture.package.join("skills/new/SKILL.md"), "live\n");
    assert_eq!(fixture.read(".tropos/skills/new/SKILL.md"), "live\n");

    std::fs::remove_file(fixture.package.join("skills/new/SKILL.md")).expect("remove");
    fixture.succeeds(&["sync", "--prune"]);
    assert!(
        std::fs::symlink_metadata(fixture.project.join(".tropos/skills/new/SKILL.md")).is_err(),
        "a file removed from the package worktree is pruned from the prepared tree"
    );
    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "skill edit\n");
}

#[test]
fn linked_package_keeps_its_remote_dependency_at_the_locked_commit() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    fixture.succeeds(&["sync"]);
    let pinned = fixture.loqui_commit();

    write(
        &fixture.dependency.join("languages/rust/README.md"),
        "Loqui v2\n",
    );
    commit_all(&fixture.dependency, "dependency v2");
    write(&fixture.package.join("skills/code/SKILL.md"), "skill v2\n");

    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "skill v2\n");
    assert_eq!(
        fixture.read(".tropos/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui v1\n"
    );
    assert_eq!(fixture.loqui_commit(), pinned);

    fixture.succeeds(&["sync", "--frozen"]);
    assert_eq!(fixture.loqui_commit(), pinned);

    fixture.succeeds(&["update", "--fast-forward"]);
    assert_eq!(
        fixture.read(".tropos/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui v2\n"
    );
    assert_ne!(fixture.loqui_commit(), pinned);
    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "skill v2\n");
    assert_eq!(
        fixture.lock().find_source("tropos").expect("pin").resolved,
        "link"
    );
}

#[test]
fn link_declared_inside_the_package_manifest_stays_rejected() {
    let manifest =
        PACKAGE_MANIFEST.replacen("path = \".\"\n", "path = \".\"\ndeploy = \"link\"\n", 1);
    let fixture = Fixture::new(&manifest);
    let out = fixture.run(&["sync"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("transitive source cannot use deploy = \"link\""),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!fixture.project.join(".tropos").exists());
}

#[test]
fn local_link_override_of_a_branch_pinned_package_syncs_the_worktree() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    fixture.configure(
        "[sources.tropos]\ngit = \"https://example.invalid/tropos.git\"\nbranch = \"main\"\ntransitive = true\n",
        &format!(
            "{LOCAL}[sources.tropos]\npath = {:?}\ndeploy = \"link\"\n",
            fixture.package.display().to_string()
        ),
    );
    write(&fixture.package.join("skills/code/SKILL.md"), "worktree\n");

    fixture.succeeds(&["sync"]);

    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "worktree\n");
    assert_eq!(
        fixture.read(".tropos/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui v1\n"
    );
    assert_eq!(fixture.package_status(), " M skills/code/SKILL.md\n");
}
