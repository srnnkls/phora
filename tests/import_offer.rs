//! An explicit import applies the importing source's own offer (`root`, `include`,
//! `exclude`) to the package's composed tree: package files and dependency mounts alike.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
mod common;

struct Package {
    root: TempDir,
    repository: PathBuf,
    loqui: PathBuf,
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

fn commit_all(repository: &Path, message: &str) {
    git(repository, &["add", "."]);
    git(repository, &["commit", "-qm", message]);
}

fn manifest(moira_target: &str) -> String {
    format!(
        r#"
[sources.tropos]
path = "."
include = ["skills/**", "rules/fas/**"]
exclude = ["rules/fas/moira/"]
[sources.moira]
git = "https://example.invalid/moira.git"
root = "rules/fas"
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[targets.tropos]
path = "."
sources.tropos = {{ collapse = false }}
{moira_target}
[targets.loqui]
path = "skills/loqui/reference/loqui"
sources.loqui = {{ collapse = false }}
"#
    )
}

const MOIRA_TARGET: &str =
    "[targets.moira]\npath = \"rules/fas/moira\"\nsources.moira = { collapse = false }";

fn package(moira_target: &str) -> Package {
    let root = tempfile::tempdir().expect("fixture root");
    let repository = root.path().join("tropos");
    let moira = root.path().join("moira");
    let loqui = root.path().join("loqui");
    let project = root.path().join("consumer");
    for path in [&repository, &moira, &loqui, &project] {
        std::fs::create_dir_all(path).expect("fixture directory");
    }
    for path in [&repository, &moira, &loqui] {
        git(path, &["init", "-q", "-b", "main", "--template="]);
    }
    write(&moira.join("rules/fas/du.cue"), "du\n");
    write(&moira.join("README.md"), "moira\n");
    commit_all(&moira, "moira");
    write(&loqui.join("languages/rust/README.md"), "Loqui\n");
    commit_all(&loqui, "loqui");
    write(&repository.join("phora.toml"), &manifest(moira_target));
    for file in [
        "skills/code/SKILL.md",
        "agents/reviewer.md",
        "henia.toml",
        "rules/fas/guidance/naming.cue",
        "rules/fas/security/secrets.cue",
        "rules/fas/workflow/review.cue",
        "rules/fas/moira/stale.cue",
    ] {
        write(&repository.join(file), &format!("{file}\n"));
    }
    commit_all(&repository, "package");
    write(
        &root.path().join("gitconfig"),
        &format!(
            "[url {:?}]\n\tinsteadOf = https://example.invalid/moira.git\n\
             [url {:?}]\n\tinsteadOf = https://example.invalid/loqui.git\n",
            moira.display().to_string(),
            loqui.display().to_string()
        ),
    );
    Package {
        root,
        repository,
        loqui,
        project,
    }
}

fn configure(package: &Package, offer: &str) {
    write(
        &package.project.join("phora.toml"),
        &format!(
            r#"
[paths]
cache = "cache"
state = "state"
[sources.tropos]
path = {repository:?}
branch = "main"
transitive = true
{offer}
[targets.fas]
path = "out"
imports = ["tropos"]
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

fn deployed(package: &Package) -> BTreeSet<String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read deployed dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let relative = path.strip_prefix(base).expect("under base");
                out.insert(relative.to_string_lossy().into_owned());
            }
        }
    }
    let base = package.project.join("out");
    let mut out = BTreeSet::new();
    walk(&base, &base, &mut out);
    out
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

fn hide(path: &Path) {
    std::fs::rename(path, path.with_extension("hidden")).expect("hide repository");
}

#[test]
fn root_reroots_package_files_and_dependency_mounts() {
    let package = package(MOIRA_TARGET);
    configure(&package, "root = \"rules/fas\"");
    succeeds(&package, &["sync"]);
    assert_eq!(
        deployed(&package),
        set(&[
            "guidance/naming.cue",
            "moira/du.cue",
            "security/secrets.cue",
            "workflow/review.cue",
        ])
    );
    succeeds(&package, &["verify"]);
}

#[test]
fn a_dependency_mounted_outside_the_root_is_never_fetched() {
    let package = package(MOIRA_TARGET);
    hide(&package.loqui);
    configure(&package, "root = \"rules/fas\"");
    succeeds(&package, &["sync"]);
    assert!(deployed(&package).contains("moira/du.cue"));
}

#[test]
fn include_and_exclude_filter_the_composed_tree_relative_to_the_root() {
    let package = package(MOIRA_TARGET);
    configure(
        &package,
        "root = \"rules/fas\"\ninclude = [\"guidance/**\", \"security/**\", \"moira/**\"]\nexclude = [\"security/\"]",
    );
    succeeds(&package, &["sync"]);
    assert_eq!(
        deployed(&package),
        set(&["guidance/naming.cue", "moira/du.cue"])
    );
}

#[test]
fn include_without_root_keeps_a_dependency_under_an_included_path() {
    let package = package(MOIRA_TARGET);
    configure(&package, "include = [\"skills/**\"]");
    succeeds(&package, &["sync"]);
    assert_eq!(
        deployed(&package),
        set(&[
            "skills/code/SKILL.md",
            "skills/loqui/reference/loqui/languages/rust/README.md",
        ])
    );
}

#[test]
fn exclude_drops_a_dependency_mount() {
    let package = package(MOIRA_TARGET);
    configure(&package, "exclude = [\"skills/loqui/\", \"rules/\"]");
    succeeds(&package, &["sync"]);
    assert_eq!(deployed(&package), set(&["skills/code/SKILL.md"]));
}

#[test]
fn a_dependency_mount_straddling_the_root_is_rerooted_below_it() {
    let package = package(MOIRA_TARGET);
    configure(
        &package,
        "root = \"skills/loqui/reference/loqui/languages\"",
    );
    succeeds(&package, &["sync"]);
    assert_eq!(deployed(&package), set(&["rust/README.md"]));
}

#[test]
fn a_straddled_dependency_mount_keeps_only_files_under_the_root() {
    let package =
        package("[targets.moira]\npath = \"rules\"\nsources.moira = { collapse = false }");
    configure(&package, "root = \"rules/fas\"");
    succeeds(&package, &["sync"]);
    assert_eq!(
        deployed(&package),
        set(&[
            "guidance/naming.cue",
            "security/secrets.cue",
            "workflow/review.cue",
        ])
    );
}

#[test]
fn a_non_flat_target_spanning_the_root_is_rejected() {
    let package = package(
        "[targets.moira]\npath = \"rules\"\nlayout = \"by-source\"\nsources.moira = { collapse = false }",
    );
    configure(&package, "root = \"rules/fas\"");
    let result = run(&package, &["sync"]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains(
            "spans the import root `rules/fas`; only a flat-layout target can be re-rooted"
        )
    );
}

#[test]
fn narrowing_the_offer_prunes_what_the_import_no_longer_admits() {
    let package = package(MOIRA_TARGET);
    configure(&package, "");
    succeeds(&package, &["sync"]);
    assert!(deployed(&package).contains("skills/code/SKILL.md"));
    configure(&package, "include = [\"rules/**\"]");
    succeeds(&package, &["sync", "--prune"]);
    assert_eq!(
        deployed(&package),
        set(&[
            "rules/fas/guidance/naming.cue",
            "rules/fas/moira/du.cue",
            "rules/fas/security/secrets.cue",
            "rules/fas/workflow/review.cue",
        ])
    );
}
