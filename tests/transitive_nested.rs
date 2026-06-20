//! RED pins for TDEP-IMPORTS-NESTED-001: nested-depth (closure) composition.
//!
//! IMPORTS-001 shipped DEPTH-1 composition only. A consumer imports dep D; D's own
//! targets compose under the anchor. But if D itself declares an inner source E as
//! `transitive = true` and a D-target that `imports = ["E"]`, E's targets are NOT
//! composed today — `walk_recurse` descends for fetch/validate/cycle/fail-fast safety
//! but never builds an `Instance` or calls `compose_dep` for E. These tests pin the
//! missing nested composition: E's artifacts must deploy at the composed nested path
//! under the consumer anchor, route through the same confine chokepoint, and namespace
//! per enclosing instance so two parents importing a same-named E do not merge.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;

/// Phrase the confinement layer owns for an escaping destination; a nested dep-of-dep
/// escape must surface this exact phrase, proving CONFINE covers nested composition.
const CONFINE_ESCAPES_ANCHOR: &str = "escapes its anchor";
/// Phrase the compose chokepoint owns for a `..`/absolute dep-target path; either this
/// or the confine escape phrase proves the nested path went through the chokepoint.
const COMPOSE_REL_SUBPATH: &str = "must be a relative \
     subpath of the anchor";

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
    /// Inner transitive remotes must be non-local; insteadOf resolves a mock URL to a
    /// temp repo offline (the escape guard rejects local-path inner remotes).
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

fn payload_leaf(dir: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, found: &mut Option<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, name, found);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                *found = Some(path);
            }
        }
    }
    let mut found = None;
    walk(dir, name, &mut found);
    found
}

/// A leaf-only flat git source holding `pkg/<file>` (a subdir artifact the binder includes).
fn leaf_repo(dir: &Path, file: &str, body: &str) {
    commit_repo(dir, &[(&format!("pkg/{file}"), body)], "version = 1\n");
}

/// Behavior #1 — Nested composition to closure.
///
/// consumer imports D under `~/.config`; D (transitive) declares inner source E
/// (`transitive = true`) and a D-target `dnode` (path `d`) that `imports = ["E"]`;
/// E declares a flat leaf source bound to E-target `enode` (path `e`). The closure
/// must compose E's `enode` target under D's `dnode` anchor under the consumer anchor,
/// so E's artifact lands at `~/.config/d/e/pkg/leaf.txt`.
///
/// Today the dep-of-dep is fetched/validated but never composed → the file is ABSENT.
#[test]
fn dep_of_dep_targets_compose_to_closure_under_the_parent_anchor() {
    let leaf = TempDir::new().expect("e leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "from-e\n");

    // E: a transitive dep that binds the leaf and exposes one target `enode` at `e`.
    let dep_e = TempDir::new().expect("dep E repo");
    let e_manifest = "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/eleaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n";
    commit_repo(dep_e.path(), &[], e_manifest);

    // D: imports E (a dep-of-dep) under D's own target `dnode` at `d`.
    let dep_d = TempDir::new().expect("dep D repo");
    let d_manifest = "version = 1\n\n[sources.einner]\ngit = \"https://github.com/mock/depe.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d\"\nimports = [\"einner\"]\n";
    commit_repo(dep_d.path(), &[], d_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/eleaf.git", leaf.path());
    fixture.map_url("https://github.com/mock/depe.git", dep_e.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep_d}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep_d = dep_d.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let composed = fixture.home_path.join(".config/d/e/pkg/leaf.txt");
    assert!(
        !composed.exists(),
        "precondition: nothing deployed before sync at {}",
        composed.display()
    );

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "composing a dep-of-dep to closure must complete the sync; stderr: {stderr}"
    );

    assert_eq!(
        std::fs::read_to_string(&composed).unwrap_or_default(),
        "from-e\n",
        "the dep-of-dep `E`'s target `enode` must compose UNDER `D`'s anchor `d` UNDER \
         the consumer anchor `~/.config`, landing E's artifact at {} — today walk_recurse \
         descends but never composes nested deps, so this file is absent; stderr: {stderr}",
        composed.display()
    );
}

/// Behavior #2 — Instance.parent reflects the enclosing instance, not always "root".
///
/// Two distinct parent deps D1 and D2 each import a SAME-NAMED dep-of-dep `einner`
/// that points at DIFFERENT repos. If nested composition keyed `Instance.parent =
/// "root"` for both (today's hard-coded value), the two `einner` instances would key
/// identically and silently merge. Namespacing by the enclosing instance keeps them
/// distinct, so both nested leaves deploy with their own identity at distinct paths.
#[test]
fn nested_same_named_dep_of_dep_under_two_parents_do_not_merge() {
    let leaf_x = TempDir::new().expect("e leaf x");
    leaf_repo(leaf_x.path(), "id.txt", "from-x\n");
    let leaf_y = TempDir::new().expect("e leaf y");
    leaf_repo(leaf_y.path(), "id.txt", "from-y\n");

    // Two distinct E repos, each exposing a target `enode` at `e`, both reached through
    // a same-named inner source `einner`.
    let dep_e_x = TempDir::new().expect("E x repo");
    commit_repo(
        dep_e_x.path(),
        &[],
        "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/ex.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n",
    );
    let dep_e_y = TempDir::new().expect("E y repo");
    commit_repo(
        dep_e_y.path(),
        &[],
        "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/ey.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n",
    );

    // D1 and D2 each declare a same-named inner source `einner` reaching a DIFFERENT E.
    let dep_d1 = TempDir::new().expect("D1 repo");
    commit_repo(
        dep_d1.path(),
        &[],
        "version = 1\n\n[sources.einner]\ngit = \"https://github.com/mock/depex.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d1\"\nimports = [\"einner\"]\n",
    );
    let dep_d2 = TempDir::new().expect("D2 repo");
    commit_repo(
        dep_d2.path(),
        &[],
        "version = 1\n\n[sources.einner]\ngit = \"https://github.com/mock/depey.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d2\"\nimports = [\"einner\"]\n",
    );

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/ex.git", leaf_x.path());
    fixture.map_url("https://github.com/mock/ey.git", leaf_y.path());
    fixture.map_url("https://github.com/mock/depex.git", dep_e_x.path());
    fixture.map_url("https://github.com/mock/depey.git", dep_e_y.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.dep1]\ngit = \"{d1}\"\ntransitive = true\n\n\
         [sources.dep2]\ngit = \"{d2}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"dep1\", \"dep2\"]\n",
        d1 = dep_d1.path().display(),
        d2 = dep_d2.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "two parents each importing a same-named dep-of-dep must resolve as distinct \
         instances, not abort; stderr: {stderr}"
    );

    let from_x = fixture.home_path.join(".config/d1/e/pkg/id.txt");
    let from_y = fixture.home_path.join(".config/d2/e/pkg/id.txt");
    assert_eq!(
        std::fs::read_to_string(&from_x).unwrap_or_default(),
        "from-x\n",
        "D1's nested `einner` must keep its own identity (from-x) at {} — if Instance.parent \
         stayed `root` for both, the two same-named nested instances would merge; stderr: {stderr}",
        from_x.display()
    );
    assert_eq!(
        std::fs::read_to_string(&from_y).unwrap_or_default(),
        "from-y\n",
        "D2's nested `einner` is a DISTINCT Instance (from-y) at {} — namespacing by the \
         enclosing parent instance is what keeps it from merging with D1's; stderr: {stderr}",
        from_y.display()
    );
}

/// Behavior #3 — Nested composed destinations are CONFINED.
///
/// A dep-of-dep E declares a target whose path escapes via `..`. Because nested
/// composition must route through the SAME `compose_dep`/confine chokepoint as depth-1,
/// the escaping nested target must be rejected with the chokepoint diagnostic — not
/// silently deployed outside the anchor, and not silently ignored (today E never
/// composes, so the `..` is never even evaluated for confinement).
#[test]
fn nested_dep_of_dep_escaping_target_is_confined() {
    let leaf = TempDir::new().expect("e leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "from-e\n");

    // E exposes a target that climbs out of its anchor via `..`.
    let dep_e = TempDir::new().expect("dep E repo");
    let e_manifest = "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/eleaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.escape]\npath = \"../../../../escape-e\"\nsources = [\"editor\"]\n";
    commit_repo(dep_e.path(), &[], e_manifest);

    let dep_d = TempDir::new().expect("dep D repo");
    commit_repo(
        dep_d.path(),
        &[],
        "version = 1\n\n[sources.einner]\ngit = \"https://github.com/mock/depe.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d\"\nimports = [\"einner\"]\n",
    );

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/eleaf.git", leaf.path());
    fixture.map_url("https://github.com/mock/depe.git", dep_e.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep_d}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        dep_d = dep_d.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);

    assert!(
        !out.status.success(),
        "a nested dep-of-dep target escaping its anchor via `..` MUST fail the sync — \
         confinement has to cover nested composition, not just depth-1; stderr: {stderr}"
    );

    let escaped = fixture.home_path.join("escape-e");
    assert!(
        !escaped.exists(),
        "the escaping nested artifact must NOT be written outside the anchor at {}; stderr: {stderr}",
        escaped.display()
    );
    assert!(
        stderr.contains(CONFINE_ESCAPES_ANCHOR) || stderr.contains(COMPOSE_REL_SUBPATH),
        "the rejection must come from the compose/confine chokepoint (phrase `{CONFINE_ESCAPES_ANCHOR}` \
         or `{COMPOSE_REL_SUBPATH}`), proving the nested path was routed through it rather than \
         failing for an unrelated reason; got: {stderr}"
    );
}

/// Behavior #4 — Diamond still dedups (one fetch) but composes per-instance.
///
/// Both D1 and D2 import the SAME dep-of-dep repo `einner` (same url/ref/digest). The
/// `FetchNode` dedup means the shared E repo is fetched ONCE, but the closure must still
/// compose E once per enclosing instance, so E's leaf deploys under BOTH parents.
#[test]
fn diamond_dep_of_dep_dedups_one_fetch_but_composes_per_instance() {
    let leaf = TempDir::new().expect("e leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "shared-e\n");

    // ONE E repo reached from both parents (a diamond at the dep-of-dep level).
    let dep_e = TempDir::new().expect("dep E repo");
    commit_repo(
        dep_e.path(),
        &[],
        "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/eleaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.enode]\npath = \"e\"\nsources = [\"editor\"]\n",
    );

    let dep_d1 = TempDir::new().expect("D1 repo");
    commit_repo(
        dep_d1.path(),
        &[],
        "version = 1\n\n[sources.einner]\ngit = \"https://github.com/mock/depe.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d1\"\nimports = [\"einner\"]\n",
    );
    let dep_d2 = TempDir::new().expect("D2 repo");
    commit_repo(
        dep_d2.path(),
        &[],
        "version = 1\n\n[sources.einner]\ngit = \"https://github.com/mock/depe.git\"\ntransitive = true\n\n\
         [targets.dnode]\npath = \"d2\"\nimports = [\"einner\"]\n",
    );

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/eleaf.git", leaf.path());
    fixture.map_url("https://github.com/mock/depe.git", dep_e.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.dep1]\ngit = \"{d1}\"\ntransitive = true\n\n\
         [sources.dep2]\ngit = \"{d2}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"dep1\", \"dep2\"]\n",
        d1 = dep_d1.path().display(),
        d2 = dep_d2.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "a diamond at the dep-of-dep level must resolve (one fetch, two instances); stderr: {stderr}"
    );

    let under_d1 = fixture.home_path.join(".config/d1/e/pkg/leaf.txt");
    let under_d2 = fixture.home_path.join(".config/d2/e/pkg/leaf.txt");
    assert_eq!(
        std::fs::read_to_string(&under_d1).unwrap_or_default(),
        "shared-e\n",
        "the shared dep-of-dep must compose under D1 at {} (per-instance composition); stderr: {stderr}",
        under_d1.display()
    );
    assert_eq!(
        std::fs::read_to_string(&under_d2).unwrap_or_default(),
        "shared-e\n",
        "the SAME deduped fetch must ALSO compose under D2 at {} — dedup is per FetchNode, \
         composition is per Instance; stderr: {stderr}",
        under_d2.display()
    );
    assert!(
        payload_leaf(&fixture.home_path.join(".config/d1"), "leaf.txt").is_some()
            && payload_leaf(&fixture.home_path.join(".config/d2"), "leaf.txt").is_some(),
        "both parent subtrees must each carry the nested leaf"
    );
}
