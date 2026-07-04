//! CLIFF-PROJID-003: per-clone `.phora-id` identity survives relocation, keeps
//! clones isolated, and adopts a legacy path-hash registry (idempotent,
//! interruption-safe, marker inside the old directory — never replacing it).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use phora::kernel::ProjectId;
use tempfile::TempDir;

mod common;

const PHORA_ID_FILE: &str = ".phora-id";

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

/// Top-level regular files directly inside `dir` (the adoption marker lands here).
fn top_level_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .map(|e| e.expect("entry").path())
        .filter(|p| p.is_file())
        .collect()
}

// ── generation ────────────────────────────────────────────────────

/// First sync must materialize the per-clone identity file next to `phora.toml`.
#[test]
fn first_sync_writes_phora_id_next_to_config() {
    let fx = build_fixture();

    let out = run(&fx, &fx.project, &["sync"]);
    assert_sync_ok(&out, "first sync");

    let id_path = fx.project.join(PHORA_ID_FILE);
    assert!(
        id_path.is_file(),
        "first `phora sync` must generate a per-clone `.phora-id` beside phora.toml at {}",
        id_path.display(),
    );
    let id = std::fs::read_to_string(&id_path).expect("read .phora-id");
    assert!(
        !id.trim().is_empty(),
        "`.phora-id` must hold a non-empty per-clone identity (UUID v4), got {id:?}"
    );
}

// ── relocation (INV-3) ────────────────────────────────────────────

/// A synced project that is renamed on disk keeps its registry via the
/// travelling `.phora-id`: the second sync at the new path reuses the single
/// existing registry rather than hashing the new path into a fresh, empty one.
#[test]
fn moved_project_keeps_single_registry_via_phora_id() {
    let fx = build_fixture();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync before move");
    assert_eq!(
        project_dirs(&fx).len(),
        1,
        "premise: the first sync creates exactly one registry directory"
    );

    let moved = fx.project.parent().expect("parent").join("proj-renamed");
    std::fs::rename(&fx.project, &moved).expect("rename project directory");

    assert!(
        moved.join(PHORA_ID_FILE).is_file(),
        "the `.phora-id` must travel with the directory (it lives beside phora.toml)"
    );

    let out = run(&fx, &moved, &["sync"]);
    assert_sync_ok(&out, "sync after move");

    let dirs = project_dirs(&fx);
    assert_eq!(
        dirs.len(),
        1,
        "moving the project must NOT spawn a second path-hash registry — the identity \
         file keeps it stable; found {dirs:?}"
    );
    assert!(
        has_records(&dirs[0]),
        "prior deployments must remain in the single adopted registry (recognized as \
         phora-owned, not reclassified Foreign into an empty second registry)"
    );
}

// ── clone isolation (INV-3) ───────────────────────────────────────

/// Two independent clones of the same repo on one machine each get their own
/// `.phora-id`, so their registries stay isolated (a committed identity would
/// collapse both onto one — the rejected design).
#[test]
fn two_clones_get_distinct_phora_ids() {
    let fx = build_fixture();

    let clone_b = fx.project.parent().expect("parent").join("clone-b");
    std::fs::create_dir_all(&clone_b).expect("create second clone");
    write(&clone_b.join("phora.toml"), fx.config.as_bytes());

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync clone a");
    assert_sync_ok(&run(&fx, &clone_b, &["sync"]), "sync clone b");

    let id_a = std::fs::read_to_string(fx.project.join(PHORA_ID_FILE))
        .expect("clone a must have a .phora-id");
    let id_b = std::fs::read_to_string(clone_b.join(PHORA_ID_FILE))
        .expect("clone b must have a .phora-id");

    assert_ne!(
        id_a.trim(),
        id_b.trim(),
        "each clone must generate a distinct per-clone identity so their registries \
         never cross-contaminate"
    );
    assert_eq!(
        project_dirs(&fx).len(),
        2,
        "two clones must keep two isolated registries, one per identity"
    );
}

// ── git-exclude wiring ────────────────────────────────────────────

/// The identity file must be excluded per-clone via `.git/info/exclude`, never
/// by editing the shared, committed `.gitignore`.
#[test]
fn first_sync_excludes_phora_id_via_git_info_exclude_not_gitignore() {
    let fx = build_fixture();
    git(&fx.project, &["init", "-b", "main", "."]);

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "sync in git repo");

    let exclude = fx.project.join(".git").join("info").join("exclude");
    let exclude_body = std::fs::read_to_string(&exclude).expect("read .git/info/exclude");
    assert!(
        exclude_body.lines().any(|l| l.trim() == PHORA_ID_FILE),
        "first sync must append `.phora-id` to .git/info/exclude, got:\n{exclude_body}"
    );

    let gitignore = fx.project.join(".gitignore");
    let gitignore_body = std::fs::read_to_string(&gitignore).unwrap_or_default();
    assert!(
        !gitignore_body.contains(PHORA_ID_FILE),
        "phora must never write `.phora-id` into the shared .gitignore, got:\n{gitignore_body}"
    );
}

#[test]
fn exclude_self_heals_on_a_later_sync_after_identity_file_exists() {
    let fx = build_fixture();
    git(&fx.project, &["init", "-b", "main", "."]);

    assert_sync_ok(
        &run(&fx, &fx.project, &["sync"]),
        "first sync wires the exclude",
    );
    assert!(
        fx.project.join(PHORA_ID_FILE).is_file(),
        "premise: the first sync materializes .phora-id"
    );
    let exclude = fx.project.join(".git").join("info").join("exclude");
    assert!(
        std::fs::read_to_string(&exclude)
            .unwrap_or_default()
            .lines()
            .any(|l| l.trim() == PHORA_ID_FILE),
        "premise: the first sync appended the exclude entry"
    );

    std::fs::write(&exclude, b"").expect("empty the exclude, as a prior exclude failure would");

    assert_sync_ok(
        &run(&fx, &fx.project, &["sync"]),
        "second sync self-heals the exclude",
    );

    let healed = std::fs::read_to_string(&exclude).expect("read .git/info/exclude");
    assert!(
        healed.lines().any(|l| l.trim() == PHORA_ID_FILE),
        "a later sync must re-exclude .phora-id even though the identity file already exists — \
         the exclude runs every sync so a prior failure self-heals, got:\n{healed}"
    );
}

// ── adoption (INV-4) ──────────────────────────────────────────────

/// A project whose only registry is a legacy path-hash one (no `.phora-id`)
/// must, on the next sync, generate an identity and adopt that registry into an
/// identity-keyed one — leaving a marker file INSIDE the old directory (never
/// replacing the directory, which would ENOTDIR old binaries). Re-running is
/// idempotent: no second adopted registry, no error.
#[test]
fn adoption_migrates_legacy_registry_leaves_marker_inside_old_dir_and_is_idempotent() {
    let fx = build_fixture();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "seed sync");
    let seeded = project_dirs(&fx);
    assert_eq!(seeded.len(), 1, "premise: one registry after the seed sync");

    let legacy_id = ProjectId::for_path(&fx.project).expect("path-hash id");
    let legacy_dir = projects_base(&fx).join(legacy_id.as_str());
    if seeded[0] != legacy_dir {
        std::fs::rename(&seeded[0], &legacy_dir).expect("reshape into legacy path-hash registry");
    }
    let _ = std::fs::remove_file(fx.project.join(PHORA_ID_FILE));
    assert!(
        has_records(&legacy_dir),
        "premise: the legacy path-hash registry holds records to adopt"
    );
    assert!(
        top_level_files(&legacy_dir).is_empty(),
        "premise: the legacy registry has no top-level marker file yet"
    );

    let out = run(&fx, &fx.project, &["sync"]);
    assert_sync_ok(&out, "adoption sync");

    assert!(
        fx.project.join(PHORA_ID_FILE).is_file(),
        "adoption must generate the missing `.phora-id`"
    );

    let adopted: Vec<PathBuf> = project_dirs(&fx)
        .into_iter()
        .filter(|d| *d != legacy_dir && has_records(d))
        .collect();
    assert_eq!(
        adopted.len(),
        1,
        "adoption must produce exactly one identity-keyed registry carrying the old \
         records (via the path-hash fallback locating the legacy dir); found {adopted:?}"
    );

    assert!(
        legacy_dir.is_dir(),
        "the old registry must remain a DIRECTORY after adoption — replacing it with a \
         tombstone file would ENOTDIR concurrent old binaries"
    );
    assert!(
        !top_level_files(&legacy_dir).is_empty(),
        "adoption must drop a marker file INSIDE the old registry directory"
    );

    let before = project_dirs(&fx).len();
    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "idempotent rerun");
    assert_eq!(
        project_dirs(&fx).len(),
        before,
        "a rerun after adoption must not spawn another registry (idempotent, INV-4)"
    );
}

/// A sync interrupted AFTER adoption but BEFORE the identity file lands (simulated
/// by deleting `.phora-id` while the adoption marker and adopted registry remain)
/// must, on rerun, reuse the id recorded in the marker instead of minting a fresh
/// one — so the run converges onto the single adopted registry rather than
/// re-adopting into a duplicate.
#[test]
fn interrupted_adoption_reruns_converge_on_the_marker_recorded_registry() {
    let fx = build_fixture();

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "seed sync");
    let seeded = project_dirs(&fx);
    assert_eq!(seeded.len(), 1, "premise: one registry after the seed sync");

    let legacy_id = ProjectId::for_path(&fx.project).expect("path-hash id");
    let legacy_dir = projects_base(&fx).join(legacy_id.as_str());
    if seeded[0] != legacy_dir {
        std::fs::rename(&seeded[0], &legacy_dir).expect("reshape into legacy path-hash registry");
    }
    let _ = std::fs::remove_file(fx.project.join(PHORA_ID_FILE));

    assert_sync_ok(&run(&fx, &fx.project, &["sync"]), "adoption sync");
    let adopted_id = std::fs::read_to_string(fx.project.join(PHORA_ID_FILE))
        .expect("adoption writes the identity file")
        .trim()
        .to_owned();
    let adopted_dir = projects_base(&fx).join(&adopted_id);
    assert!(
        has_records(&adopted_dir),
        "premise: the adopted registry carries the records"
    );

    std::fs::remove_file(fx.project.join(PHORA_ID_FILE))
        .expect("simulate a crash before the identity file is durably written");

    assert_sync_ok(
        &run(&fx, &fx.project, &["sync"]),
        "rerun after interruption",
    );

    let rewritten = std::fs::read_to_string(fx.project.join(PHORA_ID_FILE))
        .expect("rerun re-materializes the identity file")
        .trim()
        .to_owned();
    assert_eq!(
        rewritten, adopted_id,
        "the rerun must reuse the marker-recorded identity, not mint a fresh UUID"
    );

    let with_records: Vec<PathBuf> = project_dirs(&fx)
        .into_iter()
        .filter(|d| *d != legacy_dir && has_records(d))
        .collect();
    assert_eq!(
        with_records,
        vec![adopted_dir],
        "the interrupted-then-rerun sequence must converge on the single adopted registry, never \
         spawn a second one keyed by a freshly minted UUID; found {with_records:?}"
    );
}
