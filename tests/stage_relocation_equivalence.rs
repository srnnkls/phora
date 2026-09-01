use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr as _;

use phora::config::TemplateOptIn;
use phora::projection::build::project_target;
use phora::projection::model::{
    ArtifactRelativePath, BindingProjectionInput, CollapsePreference, ContentTransform, LayoutSpec,
    LayoutStyle, MaterializationPolicy, OfferSpec, ProjectedArtifact, ProjectedLeaf,
    ResolvedSourceRef, TakeSpec, TargetProjection, TemplatePolicy,
};
use phora::source::SourceName;
use phora::source::{
    ExportPolicy, GitBackend, ResolvePolicy, ResolveRequest, RevisionSpec, SourceError,
    SourceInventory, SourceLocation, SourcePath, SourceStore,
};
use phora::sync::{StageRequest, StagedArtifact, stage_artifact};
use tempfile::TempDir;

mod common;

const COMMIT_TIME: u64 = 1_700_000_000;
const OFFER_ROOT: &str = "art";

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

fn build_rooted_source_repo(root: &Path) {
    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "core.filemode", "true"]);

    let art = root.join(OFFER_ROOT);
    write(&art.join("plain.txt"), b"plain body\n");
    write(&art.join("dup.txt"), b"offer-root dup body\n");
    write(&root.join("dup.txt"), b"repo-root dup body\n");

    let script = art.join("run.sh");
    write(&script, b"#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod run.sh executable");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

struct Fixture {
    _src: TempDir,
    _git: TempDir,
    backend: GitBackend,
    url: String,
    commit: String,
}

fn build_fixture() -> Fixture {
    let src = TempDir::new().expect("src tempdir");
    build_rooted_source_repo(src.path());

    let out = Command::new("git")
        .current_dir(src.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse runs");
    let commit = String::from_utf8(out.stdout)
        .expect("utf8 sha")
        .trim()
        .to_owned();

    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let url = src.path().to_string_lossy().into_owned();
    SourceStore::resolve(
        &backend,
        &ResolveRequest {
            name: sn("fixture"),
            location: SourceLocation::Git { url: url.clone() },
            revision: RevisionSpec::Branch("main".to_owned()),
        },
        ResolvePolicy::Refresh,
    )
    .expect("refresh builds mirror");

    Fixture {
        _src: src,
        _git: git_dir,
        backend,
        url,
        commit,
    }
}

fn projected_target(inventory_paths: &[&str], root: Option<&str>) -> TargetProjection {
    let inventory = SourceInventory::from_paths(inventory_paths.iter().copied())
        .expect("fixture leaves are lexically valid");
    let offer = OfferSpec::new(Vec::new(), Vec::new(), root.map(PathBuf::from));
    let take = TakeSpec::ProjectAll;
    let templates = TemplatePolicy::suffix_only();
    let layout = LayoutSpec::new(LayoutStyle::Flat, String::new());
    let source = ResolvedSourceRef::new("fixture", "0123456789abcdef0123456789abcdef01234567");
    let input = BindingProjectionInput {
        identity: "fixture",
        source: &source,
        offer: &offer,
        inventory: &inventory,
        take: &take,
        collapse: CollapsePreference::default(),
        history: false,
        materialization: MaterializationPolicy::Copy,
        layout: &layout,
        templates: &templates,
    };
    project_target("home", &[input]).expect("clean fixture projects")
}

fn adverse_artifact(dir: &str, leaves: &[(&str, &str, ContentTransform)]) -> ProjectedArtifact {
    let first = leaves.first().expect("at least one leaf");
    ProjectedArtifact {
        destination: phora::projection::model::TargetPath::new(first.1).expect("valid dest"),
        source: ResolvedSourceRef::new("fixture", "0123456789abcdef0123456789abcdef01234567"),
        materialization: phora::projection::model::Materialization::CollapsedDir {
            dir: dir.to_owned(),
        },
        kept_leaves: Vec::new(),
        leaves: leaves
            .iter()
            .map(|(source, dest, transform)| ProjectedLeaf {
                source: SourcePath::new(source).expect("valid source path"),
                destination: ArtifactRelativePath::new(dest).expect("valid dest path"),
                transform: *transform,
            })
            .collect(),
    }
}

fn new_stage(
    fx: &Fixture,
    request: &StageRequest<'_>,
    policy: &ExportPolicy,
    staging_dir: &Path,
    opt_in: &TemplateOptIn,
) -> Result<StagedArtifact, SourceError> {
    let resolved = SourceStore::resolve(
        &fx.backend,
        &ResolveRequest {
            name: sn("fixture"),
            location: SourceLocation::Git {
                url: fx.url.clone(),
            },
            revision: RevisionSpec::Commit(fx.commit.parse().expect("fixture commit is valid hex")),
        },
        ResolvePolicy::CachedOnly,
    )
    .expect("resolve staged fixture snapshot");
    let root = match &request.artifact.materialization {
        phora::projection::model::Materialization::CollapsedDir { dir } => Some(PathBuf::from(dir)),
        phora::projection::model::Materialization::Leaf(_)
        | phora::projection::model::Materialization::WholeRoot { .. } => None,
    };
    stage_artifact(
        request,
        root.as_deref(),
        policy,
        staging_dir,
        COMMIT_TIME,
        opt_in,
        |repo_relative| {
            let path = SourcePath::new(&repo_relative.to_string_lossy().replace('\\', "/"))?;
            let entry = SourceStore::read(&fx.backend, &resolved.snapshot, &path)?;
            Ok((entry.bytes, entry.meta.kind))
        },
    )
}

#[test]
fn root_relative_key_collision_is_refused_not_silently_overwritten() {
    let fx = build_fixture();
    let target = projected_target(&["art/plain.txt"], Some(OFFER_ROOT));
    let artifact = adverse_artifact(
        OFFER_ROOT,
        &[
            ("art/dup.txt", "a.txt", ContentTransform::Identity),
            ("dup.txt", "b.txt", ContentTransform::Identity),
        ],
    );
    let request = StageRequest {
        artifact: &artifact,
        target: &target,
        variables: &BTreeMap::new(),
    };
    let staging = TempDir::new().expect("staging tempdir");

    let Err(err) = new_stage(
        &fx,
        &request,
        &ExportPolicy::default(),
        staging.path(),
        &TemplateOptIn::SuffixOnly,
    ) else {
        let a = std::fs::read(staging.path().join("a.txt")).expect("read staged a.txt");
        let b = std::fs::read(staging.path().join("b.txt")).expect("read staged b.txt");
        panic!(
            "stage_artifact must refuse a plan where two leaves collapse to the same \
             root-relative key (`art/dup.txt` and `dup.txt` both key `dup.txt` under \
             root=Some(\"art\")): the repo_relative_sources map silently overwrites, staging \
             wrong bytes with no error — unreachable from projection-produced artifacts, but \
             stage_artifact is pub and T016 wires production through it; staged \
             a.txt={:?} b.txt={:?}",
            String::from_utf8_lossy(&a),
            String::from_utf8_lossy(&b),
        )
    };
    let rendered = format!("{err}");
    assert!(
        rendered.contains("dup.txt"),
        "the collision refusal must name the colliding path so the plan is debuggable: \
         {rendered}"
    );
}
