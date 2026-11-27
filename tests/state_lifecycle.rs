//! State-dir lifecycle: recorded roots and registered state roots.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use phora::sync::state::ProjectId;
use tempfile::TempDir;

mod common;

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

struct Fixture {
    _tmp: TempDir,
    base: PathBuf,
    project: PathBuf,
    source: PathBuf,
    xdg_state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path().to_path_buf();
        let source = base.join("source");
        std::fs::create_dir_all(&source).expect("source dir");
        git(&source, &["init", "-b", "main", "."]);
        write(&source.join("editor/init.lua"), "-- init\n");
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-m", "fixture"]);
        let project = base.join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        Self {
            xdg_state: base.join("xdg/state"),
            _tmp: tmp,
            base,
            project,
            source,
        }
    }

    fn configure(&self, paths: &str, target: &str) {
        let config = format!(
            "version = 1\n\n{paths}\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
             include = [\"editor\"]\n\n[targets.home]\npath = \"{target}\"\n\
             sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
            src = self.source.display(),
            target = self.base.join(target).display(),
        );
        write(&self.project.join("phora.toml"), &config);
    }

    fn sync(&self) {
        self.phora(&["sync"]);
    }

    fn phora(&self, args: &[&str]) {
        let out = Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", self.base.join("home"))
            .env("XDG_CACHE_HOME", self.base.join("xdg/cache"))
            .env("XDG_STATE_HOME", &self.xdg_state)
            .output()
            .expect("phora binary runs");
        assert!(
            out.status.success(),
            "phora {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn default_state_root(&self) -> PathBuf {
        self.xdg_state.join("phora")
    }

    fn state_dir(&self, state_root: &Path) -> PathBuf {
        let id = ProjectId::for_path(&self.project).expect("project id");
        state_root.join("projects").join(id.as_str())
    }
}

fn read_toml(path: &Path) -> toml::Table {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{} must exist: {err}", path.display()));
    text.parse()
        .unwrap_or_else(|err| panic!("{} must parse as TOML: {err}", path.display()))
}

fn path_field<'a>(table: &'a toml::Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(toml::Value::as_str)
}

fn str_of(path: &Path) -> &str {
    path.to_str().expect("UTF-8 temp path")
}

#[test]
fn synced_project_records_root_mount_and_state_dir() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");
    fx.sync();

    let state_dir = fx.state_dir(&fx.default_state_root());
    let meta = read_toml(&state_dir.join("project.toml"));
    let canonical_root = std::fs::canonicalize(&fx.project).expect("canonical root");

    assert_eq!(path_field(&meta, "root"), Some(str_of(&canonical_root)));
    assert_eq!(
        meta.get("on_state_mount").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(path_field(&meta, "state_dir"), Some(str_of(&state_dir)));
    assert_eq!(meta.get("superseded_by"), None);
}

#[test]
fn moving_to_relative_custom_state_root_registers_it_and_supersedes_default_copy() {
    let fx = Fixture::new();
    fx.configure("", "home/first");
    fx.sync();

    let default_copy = fx.state_dir(&fx.default_state_root());
    let _ = std::fs::remove_file(default_copy.join("project.toml"));

    fx.configure("[paths]\nstate = \"../state\"\n", "home/second");
    fx.sync();

    let canonical_root = std::fs::canonicalize(&fx.project).expect("canonical root");
    let custom_root = std::fs::canonicalize(&fx.base)
        .expect("canonical base")
        .join("state");
    let custom_copy = fx.state_dir(&custom_root);

    let index = read_toml(&fx.default_state_root().join("state-roots.toml"));
    assert_eq!(
        path_field(&index, "default_root"),
        Some(str_of(&fx.default_state_root()))
    );
    let roots: Vec<(Option<&str>, Option<bool>)> = index
        .get("roots")
        .and_then(toml::Value::as_array)
        .expect("index lists registered roots")
        .iter()
        .map(|entry| {
            (
                entry.get("path").and_then(toml::Value::as_str),
                entry.get("on_state_mount").and_then(toml::Value::as_bool),
            )
        })
        .collect();
    assert_eq!(roots, vec![(Some(str_of(&custom_root)), Some(true))]);

    let current = read_toml(&custom_copy.join("project.toml"));
    assert_eq!(path_field(&current, "root"), Some(str_of(&canonical_root)));
    assert_eq!(
        path_field(&current, "state_dir"),
        Some(str_of(&custom_copy))
    );
    assert_eq!(current.get("superseded_by"), None);

    let old = read_toml(&default_copy.join("project.toml"));
    assert_eq!(path_field(&old, "root"), Some(str_of(&canonical_root)));
    assert_eq!(
        old.get("on_state_mount").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(path_field(&old, "state_dir"), Some(str_of(&default_copy)));
    assert_eq!(
        path_field(&old, "superseded_by"),
        Some(str_of(&custom_root))
    );
}

#[test]
fn local_state_root_through_symlinked_parent_keeps_records_and_meta_in_one_unmarked_dir() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");
    let outside = fx.base.join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::os::unix::fs::symlink(&outside, fx.project.join("link")).expect("symlink");
    write(
        &fx.project.join("phora.local.toml"),
        "[paths]\nstate = \"link/../state\"\n",
    );
    fx.sync();
    fx.phora(&["trust", "--list"]);

    let custom_root = std::fs::canonicalize(&fx.project)
        .expect("canonical project")
        .join("state");
    let copy = fx.state_dir(&custom_root);
    assert!(
        copy.join("targets").is_dir(),
        "records must live in the recorded state dir {}",
        copy.display()
    );
    let meta = read_toml(&copy.join("project.toml"));
    assert_eq!(path_field(&meta, "state_dir"), Some(str_of(&copy)));
    assert_eq!(meta.get("superseded_by"), None);
    assert!(!fx.state_dir(&fx.default_state_root()).exists());
}

#[test]
fn recordless_copy_in_default_root_is_not_marked() {
    let fx = Fixture::new();
    let default_copy = fx.state_dir(&fx.default_state_root());
    std::fs::create_dir_all(&default_copy).expect("recordless copy");

    fx.configure("[paths]\nstate = \"../state\"\n", "home/deploy");
    fx.sync();

    assert!(!default_copy.join("project.toml").exists());
}
