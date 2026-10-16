#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
mod common;

const PACKAGE_MANIFEST: &str = "[offers.fas]\nroot = \"rules/fas\"\n";

struct Fixture {
    root: TempDir,
    package: PathBuf,
    project: PathBuf,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn git(path: &Path, args: &[&str]) {
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
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("sandbox");
        let package = root.path().join("tropos");
        let project = root.path().join("consumer");
        std::fs::create_dir_all(&project).expect("mkdir");
        write(&package.join("phora.toml"), PACKAGE_MANIFEST);
        write(&package.join("rules/fas/a.md"), "a\n");
        write(&package.join("rules/fas/b.md"), "b\n");
        write(&package.join("rules/fas/guidance/hints.cue"), "hints\n");
        write(&package.join("rules/fas/guidance/more.cue"), "more\n");
        git(&package, &["init", "-q", "-b", "main", "--template="]);
        git(&package, &["add", "phora.toml", "rules"]);
        git(&package, &["commit", "-qm", "package"]);
        write(
            &project.join("phora.toml"),
            &format!(
                "[paths]\ncache = \"cache\"\nstate = \"state\"\n\
                 [sources.tropos-fas]\npath = {:?}\ntransitive = true\ndeploy = \"link\"\n\
                 [targets.fas-rules]\npath = \"fas\"\nsources.tropos-fas = {{ offer = \"fas\" }}\n",
                package.display().to_string()
            ),
        );
        let fixture = Self {
            root,
            package,
            project,
        };
        fixture.succeeds(&["sync"]);
        fixture
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", self.root.path().join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

    fn deployed(&self, path: &str) -> PathBuf {
        self.project.join("fas").join(path)
    }

    fn assert_absent(&self, path: &str) {
        let error = std::fs::symlink_metadata(self.deployed(path))
            .expect_err("the dropped artifact must leave the target");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{path}");
    }

    fn assert_linked(&self, path: &str) {
        assert!(
            std::fs::symlink_metadata(self.deployed(path))
                .unwrap_or_else(|e| panic!("{path}: {e}"))
                .is_symlink(),
            "{path} stays a live link"
        );
    }

    fn records(&self) -> Vec<String> {
        let mut out = Vec::new();
        collect_files(&self.project.join("state"), &mut out);
        out.into_iter()
            .filter(|path| path.contains("/artifacts/tropos-fas/"))
            .map(|path| {
                path.rsplit_once("/artifacts/tropos-fas/")
                    .expect("record path")
                    .1
                    .to_owned()
            })
            .collect()
    }

    fn target_id(&self) -> String {
        let out = self.succeeds(&["preview"]);
        let text = String::from_utf8(out.stdout).expect("utf-8 preview");
        text.lines()
            .find_map(|line| line.strip_suffix(" -> fas"))
            .unwrap_or_else(|| panic!("preview names the composed target:\n{text}"))
            .to_owned()
    }
}

fn collect_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path.display().to_string());
        }
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn sync_removes_a_file_dropped_from_a_linked_offer() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.package.join("rules/fas/b.md")).expect("drop upstream");

    let out = fixture.succeeds(&["sync"]);

    fixture.assert_absent("b.md");
    fixture.assert_linked("a.md");
    assert!(
        !fixture.records().iter().any(|r| r == "b.md.toml"),
        "the dropped record is forgotten: {:?}",
        fixture.records()
    );
    assert!(
        stderr(&out).contains("tropos-fas:b.md"),
        "sync reports the removal: {}",
        stderr(&out)
    );
}

#[test]
fn sync_removes_a_directory_dropped_from_a_linked_offer() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(fixture.package.join("rules/fas/guidance")).expect("drop upstream");

    fixture.succeeds(&["sync"]);

    fixture.assert_absent("guidance/hints.cue");
    fixture.assert_absent("guidance/more.cue");
    fixture.assert_absent("guidance");
    fixture.assert_linked("a.md");
    fixture.assert_linked("b.md");
    let mut records = fixture.records();
    records.sort();
    assert_eq!(records, ["a.md.toml", "b.md.toml"]);
}

#[test]
fn sync_removes_a_live_link_the_linked_offer_no_longer_includes() {
    let fixture = Fixture::new();
    write(
        &fixture.package.join("phora.toml"),
        &format!("{PACKAGE_MANIFEST}exclude = [\"b.md\"]\n"),
    );

    fixture.succeeds(&["sync"]);

    fixture.assert_absent("b.md");
    fixture.assert_linked("a.md");
    assert_eq!(
        std::fs::read_to_string(fixture.package.join("rules/fas/b.md")).expect("source kept"),
        "b\n"
    );
}

#[test]
fn sync_refuses_a_dropped_artifact_replaced_by_a_regular_file() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.package.join("rules/fas/b.md")).expect("drop upstream");
    std::fs::remove_file(fixture.deployed("b.md")).expect("unlink");
    write(&fixture.deployed("b.md"), "user edit\n");

    let out = fixture.run(&["sync"]);

    assert!(!out.status.success(), "sync must refuse");
    assert!(
        stderr(&out).contains("a recorded artifact is no longer in the source's offer"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.deployed("b.md")).expect("user file kept"),
        "user edit\n"
    );
}

#[test]
fn eject_and_explain_address_a_transitive_target_by_its_printed_id() {
    let fixture = Fixture::new();
    let target = fixture.target_id();
    std::fs::remove_file(fixture.package.join("rules/fas/b.md")).expect("drop upstream");
    std::fs::remove_file(fixture.deployed("b.md")).expect("unlink");
    write(&fixture.deployed("b.md"), "user edit\n");
    let refused = fixture.run(&["sync"]);
    assert!(
        stderr(&refused).contains(&format!("phora explain {target} tropos-fas b.md")),
        "{}",
        stderr(&refused)
    );

    fixture.succeeds(&["explain", &target, "tropos-fas", "a.md"]);
    fixture.succeeds(&[
        "eject",
        "--source",
        "tropos-fas",
        "--target",
        &target,
        "b.md",
    ]);
    fixture.succeeds(&["sync"]);

    assert_eq!(
        std::fs::read_to_string(fixture.deployed("b.md")).expect("ejected file kept"),
        "user edit\n"
    );
    fixture.succeeds(&[
        "uneject",
        "--source",
        "tropos-fas",
        "--target",
        &target,
        "b.md",
    ]);
}
