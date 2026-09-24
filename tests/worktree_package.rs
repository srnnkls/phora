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
        "a file removed from the package worktree is pruned from the deployed tree"
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

const BRANCH_PINNED: &str = "[sources.tropos]\ngit = \"https://example.invalid/tropos.git\"\nbranch = \"main\"\ntransitive = true\n";

impl Fixture {
    fn add_worktree(&self) -> PathBuf {
        let worktree = self.root.path().join("tropos-live");
        git(
            &self.package,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "live",
                &worktree.display().to_string(),
            ],
        );
        worktree
    }

    fn pin(&self) {
        self.pin_with("");
    }

    fn pin_with(&self, consumer: &str) {
        self.configure(
            &format!("{BRANCH_PINNED}{consumer}"),
            &format!(
                "{LOCAL}[sources.tropos]\npath = {:?}\n",
                self.package.display().to_string()
            ),
        );
    }

    fn link(&self, worktree: &Path) {
        self.link_with(worktree, "");
    }

    fn link_with(&self, worktree: &Path, consumer: &str) {
        self.configure(
            &format!("{BRANCH_PINNED}{consumer}"),
            &format!(
                "{LOCAL}[sources.tropos]\npath = {:?}\ndeploy = \"link\"\n",
                worktree.display().to_string()
            ),
        );
    }

    fn is_link(&self, path: &str) -> bool {
        std::fs::symlink_metadata(self.project.join(path))
            .unwrap_or_else(|e| panic!("stat {path}: {e}"))
            .file_type()
            .is_symlink()
    }

    fn state_text(&self) -> String {
        let mut text = String::new();
        let mut stack = vec![self.project.join("state")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("state dir").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(contents) = std::fs::read_to_string(&path) {
                    text.push_str(&path.display().to_string());
                    text.push_str(&contents);
                }
            }
        }
        text
    }

    fn sync_prune(&self) -> String {
        let out = self.succeeds(&["sync", "--prune"]);
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

fn assert_quiet(stderr: &str) {
    for needle in ["refusing", "foreign content", "confine anchor", "/./"] {
        assert!(!stderr.contains(needle), "unexpected `{needle}`:\n{stderr}");
    }
}

#[test]
fn switching_between_pin_and_worktree_reconciles_in_one_prune() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    let worktree = fixture.add_worktree();
    write(&worktree.join("skills/live/SKILL.md"), "live only\n");
    let package_before = fixture.package_status();
    let worktree_before = git_output(
        &worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    );

    fixture.pin();
    assert_quiet(&fixture.sync_prune());
    assert!(!fixture.is_link(".tropos/skills/code/SKILL.md"));
    write(&fixture.project.join(".tropos/skills/foreign.md"), "mine\n");

    fixture.link(&worktree);
    assert_quiet(&fixture.sync_prune());
    assert!(fixture.is_link(".tropos/skills/code/SKILL.md"));
    assert_eq!(fixture.read(".tropos/skills/live/SKILL.md"), "live only\n");
    assert_eq!(
        fixture.read(".tropos/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui v1\n"
    );

    fixture.pin();
    assert_quiet(&fixture.sync_prune());
    assert!(!fixture.is_link(".tropos/skills/code/SKILL.md"));
    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "skill v1\n");
    assert!(
        std::fs::symlink_metadata(fixture.project.join(".tropos/skills/live/SKILL.md")).is_err(),
        "the worktree-only artifact is pruned once the package is pinned again"
    );
    assert_eq!(fixture.read(".tropos/skills/foreign.md"), "mine\n");
    assert_eq!(fixture.package_status(), package_before);
    assert_eq!(
        git_output(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        worktree_before
    );
}

#[test]
fn fresh_worktree_sync_prunes_stale_pinned_records() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    let worktree = fixture.add_worktree();
    std::fs::remove_file(worktree.join("skills/code/SKILL.md")).expect("remove");
    write(&worktree.join("skills/live/SKILL.md"), "live only\n");
    let without_loqui = PACKAGE_MANIFEST
        .split("[targets.loqui]")
        .next()
        .expect("manifest head");
    write(&worktree.join("phora.toml"), without_loqui);
    let worktree_before = git_output(
        &worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    );

    fixture.pin();
    fixture.succeeds(&["sync"]);
    std::fs::remove_dir_all(fixture.project.join(".tropos")).expect("clear");
    write(
        &fixture.project.join(".tropos/skills/loqui/notes.md"),
        "mine\n",
    );

    fixture.link(&worktree);
    assert_quiet(&fixture.sync_prune());
    assert!(fixture.is_link(".tropos/skills/live/SKILL.md"));
    assert!(std::fs::symlink_metadata(fixture.project.join(".tropos/skills/code")).is_err());
    assert_eq!(fixture.read(".tropos/skills/loqui/notes.md"), "mine\n");
    assert!(
        !fixture.state_text().contains("%loqui"),
        "records of the dropped dep target are pruned"
    );
    assert_quiet(&fixture.sync_prune());
    assert_eq!(
        git_output(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        worktree_before
    );
}

#[test]
fn self_target_records_carry_a_normalized_deploy_root() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    fixture.pin();
    let out = fixture.succeeds(&["sync"]);
    assert_quiet(&String::from_utf8_lossy(&out.stderr));
    let state = fixture.state_text();
    assert!(!state.contains("/./"), "{state}");
}

const PATHS_ONLY: &str = "[paths]\ncache = \"cache\"\nstate = \"state\"\n";

fn linked_source(allow_symlinks: bool) -> String {
    format!(
        "[sources.fas]\npath = \"./linked\"\nroot = \"rules\"\ndeploy = \"link\"\n\
         allow_symlinks = {allow_symlinks}\n\
         [targets.out]\npath = \"out\"\nsources.fas = {{ collapse = false }}\n"
    )
}

fn plant_linked_tree(fixture: &Fixture) {
    let shared = fixture.root.path().join("shared");
    write(&shared.join("guidance/hints.cue"), "hints\n");
    write(&shared.join("top.md"), "top\n");
    let rules = fixture.project.join("linked/rules");
    std::fs::create_dir_all(&rules).expect("mkdir");
    std::os::unix::fs::symlink(shared.join("guidance"), rules.join("guidance")).expect("dir link");
    std::os::unix::fs::symlink(shared.join("top.md"), rules.join("top.md")).expect("file link");
    write(&rules.join("own.md"), "own\n");
}

#[test]
fn linked_source_allowing_symlinks_offers_what_they_point_to() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    plant_linked_tree(&fixture);
    fixture.configure(&linked_source(true), PATHS_ONLY);

    fixture.succeeds(&["sync"]);

    for (path, text) in [
        ("out/guidance/hints.cue", "hints\n"),
        ("out/top.md", "top\n"),
        ("out/own.md", "own\n"),
    ] {
        assert!(fixture.is_link(path), "{path} deploys as a link");
        assert_eq!(fixture.read(path), text);
    }
    assert_eq!(
        std::fs::read_link(fixture.project.join("out/guidance/hints.cue"))
            .expect("read link")
            .strip_prefix(fixture.project.canonicalize().expect("project"))
            .expect("link stays inside the source"),
        Path::new("linked/rules/guidance/hints.cue"),
        "the deploy links to the logical path inside the source"
    );

    write(
        &fixture.root.path().join("shared/guidance/hints.cue"),
        "edited\n",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.read("out/guidance/hints.cue"), "edited\n");
}

#[test]
fn linked_source_without_allow_symlinks_skips_them() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    plant_linked_tree(&fixture);
    fixture.configure(&linked_source(false), PATHS_ONLY);

    fixture.succeeds(&["sync"]);

    assert_eq!(fixture.read("out/own.md"), "own\n");
    for path in ["out/guidance", "out/top.md"] {
        assert!(
            std::fs::symlink_metadata(fixture.project.join(path)).is_err(),
            "{path} is not offered"
        );
    }
}

#[test]
fn removing_the_importing_target_prunes_its_composed_records() {
    let fixture = Fixture::new(PACKAGE_MANIFEST);
    fixture.pin();
    assert_quiet(&fixture.sync_prune());
    assert_eq!(fixture.read(".tropos/skills/code/SKILL.md"), "skill v1\n");

    fixture.configure("", "[paths]\ncache = \"cache\"\nstate = \"state\"\n");
    assert_quiet(&fixture.sync_prune());

    for path in [
        ".tropos/skills/code/SKILL.md",
        ".tropos/skills/loqui/reference/loqui/languages/rust/README.md",
    ] {
        assert!(
            std::fs::symlink_metadata(fixture.project.join(path)).is_err(),
            "{path} is pruned with the target that imported it"
        );
    }
    let orphans = fixture.succeeds(&["list", "--orphans"]);
    assert!(
        !String::from_utf8_lossy(&orphans.stdout).contains("tropos"),
        "no composed record outlives its anchor:\n{}",
        String::from_utf8_lossy(&orphans.stdout)
    );
}
