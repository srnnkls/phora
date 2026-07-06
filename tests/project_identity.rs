//! CLIFF-PROJID-003: registry identity is a path hash — sync writes nothing into
//! the project tree beyond `phora.toml`/`phora.lock` and never touches git.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use phora::kernel::ProjectId;
use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    _parent: TempDir,
    project: PathBuf,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
    config: String,
}

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

fn build_source_repo(root: &Path) {
    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);

    write(&root.join("editor/init.lua"), b"-- init\n");
    write(&root.join("lint/rules.toml"), b"[rules]\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

/// A project working tree under a `parent` tempdir (renameable for the
/// relocation test), plus an isolated HOME/XDG sandbox and a shared source repo.
fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let src = TempDir::new().expect("src tempdir");
    let parent = TempDir::new().expect("parent tempdir");

    build_source_repo(src.path());

    let project = parent.path().join("proj");
    let home_path = home.path().to_path_buf();
    let target_path = home_path.join("deploy");
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
         include = [\"editor\", \"lint\"]\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src.path().display(),
        target = target_path.display(),
    );
    write(&project.join("phora.toml"), config.as_bytes());

    Fixture {
        _home: home,
        _src: src,
        _parent: parent,
        project,
        home_path,
        xdg_cache,
        xdg_state,
        config,
    }
}

fn run(fx: &Fixture, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", &fx.home_path)
        .env("XDG_CACHE_HOME", &fx.xdg_cache)
        .env("XDG_STATE_HOME", &fx.xdg_state)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs")
}

fn assert_sync_ok(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx}: `phora sync` must exit 0; got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn projects_base(fx: &Fixture) -> PathBuf {
    fx.xdg_state.join("phora").join("projects")
}

/// Immediate subdirectories of `<state>/phora/projects` — one per registry.
fn project_dirs(fx: &Fixture) -> Vec<PathBuf> {
    let base = projects_base(fx);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Does this registry directory hold at least one artifact record?
fn has_records(registry_dir: &Path) -> bool {
    fn any_toml(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if any_toml(&path) {
                    return true;
                }
            } else if path.extension().is_some_and(|e| e == "toml") {
                return true;
            }
        }
        false
    }
    any_toml(&registry_dir.join("targets"))
}

/// Names of every entry directly inside the project working tree.
fn project_entries(project: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(project)
        .expect("read project dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ── no pollution ──────────────────────────────────────────────────

/// Sync must leave the project tree holding only `phora.toml` and `phora.lock` —
/// no `.phora-id`, no adoption marker, no other phora-owned dotfile.
#[test]
fn sync_leaves_only_config_and_lock_in_the_project_tree() {
    let fx = build_fixture();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync");

    assert_eq!(
        project_entries(&fx.project),
        vec!["phora.lock".to_owned(), "phora.toml".to_owned()],
        "sync must not write any file into the project tree beyond phora.toml and phora.lock"
    );
}

// ── no git mutation ───────────────────────────────────────────────

/// Sync inside a git repo must not touch `.git/info/exclude` or the shared
/// `.gitignore` — phora never edits the user's git state.
#[test]
fn sync_never_writes_into_git_or_gitignore() {
    let fx = build_fixture();
    git(&fx.project, &["init", "-b", "main", "."]);

    let exclude = fx.project.join(".git").join("info").join("exclude");
    let before = std::fs::read_to_string(&exclude).unwrap_or_default();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync in git repo");

    let after = std::fs::read_to_string(&exclude).unwrap_or_default();
    assert_eq!(
        before, after,
        "sync must leave .git/info/exclude byte-for-byte unchanged, got:\n{after}"
    );
    assert!(
        !fx.project.join(".gitignore").exists(),
        "sync must never create the shared .gitignore"
    );
}

// ── path-hash identity ────────────────────────────────────────────

/// First sync creates exactly one registry, keyed by the path hash.
#[test]
fn first_sync_creates_one_path_hash_registry() {
    let fx = build_fixture();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "first sync");

    let expected = projects_base(&fx).join(
        ProjectId::for_path(&fx.project)
            .expect("path-hash id")
            .as_str(),
    );
    assert_eq!(
        project_dirs(&fx),
        vec![expected],
        "first sync must create exactly the path-hash-keyed registry"
    );
}

/// Two clones at distinct paths hash to distinct registries — no cross-contamination.
#[test]
fn two_clones_get_isolated_registries() {
    let fx = build_fixture();

    let clone_b = fx.project.parent().expect("parent").join("clone-b");
    std::fs::create_dir_all(&clone_b).expect("create second clone");
    write(&clone_b.join("phora.toml"), fx.config.as_bytes());

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync clone a");
    assert_sync_ok(&run(&fx, &clone_b, &["sync"]), "sync clone b");

    assert_eq!(
        project_dirs(&fx).len(),
        2,
        "two clones at distinct paths must keep two isolated registries"
    );
}

// ── relocation is a known rehash ──────────────────────────────────

/// The registry key is the path, so moving the project rehashes to a fresh
/// registry at the new path and orphans the old one — its records are left
/// behind, surfaced by orphan tooling and reclaimable by prune. This test pins
/// the accepted tradeoff of path-hash identity.
#[test]
fn moved_project_rehashes_and_orphans_the_old_registry() {
    let fx = build_fixture();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync before move");
    let original = projects_base(&fx).join(ProjectId::for_path(&fx.project).expect("id").as_str());
    assert_eq!(
        project_dirs(&fx),
        vec![original.clone()],
        "premise: the first sync creates exactly the original path-hash registry"
    );

    let moved = fx.project.parent().expect("parent").join("proj-renamed");
    std::fs::rename(&fx.project, &moved).expect("rename project directory");

    assert_sync_ok(&run(&fx, &moved, &["sync"]), "sync after move");

    assert_eq!(
        project_dirs(&fx).len(),
        2,
        "moving rehashes to a second registry beside the orphaned original"
    );
    assert!(
        has_records(&original),
        "the prior deployments are orphaned in the original registry (prune reclaims it)"
    );
}
