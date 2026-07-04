//! TDEP-TRUST-CLI-001: `phora trust` CLI surface (inspect-before-trust).
//!
//! `phora trust <source>` lists each transitive hook with its commit-bound preimage and the PHORA
//! env it receives plus a note that it inherits the FULL process env; it prompts per hook and
//! records approvals in the consumer lock. Before approving it renders the file-level paths changed
//! in the dep between the last trusted commit and the current candidate commit (first trust prints a
//! no-prior-commit note; a mirror lacking the prior commit degrades to a sync directive).
//! `--list` is read-only; `--revoke` removes an approval. The first `phora sync` that strips an
//! untrusted hook prints a "run phora trust to approve N hook(s)" directive and exits non-zero.
//! Discovery works without a prior `add` via the shared root-manifest fetch. Trust mutations are
//! serialized under `registry.lock_exclusive()`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use phora::kernel::ProjectId;
use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
    insteadof: Vec<(String, String)>,
    repos: Vec<TempDir>,
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
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

fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");
    let home_path = home.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");
    Fixture {
        _home: home,
        cwd,
        home_path,
        xdg_cache,
        xdg_state,
        insteadof: Vec::new(),
        repos: Vec::new(),
    }
}

impl Fixture {
    fn map_url(&mut self, mock: &str, local: &Path) {
        self.insteadof
            .push((mock.to_owned(), local.display().to_string()));
    }

    fn finish_gitconfig(&self) {
        use std::fmt::Write as _;
        let mut body = String::new();
        for (mock, local) in &self.insteadof {
            let _ = write!(body, "[url \"{local}\"]\n\tinsteadOf = {mock}\n");
        }
        write(&self.home_path.join(".gitconfig"), body.as_bytes());
    }
}

fn run(fixture: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env("XDG_CACHE_HOME", &fixture.xdg_cache)
        .env("XDG_STATE_HOME", &fixture.xdg_state)
        .output()
        .expect("phora binary runs")
}

fn commit_repo(dir: &Path, files: &[(&str, &str)], manifest: &str) {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(&dir.join("phora.toml"), manifest.as_bytes());
    for (path, body) in files {
        write(&dir.join(path), body.as_bytes());
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "fixture"]);
}

fn leaf_repo(dir: &Path, file: &str, body: &str) {
    commit_repo(dir, &[(&format!("pkg/{file}"), body)], "version = 1\n");
}

fn sentinel(fixture: &Fixture) -> PathBuf {
    fixture.home_path.join("hook-ran.sentinel")
}

fn dep_with_on_change_hook(dep: &Path, sentinel_abs: &str) {
    let manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n\n\
         [targets.nvim.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    commit_repo(dep, &[], &manifest);
}

fn consumer_with_dep_hook() -> (Fixture, PathBuf) {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");
    let leaf_path = leaf.path().to_path_buf();

    let mut fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel_path.display().to_string());
    let dep_path = dep.path().to_path_buf();

    fixture.map_url("https://github.com/mock/leaf.git", &leaf_path);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep_path.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    fixture.repos.push(leaf);
    fixture.repos.push(dep);
    (fixture, sentinel_path)
}

fn state_root(fixture: &Fixture) -> PathBuf {
    let project = ProjectId::resolve(fixture.cwd.path()).expect("project id");
    fixture
        .xdg_state
        .join("phora")
        .join("projects")
        .join(project.as_str())
}

fn hold_project_lock(fixture: &Fixture) -> std::fs::File {
    let locks_dir = state_root(fixture).join("locks");
    std::fs::create_dir_all(&locks_dir).expect("create locks dir");
    let lock_path = locks_dir.join("state.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lock file");
    file.try_lock()
        .expect("test acquires the project lock first");
    file
}

// AC1: `phora trust <source> --list` surfaces preimage + PHORA_* env + full-env inheritance note.

#[test]
fn trust_list_shows_preimage_env_and_full_env_inheritance_note() {
    let (fixture, _sentinel) = consumer_with_dep_hook();

    // Seed: the first sync records the candidate (and its commit-bound preimage) into phora.lock.
    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let shown = format!("{stdout}{stderr}");

    assert!(
        out.status.success(),
        "`phora trust --list` is read-only and must exit zero; stderr: {stderr}"
    );
    assert!(
        shown.contains("blake3:"),
        "AC1: --list must show each candidate hook's commit-bound preimage hash; got:\n{shown}"
    );
    assert!(
        shown.contains("PHORA_TARGET"),
        "AC1: --list must show the PHORA_* env the hook receives; got:\n{shown}"
    );
    assert!(
        shown.to_lowercase().contains("full") && shown.to_lowercase().contains("environment")
            || shown.to_lowercase().contains("inherits"),
        "AC1: --list must explicitly note the hook inherits the FULL process environment; got:\n{shown}"
    );
}

// AC3: `--revoke` removes the addressed trusted_hooks entry.

#[test]
fn trust_revoke_removes_the_trusted_hooks_entry() {
    let (fixture, _sentinel) = consumer_with_dep_hook();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("seed sync wrote phora.lock");
    let preimage = extract_first_preimage(&lock_text)
        .expect("the seed sync must surface a commit-bound preimage to pin against");

    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"mydeps-instance\"\n\
         hook_id = \"composed#on_change#deadbeef\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n\
         source = \"mydeps\"\n",
    );
    write(&lock_path, approved.as_bytes());

    let out = run(&fixture, &["trust", "mydeps", "--revoke"]);
    assert!(
        out.status.success(),
        "`phora trust --revoke` must exit zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read_to_string(&lock_path).expect("revoke rewrote phora.lock");
    assert!(
        !after.contains("[[trusted_hooks]]"),
        "AC3: --revoke must remove the addressed approval — no `[[trusted_hooks]]` entry may remain \
         (the preimage legitimately persists in `[[candidate_hooks]]`, the untrusted discovery \
         surface revoke must not touch); got:\n{after}"
    );
}

// AC4: discovery WITHOUT a prior `add`/`sync` — `phora trust` surfaces the hook via the shared fetch.

#[test]
fn trust_discovers_hooks_without_a_prior_add_or_sync() {
    let (fixture, _sentinel) = consumer_with_dep_hook();

    // No prior `add`/`sync`: there is no phora.lock yet.
    assert!(
        !fixture.cwd.path().join("phora.lock").exists(),
        "premise: this test must run with NO prior lock so discovery cannot lean on recorded state"
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let shown = format!("{stdout}{stderr}");

    assert!(
        out.status.success(),
        "AC4: `phora trust` must discover transitive hooks via the shared root-manifest fetch even \
         with no prior add/sync; stderr: {stderr}"
    );
    assert!(
        shown.contains("on_change") || shown.contains("hook") || shown.contains("touch"),
        "AC4: --list with no prior lock must still surface the dep's transitive hook (discovered \
         through the shared fetch, not a full sync); got:\n{shown}"
    );

    // A full sync would deploy the dep's imports under ~/.config; trust must not deploy.
    assert!(
        !fixture.home_path.join(".config").exists(),
        "AC4: `phora trust` must use the lightweight shared fetch, NOT a full clone+sync — it must \
         not have deployed the dep's artifacts to ~/.config"
    );
}

#[test]
fn sync_with_a_stripped_untrusted_hook_surfaces_the_trust_directive_without_breaking_ci() {
    let (fixture, sentinel_path) = consumer_with_dep_hook();

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !sentinel_path.exists(),
        "premise: the untrusted hook must be stripped (not run) — sentinel must be absent"
    );
    assert!(
        out.status.success(),
        "AC6: a non-TTY sync must stay green (untrusted hooks are skipped, not fatal); got {:?}\n\
         stderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("phora trust") && stderr.contains("approve"),
        "AC6: the sync must surface a `run `phora trust <name>` to approve N hook(s)` directive so \
         the stripped hook is never silent; got:\n{stderr}"
    );
    assert!(
        stderr.contains("incomplete"),
        "AC6: the directive must warn the deployed artifacts may be incomplete (the hook that would \
         post-process them was not run); got:\n{stderr}"
    );
}

#[test]
fn no_transitive_hooks_suppresses_the_trust_directive_and_the_nonzero_exit() {
    let (fixture, _sentinel) = consumer_with_dep_hook();

    let out = run(&fixture, &["sync", "--no-transitive-hooks"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "AC6: --no-transitive-hooks must suppress the stripped-hook non-zero exit; \
         got {:?}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        !stderr.contains("phora trust"),
        "AC6: --no-transitive-hooks must suppress the `phora trust` directive entirely; got:\n{stderr}"
    );
}

// AC7: `phora trust` serializes under the project lock; a held lock blocks it (exit 75).

#[test]
fn trust_fails_fast_when_the_project_lock_is_held() {
    let (fixture, _sentinel) = consumer_with_dep_hook();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let held = hold_project_lock(&fixture);
    let out = run(&fixture, &["trust", "mydeps", "--revoke"]);
    drop(held);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(75),
        "AC7: `phora trust` must acquire the project lock before mutating trust state — under \
         contention it must fail fast with EX_TEMPFAIL (75); got {:?}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("another phora process is running for this project"),
        "AC7: a contended trust must print the contended-lock message; got:\n{stderr}"
    );
}

// C1/C5: cross-source isolation — revoking one source must not touch another's approvals,
// and --list must scope to the named source only.

fn dep_with_named_hook(dep: &Path, leaf_mock: &str, sentinel_abs: &str, tag: &str) {
    let manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"{leaf_mock}\"\ninclude = [\"pkg\"]\n\n\
         [targets.{tag}]\npath = \"{tag}\"\nsources = [\"editor\"]\n\n\
         [targets.{tag}.hooks]\non_change = \"touch '{sentinel_abs}-{tag}'\"\n",
    );
    commit_repo(dep, &[], &manifest);
}

fn consumer_with_two_dep_hooks() -> Fixture {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");
    let leaf_path = leaf.path().to_path_buf();

    let mut fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep_a = TempDir::new().expect("dep a repo");
    let dep_b = TempDir::new().expect("dep b repo");
    dep_with_named_hook(
        dep_a.path(),
        "https://github.com/mock/leaf.git",
        &sentinel_path.display().to_string(),
        "aaa",
    );
    dep_with_named_hook(
        dep_b.path(),
        "https://github.com/mock/leaf.git",
        &sentinel_path.display().to_string(),
        "bbb",
    );
    let dep_a_path = dep_a.path().to_path_buf();
    let dep_b_path = dep_b.path().to_path_buf();

    fixture.map_url("https://github.com/mock/leaf.git", &leaf_path);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.depA]\ngit = \"{a}\"\ntransitive = true\n\n\
         [sources.depB]\ngit = \"{b}\"\ntransitive = true\n\n\
         [targets.acfg]\npath = \"~/.config/a\"\nimports = [\"depA\"]\n\n\
         [targets.bcfg]\npath = \"~/.config/b\"\nimports = [\"depB\"]\n",
        a = dep_a_path.display(),
        b = dep_b_path.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    fixture.repos.push(leaf);
    fixture.repos.push(dep_a);
    fixture.repos.push(dep_b);
    fixture
}

fn approve_all_via_trusted_hooks(lock_path: &Path) {
    use std::fmt::Write as _;
    let lock_text = std::fs::read_to_string(lock_path).expect("seed sync wrote phora.lock");
    let mut approvals = String::new();
    let mut preimage: Option<String> = None;
    let mut in_candidate = false;
    for line in lock_text.lines() {
        let line = line.trim();
        if line == "[[candidate_hooks]]" {
            in_candidate = true;
            preimage = None;
        } else if line.starts_with("[[") {
            in_candidate = false;
        } else if in_candidate && let Some(rest) = line.strip_prefix("preimage = \"") {
            preimage = Some(rest.trim_end_matches('"').to_owned());
        } else if in_candidate && let Some(rest) = line.strip_prefix("source = \"") {
            let source = rest.trim_end_matches('"');
            if let Some(preimage) = preimage.as_deref()
                && !source.is_empty()
                && !preimage.is_empty()
            {
                let _ = write!(
                    approvals,
                    "\n[[trusted_hooks]]\n\
                     dep_instance = \"{source}-instance\"\n\
                     hook_id = \"{source}#on_change\"\n\
                     preimage = \"{preimage}\"\n\
                     approved_at = \"2026-06-20T00:00:00Z\"\n\
                     source = \"{source}\"\n",
                );
            }
        }
    }
    assert!(
        approvals.contains("source = \"depA\"") && approvals.contains("source = \"depB\""),
        "premise: the seed sync must record a candidate for BOTH depA and depB, got:\n{lock_text}"
    );
    write(lock_path, format!("{lock_text}{approvals}").as_bytes());
}

#[test]
fn revoke_one_source_leaves_another_sources_approval_intact() {
    let fixture = consumer_with_two_dep_hooks();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    approve_all_via_trusted_hooks(&lock_path);

    let out = run(&fixture, &["trust", "depA", "--revoke"]);
    assert!(
        out.status.success(),
        "`phora trust depA --revoke` must exit zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read_to_string(&lock_path).expect("revoke rewrote phora.lock");
    let trusted = trusted_hook_sources(&after);
    assert!(
        !trusted.contains(&"depA".to_owned()),
        "C1/C5: revoking depA must drop depA's trusted_hooks approval; got:\n{after}"
    );
    assert!(
        trusted.contains(&"depB".to_owned()),
        "C1/C5: revoking depA must LEAVE depB's approval intact — a source-blind revoke wrongly \
         over-revokes every dep; got:\n{after}"
    );

    let list = run(&fixture, &["trust", "depA", "--list"]);
    let shown = String::from_utf8_lossy(&list.stdout);
    assert!(
        shown.contains("sentinel-aaa"),
        "C1: --list depA must show depA's hook (its `aaa` target command); got:\n{shown}"
    );
    assert!(
        !shown.contains("sentinel-bbb"),
        "C1: --list depA must show ONLY depA's hook, never depB's (`bbb`) — a source-blind list \
         leaks every dep; got:\n{shown}"
    );
}

fn trusted_hook_sources(lock_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_trusted = false;
    for line in lock_text.lines() {
        let line = line.trim();
        if line == "[[trusted_hooks]]" {
            in_trusted = true;
        } else if line.starts_with("[[") {
            in_trusted = false;
        } else if in_trusted && let Some(rest) = line.strip_prefix("source = \"") {
            out.push(rest.trim_end_matches('"').to_owned());
        }
    }
    out
}

// C3 / R8: inspect-before-trust file diff between the last trusted commit and the candidate commit.

fn dep_path(fixture: &Fixture) -> PathBuf {
    fixture
        .repos
        .last()
        .expect("consumer_with_dep_hook pushes the dep last")
        .path()
        .to_path_buf()
}

fn dep_manifest_with_tracked_file(sentinel_abs: &str) -> String {
    format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n\n\
         [targets.nvim.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    )
}

fn first_candidate_commit(lock_text: &str) -> Option<String> {
    let mut in_candidate = false;
    for line in lock_text.lines() {
        let line = line.trim();
        if line == "[[candidate_hooks]]" {
            in_candidate = true;
        } else if line.starts_with("[[") {
            in_candidate = false;
        } else if in_candidate && let Some(rest) = line.strip_prefix("commit = \"") {
            return Some(rest.trim_end_matches('"').to_owned());
        }
    }
    None
}

#[test]
fn trust_list_shows_file_paths_changed_since_the_last_trusted_commit() {
    let (fixture, sentinel_path) = consumer_with_dep_hook();
    let sentinel_abs = sentinel_path.display().to_string();
    let dep = dep_path(&fixture);

    write(&dep.join("scripts/post.sh"), b"#!/bin/sh\necho original\n");
    git(&dep, &["add", "-A"]);
    git(&dep, &["commit", "-m", "add tracked script"]);

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("seed sync wrote phora.lock");
    let preimage =
        extract_first_preimage(&lock_text).expect("seed records a commit-bound preimage");
    let commit_a = first_candidate_commit(&lock_text)
        .expect("seed records the candidate's resolved commit so a prior approval can pin it");
    let hook_id = first_candidate_hook_id(&lock_text).expect("seed records the candidate hook id");
    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"any\"\n\
         hook_id = \"{hook_id}\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n\
         commit = \"{commit_a}\"\n\
         source = \"mydeps\"\n",
    );
    write(&lock_path, approved.as_bytes());

    write(&dep.join("scripts/post.sh"), b"#!/bin/sh\necho TAMPERED\n");
    write(
        &dep.join("phora.toml"),
        dep_manifest_with_tracked_file(&sentinel_abs).as_bytes(),
    );
    git(&dep, &["add", "-A"]);
    git(&dep, &["commit", "-m", "change tracked script"]);

    let resync = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        resync.status.success(),
        "re-sync must succeed; stderr: {}",
        String::from_utf8_lossy(&resync.stderr)
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust --list` is read-only and must exit zero; got:\n{shown}"
    );
    assert!(
        shown.contains("scripts/post.sh"),
        "R8: --list must show the file-level path that changed in the dep between the last trusted \
         commit and the current candidate commit, so the consumer can inspect the hook's tree \
         before re-trusting; got:\n{shown}"
    );
}

fn first_candidate_hook_id(lock_text: &str) -> Option<String> {
    let mut in_candidate = false;
    for line in lock_text.lines() {
        let line = line.trim();
        if line == "[[candidate_hooks]]" {
            in_candidate = true;
        } else if line.starts_with("[[") {
            in_candidate = false;
        } else if in_candidate && let Some(rest) = line.strip_prefix("hook_id = \"") {
            return Some(rest.trim_end_matches('"').to_owned());
        }
    }
    None
}

// THI-002: first-trust composed-surface listing.

fn dep_composing_editor(dep: &Path, sentinel_abs: &str) {
    let manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"nvim\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n\n\
         [targets.nvim.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    commit_repo(dep, &[], &manifest);
}

fn consumer_with_composed_files() -> (Fixture, PathBuf) {
    let leaf = TempDir::new().expect("leaf repo");
    commit_repo(
        leaf.path(),
        &[
            ("nvim/init.lua", "-- init\n"),
            ("nvim/lua/opts.lua", "-- opts\n"),
        ],
        "version = 1\n",
    );
    let leaf_path = leaf.path().to_path_buf();

    let mut fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_composing_editor(dep.path(), &sentinel_path.display().to_string());
    let dep_path = dep.path().to_path_buf();

    fixture.map_url("https://github.com/mock/leaf.git", &leaf_path);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep_path.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    fixture.repos.push(leaf);
    fixture.repos.push(dep);
    (fixture, sentinel_path)
}

#[test]
fn trust_list_first_trust_shows_the_composed_file_surface_offline() {
    let (fixture, _sentinel) = consumer_with_composed_files();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    // Leave the cache git mirror as the only surviving source of these paths; never delete it here.
    for repo in &fixture.repos {
        std::fs::remove_dir_all(repo.path()).ok();
    }
    std::fs::remove_dir_all(fixture.home_path.join(".config")).ok();

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust --list` is read-only and must exit zero; got:\n{shown}"
    );
    assert!(
        !shown.contains("first trust — no prior trusted commit to diff"),
        "THI-002: a first-trust candidate must NO LONGER print the bare no-prior-commit note — it \
         must list the composed file surface instead; got:\n{shown}"
    );
    assert!(
        shown.contains("nvim/init.lua") && shown.contains("nvim/lua/opts.lua"),
        "THI-002: first trust must list the dependency-repo-relative files this consumer composes \
         from the dep at the candidate commit (the editor source's `nvim/init.lua` and \
         `nvim/lua/opts.lua`, with their directory so a deployed-surface or unrelated leaf match \
         cannot false-pass), resolved OFFLINE from the mirror after the source repos AND the \
         deployed ~/.config surface were deleted; got:\n{shown}"
    );
    assert!(
        !shown
            .lines()
            .any(|l| l.trim() == "phora.toml" || l.trim_end().ends_with("/phora.toml")),
        "THI-002 (AD-1 fidelity): the composed surface must honor the binding's include/exclude — \
         the editor source declares `include = [\"nvim\"]`, so the leaf's root `phora.toml` (outside \
         `nvim/`, never composed, never deployed) must NOT appear as a composed-file entry; listing \
         the entire source subtree leaks it; got:\n{shown}"
    );
}

#[test]
fn trust_list_prior_trusted_still_shows_the_changed_diff_not_the_full_surface() {
    let (fixture, sentinel_path) = consumer_with_dep_hook();
    let sentinel_abs = sentinel_path.display().to_string();
    let dep = dep_path(&fixture);

    // An UNCHANGED tracked file present at both commits, plus a file that changes across them.
    write(&dep.join("stable/keep.txt"), b"unchanged\n");
    write(&dep.join("scripts/post.sh"), b"#!/bin/sh\necho original\n");
    git(&dep, &["add", "-A"]);
    git(&dep, &["commit", "-m", "add tracked files"]);

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("seed sync wrote phora.lock");
    let preimage =
        extract_first_preimage(&lock_text).expect("seed records a commit-bound preimage");
    let commit_a = first_candidate_commit(&lock_text)
        .expect("seed records the candidate commit so a prior approval can pin it");
    let hook_id = first_candidate_hook_id(&lock_text).expect("seed records the candidate hook id");
    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"any\"\n\
         hook_id = \"{hook_id}\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n\
         commit = \"{commit_a}\"\n\
         source = \"mydeps\"\n",
    );
    write(&lock_path, approved.as_bytes());

    write(&dep.join("scripts/post.sh"), b"#!/bin/sh\necho TAMPERED\n");
    write(
        &dep.join("phora.toml"),
        dep_manifest_with_tracked_file(&sentinel_abs).as_bytes(),
    );
    git(&dep, &["add", "-A"]);
    git(&dep, &["commit", "-m", "change tracked script"]);

    let resync = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        resync.status.success(),
        "re-sync must succeed; stderr: {}",
        String::from_utf8_lossy(&resync.stderr)
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust --list` must exit zero; got:\n{shown}"
    );
    assert!(
        shown.contains("changed since last trusted"),
        "AC5: a candidate WITH a prior trusted commit must KEEP the existing file-diff rendering — \
         THI-002 only replaces the first-trust branch; got:\n{shown}"
    );
    assert!(
        shown.contains("scripts/post.sh"),
        "AC5: the prior-trusted diff must still name the file that changed between the trusted and \
         candidate commits; got:\n{shown}"
    );
    assert!(
        !shown.contains("stable/keep.txt"),
        "AC5: the prior-trusted rendering is a DIFF (changed paths only), not the full composed \
         surface — an unchanged tracked file must not appear; got:\n{shown}"
    );
}

#[test]
fn trust_list_first_trust_with_no_resolved_commit_degrades_to_a_sync_directive() {
    let (fixture, _sentinel) = consumer_with_composed_files();

    // No prior sync: discovery yields a first-trust candidate with NO resolved commit (commit = "").
    assert!(
        !fixture.cwd.path().join("phora.lock").exists(),
        "premise: run with NO prior lock so the discovered candidate carries no resolved commit"
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust --list` must exit zero even when the surface cannot be resolved; got:\n{shown}"
    );
    assert!(
        !shown.contains("first trust — no prior trusted commit to diff"),
        "THI-002: the bare no-prior-commit note is replaced — an unresolved first-trust candidate \
         must degrade to a clear sync directive, not the old string; got:\n{shown}"
    );
    assert!(
        shown.contains("phora sync"),
        "THI-002 degradation: a first-trust candidate whose commit is unresolved/absent from the \
         mirror must fall back to a `run `phora sync`` directive — never a panic, never a silent \
         blank; got:\n{shown}"
    );
}

fn extract_first_preimage(lock_text: &str) -> Option<String> {
    for line in lock_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("preimage = \"")
            && let Some(end) = rest.find('"')
        {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

fn first_candidate_field(lock_text: &str, field: &str) -> Option<String> {
    let prefix = format!("{field} = \"");
    let mut in_candidate = false;
    for line in lock_text.lines() {
        let line = line.trim();
        if line == "[[candidate_hooks]]" {
            in_candidate = true;
        } else if line.starts_with("[[") {
            in_candidate = false;
        } else if in_candidate && let Some(rest) = line.strip_prefix(prefix.as_str()) {
            return Some(rest.trim_end_matches('"').to_owned());
        }
    }
    None
}

// HIGH/MEDIUM (gemini): nested (transitive-of-transitive) diff resolves the dep url by dep_instance.

fn nested_dep_with_hook(
    dep_d: &Path,
    dep_e: &Path,
    inner_mock: &str,
    sentinel_abs: &str,
) -> PathBuf {
    let e_manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n\n\
         [targets.enode.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    write(
        &dep_e.join("scripts/post.sh"),
        b"#!/bin/sh\necho original\n",
    );
    commit_repo(
        dep_e,
        &[("scripts/post.sh", "#!/bin/sh\necho original\n")],
        &e_manifest,
    );

    let d_manifest = format!(
        "version = 1\n\n\
         [sources.einner]\ngit = \"{inner_mock}\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d\"\nimports = [\"einner\"]\n",
    );
    commit_repo(dep_d, &[], &d_manifest);
    dep_e.to_path_buf()
}

fn consumer_with_nested_dep_hook() -> (Fixture, PathBuf, PathBuf) {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");
    let leaf_path = leaf.path().to_path_buf();

    let mut fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep_e = TempDir::new().expect("dep E repo");
    let dep_d = TempDir::new().expect("dep D repo");
    let dep_e_repo = nested_dep_with_hook(
        dep_d.path(),
        dep_e.path(),
        "https://github.com/mock/depe.git",
        &sentinel_path.display().to_string(),
    );

    fixture.map_url("https://github.com/mock/leaf.git", &leaf_path);
    fixture.map_url("https://github.com/mock/depe.git", &dep_e_repo);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep_d}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep_d = dep_d.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    fixture.repos.push(leaf);
    fixture.repos.push(dep_d);
    fixture.repos.push(dep_e);
    (fixture, sentinel_path, dep_e_repo)
}

#[test]
fn trust_list_shows_changed_paths_for_a_nested_dep_of_dep_hook() {
    let (fixture, _sentinel, dep_e) = consumer_with_nested_dep_hook();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("seed sync wrote phora.lock");
    let preimage =
        extract_first_preimage(&lock_text).expect("seed records a commit-bound preimage");
    let commit_a = first_candidate_commit(&lock_text)
        .expect("seed records the nested candidate's resolved commit");
    let hook_id = first_candidate_hook_id(&lock_text).expect("seed records the nested hook id");
    let dep_instance = first_candidate_field(&lock_text, "dep_instance")
        .expect("seed records the nested candidate's dep_instance (the depB instance key)");
    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"{dep_instance}\"\n\
         hook_id = \"{hook_id}\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n\
         commit = \"{commit_a}\"\n\
         source = \"mydeps\"\n",
    );
    write(&lock_path, approved.as_bytes());

    write(
        &dep_e.join("scripts/post.sh"),
        b"#!/bin/sh\necho TAMPERED\n",
    );
    git(&dep_e, &["add", "-A"]);
    git(
        &dep_e,
        &["commit", "-m", "change tracked script in nested dep"],
    );

    let resync = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        resync.status.success(),
        "re-sync must succeed; stderr: {}",
        String::from_utf8_lossy(&resync.stderr)
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust --list` is read-only and must exit zero; got:\n{shown}"
    );
    assert!(
        !shown.contains("diff unavailable"),
        "the nested dep's commits live in ITS mirror (resolved via dep_instance), so the diff must \
         NOT degrade to `diff unavailable` after a sync — that is the depth-1-only URL bug; got:\n{shown}"
    );
    assert!(
        shown.contains("scripts/post.sh"),
        "R8 (nested): --list must show the file-level path that changed in the NESTED dep-of-dep \
         between the last trusted commit and the current candidate commit; got:\n{shown}"
    );
}

// MEDIUM (opus L2): cross-dep key-collision — depA's diff must never render depB's changed path.

fn colliding_nested_leaf(dep_e: &Path, inner_mock: &str, sentinel_abs: &str) {
    let e_manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"{inner_mock}\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n\n\
         [targets.enode.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    write(
        &dep_e.join("scripts/post.sh"),
        b"#!/bin/sh\necho original\n",
    );
    commit_repo(
        dep_e,
        &[("scripts/post.sh", "#!/bin/sh\necho original\n")],
        &e_manifest,
    );
}

fn consumer_with_two_colliding_nested_hooks() -> (Fixture, PathBuf, PathBuf) {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");
    let leaf_path = leaf.path().to_path_buf();

    let mut fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);
    let sentinel_abs = sentinel_path.display().to_string();

    let dep_e1 = TempDir::new().expect("E1 repo");
    let dep_e2 = TempDir::new().expect("E2 repo");
    colliding_nested_leaf(
        dep_e1.path(),
        "https://github.com/mock/leaf.git",
        &sentinel_abs,
    );
    colliding_nested_leaf(
        dep_e2.path(),
        "https://github.com/mock/leaf.git",
        &sentinel_abs,
    );
    let e1 = dep_e1.path().to_path_buf();
    let e2 = dep_e2.path().to_path_buf();

    let dep_d = TempDir::new().expect("D repo");
    let d_manifest = "version = 1\n\n\
         [sources.e1]\ngit = \"https://github.com/mock/depe1.git\"\ntransitive = true\n\n\
         [sources.e2]\ngit = \"https://github.com/mock/depe2.git\"\ntransitive = true\n\n\
         [targets.d1]\npath = \"d1\"\nimports = [\"e1\"]\n\n\
         [targets.d2]\npath = \"d2\"\nimports = [\"e2\"]\n";
    commit_repo(dep_d.path(), &[], d_manifest);

    fixture.map_url("https://github.com/mock/leaf.git", &leaf_path);
    fixture.map_url("https://github.com/mock/depe1.git", &e1);
    fixture.map_url("https://github.com/mock/depe2.git", &e2);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.depA]\ngit = \"{d}\"\ntransitive = true\n\n\
         [targets.acfg]\npath = \"~/.config/a\"\nimports = [\"depA\"]\n",
        d = dep_d.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    fixture.repos.push(leaf);
    fixture.repos.push(dep_d);
    fixture.repos.push(dep_e1);
    fixture.repos.push(dep_e2);
    (fixture, e1, e2)
}

fn approvals_for_every_candidate(lock_text: &str) -> String {
    use std::fmt::Write as _;
    let mut approvals = String::new();
    let mut cur: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let mut in_candidate = false;
    let flush = |approvals: &mut String, cur: &std::collections::HashMap<&str, String>| {
        let fields = ["dep_instance", "hook_id", "preimage", "commit", "source"];
        if fields.iter().all(|f| cur.contains_key(f)) {
            let _ = write!(approvals, "\n[[trusted_hooks]]\n");
            for field in fields {
                let _ = writeln!(approvals, "{field} = \"{}\"", cur[field]);
            }
            let _ = writeln!(approvals, "approved_at = \"2026-06-20T00:00:00Z\"");
        }
    };
    for line in lock_text.lines() {
        let line = line.trim();
        if line.starts_with("[[") {
            if in_candidate {
                flush(&mut approvals, &cur);
            }
            in_candidate = line == "[[candidate_hooks]]";
            cur.clear();
        } else if in_candidate {
            for field in ["source", "hook_id", "preimage", "commit", "dep_instance"] {
                if let Some(rest) = line.strip_prefix(&format!("{field} = \"")) {
                    cur.insert(field, rest.trim_end_matches('"').to_owned());
                }
            }
        }
    }
    if in_candidate {
        flush(&mut approvals, &cur);
    }
    approvals
}

#[test]
fn trust_list_for_one_dep_does_not_render_another_deps_changed_path() {
    let (fixture, dep_e1, dep_e2) = consumer_with_two_colliding_nested_hooks();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("seed sync wrote phora.lock");
    let approvals = approvals_for_every_candidate(&lock_text);
    assert!(
        approvals.matches("source = \"depA\"").count() == 2,
        "premise: the seed sync must record TWO colliding nested candidates under root depA at \
         commit A (identical dep-target name + command, distinct nested repos); got:\n{lock_text}"
    );
    write(&lock_path, format!("{lock_text}{approvals}").as_bytes());

    write(&dep_e1.join("scripts/e1-only.sh"), b"#!/bin/sh\necho e1\n");
    git(&dep_e1, &["add", "-A"]);
    git(&dep_e1, &["commit", "-m", "tamper e1 only"]);
    write(&dep_e2.join("scripts/e2-only.sh"), b"#!/bin/sh\necho e2\n");
    git(&dep_e2, &["add", "-A"]);
    git(&dep_e2, &["commit", "-m", "tamper e2 only"]);

    let resync = run(&fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        resync.status.success(),
        "re-sync must succeed; stderr: {}",
        String::from_utf8_lossy(&resync.stderr)
    );

    let out = run(&fixture, &["trust", "depA", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust depA --list` must exit zero; got:\n{shown}"
    );

    let blocks: Vec<&str> = shown.split("hook ").skip(1).collect();
    let e1_block = blocks
        .iter()
        .find(|b| b.contains("e1-only.sh"))
        .copied()
        .unwrap_or("");
    let e2_block = blocks
        .iter()
        .find(|b| b.contains("e2-only.sh"))
        .copied()
        .unwrap_or("");
    assert!(
        !e1_block.is_empty() && !e2_block.is_empty(),
        "premise: each colliding nested hook's block must diff its OWN repo (e1-only.sh under one, \
         e2-only.sh under the other); got:\n{shown}"
    );
    assert!(
        !e1_block.contains("e2-only.sh"),
        "key-collision: the nested hook whose own change is `e1-only.sh` must NOT render the OTHER \
         nested dep's `e2-only.sh` — a stripped-key+source match cross-pairs the two; the prior \
         must be discriminated by the dep's url; got block:\n{e1_block}"
    );
    assert!(
        !e2_block.contains("e1-only.sh"),
        "key-collision: the `e2-only.sh` hook block must NOT render the other dep's `e1-only.sh`; \
         got block:\n{e2_block}"
    );
}

// THI-002 regression (gpt-5.5): a `#`-containing dep target name must not truncate the hook_id parse.

fn consumer_with_hash_named_composed_target() -> (Fixture, PathBuf) {
    let leaf = TempDir::new().expect("leaf repo");
    commit_repo(
        leaf.path(),
        &[("editor/init.lua", "-- init\n")],
        "version = 1\n",
    );
    let leaf_path = leaf.path().to_path_buf();

    let mut fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);
    let sentinel_abs = sentinel_path.display().to_string();

    let dep = TempDir::new().expect("dep repo");
    let manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"editor\"]\n\n\
         [targets.\"edit#or\"]\npath = \"editor\"\nsources = [\"editor\"]\n\n\
         [targets.\"edit#or\".hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    commit_repo(dep.path(), &[], &manifest);
    let dep_path = dep.path().to_path_buf();

    fixture.map_url("https://github.com/mock/leaf.git", &leaf_path);
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep_path.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    fixture.repos.push(leaf);
    fixture.repos.push(dep);
    (fixture, sentinel_path)
}

#[test]
fn trust_list_first_trust_lists_composed_surface_for_a_hash_named_dep_target() {
    let (fixture, _sentinel) = consumer_with_hash_named_composed_target();

    let seed = run(&fixture, &["sync", "--no-transitive-hooks"]);
    let seed_shown = format!(
        "{}{}",
        String::from_utf8_lossy(&seed.stdout),
        String::from_utf8_lossy(&seed.stderr)
    );
    assert!(
        seed.status.success(),
        "premise: a dep target NAME containing `#` (here `[targets.\"edit#or\"]`) must parse and \
         sync — the finding is only reachable if the `#`-named target is accepted; got {:?}\n{seed_shown}",
        seed.status.code()
    );

    let out = run(&fixture, &["trust", "mydeps", "--list"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`phora trust --list` is read-only and must exit zero; got:\n{shown}"
    );
    assert!(
        !shown.contains("composed surface unavailable"),
        "THI-002 regression: the candidate's composed target name is `...#edit#or`, so parsing the \
         hook_id with `split('#').next()` truncates at the FIRST `#` and loses the mapping — the \
         listing must NOT degrade to `composed surface unavailable` when the surface resolves; got:\n{shown}"
    );
    assert!(
        !shown.contains("first trust — no prior trusted commit to diff"),
        "THI-002: a first-trust candidate must list the composed surface, never the bare \
         no-prior-commit note; got:\n{shown}"
    );
    assert!(
        shown.lines().any(|l| l.trim() == "editor/init.lua"),
        "THI-002 regression: the composed surface for the `#`-named target must list the dep's \
         composed file `editor/init.lua` (matched as a standalone composed-file entry line so a \
         substring elsewhere cannot false-pass); got:\n{shown}"
    );
}

// THI-005: real-CLI `phora trust <source> --show <path>` against the actual binary, end-to-end.

fn leaf_path(fixture: &Fixture) -> PathBuf {
    fixture
        .repos
        .first()
        .expect("consumer_with_composed_files pushes the leaf first")
        .path()
        .to_path_buf()
}

fn seed_sync(fixture: &Fixture) {
    let seed = run(fixture, &["sync", "--no-transitive-hooks"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
}

#[test]
fn trust_show_prints_a_tracked_files_contents_and_exits_zero() {
    let (fixture, _sentinel) = consumer_with_composed_files();
    seed_sync(&fixture);

    let out = run(&fixture, &["trust", "mydeps", "--show", "nvim/init.lua"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "AC2: `phora trust --show <tracked file>` must exit zero; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("-- init"),
        "AC2: --show on a UTF-8 file must print the file's CONTENTS (the leaf's `nvim/init.lua` body \
         `-- init`), not a path or a directory listing; got stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("-- opts"),
        "AC2: --show on a single file must print only THAT file — never a sibling's body; got:\n{stdout}"
    );
}

#[test]
fn trust_show_lists_a_directorys_direct_entries_without_recursing() {
    let (fixture, _sentinel) = consumer_with_composed_files();
    seed_sync(&fixture);

    let out = run(&fixture, &["trust", "mydeps", "--show", "nvim"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "AC3: `phora trust --show <dir>` must exit zero; stderr:\n{stderr}"
    );
    assert!(
        stdout.lines().any(|l| l.trim() == "init.lua"),
        "AC3: --show on a directory must list its direct file entry `init.lua` ls-style; got:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.trim() == "lua/"),
        "AC3: --show on a directory must list a subdirectory as a dir entry (`lua/`, slash-suffixed), \
         not flatten into it; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("opts.lua"),
        "AC3: the directory listing must NOT recurse — the nested `lua/opts.lua` must stay behind the \
         `lua/` entry, never appear flattened at the top level; got:\n{stdout}"
    );
}

#[test]
fn trust_show_errors_clearly_for_an_absent_path() {
    let (fixture, _sentinel) = consumer_with_composed_files();
    seed_sync(&fixture);

    let out = run(&fixture, &["trust", "mydeps", "--show", "no/such/path"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "AC4: --show on a path absent at the candidate commit must error (non-zero exit); got success"
    );
    assert!(
        stderr.contains("no/such/path") && stderr.to_lowercase().contains("absent"),
        "AC4: the error must NAME the missing path and say it is absent/not-found; got stderr:\n{stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "AC4: an absent path must print NO file contents to stdout; got:\n{stdout}"
    );
}

#[test]
fn trust_show_refuses_binary_content_instead_of_dumping_raw_bytes() {
    let (fixture, _sentinel) = consumer_with_composed_files();

    let leaf = leaf_path(&fixture);
    write(&leaf.join("nvim/blob.bin"), &[0x00, 0x01, 0xff]);
    git(&leaf, &["add", "-A"]);
    git(&leaf, &["commit", "-m", "add binary blob"]);

    seed_sync(&fixture);

    let out = run(&fixture, &["trust", "mydeps", "--show", "nvim/blob.bin"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "AC5: --show on a binary (non-UTF-8) file must refuse with a non-zero exit; got success"
    );
    assert!(
        stderr.to_lowercase().contains("binary") || stderr.to_lowercase().contains("utf-8"),
        "AC5: the refusal must name the non-UTF-8/binary reason; got stderr:\n{stderr}"
    );
    assert!(
        !out.stdout.iter().any(|&b| b == 0xff || b == 0x00),
        "AC5: --show must NOT dump the raw bytes of a binary file — the `\\x00`/`\\xff` payload bytes \
         must never reach stdout; got stdout bytes: {:?}",
        out.stdout
    );
}

#[test]
fn trust_show_without_a_source_errors_naming_the_missing_source() {
    let (fixture, _sentinel) = consumer_with_composed_files();

    let out = run(&fixture, &["trust", "--show", "nvim/init.lua"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "AC6: `phora trust --show <path>` with NO source must error (non-zero exit); got success"
    );
    assert!(
        stderr.contains("source"),
        "AC6: the error must name the missing source (as `--revoke` does); got stderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("-- init"),
        "AC6: a missing source must short-circuit BEFORE any file read — no contents on stdout; got:\n{stdout}"
    );
}

#[test]
fn trust_show_reads_a_file_offline_from_the_mirror_after_the_source_repos_are_deleted() {
    let (fixture, _sentinel) = consumer_with_composed_files();
    seed_sync(&fixture);

    // Leave the cache git mirror as the only surviving source of these paths.
    for repo in &fixture.repos {
        std::fs::remove_dir_all(repo.path()).ok();
    }
    std::fs::remove_dir_all(fixture.home_path.join(".config")).ok();

    let out = run(&fixture, &["trust", "mydeps", "--show", "nvim/init.lua"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "AC7: --show must read the pinned commit OFFLINE from the cache mirror — it must not fetch; \
         stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("-- init"),
        "AC7: with the source repos AND the deployed ~/.config surface deleted, --show must still \
         print the file's contents resolved from the mirror; got:\n{stdout}"
    );
}
