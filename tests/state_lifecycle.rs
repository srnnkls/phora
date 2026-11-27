//! State-dir lifecycle: recorded roots and registered state roots.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        self.phora_in(&self.project, args);
    }

    fn phora_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.base.join("home"))
            .env("XDG_CACHE_HOME", self.base.join("xdg/cache"))
            .env("XDG_STATE_HOME", &self.xdg_state)
            .output()
            .expect("phora binary runs");
        assert!(
            out.status.success(),
            "phora {args:?} in {} failed:\n{}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// A second project under the fixture base, deploying the shared source to `target`.
    fn other_project(&self, name: &str, paths: &str, target: &Path) -> PathBuf {
        let dir = self.base.join(name);
        let config = format!(
            "version = 1\n\n{paths}\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\
             include = [\"editor\"]\n\n[targets.home]\npath = \"{target}\"\n\
             sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
            src = self.source.display(),
            target = target.display(),
        );
        write(&dir.join("phora.toml"), &config);
        dir
    }

    fn default_state_root(&self) -> PathBuf {
        self.xdg_state.join("phora")
    }

    fn state_dir(&self, state_root: &Path) -> PathBuf {
        state_dir_of(&self.project, state_root)
    }
}

fn state_dir_of(project: &Path, state_root: &Path) -> PathBuf {
    let id = ProjectId::for_path(project).expect("project id");
    state_root.join("projects").join(id.as_str())
}

fn json_records(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("stdout line {line:?} must be JSON: {err}"))
        })
        .collect()
}

/// The `--json` warning message naming `phora gc`, asserted to precede the summary record.
fn gc_warning_before_summary(records: &[serde_json::Value]) -> String {
    let summary = records
        .iter()
        .position(|record| record["type"] == "summary")
        .expect("sync --json ends with a summary record");
    let warning = records
        .iter()
        .position(|record| {
            record["type"] == "warning"
                && record["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("phora gc"))
        })
        .unwrap_or_else(|| panic!("a warning record must name `phora gc`, got {records:#?}"));
    assert!(
        warning < summary,
        "the sweep warning must precede the summary record, got {records:#?}"
    );
    records[warning]["message"]
        .as_str()
        .expect("warning message")
        .to_owned()
}

fn has_count(message: &str, count: &str) -> bool {
    message
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == count)
}

fn assert_absent(path: &Path, why: &str) {
    let err = std::fs::symlink_metadata(path)
        .err()
        .unwrap_or_else(|| panic!("{} must be gone: {why}", path.display()));
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "{}",
        path.display()
    );
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
    assert_absent(
        &default_copy,
        "a recordless copy left by a `[paths].state` change is dropped",
    );
}

#[test]
fn sweep_drops_gone_dirs_in_every_state_root_and_reports_the_stranded_one() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");

    let gone = fx.other_project(
        "alpha",
        "[paths]\nstate = \"../alpha-state\"\n",
        &fx.base.join("alpha/out"),
    );
    fx.phora_in(&gone, &["sync"]);
    let gone_root = std::fs::canonicalize(&fx.base)
        .expect("canonical base")
        .join("alpha-state");
    let gone_dir = state_dir_of(&gone, &gone_root);
    assert!(
        gone_dir.is_dir(),
        "setup: {} must exist",
        gone_dir.display()
    );

    let outside_file = fx.base.join("home/beta/editor/init.lua");
    let stranded = fx.other_project("beta", "", &fx.base.join("home/beta"));
    fx.phora_in(&stranded, &["sync"]);
    let stranded_dir = state_dir_of(&stranded, &fx.default_state_root());
    assert!(
        outside_file.is_file(),
        "setup: {} must be deployed",
        outside_file.display()
    );

    std::fs::remove_dir_all(&gone).expect("delete gone root");
    std::fs::remove_dir_all(&stranded).expect("delete stranded root");

    let out = fx.phora_in(&fx.project, &["sync", "--json"]);

    assert_absent(
        &gone_dir,
        "a root-deleted dir without outside footprint is dropped",
    );
    assert!(
        stranded_dir.is_dir(),
        "a deleted root whose outside file remains keeps its state dir"
    );
    assert!(
        outside_file.is_file(),
        "the sweep never removes a deployed file"
    );
    for trash in [
        fx.default_state_root().join("trash"),
        gone_root.join("trash"),
    ] {
        let leftovers: Vec<_> = std::fs::read_dir(&trash)
            .map(|entries| {
                entries
                    .map(|e| e.expect("trash entry").file_name())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "{} must be empty, holds {leftovers:?}",
            trash.display()
        );
    }
    let index = read_toml(&fx.default_state_root().join("state-roots.toml"));
    let registered: Vec<&str> = index
        .get("roots")
        .and_then(toml::Value::as_array)
        .map(|roots| {
            roots
                .iter()
                .filter_map(|r| r.get("path")?.as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !registered.contains(&str_of(&gone_root)),
        "a registered root left without state dirs is removed from the index, got {registered:?}"
    );

    let message = gc_warning_before_summary(&json_records(&out));
    assert!(
        message.contains("stranded") && has_count(&message, "1"),
        "the warning counts one stranded project, got {message:?}"
    );
}

#[test]
fn own_run_removes_its_empty_state_dir_at_lock_release() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");

    fx.phora(&["trust", "--list"]);

    assert_absent(
        &fx.state_dir(&fx.default_state_root()),
        "a locked run that leaves no records, target meta or journal removes its dir",
    );
}

#[test]
fn sweep_releases_dangling_links_into_a_deleted_root_and_drops_its_dir() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");

    let linked = fx.base.join("gamma");
    let source = linked.join("src");
    std::fs::create_dir_all(&source).expect("in-root source");
    git(&source, &["init", "-b", "main", "."]);
    write(&source.join("tool"), "tool\n");
    write(&source.join("other"), "other\n");
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-m", "fixture"]);
    let destination = fx.base.join("home/gamma");
    write(
        &linked.join("phora.toml"),
        &format!(
            "version = 1\n\n[sources.live]\npath = \"src\"\nbranch = \"main\"\n\
             deploy = \"link\"\ninclude = [\"tool\", \"other\"]\n\n\
             [targets.home]\npath = \"{}\"\nsources = [\"live\"]\n",
            destination.display()
        ),
    );
    fx.phora_in(&linked, &["sync"]);
    let links = [destination.join("tool"), destination.join("other")];
    for link in &links {
        let meta = std::fs::symlink_metadata(link).expect("setup: deployed link");
        assert!(
            meta.is_symlink(),
            "setup: {} must be a link",
            link.display()
        );
    }
    let linked_dir = state_dir_of(&linked, &fx.default_state_root());

    std::fs::remove_dir_all(&linked).expect("delete linked root");

    let out = fx.phora_in(&fx.project, &["sync", "--json"]);

    for link in &links {
        assert_absent(link, "a dangling link into the deleted root is released");
    }
    assert_absent(
        &linked_dir,
        "a gone dir whose links all released is dropped",
    );
    let message = gc_warning_before_summary(&json_records(&out));
    assert!(
        message.contains("link") && has_count(&message, "2"),
        "the warning counts the two released links, got {message:?}"
    );
}

fn synced_linked_project(fx: &Fixture, name: &str, target: &str) -> PathBuf {
    let root = fx.base.join(name);
    let source = root.join("src");
    std::fs::create_dir_all(&source).expect("in-root source");
    git(&source, &["init", "-b", "main", "."]);
    write(&source.join("tool"), "tool\n");
    write(&source.join("other"), "other\n");
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-m", "fixture"]);
    write(
        &root.join("phora.toml"),
        &format!(
            "version = 1\n\n[sources.live]\npath = \"src\"\nbranch = \"main\"\n\
             deploy = \"link\"\ninclude = [\"tool\", \"other\"]\n\n\
             [targets.home]\npath = \"{target}\"\nsources = [\"live\"]\n"
        ),
    );
    fx.phora_in(&root, &["sync"]);
    root
}

fn trash_names(trash: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(trash)
        .unwrap_or_else(|err| panic!("{} must be readable: {err}", trash.display()))
        .map(|entry| {
            entry
                .expect("trash entry")
                .file_name()
                .into_string()
                .expect("UTF-8 trash entry")
        })
        .collect();
    names.sort();
    names
}

fn registered_roots(fx: &Fixture) -> Vec<String> {
    read_toml(&fx.default_state_root().join("state-roots.toml"))
        .get("roots")
        .and_then(toml::Value::as_array)
        .map(|roots| {
            roots
                .iter()
                .filter_map(|r| Some(r.get("path")?.as_str()?.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn sweep_keeps_repointed_links_and_links_under_a_symlinked_deploy_root() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");

    let repointed_root = synced_linked_project(&fx, "delta", str_of(&fx.base.join("home/delta")));
    let repointed = fx.base.join("home/delta/tool");
    let users_file = fx.base.join("users-tool");
    write(&users_file, "user's own\n");
    std::fs::remove_file(&repointed).expect("setup: unlink deployed link");
    std::os::unix::fs::symlink(&users_file, &repointed).expect("setup: re-point link");

    let shadowed_root = synced_linked_project(&fx, "epsilon", "../shared");
    let shared = fx.base.join("shared");
    let real_shared = fx.base.join("real-shared");
    assert!(
        std::fs::symlink_metadata(shared.join("tool")).is_ok_and(|m| m.is_symlink()),
        "setup: epsilon links into {}",
        shared.display()
    );
    std::fs::rename(&shared, &real_shared).expect("setup: move deploy root");
    std::os::unix::fs::symlink(&real_shared, &shared).expect("setup: symlink deploy root");
    let shadowed = real_shared.join("tool");

    let dirs = [
        state_dir_of(&repointed_root, &fx.default_state_root()),
        state_dir_of(&shadowed_root, &fx.default_state_root()),
    ];
    std::fs::remove_dir_all(&repointed_root).expect("delete delta root");
    std::fs::remove_dir_all(&shadowed_root).expect("delete epsilon root");

    let out = fx.phora_in(&fx.project, &["sync", "--json"]);

    assert_eq!(
        std::fs::read_link(&repointed).ok(),
        Some(users_file),
        "a link the user re-pointed stays"
    );
    assert!(
        std::fs::symlink_metadata(&shadowed).is_ok_and(|m| m.is_symlink()),
        "a link under a symlinked deploy root stays"
    );
    for dir in &dirs {
        assert!(
            dir.is_dir(),
            "{} keeps its kept link's record",
            dir.display()
        );
    }
    let message = gc_warning_before_summary(&json_records(&out));
    assert!(
        message.contains("stranded") && has_count(&message, "2"),
        "the warning counts both dirs stranded, got {message:?}"
    );
}

#[test]
fn sweep_skips_a_locked_dead_dir_and_a_frozen_run_sweeps_nothing() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");
    fx.sync();

    let dead = fx.other_project("zeta", "", &fx.base.join("zeta/out"));
    fx.phora_in(&dead, &["sync"]);
    let dead_dir = state_dir_of(&dead, &fx.default_state_root());
    std::fs::remove_dir_all(&dead).expect("delete dead root");

    fx.phora(&["sync", "--frozen"]);
    assert!(dead_dir.is_dir(), "a frozen run sweeps nothing");

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dead_dir.join("locks/state.lock"))
        .expect("setup: dead dir's state.lock");
    held.try_lock().expect("test holds the dead dir's lock");

    let out = fx.phora_in(&fx.project, &["sync", "--json"]);

    assert!(dead_dir.is_dir(), "a dir whose lock is held is skipped");
    let message = gc_warning_before_summary(&json_records(&out));
    assert!(
        message.contains("locked") && has_count(&message, "1"),
        "the warning counts one dir skipped while locked, got {message:?}"
    );
    drop(held);
}

#[test]
fn sweep_retries_trash_leftovers_and_keeps_foreign_entries_and_their_root_registered() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    fx.configure("", "home/deploy");

    let gone = fx.other_project(
        "alpha",
        "[paths]\nstate = \"../alpha-state\"\n",
        &fx.base.join("alpha/out"),
    );
    fx.phora_in(&gone, &["sync"]);
    let gone_root = std::fs::canonicalize(&fx.base)
        .expect("canonical base")
        .join("alpha-state");
    let gone_dir = state_dir_of(&gone, &gone_root);
    let trash = gone_root.join("trash");
    write(&trash.join("old-notes/note.txt"), "mine\n");
    let id = ProjectId::for_path(&gone).expect("project id");
    let leftover = format!("{}.stuck", id.as_str());
    let sealed = trash.join(&leftover).join("sealed");
    write(&sealed.join("file"), "undeletable\n");
    let set_mode = |mode| {
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(mode))
            .expect("chmod sealed dir");
    };
    set_mode(0o555);
    std::fs::remove_dir_all(&gone).expect("delete gone root");

    let out = fx.phora_in(&fx.project, &["sync", "--json"]);
    let trash_after_failure = trash_names(&trash);
    let registered_after_failure = registered_roots(&fx);
    set_mode(0o755);

    assert_absent(&gone_dir, "the gone dir is dropped");
    assert_eq!(
        trash_after_failure,
        vec![leftover, "old-notes".to_owned()],
        "only the undeletable phora entry and the foreign entry remain"
    );
    assert!(
        registered_after_failure.contains(&str_of(&gone_root).to_owned()),
        "a root whose trash still holds a phora entry stays registered, got {registered_after_failure:?}"
    );
    let message = gc_warning_before_summary(&json_records(&out));
    assert!(
        message.contains("trash") && has_count(&message, "1"),
        "the warning counts one trash entry left behind, got {message:?}"
    );

    fx.sync();

    assert_eq!(
        trash_names(&trash),
        vec!["old-notes".to_owned()],
        "the next sweep deletes the leftover and keeps the foreign entry"
    );
    assert!(
        !registered_roots(&fx).contains(&str_of(&gone_root).to_owned()),
        "a root holding only foreign trash is unregistered"
    );
}
