use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
mod common;

struct Package {
    root: TempDir,
    repository: PathBuf,
    dependency: PathBuf,
    project: PathBuf,
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

fn git(repository: &Path, args: &[&str]) {
    common::assert_sandboxed(repository);
    let result = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(repository)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn revision(repository: &Path) -> String {
    let result = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repository)
        .output()
        .expect("revision");
    assert!(result.status.success());
    String::from_utf8(result.stdout)
        .expect("revision text")
        .trim()
        .to_owned()
}

fn manifest(reference: &str) -> String {
    format!(
        r#"
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[targets.loqui]
path = "skills/loqui/reference/{reference}"
sources = ["loqui"]
[offers.default]
include = ["skills/**"]
[offers.skills]
include = ["skills/**"]
targets = []
"#
    )
}

fn package() -> Package {
    let root = tempfile::tempdir().expect("fixture root");
    let repository = root.path().join("tropos");
    let dependency = root.path().join("loqui");
    let project = root.path().join("consumer");
    for path in [&repository, &dependency, &project] {
        std::fs::create_dir_all(path).expect("fixture directory");
    }
    for path in [&repository, &dependency] {
        git(path, &["init", "-q", "-b", "main", "--template="]);
    }
    write(&dependency.join("languages/rust/README.md"), "Loqui\n");
    git(&dependency, &["add", "."]);
    git(&dependency, &["commit", "-qm", "dependency"]);
    write(
        &repository.join("phora.toml"),
        &manifest("loqui").replace(
            "include = [\"languages/**\"]",
            &format!(
                "include = [\"languages/**\"]\nrev = {:?}",
                revision(&dependency)
            ),
        ),
    );
    write(&repository.join("skills/code/SKILL.md"), "Claude skill\n");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-qm", "Claude package"]);
    write(
        &root.path().join("gitconfig"),
        &format!(
            "[url {:?}]\n\tinsteadOf = https://example.invalid/loqui.git\n",
            dependency.display().to_string()
        ),
    );
    Package {
        root,
        repository,
        dependency,
        project,
    }
}

fn configure(package: &Package, remote: &str, targets: &str) {
    write(
        &package.project.join("phora.toml"),
        &format!(
            r#"
[paths]
cache = "cache"
state = "state"
[sources.tropos]
{remote} = {repository:?}
branch = "main"
transitive = true
{targets}
"#,
            repository = package.repository.display().to_string()
        ),
    );
}

fn run(package: &Package, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(&package.project)
        .env("GIT_CONFIG_GLOBAL", package.root.path().join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("phora runs")
}

fn succeeds(package: &Package, args: &[&str]) {
    let result = run(package, args);
    assert!(
        result.status.success(),
        "phora {args:?}: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn assert_deployed(package: &Package, target: &str, skill: &str, guides: &str) {
    let root = package.project.join(target);
    assert_eq!(
        std::fs::read_to_string(root.join("skills/code/SKILL.md")).expect("skill"),
        skill
    );
    assert_eq!(
        std::fs::read_to_string(root.join("skills/loqui/reference/loqui/languages/rust/README.md"))
            .expect("Loqui"),
        guides
    );
    assert!(!root.join("phora.toml").exists());
}

const CLAUDE: &str = "[targets.claude]\npath = \"out/claude\"\nsources = [\"tropos\"]\n";

#[test]
fn consumer_selected_local_package_imports_its_own_files_and_dependency() {
    let package = package();
    configure(&package, "path", CLAUDE);
    succeeds(&package, &["sync"]);
    assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
}

#[test]
fn imported_self_source_reads_the_package_snapshot_not_the_consumers_worktree() {
    let package = package();
    configure(&package, "git", CLAUDE);
    write(
        &package.project.join("skills/code/SKILL.md"),
        "consumer secret\n",
    );
    write(
        &package.repository.join("skills/code/SKILL.md"),
        "uncommitted edit\n",
    );
    succeeds(&package, &["sync"]);
    assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
}

#[test]
fn one_imported_package_selects_harness_refs_updates_and_replays_offline() {
    let package = package();
    git(&package.repository, &["switch", "-c", "codex"]);
    write(
        &package.repository.join("skills/code/SKILL.md"),
        "Codex skill\n",
    );
    write(
        &package.repository.join("skills/code/obsolete.txt"),
        "old resource\n",
    );
    git(&package.repository, &["add", "."]);
    git(&package.repository, &["commit", "-qm", "Codex package"]);
    configure(
        &package,
        "path",
        &format!(
            "{CLAUDE}\n[targets.codex]\npath = \"out/codex\"\nsources.tropos = {{ branch = \"codex\" }}\n"
        ),
    );
    succeeds(&package, &["sync"]);
    assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
    assert_deployed(&package, "out/codex", "Codex skill\n", "Loqui\n");
    write(
        &package.repository.join("skills/code/SKILL.md"),
        "Codex rebuilt\n",
    );
    std::fs::remove_file(package.repository.join("skills/code/obsolete.txt"))
        .expect("remove old resource");
    write(
        &package.dependency.join("languages/rust/README.md"),
        "Updated Loqui\n",
    );
    git(&package.dependency, &["add", "."]);
    git(&package.dependency, &["commit", "-qm", "Updated guides"]);
    let revision = revision(&package.dependency);
    let updated = manifest("loqui").replace(
        "include = [\"languages/**\"]",
        &format!("include = [\"languages/**\"]\nrev = {:?}", revision.trim()),
    );
    write(&package.repository.join("phora.toml"), &updated);
    git(&package.repository, &["add", "."]);
    git(&package.repository, &["commit", "-qm", "Rebuild Codex"]);
    succeeds(&package, &["sync", "--frozen"]);
    assert_deployed(&package, "out/codex", "Codex skill\n", "Loqui\n");
    succeeds(&package, &["sync"]);
    assert_deployed(&package, "out/codex", "Codex skill\n", "Loqui\n");
    // Removing another harness must not change this package's ownership identity.
    configure(
        &package,
        "path",
        "[targets.codex]\npath = \"out/codex\"\nsources.tropos = { branch = \"codex\" }\n",
    );
    succeeds(&package, &["update", "tropos", "--fast-forward"]);
    assert_deployed(&package, "out/codex", "Codex rebuilt\n", "Updated Loqui\n");
    assert!(
        !package
            .project
            .join("out/codex/skills/code/obsolete.txt")
            .exists()
    );
    std::fs::remove_dir_all(package.project.join("out")).expect("remove deployed fixture");
    std::fs::rename(
        &package.repository,
        package.root.path().join("offline-tropos"),
    )
    .expect("hide package");
    std::fs::rename(
        &package.dependency,
        package.root.path().join("offline-loqui"),
    )
    .expect("hide dependency");
    succeeds(&package, &["sync", "--frozen"]);
    assert!(!package.project.join("out/claude").exists());
    assert_deployed(&package, "out/codex", "Codex rebuilt\n", "Updated Loqui\n");
    succeeds(&package, &["verify"]);
}

#[test]
fn import_branch_tag_and_revision_keep_distinct_pins() {
    let package = package();
    let pinned = revision(&package.repository);
    git(&package.repository, &["tag", "variant"]);
    git(&package.repository, &["switch", "-c", "variant"]);
    write(
        &package.repository.join("skills/code/SKILL.md"),
        "Branch skill\n",
    );
    git(&package.repository, &["add", "."]);
    git(&package.repository, &["commit", "-qm", "Branch variant"]);
    configure(
        &package,
        "path",
        &format!(
            r#"
[targets.branch]
path = "out/branch"
sources.tropos = {{ branch = "variant" }}
[targets.tag]
path = "out/tag"
sources.tropos = {{ tag = "variant" }}
[targets.revision]
path = "out/revision"
sources.tropos = {{ rev = {pinned:?} }}
"#
        ),
    );
    succeeds(&package, &["sync"]);
    succeeds(&package, &["sync", "--frozen"]);
    assert_deployed(&package, "out/branch", "Branch skill\n", "Loqui\n");
    assert_deployed(&package, "out/tag", "Claude skill\n", "Loqui\n");
    assert_deployed(&package, "out/revision", "Claude skill\n", "Loqui\n");
}

#[test]
fn a_target_binding_the_repo_itself_is_never_offered() {
    let package = package();
    let manifest = format!(
        "[sources.content]\npath = \".\"\n{}\n[targets.content]\npath = \"content\"\nsources = [\"content\"]\n",
        manifest("loqui")
    );
    write(&package.repository.join("phora.toml"), &manifest);
    git(&package.repository, &["add", "."]);
    git(&package.repository, &["commit", "-qm", "Self path source"]);
    configure(&package, "path", CLAUDE);
    let result = run(&package, &["sync"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!package.project.join("out/claude/content").exists());
}

#[test]
fn two_offers_of_one_source_share_one_pin() {
    let package = package();
    configure(
        &package,
        "path",
        &format!(
            "{CLAUDE}\n[targets.skills]\npath = \"out/skills\"\nsources.tropos = {{ offer = \"skills\" }}\n"
        ),
    );
    succeeds(&package, &["sync"]);
    assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
    assert_eq!(
        std::fs::read_to_string(package.project.join("out/skills/skills/code/SKILL.md"))
            .expect("skill"),
        "Claude skill\n"
    );
    assert!(!package.project.join("out/skills/skills/loqui").exists());
    let lock = std::fs::read_to_string(package.project.join("phora.lock")).expect("lock");
    assert_eq!(
        lock.matches("name = \"tropos\"").count(),
        1,
        "one source, one pin, whichever offers its bindings select: {lock}"
    );
}

#[test]
fn a_failed_pre_sync_hook_keeps_the_existing_deployment_and_lock_on_update() {
    let package = package();
    configure(&package, "path", CLAUDE);
    succeeds(&package, &["sync"]);
    let lock = package.project.join("phora.lock");
    let before = std::fs::read(&lock).expect("lock before failure");
    let config_path = package.project.join("phora.toml");
    let config = std::fs::read_to_string(&config_path).expect("config");
    write(
        &config_path,
        &format!("{config}\n[hooks]\npre_sync = 'exit 23'\n"),
    );
    let result = run(&package, &["update", "tropos", "--fast-forward"]);
    assert!(!result.status.success());
    assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
    assert_eq!(std::fs::read(lock).expect("lock after failure"), before);
}

#[test]
fn imported_package_deploys_at_a_parent_relative_anchor() {
    let mut package = package();
    package.project = package.project.join("phora/stage");
    configure(
        &package,
        "path",
        "[targets.tropos]\npath = \"../../out\"\nsources = [\"tropos\"]\n",
    );
    succeeds(&package, &["sync"]);
    assert_deployed(&package, "../../out", "Claude skill\n", "Loqui\n");
    assert!(!package.project.join("out").exists());
    succeeds(&package, &["verify"]);
}

fn configure_bindings_before_import(package: &Package) {
    write(&package.project.join("notes-src/memo/n.md"), "memo\n");
    write(
        &package.project.join("phora.toml"),
        &format!(
            r#"
[paths]
cache = "cache"
state = "state"
[sources.skills]
path = {repository:?}
include = ["skills/**"]
[sources.guides]
git = "https://example.invalid/loqui.git"
branch = "main"
include = ["languages/**"]
[sources.notes]
path = "notes-src"
deploy = "link"
[targets.claude]
path = "out/claude"
sources.skills = {{ collapse = false }}
sources.notes = {{}}
[targets.guides]
path = "out/claude/skills/loqui/reference/loqui"
sources.guides = {{ collapse = false }}
"#,
            repository = package.repository.display().to_string()
        ),
    );
}

const CLAUDE_WITH_NOTES: &str = "[sources.notes]\npath = \"notes-src\"\ndeploy = \"link\"\n[targets.claude]\npath = \"out/claude\"\nsources.tropos = {}\nsources.notes = {}\n";

#[test]
fn bindings_moved_into_an_imported_package_are_adopted_in_one_sync() {
    for prune in [false, true] {
        let package = package();
        configure_bindings_before_import(&package);
        succeeds(&package, &["sync"]);
        assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
        configure(&package, "path", CLAUDE_WITH_NOTES);

        let args: &[&str] = if prune {
            &["sync", "--prune"]
        } else {
            &["sync"]
        };
        let result = run(&package, args);

        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(result.status.success(), "phora {args:?}: {stderr}");
        assert!(
            !stderr.contains("foreign"),
            "moved bindings must be adopted, not skipped as foreign: {stderr}"
        );
        assert_deployed(&package, "out/claude", "Claude skill\n", "Loqui\n");
        let listed = run(&package, &["list"]);
        let listed = String::from_utf8_lossy(&listed.stdout);
        let anchor_own: Vec<&str> = listed
            .lines()
            .take_while(|line| !line.trim_start().starts_with("via "))
            .collect();
        assert!(
            !anchor_own.iter().any(|line| line.contains("skills/")),
            "the still-configured anchor must no longer own the moved destinations: {listed}"
        );
        assert!(
            listed.contains("via tropos:"),
            "the moved destinations must list under the composed package: {listed}"
        );
        let owners = run(&package, &["where"]);
        let owners = String::from_utf8_lossy(&owners.stdout);
        let owners: Vec<&str> = owners
            .lines()
            .filter_map(|line| line.strip_prefix("  - "))
            .collect();
        assert_eq!(owners.len(), 3, "one record per destination: {owners:?}");
        for composed in ["%", "%loqui"] {
            assert!(
                owners.iter().any(|owner| owner.ends_with(composed)),
                "the composed target `{composed}` must own its moved destination: {owners:?}"
            );
        }
        assert!(owners.contains(&"claude"), "prune={prune}: {owners:?}");
    }
}
