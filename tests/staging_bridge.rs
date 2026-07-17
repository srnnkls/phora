use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::str::FromStr as _;

use phora::config::Config;
use phora::kernel::SourceName;
use phora::projection::build::project_target;
use phora::projection::model::{
    BindingProjectionInput, CollapsePreference, ContentTransform, LayoutSpec, LayoutStyle,
    Materialization, MaterializationPolicy, OfferSpec, ResolvedSourceRef, TakeSpec,
    TargetProjection, TemplatePolicy,
};
use phora::source::{GitBackend, SourceBackend as _, SourceInventory};
use phora::sync::StageBridge;
use tempfile::TempDir;

mod common;

const CONFIG: &str = "\
version = 1

[vars]
name = \"world\"

[sources.dotfiles]
path = \"__SRC__\"
branch = \"main\"
include = [\"editor\", \"lint\", \"motd.md.tmpl\"]

[targets.home]
path = \"__TARGET__\"
sources = [\"dotfiles\"]
layout = \"flat\"
";

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
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
    write(&root.join("editor/lua/opts.lua"), b"return {}\n");
    write(&root.join("lint/rules.toml"), b"[rules]\n");
    write(&root.join("motd.md.tmpl"), b"hello {{ name }}\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    src_path: PathBuf,
    target_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let src = TempDir::new().expect("src tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");

    build_source_repo(src.path());

    let home_path = home.path().to_path_buf();
    let src_path = src.path().to_path_buf();
    let target_path = home_path.join("deploy");
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");

    Fixture {
        _home: home,
        _src: src,
        cwd,
        home_path,
        src_path,
        target_path,
        xdg_cache,
        xdg_state,
    }
}

impl Fixture {
    fn config(&self) -> String {
        CONFIG
            .replace("__SRC__", &self.src_path.to_string_lossy())
            .replace("__TARGET__", &self.target_path.to_string_lossy())
    }

    fn write_config(&self) {
        write(
            &self.cwd.path().join("phora.toml"),
            self.config().as_bytes(),
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(self.cwd.path())
            .env("HOME", &self.home_path)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE")
            .output()
            .expect("phora binary runs")
    }

    fn record_dir(&self) -> PathBuf {
        let base = self.xdg_state.join("phora").join("projects");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
            .expect("read projects base")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        assert!(
            dirs.len() == 1,
            "exactly one project registry after sync, got {dirs:?}"
        );
        dirs.pop()
            .expect("one project dir")
            .join("targets/home/artifacts/dotfiles")
    }
}

fn assert_success(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx} must succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn fixture_source_commit(fx: &Fixture) -> String {
    let out = Command::new("git")
        .current_dir(&fx.src_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse runs");
    assert!(out.status.success(), "rev-parse succeeds");
    String::from_utf8(out.stdout)
        .expect("utf8 sha")
        .trim()
        .to_owned()
}

fn deploy_flow_projection(fx: &Fixture) -> TargetProjection {
    let cfg = Config::parse(&fx.config()).expect("fixture config parses");
    let parsed = cfg.parsed_sources().expect("fixture sources parse");
    let target = &cfg.targets["home"];
    let commit = fixture_source_commit(fx);

    let bindings = target.resolve_sources(&parsed);
    assert!(
        bindings.len() == 1,
        "the fixture declares exactly one binding"
    );
    let binding = &bindings[0];
    let source = &parsed[binding.source];

    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let url = fx.src_path.to_string_lossy().into_owned();
    let name = sn(binding.source);
    backend.fetch(&name, &url).expect("fetch builds mirror");
    let leaves = backend
        .list_source_leaves(&name, &url, &commit, None)
        .expect("list source leaves");

    let inventory =
        SourceInventory::from_paths(leaves.iter().map(String::as_str)).expect("valid inventory");
    let offer = OfferSpec::from(source.offer());
    let take = TakeSpec::from_entries(binding.take);
    let templates = TemplatePolicy::from(&binding.template_opt_in);
    let layout = LayoutSpec::from(&target.layout());
    let resolved = ResolvedSourceRef::new(binding.source, commit);

    let input = BindingProjectionInput {
        identity: binding.identity,
        source: &resolved,
        offer: &offer,
        inventory: &inventory,
        take: &take,
        collapse: CollapsePreference::from(binding.collapse),
        materialization: MaterializationPolicy::from(&source.deploy_mode()),
        layout: &layout,
        templates: &templates,
    };
    project_target("home", &[input]).expect("the fixture projects")
}

fn record_manifest_paths(record_file: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(record_file)
        .unwrap_or_else(|e| panic!("read record {}: {e}", record_file.display()));
    let mut paths: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("path = "))
        .map(|value| value.trim_matches('"').to_owned())
        .collect();
    paths.sort();
    paths
}

#[test]
fn bridge_exposes_deployed_destinations_for_every_staged_artifact() {
    let fx = build_fixture();
    fx.write_config();
    assert_success(&fx.run(&["sync"]), "sync");

    let projection = deploy_flow_projection(&fx);
    assert!(
        projection.artifacts.len() == 3,
        "the fixture projects editor + lint + motd.md, got {:?}",
        projection
            .artifacts
            .iter()
            .map(|a| a.destination.as_str())
            .collect::<Vec<_>>()
    );

    for artifact in &projection.artifacts {
        let bridge = StageBridge {
            artifact,
            target: &projection,
        };
        assert!(
            std::ptr::eq(bridge.artifact, artifact)
                && std::ptr::eq(bridge.target, &raw const projection),
            "the bridge carries borrows of the projection values, not clones"
        );
        let deployed = fx.target_path.join(bridge.artifact.destination.as_str());
        match &bridge.artifact.materialization {
            Materialization::Leaf(_) => assert!(
                deployed.is_file(),
                "deploy wrote the bridged leaf destination `{}` as a file",
                deployed.display()
            ),
            Materialization::CollapsedDir { .. } => {
                assert!(
                    deployed.is_dir(),
                    "deploy wrote the bridged dir destination `{}` as a directory",
                    deployed.display()
                );
                for leaf in &bridge.artifact.leaves {
                    let file = deployed.join(leaf.destination.as_str());
                    assert!(
                        file.is_file(),
                        "deploy wrote bridged leaf `{}` under the dir destination",
                        file.display()
                    );
                }
            }
        }
    }

    let bridged_keys: BTreeSet<String> = projection
        .artifacts
        .iter()
        .map(|artifact| artifact.materialization.published_key().to_owned())
        .collect();
    let recorded_keys: BTreeSet<String> = std::fs::read_dir(fx.record_dir())
        .expect("sync recorded artifact records")
        .map(|entry| {
            entry
                .expect("dir entry")
                .path()
                .file_stem()
                .expect("record file stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        bridged_keys, recorded_keys,
        "the adapter-derived artifact keys must equal the keys deploy recorded — the bridge \
         exposes the SAME artifacts the old flow stages, or T015's swap would change behavior"
    );
}

#[test]
fn bridge_leaves_match_the_staged_manifest_and_rendered_bytes() {
    let fx = build_fixture();
    fx.write_config();
    assert_success(&fx.run(&["sync"]), "sync");

    let projection = deploy_flow_projection(&fx);
    let record_dir = fx.record_dir();

    for artifact in &projection.artifacts {
        let bridge = StageBridge {
            artifact,
            target: &projection,
        };
        let key = bridge.artifact.materialization.published_key();
        let mut bridged_leaves: Vec<String> = bridge
            .artifact
            .leaves
            .iter()
            .map(|leaf| leaf.destination.as_str().to_owned())
            .collect();
        bridged_leaves.sort();
        let manifest = record_manifest_paths(&record_dir.join(format!("{key}.toml")));
        assert!(
            !manifest.is_empty(),
            "the record for `{key}` pins at least one manifest file"
        );
        assert_eq!(
            bridged_leaves, manifest,
            "artifact `{key}`: the bridged ProjectedLeaf destinations must equal the manifest \
             paths the old staging flow wrote"
        );
    }

    let motd = projection
        .artifacts
        .iter()
        .find(|artifact| artifact.materialization.published_key() == "motd.md")
        .expect("the fixture projects the templated leaf motd.md");
    assert!(
        motd.leaves.len() == 1 && motd.leaves[0].transform == ContentTransform::Template,
        "the bridged .tmpl leaf carries ContentTransform::Template, got {:?}",
        motd.leaves
    );
    let rendered = std::fs::read_to_string(fx.target_path.join("motd.md"))
        .expect("deploy wrote the rendered template");
    assert_eq!(
        rendered, "hello world\n",
        "the bytes deploy staged for the bridged Template leaf are the rendered bytes"
    );

    let editor = projection
        .artifacts
        .iter()
        .find(|artifact| artifact.materialization.published_key() == "editor")
        .expect("the fixture projects the collapsed editor dir");
    assert!(
        editor
            .leaves
            .iter()
            .all(|leaf| leaf.transform == ContentTransform::Identity),
        "non-template bridged leaves carry ContentTransform::Identity, got {:?}",
        editor.leaves
    );
}

#[test]
fn tripwire_bridge_binding_stops_compiling_at_t020() {
    let inventory = SourceInventory::from_paths(["d/a.md", "d/b.md"]).expect("valid paths");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = TemplatePolicy::suffix_only();
    let layout = LayoutSpec::new(LayoutStyle::Flat, String::new());
    let source = ResolvedSourceRef::new("s", "c0ffee");
    let input = BindingProjectionInput {
        identity: "s",
        source: &source,
        offer: &offer,
        inventory: &inventory,
        take: &take,
        collapse: CollapsePreference::Default,
        materialization: MaterializationPolicy::Copy,
        layout: &layout,
        templates: &templates,
    };
    let projection = project_target("home", &[input]).expect("the tiny fixture projects");

    let bridge = StageBridge {
        artifact: &projection.artifacts[0],
        target: &projection,
    };
    assert!(
        std::ptr::eq(bridge.artifact, &raw const projection.artifacts[0])
            && std::ptr::eq(bridge.target, &raw const projection),
        "TRIPWIRE — this test binds phora::sync::StageBridge by name, so it stops COMPILING at \
         T020 (PR8) when the temporary PR6 bridge retires; that failure is desirable-by-design \
         and is closed by DELETING tests/staging_bridge.rs and tests/staging_bridge_gate.rs, \
         never by resurrecting the bridge"
    );
}
