//! RED pins for TDEP-CONFINE-001 destination confinement against the real deploy path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

/// Named-diagnostic contract phrases the confinement layer must own; each test asserts
/// the one phrase for its vector, never a disjunction of attacker substrings.
const CONFINE_LINK_REJECTED: &str = "transitive source cannot use deploy = \"link\"";
const CONFINE_SYMLINK_ANCESTOR: &str = "anchor ancestor is a symlink";
const CONFINE_PROTECTED_PATH: &str = "protected path";
/// Phrase owned by the orthogonal foreign-content guard; a CONFINE pass must NOT be attributable to it.
const CONFINE_FOREIGN_SKIP: &str = "skipping foreign content";

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

fn reject_unknown_field_stub(stderr: &str) {
    assert!(
        !stderr.contains("unknown field"),
        "`transitive`/`imports` must drive real composition, not be rejected by \
         deny_unknown_fields; got a parse stub: {stderr}"
    );
}

fn leaf_repo(dir: &Path, file: &str, body: &str) {
    commit_repo(dir, &[(&format!("pkg/{file}"), body)], "version = 1\n");
}

/// A consumer importing one transitive dep under `~/.config`, with the dep manifest
/// supplied verbatim so each test crafts its own escape vector.
fn consumer_with_dep(dep_dir: &Path) -> String {
    format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep = dep_dir.display(),
    )
}

#[cfg(unix)]
#[test]
fn anchor_side_symlink_ancestor_is_rejected_at_write_time() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "owned.txt", "pwned\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = consumer_with_dep(dep.path());
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let escape = fixture.home_path.join("escape-via-symlink");
    std::fs::create_dir_all(&escape).expect("create escape dir");
    let anchor = fixture.home_path.join(".config");
    std::fs::create_dir_all(&anchor).expect("create anchor");
    std::os::unix::fs::symlink(&escape, anchor.join("nvim")).expect("plant symlink ancestor");

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);

    assert!(
        !out.status.success(),
        "following an anchor-side symlink ancestor must fail closed (non-zero exit); a diagnostic \
         printed alongside exit 0 is a silent success. stderr: {stderr}"
    );

    let leaked = escape.join("pkg/owned.txt");
    assert!(
        !leaked.exists(),
        "a deploy that follows a pre-existing anchor-side symlink (`~/.config/nvim -> {}`) wrote \
         THROUGH it to {} — write-time per-component no-follow is absent",
        escape.display(),
        leaked.display()
    );
    assert!(
        stderr.contains(CONFINE_SYMLINK_ANCESTOR),
        "following an anchor-side symlink ancestor must emit `{CONFINE_SYMLINK_ANCESTOR}`; got: {stderr}"
    );
}

#[test]
fn transitive_source_declaring_deploy_link_is_rejected() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ndeploy = \"link\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = consumer_with_dep(dep.path());
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);

    assert!(
        !out.status.success(),
        "a transitive source declaring deploy=\"link\" must be REJECTED (fail-closed): link emits \
         a symlink to an absolute mirror path that cannot be confined; stderr: {stderr}"
    );
    assert!(
        stderr.contains(CONFINE_LINK_REJECTED),
        "the link rejection must emit the named diagnostic `{CONFINE_LINK_REJECTED}`; got: {stderr}"
    );
    let composed = fixture.home_path.join(".config/nvim/pkg/leaf.txt");
    let meta = std::fs::symlink_metadata(&composed);
    assert!(
        !meta.is_ok_and(|m| m.file_type().is_symlink()),
        "no symlink may be emitted for a rejected transitive link source"
    );
}

/// A consumer's OWN (non-transitive) source keeps `deploy = "link"`.
#[test]
fn consumer_own_link_source_still_deploys_a_symlink() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "who.txt", "consumer\n");

    let fixture = build_fixture();
    let config = format!(
        "version = 1\n\n[sources.editor]\ngit = \"{leaf}\"\nbranch = \"main\"\ndeploy = \"link\"\ninclude = [\"pkg\"]\n\n\
         [targets.own]\npath = \"~/own\"\nsources = [\"editor\"]\nlayout = \"flat\"\n",
        leaf = leaf.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a consumer's OWN link source must keep deploy=link and sync cleanly; stderr: {stderr}"
    );
    let linked = fixture.home_path.join("own/pkg");
    let meta = std::fs::symlink_metadata(&linked);
    assert!(
        meta.is_ok_and(|m| m.file_type().is_symlink()),
        "the consumer's own link source must still emit a symlink at {}; confinement must not \
         coerce a NON-transitive source's declared mode; stderr: {stderr}",
        linked.display()
    );
}

#[test]
fn dep_cannot_write_into_the_consumer_cwd_git_directory() {
    let leaf = TempDir::new().expect("leaf repo");
    commit_repo(leaf.path(), &[("hooks/post.sh", "evil\n")], "version = 1\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"hooks\"]\n\n\
         [targets.t]\npath = \".git\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.root]\npath = \".\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);

    assert!(
        !out.status.success(),
        "writing into the consumer cwd `.git` must fail closed (non-zero exit); a diagnostic \
         printed alongside exit 0 is a silent success. stderr: {stderr}"
    );

    let planted = fixture.cwd.path().join(".git/hooks/post.sh");
    assert!(
        !planted.exists(),
        "a dep must NOT write into the consumer cwd `.git` (a ProtectedPathSet member, even when \
         absent before sync); it landed at {}",
        planted.display()
    );
    // Attribution: the no-write must come from CONFINE, not the orthogonal foreign-content guard.
    assert!(
        !stderr.contains(CONFINE_FOREIGN_SKIP),
        "the rejection must be attributable to confinement, not the foreign-content guard \
         (`{CONFINE_FOREIGN_SKIP}`), which would block this write with zero confinement; got: {stderr}"
    );
    assert!(
        stderr.contains(CONFINE_PROTECTED_PATH),
        "writing a ProtectedPathSet member must emit `{CONFINE_PROTECTED_PATH}`; got: {stderr}"
    );
}

#[test]
fn copy_staging_stays_under_the_confined_anchor_not_the_raw_target_parent() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "owned.txt", "payload\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.atroot]\npath = \".\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = consumer_with_dep(dep.path());
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let anchor = fixture.home_path.join(".config");
    std::fs::create_dir_all(&anchor).expect("anchor");
    let raw_staging_base = fixture.home_path.join(".phora-stage");
    std::fs::write(&raw_staging_base, b"obstruction").expect("plant staging obstruction");

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);

    let confined_dst = anchor.join("pkg/owned.txt");
    assert!(
        confined_dst.exists(),
        "the copy deploy must stage under the CONFINED anchor and land the artifact at {}; an \
         obstruction planted at the RAW-target staging base `{}` blocked it, proving staging is \
         derived from `target_parent(raw_target_path)` (the anchor's parent), not the confined \
         dst. stderr: {stderr}",
        confined_dst.display(),
        raw_staging_base.display(),
    );
    assert!(
        raw_staging_base.is_file(),
        "the out-of-anchor obstruction must remain an untouched file; staging must never have \
         reached `{}`",
        raw_staging_base.display(),
    );
}

#[test]
fn prune_retains_imported_transitive_artifacts() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = consumer_with_dep(dep.path());
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let first = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&first.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        first.status.success(),
        "first sync must succeed; stderr: {stderr}"
    );

    let composed = fixture.home_path.join(".config/nvim/pkg/leaf.txt");
    assert!(
        composed.exists(),
        "precondition: the transitive artifact must deploy on the first sync"
    );

    let second = run(&fixture, &["sync"]);
    let stderr2 = String::from_utf8_lossy(&second.stderr);
    assert!(
        second.status.success(),
        "second sync must succeed; stderr: {stderr2}"
    );
    assert!(
        composed.exists(),
        "prune must RETAIN an imported transitive artifact (its expected-set includes transitive \
         targets from the resolved graph); it was orphan-pruned. stderr: {stderr2}"
    );
}
