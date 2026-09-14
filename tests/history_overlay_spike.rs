use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

use phora::config::Config;
use phora::source::GitBackend;
use phora::sync::state::{ArtifactRecord, FileStateStore, StateStore};
use phora::sync::{
    Concurrency, ConflictPolicy, HookPolicy, LockSet, MovedPinPolicy, PrunePolicy, SourcePolicy,
    SyncOptions, SyncRequest,
};

mod common;

const MATERIALIZED_MTIME: i64 = 1_700_000_000;

fn git_output(cwd: &Path, args: &[&str]) -> Output {
    common::assert_sandboxed(cwd);
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
        .output()
        .expect("git runs")
}

fn git(cwd: &Path, args: &[&str]) {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git emits utf-8")
        .trim()
        .to_owned()
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent");
    }
    std::fs::write(path, bytes).expect("write fixture file");
}

fn materialize(mirror: &Path, pin: &str, target: &Path, submodule: bool) {
    std::fs::create_dir_all(target).expect("create deployment target");
    let archive = git_output(mirror, &["archive", pin]);
    assert!(
        archive.status.success(),
        "git archive {pin} failed: {}",
        String::from_utf8_lossy(&archive.stderr)
    );
    let mut tar = Command::new("tar")
        .args(["-x", "-C"])
        .arg(target)
        .stdin(Stdio::piped())
        .spawn()
        .expect("tar runs");
    tar.stdin
        .take()
        .expect("tar has stdin")
        .write_all(&archive.stdout)
        .expect("pipe archive to tar");
    assert!(
        tar.wait().expect("wait for tar").success(),
        "tar extracts archive"
    );
    if submodule {
        let vendor = target.join("vendor");
        std::fs::create_dir_all(&vendor).expect("represent gitlink as empty directory");
        assert!(
            std::fs::read_dir(&vendor)
                .expect("read empty gitlink directory")
                .next()
                .is_none(),
            "the gitlink fixture is represented by an empty directory"
        );
    }
    set_deterministic_mtimes(target);
}

fn set_deterministic_mtimes(path: &Path) {
    for entry in std::fs::read_dir(path).expect("read materialized directory") {
        let entry = entry.expect("read materialized entry");
        let path = entry.path();
        if path.is_dir() {
            set_deterministic_mtimes(&path);
        }
        filetime::set_file_mtime(
            &path,
            filetime::FileTime::from_unix_time(MATERIALIZED_MTIME, 0),
        )
        .expect("set deterministic materialized mtime");
    }
}

fn link_worktree(mirror: &Path, target: &Path, name: &str, pin: &str) {
    let admin = mirror.join("worktrees").join(name);
    std::fs::create_dir_all(&admin).expect("create linked-worktree admin directory");
    write(&admin.join("HEAD"), format!("{pin}\n").as_bytes());
    write(&admin.join("commondir"), b"../..\n");
    write(
        &admin.join("gitdir"),
        format!("{}\n", target.join(".git").display()).as_bytes(),
    );
    write(
        &target.join(".git"),
        format!("gitdir: {}\n", admin.display()).as_bytes(),
    );

    let repo = gix::open(mirror).expect("open bare mirror with gix");
    let commit = repo
        .find_commit(gix::ObjectId::from_hex(pin.as_bytes()).expect("pin is an object id"))
        .expect("pinned commit is in mirror");
    let tree = commit.tree().expect("pinned commit has a tree");
    let mut index = repo
        .index_from_tree(&tree.id)
        .expect("build index from pinned tree");
    for (entry, path) in index.entries_mut_with_paths() {
        let path = std::str::from_utf8(path.as_ref()).expect("fixture path is utf-8");
        let metadata = gix::index::fs::Metadata::from_path_no_follow(&target.join(path))
            .expect("final deployed path exists");
        entry.stat = gix::index::entry::Stat::from_fs(&metadata).expect("stat deployed path");
    }
    index.set_path(admin.join("index"));
    index
        .write(gix::index::write::Options::default())
        .expect("write linked-worktree index");
}

fn fixture_repository(fixture: &TempDir) -> (PathBuf, String, String, String) {
    let source = fixture.path().join("source");
    std::fs::create_dir(&source).expect("create source");
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.email", "test@example.com"]);
    git(&source, &["config", "user.name", "Test"]);
    git(&source, &["config", "core.autocrlf", "false"]);
    write(&source.join("plain.txt"), b"one\n");
    git(&source, &["add", "plain.txt"]);
    git(&source, &["commit", "-m", "base"]);
    let base = git_stdout(&source, &["rev-parse", "HEAD"]);

    git(
        &source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{base},vendor"),
        ],
    );
    let tree = git_stdout(&source, &["write-tree"]);
    let pin_one = git_stdout(
        &source,
        &["commit-tree", &tree, "-p", &base, "-m", "gitlink"],
    );
    git(&source, &["update-ref", "refs/heads/main", &pin_one]);
    git(&source, &["reset", "--hard", &pin_one]);

    write(&source.join("second.txt"), b"two\n");
    git(&source, &["add", "second.txt"]);
    git(&source, &["commit", "-m", "plain second"]);
    let pin_two = git_stdout(&source, &["rev-parse", "HEAD"]);

    write(&source.join(".gitattributes"), b"* text=auto eol=crlf\n");
    git(&source, &["add", ".gitattributes"]);
    git(&source, &["commit", "-m", "attributes"]);
    write(&source.join("filtered.txt"), b"filtered\r\n");
    let filtered_blob = git_stdout(
        &source,
        &["hash-object", "-w", "--no-filters", "filtered.txt"],
    );
    git(
        &source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{filtered_blob},filtered.txt"),
        ],
    );
    git(&source, &["commit", "-m", "filtered"]);
    let filtered_pin = git_stdout(&source, &["rev-parse", "HEAD"]);

    let mirror = fixture.path().join("mirror.git");
    git(
        fixture.path(),
        &[
            "clone",
            "--bare",
            source.to_str().expect("utf-8 path"),
            mirror.to_str().expect("utf-8 path"),
        ],
    );
    (mirror, pin_one, pin_two, filtered_pin)
}

#[test]
fn phora_built_linked_worktree_admin_is_accepted_by_git() {
    let fixture = TempDir::new().expect("fixture tempdir");
    let (mirror, pin_one, pin_two, filtered_pin) = fixture_repository(&fixture);

    let first = fixture.path().join("deployment-one");
    materialize(&mirror, &pin_one, &first, true);
    link_worktree(&mirror, &first, "ph-x", &pin_one);

    assert_eq!(
        git_stdout(&first, &["status", "--porcelain=v1"]),
        "",
        "the index stat must describe final deployed paths, including an empty gitlink directory"
    );
    assert_eq!(git_stdout(&first, &["log", "-1", "--format=%H"]), pin_one);

    let second = fixture.path().join("deployment-two");
    materialize(&mirror, &pin_two, &second, false);
    link_worktree(&mirror, &second, "ph-y", &pin_two);
    assert_eq!(
        git_stdout(&second, &["status", "--porcelain=v1"]),
        "",
        "the second plain deployment must also have clean status"
    );
    assert_eq!(git_stdout(&second, &["log", "-1", "--format=%H"]), pin_two);

    let filtered = fixture.path().join("deployment-filtered");
    materialize(&mirror, &filtered_pin, &filtered, false);
    link_worktree(&mirror, &filtered, "ph-z", &filtered_pin);
    assert_eq!(
        git_stdout(&filtered, &["log", "-1", "--format=%H"]),
        filtered_pin
    );
    assert_eq!(
        std::fs::read(filtered.join("filtered.txt")).expect("read raw archived filtered file"),
        b"filtered\r\n"
    );
    let filtered_tree_blob = git_stdout(
        &filtered,
        &["rev-parse", &format!("{filtered_pin}:filtered.txt")],
    );
    assert_ne!(
        git_stdout(
            &filtered,
            &["hash-object", "--path=filtered.txt", "filtered.txt"],
        ),
        filtered_tree_blob,
        "the raw archive copy must exercise the declared eol clean filter"
    );

    let worktrees = git_stdout(&mirror, &["worktree", "list", "--porcelain"]);
    for (target, pin) in [
        (&first, &pin_one),
        (&second, &pin_two),
        (&filtered, &filtered_pin),
    ] {
        assert!(
            worktrees.contains(&format!("worktree {}", target.display()))
                && worktrees.contains(&format!("HEAD {pin}")),
            "git worktree list must include {} at {pin}, got:\n{worktrees}",
            target.display()
        );
    }
}

fn sync_options() -> SyncOptions {
    SyncOptions {
        source_policy: SourcePolicy::Refresh,
        conflict_policy: ConflictPolicy::Refuse,
        prune_policy: PrunePolicy::KeepOrphans,
        hook_policy: HookPolicy::None,
        moved_pin_policy: MovedPinPolicy::Seal,
        concurrency: Concurrency::default(),
    }
}

fn sync_history(
    config: &Config,
    registry: &FileStateStore,
    backend: &GitBackend,
    options: SyncOptions,
) {
    phora::sync::sync(
        &SyncRequest {
            base_config: config,
            local_config: None,
            locks: LockSet::default(),
            options,
            resolver: None,
        },
        backend,
        registry,
    )
    .expect("history sync succeeds");
}

fn assert_legacy_record_defaults() {
    let legacy: ArtifactRecord = toml::from_str(
        "version = 1\n\
         key = { target = \"home\", source = \"ordinary\", artifact = \"artifact\" }\n\
         source = \"ordinary\"\n\
         commit = \"abc\"\n\
         digest = \"blake3:abc\"\n\
         projected_at = \"2026-01-01T00:00:00Z\"\n\
         layout = \"flat\"\n\
         allow_symlinks = false\n\
         preserve_executable = true\n\
         files = []\n",
    )
    .expect("legacy record deserializes");
    assert!(
        !legacy.history
            && legacy.worktree_admin_id.is_none()
            && legacy.mirror_key.is_none()
            && legacy.cache_git_root.is_none(),
        "legacy and ordinary records leave the history address absent"
    );
}

#[test]
fn history_whole_root_sync_publishes_the_overlay_and_complete_record_address() {
    let fixture = TempDir::new().expect("fixture tempdir");
    let (mirror, _, _, _) = fixture_repository(&fixture);
    let deploy_root = fixture.path().join("deployment");
    let config = Config::parse(&format!(
        "version = 1\n\
         [sources.history]\n\
         git = \"{}\"\n\n\
         [targets.home]\n\
         path = \"{}\"\n\n\
         [targets.home.sources]\n\
         history = {{ history = true }}\n",
        mirror.display(),
        deploy_root.display()
    ))
    .expect("history config parses");
    config.validate().expect("history config validates");
    let registry = FileStateStore::open(fixture.path().join("state")).expect("open registry");
    let backend = GitBackend::new(fixture.path().join("cache"));

    sync_history(&config, &registry, &backend, sync_options());

    let history_root = deploy_root.join("history");
    assert!(
        history_root.join(".git").is_file(),
        "history root has a gitlink"
    );
    assert!(
        history_root.join("vendor").is_dir(),
        "history gitlinks materialize as empty directories"
    );
    let record = registry
        .all_artifacts()
        .expect("read history records")
        .pop()
        .expect("history sync persists one record");
    let _: Option<String> = record.worktree_admin_id.clone();
    let _: Option<String> = record.mirror_key.clone();
    let _: Option<String> = record.cache_git_root.clone();
    assert!(
        record.history,
        "history deployment persists a history record"
    );
    assert!(
        record.worktree_admin_id.is_some()
            && record.mirror_key.is_some()
            && record.cache_git_root.is_some(),
        "a history record persists its complete linked-worktree address"
    );
    assert_eq!(
        git_stdout(&history_root, &["status", "--porcelain=v1"]),
        "",
        "a phora-deployed history root is clean to real git"
    );
    assert_eq!(
        git_stdout(&history_root, &["log", "-1", "--format=%H"]),
        record.commit,
        "the deployed history root exposes the recorded pin"
    );

    std::fs::write(history_root.join("plain.txt"), b"edited\n").expect("edit deployed content");
    std::fs::remove_file(history_root.join(".git")).expect("remove overlay gitlink");

    let verification =
        phora::sync::verify(&config, &registry, None, &backend).expect("verify runs");
    assert!(
        verification
            .mismatches
            .iter()
            .any(|mismatch| mismatch.path.ends_with("plain.txt")),
        "verify hashes an edited history deployment file"
    );
    assert_eq!(
        verification.overlay_findings.len(),
        1,
        "verify separately reports the damaged history overlay"
    );

    let mut forced_options = sync_options();
    forced_options.conflict_policy = ConflictPolicy::Overwrite;
    sync_history(&config, &registry, &backend, forced_options);
    assert_eq!(
        std::fs::read(history_root.join("plain.txt")).expect("read repaired content"),
        b"one\n"
    );
    assert_eq!(
        git_stdout(&history_root, &["status", "--porcelain=v1"]),
        "",
        "forced sync restores a clean real-git worktree"
    );
    assert_eq!(
        git_stdout(&history_root, &["log", "-1", "--format=%H"]),
        record.commit,
        "forced sync restores the recorded pin"
    );

    assert_legacy_record_defaults();
}

#[test]
fn history_prune_removes_deployment_and_overlay_administration() {
    let fixture = TempDir::new().expect("fixture tempdir");
    let (mirror, _, _, _) = fixture_repository(&fixture);
    let deploy_root = fixture.path().join("deployment");
    let config = Config::parse(&format!(
        "version = 1\n\
         [sources.history]\n\
         git = \"{}\"\n\n\
         [targets.home]\n\
         path = \"{}\"\n\n\
         [targets.home.sources]\n\
         history = {{ history = true }}\n",
        mirror.display(),
        deploy_root.display()
    ))
    .expect("history config parses");
    let registry = FileStateStore::open(fixture.path().join("state")).expect("open registry");
    let backend = GitBackend::new(fixture.path().join("cache"));
    sync_history(&config, &registry, &backend, sync_options());

    let record = registry
        .all_artifacts()
        .expect("read history records")
        .pop()
        .expect("history sync persists one record");
    let history_root = deploy_root.join("history");
    let gitlink = std::fs::read_to_string(history_root.join(".git")).expect("read gitlink");
    let admin = PathBuf::from(
        gitlink
            .strip_prefix("gitdir: ")
            .expect("gitlink prefix")
            .trim(),
    );
    let mirror = admin
        .parent()
        .and_then(Path::parent)
        .expect("admin directory belongs to a mirror")
        .to_path_buf();
    let pin_ref = format!(
        "refs/phora/worktrees/{}",
        record
            .worktree_admin_id
            .as_ref()
            .expect("history record has admin id")
    );
    assert!(
        git_output(&mirror, &["show-ref", "--verify", pin_ref.as_str()])
            .status
            .success(),
        "a deployed history overlay keeps its pin ref"
    );

    let pruned_config = Config::parse(&format!(
        "version = 1\n\n[targets.home]\npath = \"{}\"\n",
        deploy_root.display()
    ))
    .expect("pruned config parses");
    let mut prune_options = sync_options();
    prune_options.prune_policy = PrunePolicy::RemoveOrphans;
    sync_history(&pruned_config, &registry, &backend, prune_options);

    assert!(
        !history_root.exists(),
        "prune removes the retired history deployment directory"
    );
    assert!(
        !admin.exists(),
        "prune removes the retired history worktree administration"
    );
    assert!(
        !git_output(&mirror, &["show-ref", "--verify", pin_ref.as_str()])
            .status
            .success(),
        "prune removes the retired history pin ref"
    );
}
