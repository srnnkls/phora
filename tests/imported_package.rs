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
[sources.content]
path = "."
include = ["skills/**"]
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[targets.content]
path = "."
sources.content = {{ collapse = false }}
[targets.loqui]
path = "skills/loqui/reference/{reference}"
sources.loqui = {{ collapse = false }}
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

const CLAUDE: &str = "[targets.claude]\npath = \"out/claude\"\nimports = [\"tropos\"]\n";

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
            "{CLAUDE}\n[targets.codex]\npath = \"out/codex\"\nimports = [{{ source = \"tropos\", branch = \"codex\" }}]\n"
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
        "[targets.codex]\npath = \"out/codex\"\nimports = [{ source = \"tropos\", branch = \"codex\" }]\n",
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
imports = [{{ source = "tropos", branch = "variant" }}]
[targets.tag]
path = "out/tag"
imports = [{{ source = "tropos", tag = "variant" }}]
[targets.revision]
path = "out/revision"
imports = [{{ source = "tropos", rev = {pinned:?} }}]
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
fn self_source_cannot_override_its_package_pin_in_a_binding() {
    let package = package();
    let manifest = manifest("loqui").replace(
        "sources.content = { collapse = false }",
        "sources.content = { branch = \"other\", collapse = false }",
    );
    write(&package.repository.join("phora.toml"), &manifest);
    git(&package.repository, &["add", "."]);
    git(
        &package.repository,
        &["commit", "-qm", "Invalid self source"],
    );
    configure(&package, "path", CLAUDE);
    let result = run(&package, &["sync"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("self source"));
    assert!(!package.project.join("out").exists());
}
