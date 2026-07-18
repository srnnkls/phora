use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr as _;
use std::time::UNIX_EPOCH;

use phora::config::TemplateOptIn;
use phora::kernel::SourceName;
use phora::projection::build::project_target;
use phora::projection::model::{
    ArtifactRelativePath, BindingProjectionInput, CollapsePreference, ContentTransform, LayoutSpec,
    LayoutStyle, MaterializationPolicy, OfferSpec, ProjectedArtifact, ProjectedLeaf,
    ResolvedSourceRef, TakeSpec, TargetProjection, TemplatePolicy,
};
use phora::source::{
    ExportLeaf, ExportPolicy, ExportRequest, ExportResult, GitBackend, SourceBackend as _,
    SourceError, SourceInventory, SourcePath,
};
use phora::sync::{StageRequest, StagedArtifact};
use tempfile::TempDir;

mod common;

const COMMIT_TIME: u64 = 1_700_000_000;
const OFFER_ROOT: &str = "art";

const REPO_RELATIVE_LEAVES: [&str; 4] = [
    "art/plain.txt",
    "art/run.sh",
    "art/greet.txt.tmpl",
    "art/nested/deep.txt",
];

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
    write(&art.join("greet.txt.tmpl"), b"hello {{ name }}\n");
    write(&art.join("boom.txt.tmpl"), b"value={{ missing }}\n");
    write(&art.join("nested/deep.txt"), b"deep leaf body\n");
    write(&art.join("nested/motd.md.tmpl"), b"deep {{ name }}\n");
    write(&art.join("app.conf"), b"cfg={{ name }}\n");
    write(&art.join("notes.txt"), b"note {{ name }}\n");
    write(&art.join("dup.txt"), b"offer-root dup body\n");
    write(&root.join("dup.txt"), b"repo-root dup body\n");

    let script = art.join("run.sh");
    write(&script, b"#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod run.sh executable");

    #[cfg(unix)]
    std::os::unix::fs::symlink("plain.txt", art.join("link")).expect("create symlink fixture");

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
    backend
        .fetch(&sn("fixture"), &url)
        .expect("fetch builds mirror");

    Fixture {
        _src: src,
        _git: git_dir,
        backend,
        url,
        commit,
    }
}

fn name_var() -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("name".to_owned(), "world".to_owned());
    vars.insert("unused_marker".to_owned(), "unreferenced".to_owned());
    vars
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
        materialization: MaterializationPolicy::Copy,
        layout: &layout,
        templates: &templates,
    };
    project_target("home", &[input]).expect("clean fixture projects")
}

fn adverse_artifact(dir: &str, leaves: &[(&str, &str, ContentTransform)]) -> ProjectedArtifact {
    let first = leaves.first().expect("at least one leaf");
    ProjectedArtifact {
        destination: phora::sync::TargetPath::new(first.1).expect("valid dest"),
        source: ResolvedSourceRef::new("fixture", "0123456789abcdef0123456789abcdef01234567"),
        materialization: phora::kernel::Materialization::CollapsedDir {
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

fn export_leaves(artifact: &ProjectedArtifact, root: Option<&str>) -> Vec<ExportLeaf> {
    artifact
        .leaves
        .iter()
        .map(|leaf| {
            let source = match root {
                Some(root) => leaf
                    .source
                    .as_str()
                    .strip_prefix(&format!("{root}/"))
                    .expect("a rooted projection emits offer-root-joined leaf sources")
                    .to_owned(),
                None => leaf.source.as_str().to_owned(),
            };
            ExportLeaf {
                source: PathBuf::from(source),
                dest: PathBuf::from(leaf.destination.as_str()),
            }
        })
        .collect()
}

fn old_export(
    fx: &Fixture,
    root: Option<&str>,
    policy: &ExportPolicy,
    staging_dir: &Path,
    opt_in: &TemplateOptIn,
    vars: &BTreeMap<String, String>,
    leaves: &[ExportLeaf],
) -> Result<ExportResult, SourceError> {
    fx.backend.export_artifact(&ExportRequest {
        source: &sn("fixture"),
        url: &fx.url,
        commit: &fx.commit,
        root: root.map(Path::new),
        policy,
        staging_dir,
        commit_time: COMMIT_TIME,
        template_opt_in: opt_in,
        vars,
        leaves,
    })
}

fn new_stage(
    fx: &Fixture,
    request: &StageRequest<'_>,
    policy: &ExportPolicy,
    staging_dir: &Path,
    opt_in: &TemplateOptIn,
) -> Result<StagedArtifact, SourceError> {
    let resolved = phora::source::ResolvedSource {
        name: sn("fixture"),
        url: fx.url.clone(),
        snapshot: phora::source::SnapshotId::Git {
            commit: fx.commit.clone(),
        },
    };
    let root = match &request.artifact.materialization {
        phora::kernel::Materialization::CollapsedDir { dir } => Some(PathBuf::from(dir)),
        phora::kernel::Materialization::Leaf(_) => None,
    };
    phora::sync::stage_artifact(
        request,
        root.as_deref(),
        policy,
        staging_dir,
        COMMIT_TIME,
        opt_in,
        |repo_relative| {
            let path = SourcePath::new(&repo_relative.to_string_lossy().replace('\\', "/"))?;
            let entry = phora::source::SourceStore::read(&fx.backend, &resolved, &path)?;
            Ok((entry.bytes, entry.meta.kind))
        },
    )
}

struct StagedDirs {
    old_dir: TempDir,
    new_dir: TempDir,
    old: ExportResult,
    new: StagedArtifact,
}

fn stage_both(
    fx: &Fixture,
    artifact: &ProjectedArtifact,
    target: &TargetProjection,
    root: Option<&str>,
    policy: &ExportPolicy,
    opt_in: &TemplateOptIn,
    vars: &BTreeMap<String, String>,
) -> StagedDirs {
    let old_dir = TempDir::new().expect("old staging tempdir");
    let new_dir = TempDir::new().expect("new staging tempdir");
    let old = old_export(
        fx,
        root,
        policy,
        old_dir.path(),
        opt_in,
        vars,
        &export_leaves(artifact, root),
    )
    .expect("old path stages the success plan");
    let request = StageRequest {
        artifact,
        target,
        variables: vars,
    };
    let new = new_stage(fx, &request, policy, new_dir.path(), opt_in)
        .expect("new path stages the success plan");
    StagedDirs {
        old_dir,
        new_dir,
        old,
        new,
    }
}

fn both_errors(
    fx: &Fixture,
    artifact: &ProjectedArtifact,
    target: &TargetProjection,
    policy: &ExportPolicy,
    vars: &BTreeMap<String, String>,
    what: &str,
) -> (String, String) {
    let old_dir = TempDir::new().expect("old staging tempdir");
    let new_dir = TempDir::new().expect("new staging tempdir");
    let old = old_export(
        fx,
        Some(OFFER_ROOT),
        policy,
        old_dir.path(),
        &TemplateOptIn::SuffixOnly,
        vars,
        &export_leaves(artifact, Some(OFFER_ROOT)),
    )
    .expect_err(&format!("old path must reject: {what}"));
    let request = StageRequest {
        artifact,
        target,
        variables: vars,
    };
    let Err(new) = new_stage(
        fx,
        &request,
        policy,
        new_dir.path(),
        &TemplateOptIn::SuffixOnly,
    ) else {
        panic!("new path must reject: {what}")
    };
    (format!("{old}"), format!("{new}"))
}

fn mtime_secs(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .expect("stat staged file")
        .modified()
        .expect("staged mtime")
        .duration_since(UNIX_EPOCH)
        .expect("mtime after epoch")
        .as_secs()
}

type ManifestTuple = (String, u64, u64, String);

fn old_manifest_tuples(result: &ExportResult) -> Vec<ManifestTuple> {
    let mut files: Vec<ManifestTuple> = result
        .files
        .iter()
        .map(|f| {
            (
                f.path.to_string_lossy().replace('\\', "/"),
                f.size,
                f.mtime,
                f.blake3.clone(),
            )
        })
        .collect();
    files.sort();
    files
}

fn new_manifest_tuples(staged: &StagedArtifact) -> Vec<ManifestTuple> {
    let mut files: Vec<ManifestTuple> = staged
        .files
        .iter()
        .map(|f| {
            (
                f.destination.as_str().to_owned(),
                f.size,
                f.mtime,
                f.blake3.clone(),
            )
        })
        .collect();
    files.sort();
    files
}

fn assert_staged_file_parity(old_dir: &Path, new_dir: &Path, dest: &str) {
    let old_path = old_dir.join(dest);
    let new_path = new_dir.join(dest);
    assert_eq!(
        std::fs::read(&new_path).expect("read new staged file"),
        std::fs::read(&old_path).expect("read old staged file"),
        "staged bytes must be identical at {dest} (a divergence here while digests matched \
         would mean the digest no longer frames the staged bytes)"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::metadata(&new_path)
                .expect("stat new staged file")
                .permissions()
                .mode()
                & 0o111,
            std::fs::metadata(&old_path)
                .expect("stat old staged file")
                .permissions()
                .mode()
                & 0o111,
            "staged exec bits must be identical at {dest}"
        );
    }
    assert_eq!(
        mtime_secs(&new_path),
        mtime_secs(&old_path),
        "staged deterministic mtimes must be identical at {dest}"
    );
}

fn assert_artifact_parity(staged: &StagedDirs, label: &str) {
    assert_eq!(
        staged.new.digest, staged.old.digest,
        "the relocated staging must frame the identical artifact digest for {label}"
    );
    assert_eq!(
        staged.new.vars_digest, staged.old.vars_digest,
        "the relocated staging must produce the identical vars digest (Some iff a template \
         rendered) for {label}"
    );

    let old_files = old_manifest_tuples(&staged.old);
    assert_eq!(
        new_manifest_tuples(&staged.new),
        old_files,
        "the relocated staging must emit the identical per-file manifest framing \
         (destination, size, mtime, blake3) for {label}"
    );
    assert!(
        !old_files.is_empty(),
        "guard: an empty manifest would make the per-file comparison vacuous for {label}"
    );

    for (dest, _, _, _) in &old_files {
        assert!(
            !dest.starts_with("art/"),
            "staged destinations are artifact-relative deployed names — an `art/` prefix means \
             the offer root leaked into a dest: {dest}"
        );
        assert_staged_file_parity(staged.old_dir.path(), staged.new_dir.path(), dest);
    }
}

fn assert_staged_parity(fx: &Fixture, target: &TargetProjection, root: Option<&str>) {
    let vars = name_var();
    let mut rendered_any = false;
    for artifact in &target.artifacts {
        assert!(
            !artifact.leaves.is_empty(),
            "guard: a projected artifact with no leaves would make this differential vacuous \
             ({})",
            artifact.destination.as_str()
        );
        let staged = stage_both(
            fx,
            artifact,
            target,
            root,
            &ExportPolicy::default(),
            &TemplateOptIn::SuffixOnly,
            &vars,
        );
        rendered_any |= staged.old.vars_digest.is_some();
        assert_artifact_parity(&staged, artifact.destination.as_str());
    }
    assert!(
        rendered_any,
        "guard: at least one artifact must render a template, or the vars-digest parity above \
         only ever compared None with None"
    );
}

#[test]
fn success_plan_stages_identical_bytes_modes_mtimes_and_digests() {
    let fx = build_fixture();
    let target = projected_target(&REPO_RELATIVE_LEAVES, Some(OFFER_ROOT));
    assert!(
        target.artifacts.len() >= 4,
        "guard: the rooted projection must yield the three root-child leaf artifacts plus the \
         nested dir — an empty or shrunken artifact set means the repo-relative inventory / \
         offer-root pairing regressed (a set root FILTERS the inventory to entries under it) \
         and every parity assert would pass vacuously; got {}",
        target.artifacts.len()
    );
    assert_staged_parity(&fx, &target, Some(OFFER_ROOT));
}

#[test]
fn empty_offer_root_projects_like_none_and_stages_identically() {
    let fx = build_fixture();
    let with_none = projected_target(&REPO_RELATIVE_LEAVES, None);
    let with_empty = projected_target(&REPO_RELATIVE_LEAVES, Some(""));
    assert_eq!(
        with_empty, with_none,
        "an empty offer root must keep projecting exactly like no root (empirically pinned \
         today: the empty root joins as a no-op and leaf sources stay inventory-verbatim) — \
         the staging relocation must preserve this empty-vs-non-empty root boundary"
    );
    assert!(
        !with_none.artifacts.is_empty(),
        "guard: the rootless projection must yield artifacts, or the parity below is vacuous"
    );
    assert_staged_parity(&fx, &with_none, None);
}

#[test]
fn strict_undefined_template_error_matches_old_path() {
    let fx = build_fixture();
    let target = projected_target(&["art/boom.txt.tmpl"], Some(OFFER_ROOT));
    let artifact = adverse_artifact(
        OFFER_ROOT,
        &[("art/boom.txt.tmpl", "boom.txt", ContentTransform::Template)],
    );
    let (old, new) = both_errors(
        &fx,
        &artifact,
        &target,
        &ExportPolicy::default(),
        &BTreeMap::new(),
        "a template referencing an undefined variable under strict rendering",
    );
    assert_eq!(
        new, old,
        "the strict-undefined render diagnostic must be byte-identical old vs new (the T003 \
         template_error golden pins the old side)"
    );
}

#[cfg(unix)]
#[test]
fn symlink_rejection_error_matches_old_path() {
    let fx = build_fixture();
    let target = projected_target(&["art/plain.txt"], Some(OFFER_ROOT));
    let artifact = adverse_artifact(
        OFFER_ROOT,
        &[("art/link", "deployed_copy", ContentTransform::Identity)],
    );
    let (old, new) = both_errors(
        &fx,
        &artifact,
        &target,
        &ExportPolicy::default(),
        &BTreeMap::new(),
        "a symlink leaf under allow_symlinks=false",
    );
    assert!(
        new.contains("deployed_copy"),
        "the rejection must name the deployed destination, not the source path: {new}"
    );
    assert_eq!(
        new, old,
        "the symlink rejection diagnostic must be byte-identical old vs new (the T003 \
         symlink_rejection golden pins the old side)"
    );
}

#[test]
fn deployed_name_collision_error_matches_old_path() {
    let fx = build_fixture();
    let target = projected_target(&["art/plain.txt", "art/run.sh"], Some(OFFER_ROOT));
    let artifact = adverse_artifact(
        OFFER_ROOT,
        &[
            ("art/plain.txt", "dup.txt", ContentTransform::Identity),
            ("art/run.sh", "dup.txt", ContentTransform::Identity),
        ],
    );
    let (old, new) = both_errors(
        &fx,
        &artifact,
        &target,
        &ExportPolicy::default(),
        &BTreeMap::new(),
        "two leaves mapping to the same deployed name",
    );
    assert_eq!(
        new, old,
        "the deployed-name collision diagnostic must be byte-identical old vs new (the T003 \
         deployed_name_collision golden pins the old side)"
    );
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

#[test]
fn globs_opt_in_renders_matched_and_passes_unmatched_like_old_path() {
    let fx = build_fixture();
    let target = projected_target(&["art/app.conf", "art/notes.txt"], Some(OFFER_ROOT));
    let artifact = adverse_artifact(
        OFFER_ROOT,
        &[
            ("art/app.conf", "app.conf", ContentTransform::Template),
            ("art/notes.txt", "notes.txt", ContentTransform::Identity),
        ],
    );
    let mut builder = globset::GlobSetBuilder::new();
    builder.add(globset::Glob::new("*.conf").expect("valid glob"));
    let opt_in = TemplateOptIn::Globs(builder.build().expect("build glob set"));
    let vars = name_var();

    let staged = stage_both(
        &fx,
        &artifact,
        &target,
        Some(OFFER_ROOT),
        &ExportPolicy::default(),
        &opt_in,
        &vars,
    );

    assert_eq!(
        std::fs::read(staged.new_dir.path().join("app.conf")).expect("read staged app.conf"),
        b"cfg=world\n",
        "guard: the offer-root-relative glob `*.conf` must match the root-relative render key \
         and actually render — a both-sides-unrendered outcome would pass parity vacuously"
    );
    assert!(
        std::fs::read(staged.new_dir.path().join("notes.txt"))
            .expect("read staged notes.txt")
            .windows(b"{{ name }}".len())
            .any(|w| w == b"{{ name }}"),
        "guard: the unmatched leaf must pass through verbatim, template syntax intact"
    );
    assert!(
        staged.old.vars_digest.is_some(),
        "guard: the old path must report a rendered template so vars-digest parity is exercised"
    );
    assert_artifact_parity(&staged, "globs opt-in artifact");
}

#[test]
fn multi_component_offer_root_stages_identically() {
    let fx = build_fixture();
    let nested_root = "art/nested";
    let target = projected_target(
        &["art/nested/deep.txt", "art/nested/motd.md.tmpl"],
        Some(nested_root),
    );
    let artifact = adverse_artifact(
        nested_root,
        &[
            (
                "art/nested/deep.txt",
                "deep.txt",
                ContentTransform::Identity,
            ),
            (
                "art/nested/motd.md.tmpl",
                "motd.md",
                ContentTransform::Template,
            ),
        ],
    );
    let vars = name_var();

    let staged = stage_both(
        &fx,
        &artifact,
        &target,
        Some(nested_root),
        &ExportPolicy::default(),
        &TemplateOptIn::SuffixOnly,
        &vars,
    );

    assert_eq!(
        std::fs::read(staged.new_dir.path().join("motd.md")).expect("read staged motd.md"),
        b"deep world\n",
        "guard: the template under the multi-component root must render — a both-sides-failed \
         render cannot hide behind parity"
    );
    assert!(
        staged.old.vars_digest.is_some(),
        "guard: the old path must report a rendered template so vars-digest parity is exercised"
    );
    assert_artifact_parity(&staged, "multi-component-root artifact");
}
