#![cfg(unix)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

mod common;

use common::{run_git, run_phora, run_phora_ok};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// Two local git sources; `editor` is synced into `dest` and pinned, `extra` is not declared yet.
    fn synced() -> Self {
        let dir = tempfile::tempdir().expect("fixture directory");
        let root = dir.path().to_path_buf();
        for name in ["editor", "extra"] {
            let source = root.join(name);
            fs::create_dir_all(source.join(name)).expect("source tree");
            fs::write(source.join(name).join("init.lua"), format!("-- {name}\n"))
                .expect("source file");
            run_git(&source, &["init", "-b", "main", "--template="]);
            run_git(&source, &["add", name]);
            run_git(&source, &["commit", "-m", "fixture"]);
        }
        let fixture = Self { _dir: dir, root };
        fixture.write_config(&["editor"]);
        run_phora_ok(&fixture.root, &["sync", "--no-hooks"]);
        fixture
    }

    fn write_config(&self, sources: &[&str]) {
        let mut declared = String::new();
        for name in sources {
            let path = self.root.join(name);
            let path = path.to_str().expect("source path");
            writeln!(
                declared,
                "[sources.{name}]\npath = {path:?}\nbranch = \"main\""
            )
            .expect("write to string");
        }
        fs::write(
            self.root.join("phora.toml"),
            format!(
                "version = 1\n[paths]\ncache = \"cache\"\nstate = \"state\"\n{declared}\
                 [targets.dest]\npath = {:?}\nsources = {sources:?}\n",
                self.root.join("dest").to_str().expect("dest path"),
            ),
        )
        .expect("config");
    }

    fn lock(&self) -> String {
        fs::read_to_string(self.root.join("phora.lock")).expect("lock")
    }

    fn empty_cache(&self) {
        fs::rename(self.root.join("cache"), self.root.join("cache-before")).expect("move cache");
    }

    fn deployed(&self, name: &str) -> PathBuf {
        self.root.join("dest").join(name)
    }
}

fn stderr_of(root: &Path, args: &[&str]) -> String {
    let output = run_phora(root, args);
    assert!(!output.status.success(), "phora {args:?} must fail");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn locked_sync_fetches_a_pinned_commit_missing_from_the_cache() {
    let fixture = Fixture::synced();
    let lock = fixture.lock();
    fixture.empty_cache();

    run_phora_ok(&fixture.root, &["sync", "--locked", "--no-hooks"]);

    assert_eq!(
        fixture.lock(),
        lock,
        "--locked fetches the pin without moving it"
    );
}

#[test]
fn frozen_sync_refuses_a_pinned_commit_missing_from_the_cache() {
    let fixture = Fixture::synced();
    fixture.empty_cache();

    stderr_of(&fixture.root, &["sync", "--frozen", "--no-hooks"]);
}

#[test]
fn locked_sync_refuses_a_source_the_lock_does_not_pin() {
    let fixture = Fixture::synced();
    let lock = fixture.lock();
    fixture.write_config(&["editor", "extra"]);

    let stderr = stderr_of(&fixture.root, &["sync", "--locked", "--no-hooks"]);

    assert!(
        stderr.contains(
            "source `extra` is not pinned in the lock; --locked requires every source to be pinned"
        ),
        "the refusal names the source and the flag; got:\n{stderr}"
    );
    assert_eq!(fixture.lock(), lock, "a refused sync writes no lock");
    assert!(
        !fixture.deployed("extra").exists(),
        "nothing of the unpinned source deploys"
    );

    run_phora_ok(&fixture.root, &["sync", "--no-hooks"]);
    assert!(
        fixture.deployed("extra").exists(),
        "a plain sync resolves it"
    );
}
