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

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.base.join("home"))
            .env("XDG_CACHE_HOME", self.base.join("xdg/cache"))
            .env("XDG_STATE_HOME", &self.xdg_state)
            .output()
            .expect("phora binary runs")
    }

    fn phora_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let out = self.run_in(cwd, args);
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

/// A committed git repo at `dir` holding `files`.
fn git_repo(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-b", "main", "."]);
    for (name, body) in files {
        write(&dir.join(name), body);
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "fixture"]);
    dir.to_path_buf()
}

fn write_project(dir: &Path, body: &str) -> PathBuf {
    write(&dir.join("phora.toml"), &format!("version = 1\n\n{body}"));
    dir.to_path_buf()
}

/// Config deploying `source` to `target` as a history worktree named `hist`.
fn history_config(paths: &str, source: &Path, target: &Path) -> String {
    format!(
        "{paths}\n[sources.hist]\npath = \"{}\"\nbranch = \"main\"\n\n\
         [targets.home]\npath = \"{}\"\nlayout = \"flat\"\n\n\
         [targets.home.sources.hist]\nhistory = true\n",
        source.display(),
        target.display()
    )
}

/// Config deploying the fixture's shared source to `target` by copy.
fn copy_config(fx: &Fixture, paths: &str, target: &Path) -> String {
    format!(
        "{paths}\n[sources.dotfiles]\npath = \"{}\"\nbranch = \"main\"\n\
         include = [\"editor\"]\n\n[targets.home]\npath = \"{}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        fx.source.display(),
        target.display()
    )
}

fn id_of(project: &Path) -> String {
    ProjectId::for_path(project)
        .expect("project id")
        .as_str()
        .to_owned()
}

fn output_text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Whether `text` names `path` as given or canonicalized.
fn mentions(text: &str, path: &Path) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    text.contains(str_of(path)) || text.contains(str_of(&canonical))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Every entry under `root`, keyed by its path relative to `root`.
fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Node> {
    let mut nodes = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|err| panic!("{} must be readable: {err}", path.display()));
        let node = if meta.is_symlink() {
            Node::Link(std::fs::read_link(&path).expect("read link"))
        } else if meta.is_dir() {
            for entry in std::fs::read_dir(&path).expect("read dir") {
                pending.push(entry.expect("dir entry").path());
            }
            Node::Dir
        } else {
            Node::File(std::fs::read(&path).expect("read file"))
        };
        let relative = path.strip_prefix(root).expect("under root").to_path_buf();
        nodes.insert(relative, node);
    }
    nodes
}

/// The one `--json` record describing the state dir `id`.
fn gc_record<'a>(records: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    let matching: Vec<_> = records.iter().filter(|r| r["id"] == id).collect();
    assert_eq!(
        matching.len(),
        1,
        "exactly one gc record must name {id}, got {records:#?}"
    );
    matching[0]
}

/// Content-store objects whose bytes equal `body`, aged past the prune grace period.
fn aged_content_objects(fx: &Fixture, body: &str) -> Vec<PathBuf> {
    let store = fx.base.join("xdg/cache/phora/content");
    let past_grace = std::time::SystemTime::now() - std::time::Duration::from_hours(2);
    let objects: Vec<PathBuf> = snapshot(&store)
        .into_iter()
        .filter(|(_, node)| *node == Node::File(body.as_bytes().to_vec()))
        .map(|(path, _)| store.join(path))
        .collect();
    for object in &objects {
        std::fs::File::open(object)
            .and_then(|file| file.set_modified(past_grace))
            .expect("setup: backdate content object");
    }
    objects
}

/// The mirror admin dir a history worktree's gitlink points at.
fn gitlink_admin(worktree: &Path) -> PathBuf {
    let link = std::fs::read_to_string(worktree.join(".git"))
        .unwrap_or_else(|err| panic!("{} must hold a gitlink: {err}", worktree.display()));
    PathBuf::from(
        link.trim()
            .strip_prefix("gitdir: ")
            .unwrap_or_else(|| panic!("gitlink must name its gitdir, got {link:?}")),
    )
}

/// A project `name` deploying its own source to `home/<name>` as a history worktree, synced.
fn synced_history_project(fx: &Fixture, name: &str) -> PathBuf {
    let source = git_repo(
        &fx.base.join(format!("{name}-src")),
        &[("file.txt", &format!("{name}\n"))],
    );
    let project = write_project(
        &fx.base.join(name),
        &history_config("", &source, &fx.base.join("home").join(name)),
    );
    fx.phora_in(&project, &["sync"]);
    project
}

/// The snapshot without the subtree at `excluded`.
fn outside(
    nodes: &std::collections::BTreeMap<PathBuf, Node>,
    excluded: &Path,
) -> Vec<(PathBuf, Node)> {
    nodes
        .iter()
        .filter(|(path, _)| !path.starts_with(excluded))
        .map(|(path, node)| (path.clone(), node.clone()))
        .collect()
}

struct RestoreMode(PathBuf, u32);

impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

#[test]
fn gc_dry_run_lists_every_non_live_dir_with_its_evidence_and_changes_nothing() {
    let fx = Fixture::new();
    fx.configure("", "home/deploy");
    fx.sync();

    let retired = fx.other_project(
        "rho",
        "[paths]\nstate = \"state\"\n",
        &fx.base.join("rho/out"),
    );
    fx.phora_in(&retired, &["sync"]);
    let stranded = fx.other_project("beta", "", &fx.base.join("home/beta"));
    fx.phora_in(&stranded, &["sync"]);
    let gone = fx.other_project("alpha", "", &fx.base.join("alpha/out"));
    fx.phora_in(&gone, &["sync"]);
    let unrooted = fx.other_project("sigma", "", &fx.base.join("home/sigma"));
    fx.phora_in(&unrooted, &["sync"]);
    let empty_id = "0123456789abcdef";
    std::fs::create_dir_all(fx.default_state_root().join("projects").join(empty_id))
        .expect("setup: empty state dir");

    let stranded_id = id_of(&stranded);
    let gone_id = id_of(&gone);
    std::fs::remove_file(retired.join("phora.toml")).expect("retire rho");
    std::fs::remove_dir_all(&stranded).expect("delete beta root");
    std::fs::remove_dir_all(&gone).expect("delete alpha root");
    std::fs::remove_file(state_dir_of(&unrooted, &fx.default_state_root()).join("project.toml"))
        .expect("setup: legacy dir without project.toml");

    let before = snapshot(&fx.base);
    let text = output_text(&fx.phora_in(&fx.base, &["gc", "--dry-run"]));
    let json = fx.phora_in(&fx.base, &["gc", "--dry-run", "--json"]);
    assert!(
        snapshot(&fx.base) == before,
        "a dry run changes no file under any state, cache or deploy root"
    );

    let live_id = id_of(&fx.project);
    assert!(
        !text.contains(&live_id),
        "a live dir is not listed, got:\n{text}"
    );
    for (id, reason) in [
        (id_of(&retired), "retired"),
        (stranded_id.clone(), "stranded"),
        (gone_id.clone(), "gone"),
        (id_of(&unrooted), "unrooted"),
        (empty_id.to_owned(), "empty"),
    ] {
        assert!(
            text.contains(&id) && text.contains(reason),
            "the plan lists {id} as {reason}, got:\n{text}"
        );
    }
    assert!(
        mentions(&text, &retired),
        "the retired record names its root, got:\n{text}"
    );

    let records = json_records(&json);
    assert!(
        records.iter().all(|r| r["id"] != live_id.as_str()),
        "no record for the live dir, got {records:#?}"
    );
    let canonical = |path: &Path| std::fs::canonicalize(path).expect("canonical path");
    let retired_record = gc_record(&records, &id_of(&retired));
    assert_eq!(retired_record["status"], "retired");
    assert_eq!(
        retired_record["root"].as_str(),
        Some(str_of(&canonical(&retired)))
    );
    assert!(
        retired_record["action"].is_string() || retired_record["action"].is_object(),
        "every record carries its planned action, got {retired_record:#}"
    );
    let stranded_record = gc_record(&records, &stranded_id);
    assert_eq!(stranded_record["status"], "stranded");
    let home_beta = canonical(&fx.base.join("home/beta"));
    let leftovers: Vec<PathBuf> = stranded_record["leftovers"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("a record lists its leftover destinations, got {stranded_record:#}")
        })
        .iter()
        .filter_map(|p| std::fs::canonicalize(p.as_str()?).ok())
        .collect();
    assert!(
        leftovers.iter().any(|p| p.starts_with(&home_beta)),
        "the stranded record lists the outside destination it leaves, got {stranded_record:#}"
    );
    assert_eq!(gc_record(&records, &gone_id)["status"], "gone");
    assert_eq!(gc_record(&records, empty_id)["status"], "empty");
    let unrooted_record = gc_record(&records, &id_of(&unrooted));
    assert_eq!(unrooted_record["status"], "unrooted");
    assert!(
        mentions(
            &unrooted_record["evidence"].to_string(),
            &fx.base.join("home/sigma")
        ),
        "unrooted evidence lists the recorded paths, got {unrooted_record:#}"
    );
}

#[test]
fn plain_gc_drops_only_drop_class_dirs_and_prunes_content_kept_by_registered_roots() {
    let fx = Fixture::new();
    let live_src = git_repo(&fx.base.join("live-src"), &[("live.txt", "live only\n")]);
    write_project(
        &fx.project,
        &format!(
            "[paths]\nstate = \"../custom-state\"\n\n[sources.live]\npath = \"{}\"\n\
             branch = \"main\"\n\n[targets.home]\npath = \"{}\"\nsources = [\"live\"]\n\
             layout = \"flat\"\n",
            live_src.display(),
            fx.base.join("home/live").display()
        ),
    );
    fx.sync();

    let gone_src = git_repo(&fx.base.join("gone-src"), &[("gone.txt", "gone only\n")]);
    let gone = write_project(
        &fx.base.join("alpha"),
        &format!(
            "[sources.gone]\npath = \"{}\"\nbranch = \"main\"\n\n[targets.home]\n\
             path = \"{}\"\nsources = [\"gone\"]\nlayout = \"flat\"\n",
            gone_src.display(),
            fx.base.join("alpha/out").display()
        ),
    );
    fx.phora_in(&gone, &["sync"]);

    let stranded = fx.base.join("beta");
    git_repo(&stranded.join("src"), &[("tool", "tool\n")]);
    write_project(
        &stranded,
        &format!(
            "[sources.links]\npath = \"src\"\nbranch = \"main\"\ndeploy = \"link\"\n\
             include = [\"tool\"]\n\n[sources.dotfiles]\npath = \"{}\"\nbranch = \"main\"\n\
             include = [\"editor\"]\n\n[targets.links]\npath = \"{}\"\nsources = [\"links\"]\n\n\
             [targets.home]\npath = \"{}\"\nsources = [\"dotfiles\"]\nlayout = \"flat\"\n",
            fx.source.display(),
            fx.base.join("home/beta-links").display(),
            fx.base.join("home/beta").display()
        ),
    );
    fx.phora_in(&stranded, &["sync"]);
    let link = fx.base.join("home/beta-links/tool");
    let outside_copy = fx.base.join("home/beta/editor/init.lua");
    assert!(
        std::fs::symlink_metadata(&link).is_ok_and(|m| m.is_symlink()),
        "setup: {} must be a link",
        link.display()
    );

    let unrooted = write_project(
        &fx.base.join("sigma"),
        &copy_config(&fx, "", &fx.base.join("home/sigma")),
    );
    fx.phora_in(&unrooted, &["sync"]);
    let empty_dir = fx.default_state_root().join("projects/0123456789abcdef");
    std::fs::create_dir_all(&empty_dir).expect("setup: empty state dir");

    let gone_dir = state_dir_of(&gone, &fx.default_state_root());
    let stranded_dir = state_dir_of(&stranded, &fx.default_state_root());
    let unrooted_dir = state_dir_of(&unrooted, &fx.default_state_root());
    let stranded_id = id_of(&stranded);
    std::fs::remove_dir_all(&gone).expect("delete alpha root");
    std::fs::remove_dir_all(&stranded).expect("delete beta root");
    std::fs::remove_file(unrooted_dir.join("project.toml")).expect("setup: legacy dir");

    let live_objects = aged_content_objects(&fx, "live only\n");
    let gone_objects = aged_content_objects(&fx, "gone only\n");
    assert!(
        !live_objects.is_empty() && !gone_objects.is_empty(),
        "setup: both deployments store content objects"
    );

    fx.phora_in(&fx.base, &["gc"]);

    assert_absent(&empty_dir, "plain gc drops an empty dir");
    assert_absent(&gone_dir, "plain gc drops a gone dir");
    assert_absent(
        &link,
        "plain gc releases a dangling link into a deleted root",
    );
    assert!(stranded_dir.is_dir(), "plain gc keeps a stranded dir");
    assert!(unrooted_dir.is_dir(), "plain gc keeps an unrooted dir");
    assert!(outside_copy.is_file(), "plain gc deletes no deployed file");
    for object in &gone_objects {
        assert_absent(object, "content only the dropped dir referenced is pruned");
    }
    for object in &live_objects {
        assert!(
            object.is_file(),
            "{} is referenced by a live dir in a registered state root",
            object.display()
        );
    }

    let text = output_text(&fx.phora_in(&fx.base, &["gc", "--eject"]));

    assert!(
        stranded_dir.is_dir() && unrooted_dir.is_dir(),
        "without a TTY or --yes nothing is ejected or dropped"
    );
    assert!(outside_copy.is_file(), "the outside copy stays");
    assert!(
        text.contains(&stranded_id) && text.contains("pending"),
        "the unconfirmed stranded dir is reported pending, got:\n{text}"
    );
}

#[test]
fn gc_eject_yes_ejects_release_class_worktrees_and_drops_dirs_without_deleting_deployed_files() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    let home = fx.base.join("home");

    let ejected = synced_history_project(&fx, "kappa");
    let ejected_tree = home.join("kappa/hist");
    let ejected_admin = gitlink_admin(&ejected_tree);

    let orphaned = synced_history_project(&fx, "lam");
    let orphaned_tree = home.join("lam/hist");
    let orphaned_gitlink = std::fs::read(orphaned_tree.join(".git")).expect("lam gitlink");
    let orphaned_mirror = gitlink_admin(&orphaned_tree)
        .ancestors()
        .nth(2)
        .expect("mirror of the admin dir")
        .to_path_buf();

    let moved_src = git_repo(&fx.base.join("nu-src"), &[("n.txt", "nu\n")]);
    let moved = fx.base.join("nu");
    write_project(
        &moved,
        &history_config("", &moved_src, &home.join("nu-first")),
    );
    fx.phora_in(&moved, &["sync"]);
    write_project(
        &moved,
        &history_config(
            "[paths]\nstate = \"../nu-state\"\n",
            &moved_src,
            &home.join("nu-second"),
        ),
    );
    fx.phora_in(&moved, &["sync"]);
    let old_copy = state_dir_of(&moved, &fx.default_state_root());
    let successor = state_dir_of(
        &moved,
        &std::fs::canonicalize(&fx.base)
            .expect("canonical base")
            .join("nu-state"),
    );
    let old_admin = gitlink_admin(&home.join("nu-first/hist"));
    let successor_admin = gitlink_admin(&home.join("nu-second/hist"));

    let legacy = write_project(&fx.base.join("mu"), &copy_config(&fx, "", &home.join("mu")));
    fx.phora_in(&legacy, &["sync"]);

    let unreachable = write_project(
        &fx.base.join("xi"),
        &copy_config(&fx, "", &fx.base.join("xi/out")),
    );
    fx.phora_in(&unreachable, &["sync"]);

    let dirs = [
        state_dir_of(&ejected, &fx.default_state_root()),
        state_dir_of(&orphaned, &fx.default_state_root()),
        old_copy.clone(),
        state_dir_of(&legacy, &fx.default_state_root()),
        state_dir_of(&unreachable, &fx.default_state_root()),
    ];
    std::fs::remove_dir_all(&ejected).expect("delete kappa root");
    std::fs::remove_dir_all(&orphaned).expect("delete lam root");
    std::fs::remove_dir_all(&orphaned_mirror).expect("remove lam's mirror");
    std::fs::remove_file(dirs[3].join("project.toml")).expect("setup: legacy dir");
    let sealed = RestoreMode(unreachable.clone(), 0o755);
    std::fs::set_permissions(&sealed.0, std::fs::Permissions::from_mode(0o000))
        .expect("setup: seal xi root");
    let successor_before = snapshot(&successor);
    let deployed_before = snapshot(&home);

    let text = output_text(&fx.phora_in(&fx.base, &["gc", "--eject", "--yes"]));

    for dir in &dirs {
        assert_absent(dir, "a confirmed eject drops every non-live dir");
    }
    assert!(
        snapshot(&successor) == successor_before,
        "the live successor's dir is untouched"
    );
    assert!(
        ejected_tree.join(".git").is_dir(),
        "a stranded history worktree becomes a standalone repository"
    );
    assert_absent(
        &ejected_admin,
        "the mirror holds no admin entry for the ejected worktree",
    );
    assert_eq!(
        std::fs::read(orphaned_tree.join(".git")).ok(),
        Some(orphaned_gitlink),
        "a worktree whose mirror is gone keeps its gitlink"
    );
    assert!(
        mentions(&text, &orphaned_tree),
        "the output lists the worktree left linked to a missing mirror, got:\n{text}"
    );
    assert!(
        old_admin.is_dir() && successor_admin.is_dir(),
        "an abandoned dir's worktree stays linked to the mirror"
    );
    assert!(
        mentions(&text, &ejected_tree),
        "the output lists the outside destinations left behind, got:\n{text}"
    );
    assert_eq!(
        outside(&snapshot(&home), Path::new("kappa/hist/.git")),
        outside(&deployed_before, Path::new("kappa/hist/.git")),
        "gc changes no deployed file outside the ejected gitdir"
    );
}

#[test]
fn gc_eject_keeps_and_reports_a_dir_whose_eject_fails_and_exits_non_zero() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    let stranded = synced_history_project(&fx, "kappa");
    let stranded_dir = state_dir_of(&stranded, &fx.default_state_root());
    let stranded_id = id_of(&stranded);
    let admin = gitlink_admin(&fx.base.join("home/kappa/hist"));
    std::fs::remove_dir_all(&stranded).expect("delete kappa root");
    let sealed = RestoreMode(admin, 0o755);
    std::fs::set_permissions(&sealed.0, std::fs::Permissions::from_mode(0o555))
        .expect("setup: seal the mirror admin dir");

    let out = fx.run_in(&fx.base, &["gc", "--eject", "--yes"]);
    let text = output_text(&out);

    assert!(
        stranded_dir.is_dir(),
        "a dir whose eject errors is kept, got:\n{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.contains(&stranded_id) && line.contains("error")),
        "the output reports the dir's eject error, got:\n{text}"
    );
    assert!(
        !out.status.success(),
        "an eject error exits non-zero, got {:?}:\n{text}",
        out.status
    );
}

#[test]
fn gc_unregisters_an_absent_unproven_state_root_only_once_confirmed() {
    let fx = Fixture::new();
    let project = fx.other_project(
        "omega",
        "[paths]\nstate = \"../omega-state\"\n",
        &fx.base.join("omega/out"),
    );
    fx.phora_in(&project, &["sync"]);
    let custom_root = std::fs::canonicalize(&fx.base)
        .expect("canonical base")
        .join("omega-state");
    let custom = str_of(&custom_root).to_owned();
    assert!(
        registered_roots(&fx).contains(&custom),
        "setup: the custom state root is registered"
    );
    // A root recorded off the default root's mount cannot be proven deleted.
    let index_path = fx.default_state_root().join("state-roots.toml");
    let mut index = read_toml(&index_path);
    for root in index
        .get_mut("roots")
        .and_then(toml::Value::as_array_mut)
        .expect("setup: registered roots")
    {
        if root.get("path").and_then(toml::Value::as_str) == Some(custom.as_str()) {
            root.as_table_mut()
                .expect("setup: root entry table")
                .insert("on_state_mount".to_owned(), toml::Value::Boolean(false));
        }
    }
    write(
        &index_path,
        &toml::to_string(&index).expect("setup: serialize index"),
    );
    std::fs::remove_dir_all(&custom_root).expect("remove the custom state root");

    let text = output_text(&fx.phora_in(&fx.base, &["gc", "--eject"]));

    assert!(
        registered_roots(&fx).contains(&custom),
        "without --yes the absent root stays registered"
    );
    assert!(
        mentions(&text, &custom_root),
        "the plan lists the absent root, got:\n{text}"
    );

    fx.phora_in(&fx.base, &["gc", "--eject", "--yes"]);

    assert!(
        !registered_roots(&fx).contains(&custom),
        "a confirmed eject unregisters the absent root, got {:?}",
        registered_roots(&fx)
    );
}

#[test]
fn gc_eject_yes_trashes_a_gone_dir_whose_confined_link_stays_and_lists_the_link() {
    let fx = Fixture::new();
    let canonical_base = std::fs::canonicalize(&fx.base).expect("canonical base");
    let confined = |name: &str| {
        let root = synced_linked_project(&fx, name, &format!("../{name}-shared"));
        let shared = fx.base.join(format!("{name}-shared"));
        let real_shared = fx.base.join(format!("{name}-real"));
        std::fs::rename(&shared, &real_shared).expect("setup: move deploy root");
        std::os::unix::fs::symlink(&real_shared, &shared).expect("setup: symlink deploy root");
        let dir = state_dir_of(&root, &fx.default_state_root());
        let id = id_of(&root);
        std::fs::remove_dir_all(&root).expect("delete linked root");
        let link_names = [
            shared.join("tool"),
            canonical_base.join(format!("{name}-shared/tool")),
        ];
        (dir, id, real_shared.join("tool"), link_names)
    };
    let is_link = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink());

    let (json_dir, json_id, json_link, json_names) = confined("epsilon");
    fx.phora_in(&fx.base, &["gc"]);
    assert!(json_dir.is_dir(), "plain gc keeps the dir");

    let records = json_records(&fx.phora_in(&fx.base, &["gc", "--eject", "--yes", "--json"]));

    assert_absent(&json_dir, "a confirmed eject trashes the dir");
    assert!(is_link(&json_link), "the confined link stays on disk");
    let record = gc_record(&records, &json_id);
    let leftovers: Vec<&str> = record["leftovers"]
        .as_array()
        .unwrap_or_else(|| panic!("the record lists its leftovers, got {record:#}"))
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        json_names
            .iter()
            .any(|name| leftovers.contains(&str_of(name))),
        "the --json leftovers list the kept link, got {record:#}"
    );

    let (text_dir, _, text_link, text_names) = confined("eta");
    let text = output_text(&fx.phora_in(&fx.base, &["gc", "--eject", "--yes"]));

    assert_absent(&text_dir, "a confirmed eject trashes the dir");
    assert!(is_link(&text_link), "the confined link stays on disk");
    assert!(
        text.lines().any(|line| {
            line.contains("leaves") && text_names.iter().any(|name| line.contains(str_of(name)))
        }),
        "the text output lists the kept link as left behind, got:\n{text}"
    );
}
