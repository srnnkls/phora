use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
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

const PACKAGE: &str = r#"
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[sources.harnesses]
build = { tool = "test:harness@1", cmd = ["harness"] }
[targets.loqui]
path = "skills/loqui/reference/loqui"
sources = ["loqui"]
[targets.claude]
path = "dist/claude"
sources.harnesses = { take = [{ "claude/" = "." }] }
[offers.claude]
root = "dist/claude"
targets = ["claude"]
"#;

impl Fixture {
    fn new(package_manifest: &str) -> Self {
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
        write(&package.join("skills/code/SKILL.md"), "skill\n");
        write(&package.join("phora.toml"), package_manifest);
        git(&package, &["add", "."]);
        git(&package, &["commit", "-qm", "package"]);
        write(
            &root.path().join("gitconfig"),
            &format!(
                "[url {:?}]\n\tinsteadOf = https://example.invalid/loqui.git\n",
                dependency.display().to_string()
            ),
        );
        Self {
            root,
            package,
            project,
        }
    }

    fn builds_log(&self) -> PathBuf {
        self.root.path().join("builds")
    }

    fn configure(&self, tools: &str, binding: &str) {
        write(
            &self.project.join("phora.toml"),
            &format!(
                r#"
[paths]
cache = "cache"
state = "state"
{tools}
[sources.tropos]
path = {package:?}
branch = "main"
transitive = true
[targets.claude]
path = "home/.claude"
sources.tropos = {{ {binding} }}
"#,
                package = self.package.display().to_string()
            ),
        );
    }

    fn harness_script(&self, run: &str) -> PathBuf {
        let script = self.root.path().join("tools/harness");
        write(
            &script,
            &format!(
                "#!/bin/sh\nprintf 'build\\n' >> {log:?} && mkdir -p \"$PHORA_OUTPUT/claude\" && {run}\n",
                log = self.builds_log().display().to_string()
            ),
        );
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        script
    }

    fn harness(&self, run: &str) -> String {
        format!(
            "[tools.\"test:harness\"]\npath = {:?}\n",
            self.harness_script(run).display().to_string()
        )
    }

    fn mise_log(&self) -> PathBuf {
        self.root.path().join("mise.log")
    }

    fn harness_through_mise(&self, run: &str, installed: bool) -> String {
        let script = self.harness_script(run);
        let marker = self.root.path().join("mise-installed");
        if installed {
            write(&marker, "");
        }
        let mise = self.root.path().join("bin/mise");
        write(
            &mise,
            &format!(
                "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"${{MISE_OFFLINE:-online}}\" >> {log:?}\n\
                 case \"$1\" in\n\
                 bin-paths) if [ -f {marker:?} ]; then printf '[{{\"name\":\"harness\",\"path\":\"%s\",\"symlink\":false}}]' {script:?}; else printf '[]'; fi ;;\n\
                 install) touch {marker:?} ;;\n\
                 esac\n",
                log = self.mise_log().display().to_string(),
                marker = marker.display().to_string(),
                script = script.display().to_string(),
            ),
        );
        std::fs::set_permissions(&mise, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        "[tools.\"test:harness\"]\nmise = true\n".to_owned()
    }

    fn mise_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.mise_log())
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(&self.project)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("GIT_CONFIG_GLOBAL", self.root.path().join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("phora")
    }

    fn succeeds(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "phora {args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "phora {args:?} unexpectedly succeeded"
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn builds(&self) -> usize {
        std::fs::read_to_string(self.builds_log())
            .map(|log| log.lines().count())
            .unwrap_or_default()
    }

    fn deployed(&self) -> BTreeSet<String> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    let relative = path.strip_prefix(base).expect("under base");
                    out.insert(relative.to_string_lossy().into_owned());
                }
            }
        }
        let base = self.project.join("home/.claude");
        let mut out = BTreeSet::new();
        walk(&base, &base, &mut out);
        out
    }
}

const COPY_INPUT: &str = "cp -R \"$PHORA_INPUT/.\" \"$PHORA_OUTPUT/claude/\"";

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

#[test]
fn an_offered_build_runs_the_granted_tool_over_the_repo_offer() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(&fixture.harness(COPY_INPUT), "offer = \"claude\"");
    fixture.succeeds(&["sync"]);
    assert_eq!(
        fixture.deployed(),
        set(&[
            "skills/code/SKILL.md",
            "skills/loqui/reference/loqui/languages/rust/README.md",
        ]),
        "the build reads the default offer without the target that reads the build"
    );
    assert_eq!(fixture.builds(), 1);
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1, "an unchanged input skips the build");
}

#[test]
fn frozen_replays_an_offered_build_without_running_it() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(&fixture.harness(COPY_INPUT), "offer = \"claude\"");
    fixture.succeeds(&["sync"]);
    std::fs::remove_dir_all(fixture.project.join("home")).expect("remove deployment");
    fixture.succeeds(&["sync", "--frozen"]);
    assert_eq!(fixture.builds(), 1);
    assert!(fixture.deployed().contains("skills/code/SKILL.md"));
}

#[test]
fn an_offered_build_needs_its_tool_granted() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure("", "offer = \"claude\"");
    let stderr = fixture.fails(&["sync"]);
    assert!(
        stderr.contains(
            "target `claude`: offer `claude` of `tropos` builds with tool `test:harness@1`"
        ) && stderr.contains("[tools.\"test:harness\"]")
            && stderr.contains("defined:")
            && stderr.contains("phora.toml:6"),
        "{stderr}"
    );
    assert_eq!(fixture.builds(), 0);
}

#[test]
fn a_dependency_build_with_its_own_command_is_never_offered() {
    let manifest = PACKAGE.replace(
        "build = { tool = \"test:harness@1\", cmd = [\"harness\"] }",
        "build = { inputs = [\"loqui\"], run = \"touch \\\"$HOME/pwned\\\"\" }",
    );
    let fixture = Fixture::new(&manifest);
    fixture.configure(&fixture.harness(COPY_INPUT), "offer = \"claude\"");
    let stderr = fixture.fails(&["sync"]);
    assert!(stderr.contains("runs its own command"), "{stderr}");
    fixture.configure(&fixture.harness(COPY_INPUT), "");
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 0);
    assert!(
        !fixture
            .deployed()
            .iter()
            .any(|path| path.starts_with("dist")),
        "the default offer skips the target bound to the dependency's own command"
    );
}

#[test]
fn an_offered_build_runs_outside_the_consumer_project() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness("pwd > \"$PHORA_OUTPUT/claude/cwd\""),
        "offer = \"claude\"",
    );
    fixture.succeeds(&["sync"]);
    let cwd = std::fs::read_to_string(fixture.project.join("home/.claude/cwd")).expect("cwd");
    assert!(
        !Path::new(cwd.trim()).starts_with(&fixture.project),
        "the tool ran in the consumer project: {cwd}"
    );
}

#[test]
fn an_offered_build_cannot_emit_a_link_out_of_its_output() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness("ln -s /etc/hosts \"$PHORA_OUTPUT/claude/hosts\""),
        "offer = \"claude\"",
    );
    let stderr = fixture.fails(&["sync"]);
    assert!(stderr.contains("resolves outside"), "{stderr}");
    assert!(!fixture.project.join("home/.claude/hosts").exists());
}

#[test]
fn an_offer_a_build_reads_cannot_select_the_build_itself() {
    let manifest = format!("{PACKAGE}[offers.loop]\ntargets = [\"claude\"]\n").replace(
        "build = { tool = \"test:harness@1\", cmd = [\"harness\"] }",
        "build = { tool = \"test:harness@1\", cmd = [\"harness\"], offer = \"loop\" }",
    );
    let fixture = Fixture::new(&manifest);
    fixture.configure(&fixture.harness(COPY_INPUT), "offer = \"claude\"");
    let stderr = fixture.fails(&["sync"]);
    assert!(stderr.contains("reads its own output"), "{stderr}");
}

#[test]
fn a_repo_builds_its_own_offer_when_it_syncs_itself() {
    let fixture = Fixture::new(PACKAGE);
    write(
        &fixture.package.join("phora.toml"),
        &format!(
            "[paths]\ncache = \"../cache\"\nstate = \"../state\"\n{}{PACKAGE}",
            fixture.harness(COPY_INPUT)
        ),
    );
    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .arg("sync")
        .current_dir(&fixture.package)
        .env("GIT_CONFIG_GLOBAL", fixture.root.path().join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("phora");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.package.join("dist/claude/skills/code/SKILL.md"))
            .expect("built own files"),
        "skill\n"
    );
    assert!(
        fixture
            .package
            .join("dist/claude/skills/loqui/reference/loqui/languages/rust/README.md")
            .exists(),
        "the build reads the targets its offer composes"
    );
}

#[test]
fn an_offered_build_cannot_read_a_link_out_of_its_input() {
    let manifest = PACKAGE.replace(
        "include = [\"languages/**\"]",
        "include = [\"languages/**\"]\nallow_symlinks = true",
    );
    let control = Fixture::new(&manifest);
    control.configure(&control.harness(COPY_INPUT), "offer = \"claude\"");
    control.succeeds(&["sync"]);

    let fixture = Fixture::new(&manifest);
    let loqui = fixture.root.path().join("loqui");
    std::os::unix::fs::symlink("/etc/hosts", loqui.join("languages/hosts")).expect("plant link");
    git(&loqui, &["add", "."]);
    git(&loqui, &["commit", "-qm", "link"]);
    fixture.configure(&fixture.harness(COPY_INPUT), "offer = \"claude\"");
    fixture.fails(&["sync"]);
    assert_eq!(fixture.builds(), 0, "the tool never sees the planted link");
    assert!(
        !fixture
            .project
            .join("home/.claude/skills/loqui/reference/loqui/languages/hosts")
            .exists()
    );
}

#[test]
fn bindings_sharing_an_offered_build_run_it_once() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness(COPY_INPUT),
        "offer = \"claude\" }\n[targets.again]\npath = \"home/again\"\nsources.tropos = { offer = \"claude\"",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1);
    assert!(
        fixture
            .project
            .join("home/again/skills/code/SKILL.md")
            .exists()
    );
}

#[test]
fn a_linked_package_builds_from_its_working_tree() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness("cp -RL \"$PHORA_INPUT/.\" \"$PHORA_OUTPUT/claude/\""),
        "offer = \"claude\"",
    );
    let config = std::fs::read_to_string(fixture.project.join("phora.toml"))
        .expect("config")
        .replace("branch = \"main\"", "deploy = \"link\"");
    write(&fixture.project.join("phora.toml"), &config);
    write(
        &fixture.package.join("skills/code/SKILL.md"),
        "uncommitted\n",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(
        std::fs::read_to_string(fixture.project.join("home/.claude/skills/code/SKILL.md"))
            .expect("built"),
        "uncommitted\n"
    );
}

#[test]
fn a_consumer_take_composes_over_the_dependency_target_take() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness(COPY_INPUT),
        "offer = \"claude\", take = [{ \"skills/code/SKILL.md\" = \"skill\" }]",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.deployed(), set(&["skill"]));
}

#[test]
fn a_tool_granted_through_mise_installs_its_pin_then_runs_from_the_listed_path() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness_through_mise(COPY_INPUT, false),
        "offer = \"claude\"",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1);
    assert_eq!(
        fixture.mise_calls(),
        vec!["bin-paths 1", "install online", "bin-paths 1"],
        "resolution is offline; only a missing pin installs"
    );
    assert!(fixture.deployed().contains("skills/code/SKILL.md"));
}

#[test]
fn frozen_never_asks_mise() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        &fixture.harness_through_mise(COPY_INPUT, true),
        "offer = \"claude\"",
    );
    fixture.succeeds(&["sync"]);
    std::fs::remove_file(fixture.mise_log()).expect("reset mise log");
    std::fs::remove_dir_all(fixture.project.join("home")).expect("remove deployment");
    fixture.succeeds(&["sync", "--frozen"]);
    assert!(
        fixture.mise_calls().is_empty(),
        "{:?}",
        fixture.mise_calls()
    );
    assert!(fixture.deployed().contains("skills/code/SKILL.md"));
}

#[test]
fn a_tool_runs_only_executables_it_provides() {
    let manifest = PACKAGE.replace(
        "cmd = [\"harness\"]",
        "cmd = [\"sh\", \"-c\", \"touch pwned\"]",
    );
    let fixture = Fixture::new(&manifest);
    fixture.configure(
        &fixture.harness_through_mise(COPY_INPUT, true),
        "offer = \"claude\"",
    );
    let stderr = fixture.fails(&["sync"]);
    assert!(
        stderr.contains("provides no executable `sh`") && stderr.contains("[harness]"),
        "{stderr}"
    );
    assert_eq!(fixture.builds(), 0);
}

#[test]
fn an_interpreter_cannot_be_granted_as_a_tool() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(
        "[tools.\"test:harness\"]\npath = \"/bin/sh\"\n",
        "offer = \"claude\"",
    );
    let stderr = fixture.fails(&["sync"]);
    assert!(stderr.contains("interpreter `sh`"), "{stderr}");
}

#[test]
fn a_changed_tool_binary_rebuilds() {
    let fixture = Fixture::new(PACKAGE);
    fixture.configure(&fixture.harness(COPY_INPUT), "offer = \"claude\"");
    fixture.succeeds(&["sync"]);
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 1);
    fixture.configure(
        &fixture.harness(&format!("{COPY_INPUT} && true")),
        "offer = \"claude\"",
    );
    fixture.succeeds(&["sync"]);
    assert_eq!(fixture.builds(), 2);
}
