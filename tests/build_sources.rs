use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
mod common;

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

const BUILD: &str = "printf 'build\\n' >> order && test ! -e fail-build && mkdir -p \"$PHORA_OUTPUT/claude\" && cp -R \"$PHORA_INPUT/tropos/skills\" \"$PHORA_OUTPUT/claude/\"";

impl Fixture {
    fn new() -> Self {
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
        write(&dependency.join("languages/rust/README.md"), "Loqui\n");
        git(&dependency, &["add", "."]);
        git(&dependency, &["commit", "-qm", "dependency"]);
        write(&package.join("skills/code/SKILL.md"), "skill v1\n");
        write(&package.join("skills/code/resource.md"), "resource\n");
        write(
            &package.join("phora.toml"),
            r#"
[sources.tropos]
path = "."
include = ["skills/**"]
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[targets.skills]
path = "."
sources.tropos = { collapse = false }
[targets.loqui]
path = "skills/loqui/reference/loqui"
sources.loqui = { collapse = false }
"#,
        );
        git(&package, &["add", "."]);
        git(&package, &["commit", "-qm", "package"]);
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
            project,
        };
        fixture.configure("branch = \"main\"", "");
        fixture
    }

    fn configure(&self, tropos: &str, build_extra: &str) {
        write(
            &self.project.join("phora.toml"),
            &format!(
                r#"
[sources.tropos]
path = {package:?}
{tropos}
transitive = true
[sources.henia]
build = {{ inputs = ["tropos"], run = {BUILD:?}{build_extra} }}
"#,
                package = self.package.display().to_string()
            ),
        );
        write(
            &self.project.join("phora.local.toml"),
            r#"
[paths]
cache = "cache"
state = "state"
[targets.claude]
path = "home/.claude"
sources.henia = { take = [{ "claude/" = "." }], collapse = false }
"#,
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(&self.project)
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

    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "phora {args:?} unexpectedly succeeded: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.project.join(path)).expect("read fixture")
    }

    fn builds(&self) -> usize {
        std::fs::read_to_string(self.project.join("order"))
            .map(|order| order.lines().count())
            .unwrap_or_default()
    }

    fn update_package(&self) {
        write(&self.package.join("skills/code/SKILL.md"), "skill v2\n");
        git(&self.package, &["add", "."]);
        git(&self.package, &["commit", "-qm", "update"]);
    }
}

#[test]
fn first_sync_builds_from_a_transitive_input_and_deploys_verified_copies() {
    let fixture = Fixture::new();
    let output = fixture.succeeds(&["sync", "--json"]);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
    assert_eq!(
        fixture.read("home/.claude/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui\n"
    );
    assert!(
        !fixture
            .project
            .join("home/.claude/skills/code/SKILL.md")
            .is_symlink()
    );
    assert_eq!(fixture.builds(), 1);
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .expect("json")
        .lines()
        .map(|line| serde_json::from_str(line).expect("event"))
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "phase_started" && e["phase"] == "build")
    );
    let lock = fixture.read("phora.lock");
    assert!(lock.contains("resolved = \"build\""), "{lock}");
    assert!(lock.contains("build = \"blake3:"), "{lock}");
    assert!(lock.contains("name = \"tropos\""), "{lock}");
    fixture.succeeds(&["verify"]);
}

#[test]
fn unchanged_inputs_skip_the_build() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1);
}

#[test]
fn updating_an_input_rebuilds_and_frozen_replays_without_building() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    fixture.update_package();
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1, "a plain sync keeps the locked input");
    fixture.succeeds(&["update", "tropos"]);
    assert_eq!(fixture.builds(), 2);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v2\n"
    );
    std::fs::remove_dir_all(&fixture.package).expect("remove origin");
    std::fs::remove_dir_all(fixture.root.path().join("loqui")).expect("remove dependency");
    fixture.succeeds(&["sync", "--frozen"]);
    assert_eq!(fixture.builds(), 2);
}

#[test]
fn updating_the_build_source_forces_a_rebuild() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    fixture.succeeds(&["update", "henia"]);
    assert_eq!(fixture.builds(), 2);
}

#[test]
fn a_changed_key_rebuilds_but_frozen_refuses() {
    let fixture = Fixture::new();
    fixture.configure("branch = \"main\"", ", key = \"cat key\"");
    write(&fixture.project.join("key"), "1\n");
    fixture.succeeds(&["sync"]);
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1);
    write(&fixture.project.join("key"), "2\n");
    let stderr = fixture.fails(&["sync", "--frozen"]);
    assert!(
        stderr.contains("--frozen refuses to run a build"),
        "{stderr}"
    );
    assert_eq!(fixture.builds(), 1);
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 2);
}

#[test]
fn a_failed_rebuild_keeps_the_previous_output_and_retries() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    let henia = |lock: &str| {
        lock.split("[[sources]]")
            .find(|entry| entry.contains("name = \"henia\""))
            .expect("henia entry")
            .to_owned()
    };
    let before = henia(&fixture.read("phora.lock"));
    fixture.update_package();
    write(&fixture.project.join("fail-build"), "");
    let stderr = fixture.fails(&["update", "tropos"]);
    assert!(stderr.contains("build `henia` failed"), "{stderr}");
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
    assert_eq!(henia(&fixture.read("phora.lock")), before);
    std::fs::remove_file(fixture.project.join("fail-build")).expect("unfail");
    fixture.succeeds(&["sync"]);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v2\n"
    );
}

#[test]
fn a_failed_first_build_deploys_nothing() {
    let fixture = Fixture::new();
    write(&fixture.project.join("fail-build"), "");
    let stderr = fixture.fails(&["sync"]);
    assert!(stderr.contains("build `henia` failed"), "{stderr}");
    assert!(!fixture.project.join("home").exists());
}

#[test]
fn an_uncommitted_edit_to_a_linked_input_rebuilds() {
    let fixture = Fixture::new();
    fixture.configure("deploy = \"link\"", "");
    fixture.succeeds(&["sync"]);
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1);
    write(
        &fixture.package.join("skills/code/SKILL.md"),
        "skill edit\n",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 2);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill edit\n"
    );
}

#[test]
fn invalid_build_configurations_are_rejected() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.project.join("phora.local.toml")).expect("drop local config");
    let cases = [
        (
            "[sources.x]\nbuild = { inputs = [\"y\"], run = \"true\" }\n",
            "references undefined source `y`",
        ),
        (
            "[sources.x]\nbuild = { inputs = [\"x\"], run = \"true\" }\n",
            "cannot name the build source itself",
        ),
        (
            "[sources.y]\nbuild = { inputs = [\"z\"], run = \"true\" }\n[sources.z]\npath = \".\"\n[sources.x]\nbuild = { inputs = [\"y\"], run = \"true\" }\n",
            "names build source `y`",
        ),
        (
            "[sources.z]\npath = \".\"\n[sources.x]\nbranch = \"main\"\nbuild = { inputs = [\"z\"], run = \"true\" }\n",
            "`branch` is meaningless on a `build` source",
        ),
        (
            "[sources.z]\npath = \".\"\n[sources.x]\nbuild = { inputs = [\"z\"] }\n",
            "must set `run` (shell) or `cmd` (exec)",
        ),
        (
            "[sources.z]\npath = \".\"\n[sources.x]\nbuild = { inputs = [\"z\"], run = \"true\" }\n[targets.t]\npath = \"t\"\nsources.x = { history = true }\n",
            "`history` cannot be combined with `build`",
        ),
        (
            "[hooks]\npost_prepare = \"true\"\n",
            "unknown field `post_prepare`",
        ),
        (
            "[targets.t]\npath = \"t\"\nphase = \"prepare\"\n",
            "unknown field `phase`",
        ),
    ];
    for (config, expected) in cases {
        write(&fixture.project.join("phora.toml"), config);
        let stderr = fixture.fails(&["sync"]);
        assert!(stderr.contains(expected), "{config}\n{stderr}");
    }
}

#[test]
fn replacing_linked_output_shared_by_two_targets_with_a_build_syncs() {
    let fixture = Fixture::new();
    write(
        &fixture.project.join("out/claude/skills/code/SKILL.md"),
        "linked\n",
    );
    write(
        &fixture.project.join("phora.toml"),
        "[sources.henia]\npath = \"./out\"\ndeploy = \"link\"\n",
    );
    let local = r#"
[paths]
cache = "cache"
state = "state"
[targets.claude]
path = "home/.claude"
sources.henia = { take = [{ "claude/" = "." }], collapse = false }
[targets.bin]
path = "home/bin"
sources.henia = { take = [{ "claude/skills/code/SKILL.md" = "skill" }] }
"#;
    write(&fixture.project.join("phora.local.toml"), local);
    fixture.succeeds(&["sync"]);
    assert!(fixture.project.join("home/bin/skill").is_symlink());

    fixture.configure("branch = \"main\"", "");
    write(&fixture.project.join("phora.local.toml"), local);
    fixture.succeeds(&["sync", "--force"]);
    assert!(!fixture.project.join("home/bin/skill").is_symlink());
    assert_eq!(fixture.read("home/bin/skill"), "skill v1\n");
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
}
