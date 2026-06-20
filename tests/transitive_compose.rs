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
    /// Inner transitive remotes must be non-local (the escape guard rejects local-path inner remotes); insteadOf resolves a mock URL to a temp repo offline.
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
        "`transitive`/`imports` must be accepted wire keys driving real composition, \
         not rejected by deny_unknown_fields; got a parse stub: {stderr}"
    );
}

/// Named-diagnostic contract phrase the composed-path collision check must emit.
const COMPOSED_DEST_COLLISION: &str = "composed targets resolve to the same destination";

fn payload_leaf(dir: &Path, name: &str) -> PathBuf {
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
    found.unwrap_or_else(|| panic!("no deployed file named `{name}` under {}", dir.display()))
}

/// A leaf-only flat git source holding `pkg/<file>` (a subdir artifact the binder includes).
fn leaf_repo(dir: &Path, file: &str, body: &str) {
    commit_repo(dir, &[(&format!("pkg/{file}"), body)], "version = 1\n");
}

#[test]
fn control_consumer_own_flat_target_deploys() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "who.txt", "consumer\n");
    let fixture = build_fixture();
    let config = format!(
        "version = 1\n\n[sources.editor]\ngit = \"{leaf}\"\nbranch = \"main\"\ninclude = [\"pkg\"]\n\n\
         [targets.own]\npath = \"~/own\"\nsources = [\"editor\"]\nlayout = \"flat\"\n",
        leaf = leaf.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());
    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "control sync must succeed; stderr: {stderr}"
    );
    let deployed = fixture.home_path.join("own/pkg/who.txt");
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap_or_default(),
        "consumer\n",
        "CONTROL: a plain consumer-own flat target must deploy its file at {}; stderr: {stderr}",
        deployed.display()
    );
}

#[test]
fn imported_dep_targets_are_composed_under_the_anchor_path() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
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
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "mounting a transitive dep must compose its targets and complete the sync; stderr: {stderr}"
    );

    let composed = fixture.home_path.join(".config/nvim/pkg/leaf.txt");
    assert!(
        composed.exists(),
        "the dep target `nvim` (path=\"nvim\") must compose UNDER the anchor `~/.config`, \
         producing {}, but it was not deployed; stderr: {stderr}",
        composed.display()
    );
}

#[test]
fn dep_layout_governs_composed_artifacts_not_consumer_anchor_layout() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    // The dep target uses `by-source` layout: artifact lands at <relpath>/<source>/<artifact>.
    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nlayout = \"by-source\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    // The consumer anchor declares a DIFFERENT layout (prefixed). It must NOT re-apply.
    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leaf.git", leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n[sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nlayout = \"prefixed\"\nimports = [\"mydeps\"]\n",
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "composing a dep with its own layout must complete the sync; stderr: {stderr}"
    );

    let dep_governed = fixture.home_path.join(".config/nvim/editor/pkg/leaf.txt");
    assert!(
        dep_governed.exists(),
        "the DEP's own `by-source` layout must govern its artifacts \
         (anchor/nvim/editor/leaf.txt), formula \
         anchor.expanded_path / dep_target.relative_path / dep_target.layout.artifact_path; \
         expected {}, stderr: {stderr}",
        dep_governed.display()
    );

    let anchor_governed = fixture.home_path.join(".config/editor-leaf.txt");
    let anchor_flat = fixture.home_path.join(".config/nvim/pkg/leaf.txt");
    assert!(
        !anchor_governed.exists() && !anchor_flat.exists(),
        "the consumer anchor `prefixed` layout must NOT re-apply to the mounted subtree; \
         found a non-dep-governed path under the anchor"
    );
}

#[test]
fn consumer_wins_on_consumer_vs_dep_source_name_collision() {
    // The consumer and the dep both define a source named `editor` pointing at DIFFERENT repos.
    let consumer_leaf = TempDir::new().expect("consumer leaf");
    leaf_repo(consumer_leaf.path(), "who.txt", "consumer\n");
    let dep_leaf = TempDir::new().expect("dep leaf");
    leaf_repo(dep_leaf.path(), "who.txt", "dep\n");

    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n[sources.editor]\ngit = \"https://github.com/mock/depleaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.nvim]\npath = \"nvim\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/depleaf.git", dep_leaf.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.editor]\ngit = \"{consumer_leaf}\"\ninclude = [\"pkg\"]\n\n\
         [sources.mydeps]\ngit = \"{dep}\"\ntransitive = true\n\n\
         [targets.own]\npath = \"~/own\"\nsources = [\"editor\"]\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"mydeps\"]\n",
        consumer_leaf = consumer_leaf.path().display(),
        dep = dep.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "a consumer-vs-dep name collision must resolve (consumer wins), not abort; stderr: {stderr}"
    );

    let consumer_artifact = fixture.home_path.join("own/pkg/who.txt");
    assert_eq!(
        std::fs::read_to_string(&consumer_artifact).unwrap_or_default(),
        "consumer\n",
        "the consumer's own `editor` source must resolve to the CONSUMER repo, not the dep's \
         same-named source (consumer wins on collision); stderr: {stderr}"
    );
    let dep_artifact = payload_leaf(&fixture.home_path.join(".config"), "who.txt");
    assert_eq!(
        std::fs::read_to_string(&dep_artifact).unwrap_or_default(),
        "dep\n",
        "the dep's `editor` source is a DISTINCT namespaced instance and must still resolve \
         to the dep repo, proving no silent merge with the consumer's `editor`; stderr: {stderr}"
    );
}

#[test]
fn two_deps_with_same_inner_source_name_do_not_silently_merge() {
    // Two independent deps each define an inner source named `pkg` pointing at different repos.
    let leaf_a = TempDir::new().expect("leaf a");
    leaf_repo(leaf_a.path(), "id.txt", "from-a\n");
    let leaf_b = TempDir::new().expect("leaf b");
    leaf_repo(leaf_b.path(), "id.txt", "from-b\n");

    let dep_a = TempDir::new().expect("dep a");
    commit_repo(
        dep_a.path(),
        &[],
        "version = 1\n\n[sources.pkg]\ngit = \"https://github.com/mock/leafa.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.t]\npath = \"a\"\nsources = [\"pkg\"]\n",
    );
    let dep_b = TempDir::new().expect("dep b");
    commit_repo(
        dep_b.path(),
        &[],
        "version = 1\n\n[sources.pkg]\ngit = \"https://github.com/mock/leafb.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.t]\npath = \"b\"\nsources = [\"pkg\"]\n",
    );

    let mut fixture = build_fixture();
    fixture.map_url("https://github.com/mock/leafa.git", leaf_a.path());
    fixture.map_url("https://github.com/mock/leafb.git", leaf_b.path());
    fixture.finish_gitconfig();
    let config = format!(
        "version = 1\n\n\
         [sources.depa]\ngit = \"{dep_a}\"\ntransitive = true\n\n\
         [sources.depb]\ngit = \"{dep_b}\"\ntransitive = true\n\n\
         [targets.dotcfg]\npath = \"~/.config\"\nimports = [\"depa\", \"depb\"]\n",
        dep_a = dep_a.path().display(),
        dep_b = dep_b.path().display(),
    );
    write(&fixture.cwd.path().join("phora.toml"), config.as_bytes());

    let out = run(&fixture, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    reject_unknown_field_stub(&stderr);
    assert!(
        out.status.success(),
        "two deps with a same-named inner source mount at distinct anchors and must both \
         deploy as distinct instances; stderr: {stderr}"
    );

    let from_a = fixture.home_path.join(".config/a/pkg/id.txt");
    let from_b = fixture.home_path.join(".config/b/pkg/id.txt");
    assert_eq!(
        std::fs::read_to_string(&from_a).unwrap_or_default(),
        "from-a\n",
        "depa's inner `pkg` must keep its own identity (from-a), not be merged with depb's `pkg`; stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&from_b).unwrap_or_default(),
        "from-b\n",
        "depb's inner `pkg` is a DISTINCT Instance (from-b) — same inner name must NOT silently \
         merge into one node; stderr: {stderr}"
    );
}

#[test]
fn two_dep_targets_composing_to_the_same_destination_is_a_hard_error() {
    let leaf = TempDir::new().expect("leaf repo");
    leaf_repo(leaf.path(), "leaf.txt", "payload\n");

    // A dep whose TWO targets both compose to the SAME relative subpath `clash`.
    let dep = TempDir::new().expect("dep repo");
    let dep_manifest = "version = 1\n\n\
         [sources.editor]\ngit = \"https://github.com/mock/leaf.git\"\ninclude = [\"pkg\"]\n\n\
         [targets.one]\npath = \"clash\"\nsources = [\"editor\"]\n\n\
         [targets.two]\npath = \"clash\"\nsources = [\"editor\"]\n";
    commit_repo(dep.path(), &[], dep_manifest);

    let mut fixture = build_fixture();
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
    reject_unknown_field_stub(&stderr);
    assert!(
        !out.status.success(),
        "two composed dep targets landing on the SAME destination path must be a HARD ERROR, \
         not a silent last-writer-wins; stderr: {stderr}"
    );
    assert!(
        stderr.contains(COMPOSED_DEST_COLLISION),
        "the collision must emit the named diagnostic `{COMPOSED_DEST_COLLISION}`, \
         not a generic fs error or unrelated sync failure; got: {stderr}"
    );
    let collided = fixture
        .home_path
        .join(".config/clash")
        .display()
        .to_string();
    assert!(
        stderr.contains(&collided),
        "the diagnostic must name the concrete composed destination `{collided}` \
         so it cannot be satisfied by an unrelated collision; got: {stderr}"
    );
}
