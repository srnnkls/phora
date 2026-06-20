//! TDEP-HOOK-TRUST-001: behavior-pinned, consumer-owned trust for transitive dep hooks.
//! A composed dep `on_change` hook is stripped by default; trust (pinned to the R5
//! preimage, which binds the dep's resolved commit SHA) is what lets a pinned hook run
//! under CI/non-TTY, while untrusted hooks are skipped. A new approval persists to the
//! consumer lock's `trusted_hooks` section.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
    insteadof: Vec<(String, String)>,
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

/// Runs the binary with no controlling terminal: `stdin` is the closed default, so
/// `IsTerminal` is false — this is the CI / non-TTY path the trust gate must respect.
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

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", message]);
}

fn head_commit(dir: &Path) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "HEAD"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git rev-parse runs");
    String::from_utf8(out.stdout)
        .expect("rev-parse utf8")
        .trim()
        .to_owned()
}

/// A leaf source holding `pkg/<file>` that the dep target includes.
fn leaf_repo(dir: &Path, file: &str, body: &str) {
    commit_repo(dir, &[(&format!("pkg/{file}"), body)], "version = 1\n");
}

/// The sentinel a dep `on_change` hook writes; its presence proves the hook RAN.
fn sentinel(fixture: &Fixture) -> PathBuf {
    fixture.home_path.join("hook-ran.sentinel")
}

/// A dep whose composed `on_change` hook writes the sentinel into `$HOME`. The hook
/// `run` touches an absolute path so the side-effect is observable from the test.
fn dep_with_on_change_hook(dep: &Path, sentinel_abs: &str) {
    let manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n\n\
         [targets.nvim.hooks]\non_change = \"touch '{sentinel_abs}'\"\n",
    );
    commit_repo(dep, &[], &manifest);
}

const CONSUMER_HOOK_SENTINEL: &str = "consumer-hook-ran.sentinel";

#[test]
fn untrusted_transitive_hook_is_skipped_under_non_tty_without_error() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel_path.display().to_string());

    let mut fixture = fixture;
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "AC4: an untrusted transitive hook under non-TTY must be SKIPPED, not error the sync; \
         stderr: {stderr}"
    );
    assert!(
        !sentinel_path.exists(),
        "AC4: with no consumer trust approval and no TTY to prompt, the dep's on_change hook must \
         NOT run — the sentinel at {} must be absent",
        sentinel_path.display()
    );
}

#[test]
fn recording_a_candidate_does_not_by_itself_trust_it() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel_path.display().to_string());

    let mut fixture = fixture;
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let first = run(&fixture, &["sync"]);
    assert!(
        first.status.success(),
        "first sync must exit zero; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        !sentinel_path.exists(),
        "first sync surfaces the candidate but must NOT run the untrusted hook"
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("first sync wrote phora.lock");
    assert!(
        extract_first_preimage(&lock_text).is_some(),
        "premise: the first sync must have RECORDED the candidate's commit-bound preimage so a \
         consumer could later inspect-then-trust it; phora.lock:\n{lock_text}"
    );

    let second = run(&fixture, &["sync"]);
    assert!(
        second.status.success(),
        "second sync with no approval must still exit zero; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        !sentinel_path.exists(),
        "anti-TOFU: merely RECORDING a candidate preimage must NOT grant trust — a second sync \
         with no [[trusted_hooks]] approval must STILL skip the hook; the sentinel at {} must be \
         absent",
        sentinel_path.display()
    );
}

#[test]
fn trusted_transitive_hook_runs_under_non_tty_when_pinned_in_consumer_lock() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel_path.display().to_string());

    let mut fixture = fixture;
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let first = run(&fixture, &["sync"]);
    assert!(
        first.status.success(),
        "the seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        !sentinel_path.exists(),
        "premise: the untrusted hook must not have run during the seeding sync"
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("phora.lock written by first sync");

    let preimage = extract_first_preimage(&lock_text).expect(
        "AC6 premise: after a sync that interpreted a transitive on_change hook, the consumer must \
         have a commit-bound `blake3:` preimage to pin a trust approval against",
    );

    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"approved-by-test\"\n\
         hook_id = \"composed#on_change#deadbeef\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n",
    );
    write(&lock_path, approved.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a pinned-trust sync must complete; stderr: {stderr}"
    );
    assert!(
        sentinel_path.exists(),
        "AC4: a transitive on_change hook whose commit-bound preimage is pinned in the consumer's \
         trusted_hooks must RUN even under non-TTY — the sentinel at {} must exist; stderr: {stderr}",
        sentinel_path.display()
    );
}

#[test]
fn no_transitive_hooks_flag_suppresses_trusted_dep_hooks_but_keeps_consumer_own_hooks() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");
    let consumer_leaf = TempDir::new().expect("consumer leaf");
    leaf_repo(consumer_leaf.path(), "own.txt", "consumer\n");

    let fixture = build_fixture();
    let dep_sentinel = sentinel(&fixture);
    let consumer_sentinel = fixture.home_path.join(CONSUMER_HOOK_SENTINEL);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &dep_sentinel.display().to_string());

    let mut fixture = fixture;
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.own]\ngit = \"{consumer_leaf}\"\ninclude = [\"pkg\"]\n\n\
         [sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.mine]\npath = \"~/mine\"\nsources = [\"own\"]\n\n\
         [targets.mine.hooks]\non_change = \"touch '{consumer_sentinel}'\"\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        consumer_leaf = consumer_leaf.path().display(),
        dep = dep.path().display(),
        consumer_sentinel = consumer_sentinel.display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    // Seed + approve the dep hook so it WOULD run absent the flag; only then does
    // --no-transitive-hooks have something to suppress (a stripped, never-trusted hook would
    // make this assertion pass vacuously).
    let seed = run(&fixture, &["sync"]);
    assert!(
        seed.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("seed sync wrote phora.lock");
    let preimage = extract_first_preimage(&lock_text)
        .expect("the seed sync must surface a commit-bound preimage for the dep hook");
    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"approved-by-test\"\n\
         hook_id = \"composed#on_change#deadbeef\"\n\
         preimage = \"{preimage}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n",
    );
    write(&lock_path, approved.as_bytes());
    let _ = std::fs::remove_file(&dep_sentinel);
    let _ = std::fs::remove_file(&consumer_sentinel);

    // INV-3 (consumer on_change is change-gated): advance the leaf on `main` so [targets.mine]
    // deploys new content and its own hook legitimately re-fires. `--force` re-resolves past the
    // lock pin so the new leaf commit is picked up.
    write(&consumer_leaf.path().join("pkg/own.txt"), b"consumer v2\n");
    commit_all(consumer_leaf.path(), "consumer leaf change");

    let out = run(&fixture, &["sync", "--no-transitive-hooks", "--force"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "AC5: --no-transitive-hooks sync must complete; stderr: {stderr}"
    );
    assert!(
        consumer_sentinel.exists(),
        "AC5: --no-transitive-hooks must NOT suppress the consumer's OWN target hook — on a real \
         change to [targets.mine] its sentinel at {} must exist; stderr: {stderr}",
        consumer_sentinel.display()
    );
    assert!(
        !dep_sentinel.exists(),
        "AC5: --no-transitive-hooks must suppress the composed dep's transitive hook EVEN WHEN it \
         is trusted — its sentinel at {} must be absent; stderr: {stderr}",
        dep_sentinel.display()
    );
}

#[test]
fn matching_trusted_hook_approval_persists_across_a_resync() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel_path.display().to_string());

    let mut fixture = fixture;
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let first = run(&fixture, &["sync"]);
    assert!(
        first.status.success(),
        "seeding sync must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    // Approve the candidate by appending a trusted_hooks entry to the consumer lock, pinned to
    // the commit-bound preimage the first sync surfaced. The next sync must PERSIST this
    // approval back into phora.lock rather than dropping it (today split_locks writes an empty
    // trusted_hooks, so a pre-existing approval is lost — the RED this guards).
    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("first sync wrote phora.lock");
    let approved = format!(
        "{lock_text}\n[[trusted_hooks]]\n\
         dep_instance = \"approved-by-test\"\n\
         hook_id = \"composed#on_change#deadbeef\"\n\
         preimage = \"blake3:approvedpreimage\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n",
    );
    write(&lock_path, approved.as_bytes());

    let out = run(&fixture, &["sync"]);
    assert!(
        out.status.success(),
        "re-sync with an approval present must complete; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rewritten = std::fs::read_to_string(&lock_path).expect("re-sync rewrote phora.lock");
    assert!(
        rewritten.contains("trusted_hooks"),
        "AC6: a consumer-recorded trusted_hooks approval must SURVIVE a re-sync — the producer must \
         write the trusted_hooks section back into phora.lock, not drop it; phora.lock:\n{rewritten}"
    );
    assert!(
        rewritten.contains("blake3:approvedpreimage"),
        "AC6: the specific approved preimage must persist verbatim across the re-sync; phora.lock:\n{rewritten}"
    );
}

#[test]
fn changing_the_dep_commit_invalidates_a_commit_pinned_trust_approval() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let fixture = build_fixture();
    let sentinel_path = sentinel(&fixture);

    let dep = TempDir::new().expect("dep repo");
    dep_with_on_change_hook(dep.path(), &sentinel_path.display().to_string());
    let commit_a = head_commit(dep.path());

    let mut fixture = fixture;
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let first = run(&fixture, &["sync"]);
    assert!(
        first.status.success(),
        "seeding sync at commit A must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let lock_path = fixture.cwd.path().join("phora.lock");
    let lock_at_a = std::fs::read_to_string(&lock_path).expect("first sync wrote phora.lock");
    let preimage_a = extract_first_preimage(&lock_at_a)
        .expect("the seeding sync must surface a commit-bound preimage to pin against");

    // The dep's hook SCRIPT body changes but its EXPORT SET (the included `pkg/`) does not.
    // Editing the hook command alone moves the commit SHA; the preimage binds the SHA, so the
    // approval recorded at commit A must NOT match at commit B.
    let edited_manifest = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n\n\
         [targets.nvim.hooks]\non_change = \"touch '{sentinel}' # commit B body\"\n",
        sentinel = sentinel_path.display(),
    );
    write(&dep.path().join("phora.toml"), edited_manifest.as_bytes());
    commit_all(dep.path(), "edit hook body, same export set");
    let commit_b = head_commit(dep.path());
    assert_ne!(
        commit_a, commit_b,
        "premise: the dep advanced to a new commit"
    );

    // Re-resolve to commit B with the commit-A approval pinned in the consumer lock.
    let approved = format!(
        "{lock_at_a}\n[[trusted_hooks]]\n\
         dep_instance = \"approved-at-commit-a\"\n\
         hook_id = \"composed#on_change#deadbeef\"\n\
         preimage = \"{preimage_a}\"\n\
         approved_at = \"2026-06-20T00:00:00Z\"\n",
    );
    write(&lock_path, approved.as_bytes());

    // Wipe any prior decision state so resolution is fresh against commit B.
    let _ = std::fs::remove_file(&sentinel_path);

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "re-sync at commit B must complete without error; stderr: {stderr}"
    );
    assert!(
        !sentinel_path.exists(),
        "AC1: the trust approval was pinned to commit A's preimage; at commit B the preimage binds \
         a different commit SHA, so the approval must NOT match — the hook must NOT auto-run under \
         non-TTY, it must re-prompt (and thus stay skipped here); sentinel: {}",
        sentinel_path.display()
    );
}

/// Pulls the first `preimage = "blake3:..."` value out of a serialized lock.
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
