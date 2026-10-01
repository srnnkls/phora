//! A binding of a transitive source composes one of its manifest's offers: the offer's own
//! files plus the dependencies its targets place, never anything derived from paths.

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

fn manifest() -> String {
    r#"
[sources.moira]
git = "https://example.invalid/moira.git"
root = "rules/fas"
[sources.loqui]
git = "https://example.invalid/loqui.git"
include = ["languages/**"]
[targets.loqui]
path = "skills/loqui/reference/loqui"
sources = ["loqui"]
[targets.moira]
path = "rules/fas/moira"
sources = ["moira"]
[targets.home]
path = "~/.never-offered"
sources = ["loqui"]
[offers.default]
include = ["skills/**", "rules/fas/**"]
[offers.fas]
root = "rules/fas"
targets = ["moira"]
[offers.skills]
include = ["skills/**"]
targets = ["loqui"]
[offers.bare]
include = ["skills/**"]
targets = []
"#
    .to_owned()
}

fn package() -> Package {
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
    write(&repository.join("phora.toml"), &manifest());
    for file in [
        "skills/code/SKILL.md",
        "agents/reviewer.md",
        "henia.toml",
        "rules/fas/guidance/naming.cue",
        "rules/fas/security/secrets.cue",
        "rules/fas/workflow/review.cue",
        "rules/fas/moira/stale.cue",
        "skills/loqui/reference/loqui/stale.md",
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

fn configure(package: &Package, binding: &str) {
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
[targets.fas]
path = "out"
sources.tropos = {{ {binding} }}
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

const FAS_OFFER: &[&str] = &[
    "guidance/naming.cue",
    "moira/du.cue",
    "security/secrets.cue",
    "workflow/review.cue",
];

#[test]
fn a_named_offer_places_its_dependencies_where_it_declares() {
    let package = package();
    configure(&package, "offer = \"fas\"");
    succeeds(&package, &["sync"]);
    assert_eq!(deployed(&package), set(FAS_OFFER));
    succeeds(&package, &["verify"]);
}

#[test]
fn a_dependency_the_chosen_offer_does_not_bind_is_never_fetched() {
    let package = package();
    hide(&package.loqui);
    configure(&package, "offer = \"fas\"");
    succeeds(&package, &["sync"]);
    assert!(deployed(&package).contains("moira/du.cue"));
}

#[test]
fn the_default_offer_publishes_own_files_and_every_placed_dependency() {
    let package = package();
    configure(&package, "");
    succeeds(&package, &["sync"]);
    assert_eq!(
        deployed(&package),
        set(&[
            "rules/fas/guidance/naming.cue",
            "rules/fas/moira/du.cue",
            "rules/fas/security/secrets.cue",
            "rules/fas/workflow/review.cue",
            "skills/code/SKILL.md",
            "skills/loqui/reference/loqui/languages/rust/README.md",
        ]),
        "own files give way to dependency paths: the committed `rules/fas/moira/stale.cue` never deploys"
    );
}

#[test]
fn a_target_an_offer_does_not_select_keeps_its_committed_copy_out_of_own_files() {
    let package = package();
    configure(&package, "offer = \"bare\"");
    succeeds(&package, &["sync"]);
    assert_eq!(deployed(&package), set(&["skills/code/SKILL.md"]));
}

fn owner_of(package: &Package, artifact: &str) -> String {
    let output = run(package, &["where"]);
    let output = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = output.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("Artifact:") && line.contains(artifact) {
            return lines
                .next()
                .and_then(|owner| owner.strip_prefix("  - "))
                .expect("an owner follows the artifact")
                .to_owned();
        }
    }
    panic!("no record for {artifact}: {output}");
}

#[test]
fn switching_offers_keeps_the_owner_of_a_target_both_place() {
    let package = package();
    configure(&package, "");
    succeeds(&package, &["sync"]);
    let before = owner_of(&package, "languages/rust/README.md");
    configure(&package, "offer = \"skills\"");
    succeeds(&package, &["sync"]);
    assert_eq!(owner_of(&package, "languages/rust/README.md"), before);
}

#[test]
fn an_undeclared_offer_is_rejected_naming_the_declared_ones() {
    let package = package();
    configure(&package, "offer = \"agents\"");
    let result = run(&package, &["sync"]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("no offer `agents`") && stderr.contains("default, fas"),
        "{stderr}"
    );
}

#[test]
fn take_shapes_the_offer_output_across_dependencies() {
    let package = package();
    configure(&package, "take = [\"skills/**\"]");
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
fn switching_offers_prunes_what_the_new_offer_no_longer_publishes() {
    let package = package();
    configure(&package, "");
    succeeds(&package, &["sync"]);
    assert!(deployed(&package).contains("skills/code/SKILL.md"));
    configure(&package, "offer = \"fas\"");
    succeeds(&package, &["sync", "--prune"]);
    assert_eq!(deployed(&package), set(FAS_OFFER));
}

fn configure_fas(package: &Package, rules: &str) {
    write(&package.project.join("dots/config.toml"), "fas\n");
    write(
        &package.project.join("phora.toml"),
        &format!(
            r#"
[paths]
cache = "cache"
state = "state"
[sources.fas]
path = "dots"
deploy = "link"
{rules}
[targets.fas]
path = "out"
sources.fas = {{ collapse = false }}
"#,
        ),
    );
}

fn bind_rules_in_the_fas_target(package: &Package) -> String {
    format!(
        r#"
[sources.fas-rules]
path = {repository:?}
branch = "main"
root = "rules"
exclude = ["fas/moira/"]
[sources.moira]
git = "https://example.invalid/moira.git"
root = "rules/fas"
[targets.fas.sources.fas-rules]
take = [{{ "fas/" = "rules" }}]
[targets.moira]
path = "out/rules/moira"
sources.moira = {{ collapse = false }}
"#,
        repository = package.repository.display().to_string()
    )
}

fn import_rules_into_their_own_target(package: &Package) -> String {
    format!(
        r#"
[sources.fas-rules]
path = {repository:?}
branch = "main"
transitive = true
[targets.fas-rules]
path = "out/rules"
sources.fas-rules = {{ offer = "fas" }}
"#,
        repository = package.repository.display().to_string()
    )
}

const FAS_RULES: &[&str] = &[
    "config.toml",
    "rules/guidance/naming.cue",
    "rules/moira/du.cue",
    "rules/security/secrets.cue",
    "rules/workflow/review.cue",
];

#[test]
fn rules_moved_from_a_live_target_into_their_own_import_are_adopted_in_one_sync() {
    for prune in [false, true] {
        let package = package();
        configure_fas(&package, &bind_rules_in_the_fas_target(&package));
        succeeds(&package, &["sync"]);
        assert_eq!(deployed(&package), set(FAS_RULES), "premise");
        configure_fas(&package, &import_rules_into_their_own_target(&package));

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
            "moved rules must be adopted, not skipped as foreign: {stderr}"
        );
        assert_eq!(deployed(&package), set(FAS_RULES), "prune={prune}");
        let owners = run(&package, &["where"]);
        let owners = String::from_utf8_lossy(&owners.stdout);
        let owners: Vec<&str> = owners
            .lines()
            .filter_map(|line| line.strip_prefix("  - "))
            .collect();
        assert_eq!(
            owners.len(),
            FAS_RULES.len(),
            "one record per file: {owners:?}"
        );
        assert_eq!(
            owners.iter().filter(|owner| owner.ends_with('%')).count(),
            3,
            "the composed own-file target owns the moved rules: {owners:?}"
        );
        assert!(
            owners.iter().any(|owner| owner.ends_with("%moira")),
            "the composed dependency owns its moved file: {owners:?}"
        );
        assert!(owners.contains(&"fas"), "prune={prune}: {owners:?}");
    }
}
