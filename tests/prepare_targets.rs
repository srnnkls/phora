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

const BUILD: &str = "test \"$PHORA_TARGETS\" = \"claude tropos\" && test ! -e fail-build && mkdir -p .henia/claude && rm -rf .henia/claude/skills && cp -R stage/skills .henia/claude/ && printf 'build\\n' >> order";

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
        fixture.configure(BUILD, "stage");
        fixture
    }

    fn configure(&self, build: &str, stage: &str) {
        write(
            &self.project.join("phora.toml"),
            &format!(
                r#"
[hooks]
pre_sync = "printf 'pre\\n' >> order"
post_prepare = {build:?}
post_sync = "test -f home/.claude/skills/code/SKILL.md && printf 'smoke\\n' >> order"
[sources.tropos]
path = {package:?}
branch = "main"
transitive = true
[sources.claude]
path = "./.henia/claude"
deploy = "link"
"#,
                package = self.package.display().to_string()
            ),
        );
        write(
            &self.project.join("phora.local.toml"),
            &format!(
                r#"
[paths]
cache = "cache"
state = "state"
[targets.tropos]
phase = "prepare"
path = {stage:?}
imports = ["tropos"]
[targets.claude]
path = "home/.claude"
sources.claude = {{ collapse = false }}
"#
            ),
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

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.project.join(path)).expect("read fixture")
    }

    fn update_package(&self) {
        write(&self.package.join("skills/code/SKILL.md"), "skill v2\n");
        std::fs::remove_file(self.package.join("skills/code/resource.md")).expect("remove");
        git(&self.package, &["add", "."]);
        git(&self.package, &["commit", "-qm", "update"]);
    }
}

#[test]
fn first_sync_prepares_transitive_inputs_builds_then_deploys_from_one_config_pair() {
    let fixture = Fixture::new();
    assert!(!fixture.project.join(".henia").exists());
    let output = fixture.succeeds(&["sync", "--json"]);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
    assert_eq!(
        fixture.read("home/.claude/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui\n"
    );
    assert_eq!(fixture.read("order"), "pre\nbuild\nsmoke\n");
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .expect("json")
        .lines()
        .map(|line| serde_json::from_str(line).expect("event"))
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "summary")
            .count(),
        1
    );
    assert!(events.iter().any(|event| event["scope"] == "post_prepare"));
    fixture.succeeds(&["sync", "--prune"]);
    assert_eq!(
        fixture.read("order"),
        "pre\nbuild\nsmoke\npre\nbuild\nsmoke\n"
    );
    assert_eq!(fixture.read("stage/skills/code/SKILL.md"), "skill v1\n");
    std::fs::remove_dir_all(&fixture.package).expect("remove origin");
    std::fs::remove_dir_all(fixture.root.path().join("loqui")).expect("remove dependency");
    fixture.succeeds(&["sync", "--frozen", "--no-hooks", "--prune"]);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
}

#[test]
fn sync_recovers_deleted_cache_and_outputs_without_advancing_package_pins() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    let lock_before = fixture.read("phora.lock");
    fixture.update_package();
    let dependency = fixture.root.path().join("loqui");
    write(&dependency.join("languages/rust/README.md"), "Loqui v2\n");
    git(&dependency, &["add", "."]);
    git(&dependency, &["commit", "-qm", "new dependency"]);
    for directory in ["cache", "state", "stage", ".henia", "home"] {
        std::fs::remove_dir_all(fixture.project.join(directory)).expect("remove generated tree");
    }

    let frozen = fixture.run(&["sync", "--frozen", "--no-hooks"]);
    assert!(
        !frozen.status.success(),
        "frozen sync must not fetch missing mirrors"
    );
    assert_eq!(fixture.read("phora.lock"), lock_before);
    assert!(!fixture.project.join("stage").exists());
    assert!(!fixture.project.join(".henia").exists());

    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.read("stage/skills/code/SKILL.md"), "skill v1\n");
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
    assert_eq!(
        fixture.read("home/.claude/skills/loqui/reference/loqui/languages/rust/README.md"),
        "Loqui\n"
    );
    assert_eq!(fixture.read("phora.lock"), lock_before);
    fixture.succeeds(&["verify"]);
}

#[test]
fn failed_build_keeps_deployment_and_its_pin_then_retry_uses_prepared_inputs() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    let before: phora::lock::Lock = toml::from_str(&fixture.read("phora.lock")).expect("lock");
    fixture.update_package();
    write(&fixture.project.join("fail-build"), "fail");
    let out = fixture.run(&["update", "--fast-forward"]);
    assert!(!out.status.success());
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
    assert_eq!(fixture.read("stage/skills/code/SKILL.md"), "skill v2\n");
    assert_eq!(fixture.read("order"), "pre\nbuild\nsmoke\npre\n");
    let after: phora::lock::Lock = toml::from_str(&fixture.read("phora.lock")).expect("lock");
    assert_eq!(
        after.find_source("claude").expect("output pin").digest,
        before.find_source("claude").expect("old output pin").digest
    );
    assert_ne!(
        after.find_source("tropos").expect("prepared pin").commit,
        before.find_source("tropos").expect("old input pin").commit
    );
    std::fs::remove_file(fixture.project.join("fail-build")).expect("retry");
    write(&fixture.project.join("home/.claude/personal.md"), "keep\n");
    fixture.succeeds(&["sync", "--prune"]);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v2\n"
    );
    assert!(
        !fixture
            .project
            .join("home/.claude/skills/code/resource.md")
            .is_symlink()
    );
    assert_eq!(fixture.read("home/.claude/personal.md"), "keep\n");
}

#[test]
fn preparation_destination_cannot_overlap_deployment() {
    let fixture = Fixture::new();
    fixture.configure(BUILD, "home/.claude/inputs");
    let out = fixture.run(&["sync"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("prepare and deploy targets"));
    assert!(!fixture.project.join("home").exists());
    assert!(!fixture.project.join(".henia").exists());
}

#[test]
fn preparation_conflict_stops_build_and_deployment() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    write(
        &fixture.project.join("stage/skills/code/SKILL.md"),
        "local input edit\n",
    );
    fixture.update_package();
    let out = fixture.run(&["update", "--fast-forward"]);
    assert!(!out.status.success());
    assert_eq!(fixture.read("order"), "pre\nbuild\nsmoke\npre\n");
    assert_eq!(
        fixture.read("stage/skills/code/SKILL.md"),
        "local input edit\n"
    );
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v1\n"
    );
}

#[test]
fn preparation_hooks_stop_at_first_failure_and_no_hooks_suppresses_them() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    let config = fixture.read("phora.toml");
    let config = config
        .lines()
        .map(|line| {
            if line.starts_with("post_prepare =") {
                "post_prepare = [\"false\", \"touch unexpected\"]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    write(&fixture.project.join("phora.toml"), &config);
    assert!(!fixture.run(&["sync"]).status.success());
    assert!(!fixture.project.join("unexpected").exists());
    assert_eq!(fixture.read("order"), "pre\nbuild\nsmoke\npre\n");
    fixture.succeeds(&["sync", "--frozen", "--no-hooks"]);
    assert_eq!(fixture.read("order"), "pre\nbuild\nsmoke\npre\n");
}

#[test]
fn prepare_phase_survives_local_path_override_and_rejects_unknown_phases() {
    let base = phora::config::Config::parse("[targets.input]\npath='base'\nphase='prepare'\n")
        .expect("base");
    let local = phora::config::Config::parse("[targets.input]\npath='local'\n").expect("local");
    let merged = phora::config::merge_configs(base, Some(local));
    assert_eq!(
        merged.targets["input"].phase(),
        phora::config::TargetPhase::Prepare
    );
    assert_eq!(merged.targets["input"].path, Path::new("local"));
    assert!(phora::config::Config::parse("[targets.input]\npath='x'\nphase='build'\n").is_err());
}

#[cfg(unix)]
#[test]
fn physical_overlap_is_rejected_before_preparation() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.project.join("home/.claude")).expect("home");
    std::os::unix::fs::symlink("home/.claude", fixture.project.join("alias")).expect("alias");
    fixture.configure(BUILD, "alias/inputs");
    let out = fixture.run(&["sync"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("prepare and deploy targets"));
    assert!(!fixture.project.join("home/.claude/inputs").exists());
}

#[test]
fn update_prunes_removed_inputs_and_generated_links_in_the_same_sync() {
    let fixture = Fixture::new();
    fixture.succeeds(&["sync"]);
    fixture.update_package();
    write(&fixture.project.join("home/.claude/personal.md"), "keep\n");
    fixture.succeeds(&["update", "tropos", "--fast-forward", "--prune"]);
    assert_eq!(
        fixture.read("home/.claude/skills/code/SKILL.md"),
        "skill v2\n"
    );
    assert!(
        !fixture
            .project
            .join("stage/skills/code/resource.md")
            .exists()
    );
    assert!(
        !fixture
            .project
            .join("home/.claude/skills/code/resource.md")
            .is_symlink()
    );
    assert_eq!(fixture.read("home/.claude/personal.md"), "keep\n");
    fixture.succeeds(&["verify"]);
}

#[test]
fn failed_build_keeps_a_different_import_ref_used_only_by_deployment() {
    let fixture = Fixture::new();
    git(&fixture.package, &["branch", "other"]);
    fixture.configure(
        &BUILD.replace("claude tropos", "claude direct tropos"),
        "stage",
    );
    let local = fixture.read("phora.local.toml")
        + r#"
[targets.direct]
path = "direct"
imports = [{ source = "tropos", branch = "other" }]
"#;
    write(&fixture.project.join("phora.local.toml"), &local);
    fixture.succeeds(&["sync"]);
    let before: phora::lock::Lock = toml::from_str(&fixture.read("phora.lock")).expect("lock");
    fixture.update_package();
    git(&fixture.package, &["switch", "-q", "other"]);
    write(&fixture.package.join("skills/code/SKILL.md"), "other v2\n");
    git(&fixture.package, &["add", "."]);
    git(&fixture.package, &["commit", "-qm", "other update"]);
    write(&fixture.project.join("fail-build"), "fail");
    assert!(!fixture.run(&["update", "--fast-forward"]).status.success());
    let after: phora::lock::Lock = toml::from_str(&fixture.read("phora.lock")).expect("lock");
    assert_eq!(fixture.read("stage/skills/code/SKILL.md"), "skill v2\n");
    assert_eq!(fixture.read("direct/skills/code/SKILL.md"), "skill v1\n");
    assert_eq!(
        after
            .find_entry("tropos", Some("branch:other"))
            .expect("output ref")
            .commit,
        before
            .find_entry("tropos", Some("branch:other"))
            .expect("old output ref")
            .commit
    );
}
