//! Random lifecycle sequences against the real binary, checked step by step for the invariants
//! every command must keep: orphans stay visible, sync keeps pins, local edits and ejected files
//! survive, `rm` leaves nothing behind, and a repeated sync changes nothing.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use proptest::prelude::*;

mod common;

use common::{run_git, run_phora};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Git,
    Package,
    Url,
}

/// `a` and `b` are plain git sources, `p` a transitive package, `u` a downloaded archive.
const SOURCES: [(&str, Kind); 4] = [
    ("a", Kind::Git),
    ("b", Kind::Git),
    ("p", Kind::Package),
    ("u", Kind::Url),
];
/// Target `t` always exists; `v` comes and goes.
const TARGETS: [(&str, &str); 2] = [("t", "dest"), ("v", "dest2")];
const ARTIFACTS: [usize; 3] = [1, 2, 3];

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
    Bind(usize, usize),
    Unbind(usize, usize),
    TargetAdd,
    TargetRm { force: bool },
    Sync,
    SyncPrune,
    SyncForce,
    Update,
    RebuildRegistry,
    UpstreamEdit(usize),
    UpstreamDrop(usize),
    UpstreamRestore(usize),
    EditDeployed(usize),
    DeleteDeployed(usize),
    Narrow(usize, bool),
    Eject(usize),
    Uneject(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let source = 0..SOURCES.len();
    let target = 0..TARGETS.len();
    prop_oneof![
        3 => source.clone().prop_map(Op::Add),
        1 => source.clone().prop_map(Op::Rm),
        1 => (source.clone(), target.clone()).prop_map(|(i, t)| Op::Bind(i, t)),
        1 => (source.clone(), target).prop_map(|(i, t)| Op::Unbind(i, t)),
        1 => Just(Op::TargetAdd),
        1 => any::<bool>().prop_map(|force| Op::TargetRm { force }),
        3 => Just(Op::Sync),
        1 => Just(Op::SyncPrune),
        1 => Just(Op::SyncForce),
        1 => Just(Op::Update),
        1 => Just(Op::RebuildRegistry),
        1 => source.clone().prop_map(Op::UpstreamEdit),
        1 => source.clone().prop_map(Op::UpstreamDrop),
        1 => source.clone().prop_map(Op::UpstreamRestore),
        1 => source.clone().prop_map(Op::EditDeployed),
        1 => source.clone().prop_map(Op::DeleteDeployed),
        1 => (source.clone(), any::<bool>()).prop_map(|(i, narrow)| Op::Narrow(i, narrow)),
        1 => source.clone().prop_map(Op::Eject),
        1 => source.prop_map(Op::Uneject),
    ]
}

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![Just(Mode::Copy), Just(Mode::Link), Just(Mode::History)]
}

/// Serves the current archive bytes for every request until the process ends.
fn serve(body: Arc<Mutex<Vec<u8>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("local address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let bytes = body.lock().expect("archive lock").clone();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&bytes);
        }
    });
    format!("http://{address}/u.tar")
}

/// What the test knows about config and the user's own changes; the binary owns everything else.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    modes: [Mode; 2],
    archive: Arc<Mutex<Vec<u8>>>,
    url: String,
    configured: BTreeSet<usize>,
    bound: BTreeSet<(usize, usize)>,
    second_target: bool,
    edits: BTreeMap<PathBuf, String>,
    ejected: BTreeMap<PathBuf, String>,
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
        for (name, kind) in SOURCES {
            let source = root.join("sources").join(name);
            for artifact in ARTIFACTS {
                let leaf = source.join(format!("{name}{artifact}"));
                fs::create_dir_all(&leaf).expect("leaf");
                fs::write(leaf.join("f.txt"), format!("{name}{artifact} r0\n")).expect("file");
            }
            if kind == Kind::Package {
                fs::write(
                    source.join("phora.toml"),
                    "version = 1\n[offers.default]\ninclude = [\"p1/**\", \"p2/**\", \"p3/**\"]\n",
                )
                .expect("package config");
            }
            if kind != Kind::Url {
                run_git(&source, &["init", "-b", "main", "--template="]);
                run_git(&source, &["add", "-A", "."]);
                run_git(&source, &["commit", "-m", "r0"]);
            }
        }
        let archive = Arc::new(Mutex::new(Vec::new()));
        let url = serve(Arc::clone(&archive));
        let world = Self {
            _dir: dir,
            root,
            project,
            modes,
            archive,
            url,
            configured: BTreeSet::new(),
            bound: BTreeSet::new(),
            second_target: false,
            edits: BTreeMap::new(),
            ejected: BTreeMap::new(),
            revision: 0,
            log: Vec::new(),
        };
        world.pack_archive();
        world.phora_ok(&["target", "add", "--path", TARGETS[0].1, TARGETS[0].0]);
        world
    }

    fn name(i: usize) -> &'static str {
        SOURCES[i].0
    }

    fn kind(i: usize) -> Kind {
        SOURCES[i].1
    }

    fn mode(&self, i: usize) -> Mode {
        self.modes.get(i).copied().unwrap_or(Mode::Copy)
    }

    fn source_dir(&self, i: usize) -> PathBuf {
        self.root.join("sources").join(Self::name(i))
    }

    fn target_present(&self, t: usize) -> bool {
        t == 0 || self.second_target
    }

    fn deployed(&self, i: usize, artifact: usize, t: usize) -> PathBuf {
        self.project
            .join(TARGETS[t].1)
            .join(format!("{}{artifact}", Self::name(i)))
    }

    fn pack_archive(&self) {
        let source = self.source_dir(3);
        let leaves: Vec<String> = ARTIFACTS
            .iter()
            .map(|artifact| format!("u{artifact}"))
            .filter(|leaf| source.join(leaf).exists())
            .collect();
        let output = std::process::Command::new("tar")
            .current_dir(&source)
            .arg("-cf")
            .arg("-")
            .args(&leaves)
            .env("COPYFILE_DISABLE", "1")
            .output()
            .expect("tar runs");
        assert!(output.status.success(), "tar packs the url archive");
        *self.archive.lock().expect("archive lock") = output.stdout;
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
                self.phora_ok(&["rm", Self::name(i)]);
                self.configured.remove(&i);
                self.bound.retain(|(source, _)| *source != i);
                self.check_rm_left_nothing(i);
            }
            Op::Bind(i, t)
                if self.configured.contains(&i)
                    && self.target_present(t)
                    && !self.bound.contains(&(i, t)) =>
            {
                let mut args = vec!["bind", Self::name(i), "--to", TARGETS[t].0];
                if matches!(self.mode(i), Mode::History) {
                    args.push("--history");
                }
                self.phora_ok(&args);
                self.bound.insert((i, t));
            }
            Op::Unbind(i, t) if self.bound.contains(&(i, t)) => self.unbind(i, t),
            Op::TargetAdd if !self.second_target => {
                self.phora_ok(&["target", "add", "--path", TARGETS[1].1, TARGETS[1].0]);
                self.second_target = true;
            }
            Op::TargetRm { force } if self.second_target => self.remove_target(force),
            Op::Sync => self.sync_twice(&["sync"]),
            Op::SyncPrune => self.sync_twice(&["sync", "--prune"]),
            Op::SyncForce => {
                self.forget_edits_force_may_overwrite();
                self.sync_twice(&["sync", "--force"]);
            }
            Op::Update => self.sync_twice(&["update"]),
            Op::RebuildRegistry if self.project.join("phora.lock").exists() => {
                self.phora_ok(&["rebuild-registry"]);
            }
            Op::UpstreamEdit(i) => self.change_upstream(i, |world, source| {
                let file = source.join(format!("{}1/f.txt", Self::name(i)));
                fs::write(file, format!("{}1 r{}\n", Self::name(i), world.revision))
                    .expect("upstream edit");
            }),
            Op::UpstreamDrop(i) => {
                let leaf = self.source_dir(i).join(format!("{}2", Self::name(i)));
                if leaf.exists() {
                    self.change_upstream(i, |_, _| {
                        fs::remove_dir_all(&leaf).expect("upstream drop");
                    });
                }
            }
            Op::UpstreamRestore(i) => {
                let leaf = self.source_dir(i).join(format!("{}2", Self::name(i)));
                if !leaf.exists() {
                    self.change_upstream(i, |_, _| {
                        fs::create_dir_all(&leaf).expect("restore leaf");
                        fs::write(leaf.join("f.txt"), "restored\n").expect("restore file");
                    });
                }
            }
            Op::EditDeployed(i) if !matches!(self.mode(i), Mode::Link) => self.edit_deployed(i),
            Op::DeleteDeployed(i) => {
                let deployed = self.deployed(i, 1, 0);
                match fs::symlink_metadata(&deployed) {
                    Ok(meta) if meta.is_dir() => fs::remove_dir_all(&deployed).expect("delete"),
                    Ok(_) => fs::remove_file(&deployed).expect("delete"),
                    Err(_) => {}
                }
                self.edits.retain(|path, _| !path.starts_with(&deployed));
                self.ejected.retain(|path, _| !path.starts_with(&deployed));
            }
            Op::Narrow(i, narrow)
                if self.configured.contains(&i)
                    && Self::kind(i) != Kind::Package
                    && !matches!(self.mode(i), Mode::History) =>
            {
                self.narrow(i, narrow);
            }
            Op::Eject(i) if Self::kind(i) == Kind::Git && self.bound.contains(&(i, 0)) => {
                self.eject(i);
            }
            Op::Uneject(i) => {
                let deployed = self.deployed(i, 1, 0);
                if self.ejected.remove(&deployed).is_some() {
                    let artifact = format!("{}1", Self::name(i));
                    self.phora_ok(&[
                        "uneject",
                        &artifact,
                        "--source",
                        Self::name(i),
                        "--target",
                        "t",
                    ]);
                }
            }
            _ => {}
        }
        self.check_invariants();
    }

    fn add(&mut self, i: usize) {
        match Self::kind(i) {
            Kind::Git => {
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
                if matches!(self.mode(i), Mode::History) {
                    args.push("--history");
                }
                self.phora_ok(&args);
                if matches!(self.mode(i), Mode::Link) {
                    let table = format!("[sources.{}]\n", Self::name(i));
                    self.replace_in_config(&table, &format!("{table}deploy = \"link\"\n"));
                }
            }
            Kind::Package => {
                self.append_config(&format!(
                    "\n[sources.p]\npath = {:?}\nbranch = \"main\"\ntransitive = true\n",
                    self.source_dir(i).to_string_lossy()
                ));
                self.phora_ok(&["bind", "p", "--to", "t"]);
            }
            Kind::Url => {
                self.append_config(&format!("\n[sources.u]\nurl = {:?}\n", self.url));
                self.phora_ok(&["bind", "u", "--to", "t"]);
            }
        }
        self.configured.insert(i);
        self.bound.insert((i, 0));
    }

    /// A transitive source must stay bound somewhere, so phora refuses to unbind its last binding.
    fn unbind(&mut self, i: usize, t: usize) {
        let last = self.bound.iter().filter(|(source, _)| *source == i).count() == 1;
        let (ok, text) = self.phora(&["unbind", Self::name(i), "--from", TARGETS[t].0]);
        if Self::kind(i) == Kind::Package && last {
            assert!(
                !ok && text.contains("a transitive source must be bound"),
                "unbinding the last binding of a transitive source must be refused\n{text}\n{}",
                self.history()
            );
            return;
        }
        assert!(ok, "unbind failed\n{text}\n{}", self.history());
        self.bound.remove(&(i, t));
    }

    fn append_config(&self, text: &str) {
        let config = self.project.join("phora.toml");
        let mut current = fs::read_to_string(&config).expect("config");
        current.push_str(text);
        fs::write(config, current).expect("config");
    }

    fn replace_in_config(&self, from: &str, to: &str) {
        let config = self.project.join("phora.toml");
        let text = fs::read_to_string(&config).expect("config");
        assert!(text.contains(from), "premise: config contains {from}");
        fs::write(&config, text.replacen(from, to, 1)).expect("config");
    }

    /// Selects only the first artifact of source `i`, or every artifact again.
    fn narrow(&self, i: usize, narrow: bool) {
        let table = format!("[sources.{}]\n", Self::name(i));
        let narrowed = format!("{table}include = [\"{}1/**\"]\n", Self::name(i));
        let config = fs::read_to_string(self.project.join("phora.toml")).expect("config");
        match (narrow, config.contains(&narrowed)) {
            (true, false) => self.replace_in_config(&table, &narrowed),
            (false, true) => self.replace_in_config(&narrowed, &table),
            _ => {}
        }
    }

    fn edit_deployed(&mut self, i: usize) {
        let file = self.deployed(i, 1, 0).join("f.txt");
        let is_file = fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
        let ejected = self.ejected.keys().any(|path| file.starts_with(path));
        if is_file && !ejected {
            self.revision += 1;
            let content = format!("local edit {}\n", self.revision);
            fs::write(&file, &content).expect("local edit");
            self.edits.insert(file, content);
        }
    }

    fn eject(&mut self, i: usize) {
        let deployed = self.deployed(i, 1, 0);
        let artifact = format!("{}1", Self::name(i));
        let recorded = self.records().iter().any(|(source, record, target)| {
            source == Self::name(i) && *record == artifact && target == "t"
        });
        if recorded && !self.ejected.contains_key(&deployed) {
            self.phora_ok(&[
                "eject",
                &artifact,
                "--source",
                Self::name(i),
                "--target",
                "t",
            ]);
            self.edits.retain(|path, _| !path.starts_with(&deployed));
            self.ejected.insert(deployed.clone(), describe(&deployed));
        }
    }

    /// `--force` may overwrite edits in artifacts a bound source still deploys.
    fn forget_edits_force_may_overwrite(&mut self) {
        let bound: Vec<PathBuf> = self
            .bound
            .iter()
            .flat_map(|(i, t)| ARTIFACTS.iter().map(move |artifact| (*i, *artifact, *t)))
            .map(|(i, artifact, t)| self.deployed(i, artifact, t))
            .collect();
        self.edits
            .retain(|path, _| !bound.iter().any(|deployed| path.starts_with(deployed)));
    }

    fn remove_target(&mut self, force: bool) {
        let mut args = vec!["target", "rm", TARGETS[1].0];
        if force {
            args.push("--force");
        }
        let (ok, text) = self.phora(&args);
        if !ok {
            assert!(
                !force && text.contains("still has deployed artifacts registered"),
                "target rm failed for another reason\n{text}\n{}",
                self.history()
            );
            return;
        }
        self.second_target = false;
        self.bound.retain(|(_, t)| *t != 1);
    }

    fn change_upstream(&mut self, i: usize, change: impl FnOnce(&Self, &Path)) {
        self.revision += 1;
        let source = self.source_dir(i);
        change(self, &source);
        if Self::kind(i) == Kind::Url {
            self.pack_archive();
        } else {
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
            .position(|(source, _)| *source == name)
            .is_none_or(|i| !matches!(self.mode(i), Mode::Link))
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
        for (_, path) in TARGETS {
            out.push_str(&describe(&self.project.join(path)));
        }
        out
    }

    /// Every registry record as `(source, artifact, target)`.
    fn records(&self) -> Vec<(String, String, String)> {
        let mut records = Vec::new();
        let mut current: Option<(String, String)> = None;
        for line in self.phora_ok(&["where"]).lines() {
            if let Some(rest) = line.strip_prefix("Artifact: ") {
                let key = rest.split_whitespace().next().unwrap_or_default();
                current = key
                    .split_once('/')
                    .map(|(source, artifact)| (source.to_owned(), artifact.to_owned()));
            } else if let (Some(target), Some((source, artifact))) =
                (line.trim().strip_prefix("- "), &current)
            {
                let target = target.trim_end_matches(" (ejected)");
                records.push((source.clone(), artifact.clone(), target.to_owned()));
            }
        }
        records
    }

    fn is_bound(&self, source: &str, target: &str) -> bool {
        let Some(i) = SOURCES.iter().position(|(name, _)| *name == source) else {
            return false;
        };
        if target.contains('%') {
            return self.bound.iter().any(|(bound, _)| *bound == i);
        }
        TARGETS
            .iter()
            .position(|(name, _)| *name == target)
            .is_some_and(|t| self.target_present(t) && self.bound.contains(&(i, t)))
    }

    fn check_invariants(&self) {
        let orphans = self.phora_ok(&["list", "--orphans"]);
        for (source, artifact, target) in self.records() {
            if self.is_bound(&source, &target) {
                continue;
            }
            let listed = orphans.lines().any(|line| {
                line.contains(&format!("{source}/{artifact} "))
                    && line.contains(&format!("[was {target}]"))
            });
            assert!(
                listed,
                "record {source}/{artifact} in {target} is neither bound nor listed as an \
                 orphan\n{orphans}\n{}",
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
        for (path, snapshot) in &self.ejected {
            assert_eq!(
                &describe(path),
                snapshot,
                "phora changed the ejected {}\n{}",
                path.display(),
                self.history()
            );
        }
    }

    fn check_rm_left_nothing(&self, i: usize) {
        let kept = |path: &Path| {
            self.edits.keys().any(|edit| edit.starts_with(path))
                || self.ejected.keys().any(|ejected| ejected.starts_with(path))
        };
        let worktrees = registered_worktrees(&self.project.join("cache/phora/git"));
        for t in 0..TARGETS.len() {
            for artifact in ARTIFACTS {
                let deployed = self.deployed(i, artifact, t);
                if kept(&deployed) {
                    continue;
                }
                assert!(
                    fs::symlink_metadata(&deployed).is_err(),
                    "rm left {} on disk\n{}",
                    deployed.display(),
                    self.history()
                );
                assert!(
                    !worktrees.contains(deployed.to_str().expect("utf-8 path")),
                    "rm left the worktree {} registered\n{worktrees}\n{}",
                    deployed.display(),
                    self.history()
                );
            }
        }
        if !self.edits.is_empty() || !self.ejected.is_empty() {
            return;
        }
        let left: Vec<_> = self
            .records()
            .into_iter()
            .filter(|(source, _, _)| source == Self::name(i))
            .collect();
        assert!(
            left.is_empty(),
            "rm left records {left:?}\n{}",
            self.history()
        );
    }
}

fn describe(path: &Path) -> String {
    let mut out = String::new();
    describe_into(path, &mut out);
    out
}

fn describe_into(path: &Path, out: &mut String) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(path).expect("link target");
        let _ = writeln!(out, "{} -> {}", path.display(), target.display());
    } else if meta.is_dir() {
        let _ = writeln!(out, "{}/", path.display());
        let mut entries: Vec<_> = fs::read_dir(path)
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            describe_into(&entry, out);
        }
    } else {
        let content = fs::read(path).expect("file content");
        let _ = writeln!(
            out,
            "{} {:?}",
            path.display(),
            String::from_utf8_lossy(&content)
        );
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
