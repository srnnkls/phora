//! A `phora.local.toml` override shadows a source without unpinning it from the
//! shared `phora.lock`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    _upstream: TempDir,
    checkout: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

fn build_repo(root: &Path, body: &str) {
    git(root, &["init", "-q", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    write(&root.join("notes/readme.md"), body);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "fixture"]);
}

fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let upstream = TempDir::new().expect("upstream tempdir");
    let checkout = TempDir::new().expect("checkout tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    build_repo(upstream.path(), "upstream\n");
    build_repo(checkout.path(), "checkout\n");

    let config = format!(
        "version = 1\n\n[sources.notes]\npath = \"{upstream}\"\nbranch = \"main\"\n\n\
         [targets.resources]\npath = \"resources\"\nlayout = \"by-source\"\nsources = [\"notes\"]\n",
        upstream = upstream.path().display(),
    );
    write(&cwd.path().join("phora.toml"), &config);

    let home_path = home.path().to_path_buf();
    Fixture {
        _home: home,
        _upstream: upstream,
        checkout,
        cwd,
        home_path,
    }
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(self.cwd.path())
            .env("HOME", &self.home_path)
            .env("XDG_CACHE_HOME", self.home_path.join("xdg/cache"))
            .env("XDG_STATE_HOME", self.home_path.join("xdg/state"))
            .output()
            .expect("phora binary runs")
    }

    fn sync(&self, args: &[&str]) {
        let out = self.run(&[&["sync"], args].concat());
        assert!(
            out.status.success(),
            "phora sync {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn lock(&self) -> String {
        std::fs::read_to_string(self.cwd.path().join("phora.lock")).expect("read phora.lock")
    }

    fn set_local_override(&self) {
        write(
            &self.cwd.path().join("phora.local.toml"),
            &format!(
                "[sources.notes]\npath = \"{}\"\ndeploy = \"link\"\n",
                self.checkout.path().display()
            ),
        );
    }
}

#[test]
fn local_override_keeps_the_shared_pin_in_phora_lock() {
    let fixture = build_fixture();
    fixture.sync(&[]);
    let pinned = fixture.lock();
    assert!(
        pinned.contains("name = \"notes\""),
        "base sync pins notes:\n{pinned}"
    );

    fixture.set_local_override();
    fixture.sync(&[]);

    assert_eq!(
        fixture.lock(),
        pinned,
        "a local override must not rewrite the shared lock"
    );
}

#[test]
fn frozen_sync_under_a_local_override_leaves_phora_lock_untouched() {
    let fixture = build_fixture();
    fixture.sync(&[]);
    let pinned = fixture.lock();

    fixture.set_local_override();
    fixture.sync(&["--frozen"]);

    assert_eq!(fixture.lock(), pinned);
}

#[test]
fn frozen_sync_without_the_override_deploys_from_the_kept_pin() {
    let fixture = build_fixture();
    fixture.sync(&[]);
    fixture.set_local_override();
    fixture.sync(&[]);

    std::fs::remove_file(fixture.cwd.path().join("phora.local.toml")).expect("drop override");
    std::fs::remove_file(fixture.cwd.path().join("phora.local.lock")).expect("drop local lock");
    fixture.sync(&["--frozen", "--force"]);

    let deployed = fixture.cwd.path().join("resources/notes/notes/readme.md");
    assert_eq!(
        std::fs::read_to_string(&deployed).expect("read deployed file"),
        "upstream\n"
    );
}
