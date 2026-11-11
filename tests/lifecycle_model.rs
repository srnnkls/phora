//! Random lifecycle sequences against the real binary, checked step by step for the invariants
//! every command must keep: orphans stay visible, sync keeps pins, local edits survive, `rm`
//! leaves nothing behind, and a repeated sync changes nothing.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;

mod common;

use common::{run_git, run_phora};

const SOURCES: [&str; 2] = ["a", "b"];
const TARGET_DIR: &str = "dest";

#[derive(Clone, Copy, Debug)]
enum Mode {
    Copy,
    Link,
    History,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Add(usize),
    Rm(usize),
    Unbind(usize),
    Bind(usize),
    Sync,
    SyncPrune,
    Update,
    RebuildRegistry,
    UpstreamEdit(usize),
    UpstreamDrop(usize),
    UpstreamRestore(usize),
    EditDeployed(usize),
    DeleteDeployed(usize),
    Narrow(usize, bool),
}

fn op() -> impl Strategy<Value = Op> {
    let source = 0..SOURCES.len();
    prop_oneof![
        3 => source.clone().prop_map(Op::Add),
        1 => source.clone().prop_map(Op::Rm),
        1 => source.clone().prop_map(Op::Unbind),
        1 => source.clone().prop_map(Op::Bind),
        3 => Just(Op::Sync),
        1 => Just(Op::SyncPrune),
        1 => Just(Op::Update),
        1 => Just(Op::RebuildRegistry),
        1 => source.clone().prop_map(Op::UpstreamEdit),
        1 => source.clone().prop_map(Op::UpstreamDrop),
        1 => source.clone().prop_map(Op::UpstreamRestore),
        1 => source.clone().prop_map(Op::EditDeployed),
        1 => source.clone().prop_map(Op::DeleteDeployed),
        1 => (source, any::<bool>()).prop_map(|(i, narrow)| Op::Narrow(i, narrow)),
    ]
}

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![Just(Mode::Copy), Just(Mode::Link), Just(Mode::History)]
}

/// What the test knows about config and the user's own edits; the binary owns everything else.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    modes: [Mode; 2],
    configured: BTreeSet<usize>,
    bound: BTreeSet<usize>,
    edits: BTreeMap<PathBuf, String>,
    revision: usize,
    log: Vec<String>,
}

impl World {
    fn new(modes: [Mode; 2]) -> Self {
        let dir = tempfile::tempdir().expect("world directory");
        let root = dir.path().canonicalize().expect("canonical root");
        let project = root.join("proj");
        fs::create_dir_all(&project).expect("project");
        fs::write(project.join("phora.toml"), "version = 1\n").expect("config");
        for name in SOURCES {
            let source = root.join("sources").join(name);
            for artifact in [1, 2] {
                let leaf = source.join(format!("{name}{artifact}"));
                fs::create_dir_all(&leaf).expect("leaf");
                fs::write(leaf.join("f.txt"), format!("{name}{artifact} r0\n")).expect("file");
            }
            run_git(&source, &["init", "-b", "main", "--template="]);
            run_git(&source, &["add", &format!("{name}1"), &format!("{name}2")]);
            run_git(&source, &["commit", "-m", "r0"]);
        }
        let world = Self {
            _dir: dir,
            root,
            project,
            modes,
            configured: BTreeSet::new(),
            bound: BTreeSet::new(),
            edits: BTreeMap::new(),
            revision: 0,
            log: Vec::new(),
        };
        world.phora_ok(&["target", "add", "--path", TARGET_DIR, "t"]);
        world
    }

    fn source_dir(&self, index: usize) -> PathBuf {
        self.root.join("sources").join(SOURCES[index])
    }

    fn deployed(&self, index: usize, artifact: usize) -> PathBuf {
        self.project
            .join(TARGET_DIR)
            .join(format!("{}{artifact}", SOURCES[index]))
    }

    fn phora(&self, args: &[&str]) -> (bool, String) {
        let output = run_phora(&self.project, args);
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    }

    fn phora_ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.phora(args);
        assert!(ok, "phora {args:?} failed\n{}\n{text}", self.history());
        text
    }

    fn history(&self) -> String {
        let mut out = format!("modes {:?}; steps:\n", self.modes);
        for step in &self.log {
            let _ = writeln!(out, "  {step}");
        }
        out
    }

    fn apply(&mut self, op: Op) {
        self.log.push(format!("{op:?}"));
        match op {
            Op::Add(i) if !self.configured.contains(&i) => self.add(i),
            Op::Rm(i) if self.configured.contains(&i) => {
                self.phora_ok(&["rm", SOURCES[i]]);
                self.configured.remove(&i);
                self.bound.remove(&i);
                self.check_rm_left_nothing(i);
            }
            Op::Unbind(i) if self.bound.contains(&i) => {
                self.phora_ok(&["unbind", SOURCES[i], "--from", "t"]);
                self.bound.remove(&i);
            }
            Op::Bind(i) if self.configured.contains(&i) && !self.bound.contains(&i) => {
                let mut args = vec!["bind", SOURCES[i], "--to", "t"];
                if matches!(self.modes[i], Mode::History) {
                    args.push("--history");
                }
                self.phora_ok(&args);
                self.bound.insert(i);
            }
            Op::Sync => self.sync_twice(&["sync"]),
            Op::SyncPrune => self.sync_twice(&["sync", "--prune"]),
            Op::Update => self.sync_twice(&["update"]),
            Op::RebuildRegistry if self.project.join("phora.lock").exists() => {
                self.phora_ok(&["rebuild-registry"]);
            }
            Op::UpstreamEdit(i) => self.commit_upstream(i, |world, source| {
                let file = source.join(format!("{}1/f.txt", SOURCES[i]));
                fs::write(file, format!("{}1 r{}\n", SOURCES[i], world.revision))
                    .expect("upstream edit");
            }),
            Op::UpstreamDrop(i) => {
                let leaf = self.source_dir(i).join(format!("{}2", SOURCES[i]));
                if leaf.exists() {
                    self.commit_upstream(i, |_, _| {
                        fs::remove_dir_all(&leaf).expect("upstream drop");
                    });
                }
            }
            Op::UpstreamRestore(i) => {
                let leaf = self.source_dir(i).join(format!("{}2", SOURCES[i]));
                if !leaf.exists() {
                    self.commit_upstream(i, |_, _| {
                        fs::create_dir_all(&leaf).expect("restore leaf");
                        fs::write(leaf.join("f.txt"), "restored\n").expect("restore file");
                    });
                }
            }
            Op::EditDeployed(i) if !matches!(self.modes[i], Mode::Link) => {
                let file = self.deployed(i, 1).join("f.txt");
                let is_file = fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
                if is_file {
                    self.revision += 1;
                    let content = format!("local edit {}\n", self.revision);
                    fs::write(&file, &content).expect("local edit");
                    self.edits.insert(file, content);
                }
            }
            Op::DeleteDeployed(i) => {
                let deployed = self.deployed(i, 1);
                match fs::symlink_metadata(&deployed) {
                    Ok(meta) if meta.is_dir() => fs::remove_dir_all(&deployed).expect("delete"),
                    Ok(_) => fs::remove_file(&deployed).expect("delete"),
                    Err(_) => {}
                }
                self.edits.retain(|path, _| !path.starts_with(&deployed));
            }
            Op::Narrow(i, narrow)
                if self.configured.contains(&i) && !matches!(self.modes[i], Mode::History) =>
            {
                self.narrow(i, narrow);
            }
            _ => {}
        }
        self.check_invariants();
    }

    fn add(&mut self, i: usize) {
        let source = self.source_dir(i).to_string_lossy().into_owned();
        let mut args = vec![
            "add",
            source.as_str(),
            "--branch",
            "main",
            "--to",
            "t",
            "-y",
        ];
        if matches!(self.modes[i], Mode::History) {
            args.push("--history");
        }
        self.phora_ok(&args);
        if matches!(self.modes[i], Mode::Link) {
            let config = self.project.join("phora.toml");
            let table = format!("[sources.{}]\n", SOURCES[i]);
            let text = fs::read_to_string(&config).expect("config");
            assert!(text.contains(&table), "premise: add wrote {table}");
            fs::write(
                &config,
                text.replace(&table, &format!("{table}deploy = \"link\"\n")),
            )
            .expect("config");
        }
        self.configured.insert(i);
        self.bound.insert(i);
    }

    /// Selects only the first artifact of source `i`, or every artifact again.
    fn narrow(&self, i: usize, narrow: bool) {
        let config = self.project.join("phora.toml");
        let table = format!("[sources.{}]\n", SOURCES[i]);
        let include = format!("include = [\"{}1/**\"]\n", SOURCES[i]);
        let text = fs::read_to_string(&config).expect("config");
        let widened = text.replace(&format!("{table}{include}"), &table);
        let text = if narrow {
            widened.replace(&table, &format!("{table}{include}"))
        } else {
            widened
        };
        fs::write(&config, text).expect("config");
    }

    fn commit_upstream(&mut self, i: usize, change: impl FnOnce(&Self, &Path)) {
        self.revision += 1;
        let source = self.source_dir(i);
        change(self, &source);
        run_git(&source, &["add", "-A", "."]);
        run_git(
            &source,
            &[
                "commit",
                "--allow-empty",
                "-m",
                &format!("r{}", self.revision),
            ],
        );
    }

    /// A sync, then a second plain sync that must change nothing the first left behind.
    fn sync_twice(&self, first: &[&str]) {
        let lock_before = self.lock_pins();
        self.phora_ok(first);
        let pinned_after = self.lock_pins();
        if first != ["update"] {
            for (name, commit) in &lock_before {
                if let Some(after) = pinned_after.get(name)
                    && self.pin_is_stable(name)
                {
                    assert_eq!(
                        after,
                        commit,
                        "`{}` moved the pin of {name}\n{}",
                        first.join(" "),
                        self.history()
                    );
                }
            }
        }
        let settled = self.snapshot();
        self.phora_ok(&["sync"]);
        assert_eq!(
            self.snapshot(),
            settled,
            "a repeated sync changed state after `{}`\n{}",
            first.join(" "),
            self.history()
        );
    }

    /// A link source pins its live working tree, so its lock entry follows upstream commits.
    fn pin_is_stable(&self, name: &str) -> bool {
        SOURCES
            .iter()
            .position(|source| *source == name)
            .is_none_or(|i| !matches!(self.modes[i], Mode::Link))
    }

    fn lock_pins(&self) -> BTreeMap<String, String> {
        let Ok(text) = fs::read_to_string(self.project.join("phora.lock")) else {
            return BTreeMap::new();
        };
        let lock: phora::lock::Lock = toml::from_str(&text).expect("lock parses");
        lock.sources
            .into_iter()
            .filter(|entry| entry.instance.is_none())
            .map(|entry| (entry.name, entry.commit))
            .collect()
    }

    fn snapshot(&self) -> String {
        let mut out = self.phora_ok(&["where"]);
        out.push_str(&fs::read_to_string(self.project.join("phora.lock")).unwrap_or_default());
        describe_tree(&self.project.join(TARGET_DIR), &mut out);
        out
    }

    fn records(&self) -> BTreeSet<String> {
        self.phora_ok(&["where"])
            .lines()
            .filter_map(|line| line.strip_prefix("Artifact: "))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(str::to_owned)
            .collect()
    }

    fn check_invariants(&self) {
        let orphans = self.phora_ok(&["list", "--orphans"]);
        for record in self.records() {
            let source = record.split('/').next().unwrap_or_default();
            let bound = SOURCES
                .iter()
                .position(|name| *name == source)
                .is_some_and(|i| self.bound.contains(&i));
            assert!(
                bound || orphans.contains(&record),
                "record {record} is neither bound nor listed as an orphan\n{orphans}\n{}",
                self.history()
            );
        }
        for (path, content) in &self.edits {
            assert_eq!(
                fs::read_to_string(path).ok().as_deref(),
                Some(content.as_str()),
                "a local edit at {} was lost\n{}",
                path.display(),
                self.history()
            );
        }
    }

    fn check_rm_left_nothing(&self, i: usize) {
        let kept: BTreeSet<&Path> = self.edits.keys().filter_map(|path| path.parent()).collect();
        for artifact in [1, 2] {
            let deployed = self.deployed(i, artifact);
            if kept.contains(deployed.as_path()) {
                continue;
            }
            assert!(
                fs::symlink_metadata(&deployed).is_err(),
                "rm left {} on disk\n{}",
                deployed.display(),
                self.history()
            );
            let record = format!("{0}/{0}{artifact}", SOURCES[i]);
            assert!(
                !self.records().contains(&record),
                "rm left the record {record}\n{}",
                self.history()
            );
            let worktrees = registered_worktrees(&self.project.join("cache/phora/git"));
            assert!(
                !worktrees.contains(deployed.to_str().expect("utf-8 path")),
                "rm left the worktree {} registered\n{worktrees}\n{}",
                deployed.display(),
                self.history()
            );
        }
    }
}

fn describe_tree(dir: &Path, out: &mut String) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let meta = fs::symlink_metadata(&path).expect("entry metadata");
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).expect("link target");
            let _ = writeln!(out, "{} -> {}", path.display(), target.display());
        } else if meta.is_dir() {
            let _ = writeln!(out, "{}/", path.display());
            describe_tree(&path, out);
        } else {
            let content = fs::read(&path).expect("file content");
            let _ = writeln!(
                out,
                "{} {:?}",
                path.display(),
                String::from_utf8_lossy(&content)
            );
        }
    }
}

fn registered_worktrees(mirrors: &Path) -> String {
    let Ok(entries) = fs::read_dir(mirrors) else {
        return String::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "git"))
        .map(|mirror| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&mirror)
                .args(["worktree", "list"])
                .output()
                .expect("git worktree list");
            String::from_utf8_lossy(&output.stdout).into_owned()
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn lifecycle_sequences_keep_the_invariants(
        modes in [mode(), mode()],
        ops in proptest::collection::vec(op(), 1..24),
    ) {
        let mut world = World::new(modes);
        for op in ops {
            world.apply(op);
        }
    }
}
