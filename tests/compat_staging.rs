//! T003 staging-matrix baselines (INV-5): byte-exact golden fixtures pinning the
//! staging surface of `SourceBackend::export_artifact` on unmoved `main`, so every
//! staged byte stays identical through the source → projection → sync refactor.
//!
//! Every pinned value is either content-addressed (the artifact/manifest blake3
//! digests frame the deployed path + kind tag + rendered bytes, independent of the
//! git commit id) or caller-fixed (deterministic mtimes come from a constant
//! `COMMIT_TIME`; deployed paths are chosen by the leaf plan; exec bits are pinned
//! as the `mode & 0o111` mask production sets, umask-independent). Nothing here
//! varies per run, so no path/id normalization or hex sweep is applied — the raw
//! bytes are the assertion.
//!
//! Serialized under `tests/compat/staging/` — a directory distinct from the T002
//! `tests/compat/serialized/` matrix — so the two baselines never race on a shared
//! fixture write.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr as _;
use std::time::UNIX_EPOCH;

use phora::config::TemplateOptIn;
use phora::kernel::SourceName;
use phora::source::{
    ExportLeaf, ExportPolicy, ExportRequest, ExportResult, GitBackend, SourceBackend as _,
    SourceError,
};
use tempfile::TempDir;

mod common;

/// The unmoved production baseline this matrix is pinned against (INV-5). HEAD has
/// moved past it with test-only commits, so regeneration guards on production parity
/// (`src/`, `Cargo.toml`, `Cargo.lock` unchanged since here) rather than `HEAD == this`.
const BASELINE_COMMIT: &str = "92c784e3b14496be25dcecc8d4500e32b52b1c50";

/// Fixed staging clock: every staged file's deterministic mtime is exactly this,
/// so the mtime baseline is a pinned constant rather than a wall-clock capture.
const COMMIT_TIME: u64 = 1_700_000_000;

// ─── golden-fixture harness ────────────────────────────────────────────────

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat/staging")
}

fn line_diff(old: &str, new: &str) -> String {
    let mut diff = String::new();
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    for i in 0..old_lines.len().max(new_lines.len()) {
        match (old_lines.get(i), new_lines.get(i)) {
            (a, b) if a == b => {}
            (a, b) => {
                if let Some(a) = a {
                    let _ = writeln!(diff, "- {a}");
                }
                if let Some(b) = b {
                    let _ = writeln!(diff, "+ {b}");
                }
            }
        }
    }
    let terminator = |s: &str| {
        if s.ends_with('\n') {
            "present"
        } else {
            "absent"
        }
    };
    if old.ends_with('\n') != new.ends_with('\n') {
        let _ = writeln!(
            diff,
            "~ final newline: old={} new={}",
            terminator(old),
            terminator(new),
        );
    }
    diff
}

/// `true` iff production (`src/`, `Cargo.toml`, `Cargo.lock`) is byte-identical to the
/// pinned baseline. `None` when git cannot answer, so regeneration fails closed.
fn production_matches_baseline() -> Option<bool> {
    let out = Command::new("git")
        .args([
            "diff",
            "--quiet",
            BASELINE_COMMIT,
            "HEAD",
            "--",
            "src/",
            "Cargo.toml",
            "Cargo.lock",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// `Some(reason)` when the production pathspec (`src/`, `Cargo.toml`, `Cargo.lock`) carries any
/// uncommitted change — a tracked modification or an untracked file — or when git cannot answer,
/// so regeneration fails closed. `None` only when git confirms production is clean; untracked
/// compatibility fixtures and edited test harnesses outside the pathspec stay permitted.
fn tracked_worktree_is_dirty() -> Option<String> {
    let out = match Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            "src",
            "Cargo.toml",
            "Cargo.lock",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
    {
        Ok(out) if out.status.success() => out,
        Ok(out) => {
            return Some(format!(
                "git status failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            ));
        }
        Err(e) => return Some(format!("git status did not run: {e}")),
    };
    let dirty = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!dirty.is_empty()).then_some(dirty)
}

fn regenerate_golden(path: &Path, actual: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "PHORA_UPDATE_GOLDEN must never be set while CI is set: a golden fixture may not \
         self-bless in CI. Unset PHORA_UPDATE_GOLDEN and commit regenerated fixtures explicitly."
    );
    match production_matches_baseline() {
        Some(true) => {}
        Some(false) => panic!(
            "refusing to regenerate T003 staging baselines: production (src/, Cargo.toml, \
             Cargo.lock) has diverged from the pinned baseline {BASELINE_COMMIT}, so a capture \
             would freeze post-refactor behavior instead of the baseline it must diff against."
        ),
        None => panic!(
            "refusing to regenerate T003 staging baselines: `git diff` against {BASELINE_COMMIT} \
             did not run (git unavailable or not a repo); baselines must reflect unmoved production."
        ),
    }
    if let Some(reason) = tracked_worktree_is_dirty() {
        panic!(
            "refusing to regenerate T003 staging baselines: the production pathspec (src/, \
             Cargo.toml, Cargo.lock) is not clean or git could not verify it, so the capture \
             would not reflect pinned production. Commit or stash production changes first \
             (untracked fixtures and edited test harnesses elsewhere are fine). Details: {reason}"
        );
    }
    if let Ok(old) = std::fs::read_to_string(path)
        && old != actual
    {
        eprintln!(
            "REGENERATING {} — bytes changed (old ↓ / new ↓):\n{}",
            path.display(),
            line_diff(&old, actual)
        );
    }
    std::fs::create_dir_all(path.parent().expect("golden parent")).expect("create golden dir");
    std::fs::write(path, actual).expect("write golden fixture");
}

fn assert_golden(name: &str, actual: &str) {
    let path = golden_dir().join(name);
    if let Some(val) = std::env::var_os("PHORA_UPDATE_GOLDEN") {
        assert_eq!(
            val,
            std::ffi::OsStr::new("1"),
            "PHORA_UPDATE_GOLDEN must be exactly \"1\" to regenerate golden fixtures, got {}; \
             refusing an ambiguous regeneration that could silently bless a regression",
            val.display()
        );
        regenerate_golden(&path, actual);
        return;
    }
    assert!(
        path.exists(),
        "golden fixture missing: {} (regenerate the T003 baseline with PHORA_UPDATE_GOLDEN=1)",
        path.display()
    );
    let expected = std::fs::read_to_string(&path).expect("read golden fixture");
    assert_eq!(
        actual,
        expected,
        "staged bytes drifted from the pinned baseline {}",
        path.display()
    );
}

// ─── harness self-test (invariant, not a staging golden) ────────────────────

#[test]
fn line_diff_announces_final_newline_only_drift() {
    let dropped = line_diff("baseline\n", "baseline");
    assert!(
        !dropped.is_empty(),
        "line_diff must not be silent when the only byte change is a dropped final newline"
    );
    assert!(
        dropped.contains("newline"),
        "line_diff must name the final-newline drift, got: {dropped:?}"
    );

    let added = line_diff("baseline", "baseline\n");
    assert!(
        !added.is_empty(),
        "line_diff must not be silent when the only byte change is an added final newline"
    );
    assert!(
        added.contains("newline"),
        "line_diff must name the final-newline drift, got: {added:?}"
    );
    assert_ne!(
        dropped, added,
        "dropped vs added final newline must render as distinct diffs"
    );
    assert!(
        !line_diff("same\n", "same\n").contains("newline"),
        "line_diff must not announce newline drift when both terminators match"
    );
}

// ─── sandboxed source fixture ───────────────────────────────────────────────

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

/// A source repo carrying every staging-surface shape: a plain file, an executable
/// file, a `.tmpl` template that renders, a `.tmpl` template that fails strict-undefined
/// rendering, and a symlink. Fixed content/author/date make the commit deterministic.
fn build_source_repo(root: &Path) {
    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "core.filemode", "true"]);

    write(&root.join("plain.txt"), b"plain body\n");
    write(&root.join("greet.txt.tmpl"), b"hello {{ name }}\n");
    write(&root.join("boom.txt.tmpl"), b"value={{ missing }}\n");

    let script = root.join("run.sh");
    write(&script, b"#!/bin/sh\necho hi\n");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod run.sh executable");

    std::os::unix::fs::symlink("plain.txt", root.join("link")).expect("create symlink fixture");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
}

struct StagingFixture {
    _src: TempDir,
    _git: TempDir,
    backend: GitBackend,
    url: String,
    commit: String,
}

fn build_staging_fixture() -> StagingFixture {
    let src = TempDir::new().expect("src tempdir");
    build_source_repo(src.path());

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

    StagingFixture {
        _src: src,
        _git: git_dir,
        backend,
        url,
        commit,
    }
}

fn leaf(source: &str, dest: &str) -> ExportLeaf {
    ExportLeaf {
        source: PathBuf::from(source),
        dest: PathBuf::from(dest),
    }
}

fn name_var() -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("name".to_owned(), "world".to_owned());
    vars.insert("unused_marker".to_owned(), "unreferenced".to_owned());
    vars
}

/// Staged output of the success plan (plain + executable + rendered template), with the
/// staging dir kept alive so the materialized files can be read back.
struct Staged {
    _staging: TempDir,
    dir: PathBuf,
    result: ExportResult,
}

/// The deployed names of the success plan, sorted, so byte/mode/mtime dumps are order-stable.
const STAGED_DESTS: [&str; 3] = ["greet.txt", "plain.txt", "run.sh"];

fn run_success_export(fx: &StagingFixture) -> Staged {
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy::default();
    let vars = name_var();
    let leaves = [
        leaf("plain.txt", "plain.txt"),
        leaf("run.sh", "run.sh"),
        leaf("greet.txt.tmpl", "greet.txt"),
    ];
    let result = fx
        .backend
        .export_artifact(&ExportRequest {
            source: &sn("fixture"),
            url: &fx.url,
            commit: &fx.commit,
            root: None,
            policy: &policy,
            staging_dir: staging.path(),
            commit_time: COMMIT_TIME,
            template_opt_in: &TemplateOptIn::SuffixOnly,
            vars: &vars,
            leaves: &leaves,
        })
        .expect("export_artifact stages the success plan");
    Staged {
        dir: staging.path().to_path_buf(),
        _staging: staging,
        result,
    }
}

// ─── 1. staged file bytes (plain passthrough + template render output) ───────

#[test]
fn staged_file_bytes_are_byte_identical() {
    let fx = build_staging_fixture();
    let staged = run_success_export(&fx);

    let mut doc = String::new();
    for dest in STAGED_DESTS {
        let bytes = std::fs::read(staged.dir.join(dest)).expect("read staged file");
        let body = String::from_utf8(bytes.clone()).expect("staged body is utf8");
        let final_newline = if body.ends_with('\n') {
            "present"
        } else {
            "absent"
        };
        let _ = writeln!(
            doc,
            "--- {dest} (len={} final_newline={final_newline}) ---",
            bytes.len(),
        );
        doc.push_str(&body);
        if !body.ends_with('\n') {
            doc.push('\n');
        }
    }
    assert_golden("staged_bytes.golden", &doc);
}

// ─── 2. staged file modes (executable bit preserved / normalized) ────────────

#[test]
fn staged_file_modes_are_byte_identical() {
    let fx = build_staging_fixture();
    let staged = run_success_export(&fx);

    let mut doc = String::new();
    for dest in STAGED_DESTS {
        let mode = std::fs::metadata(staged.dir.join(dest))
            .expect("stat staged file")
            .permissions()
            .mode();
        let _ = writeln!(doc, "{dest} exec_bits={:03o}", mode & 0o111);
    }
    assert_golden("staged_modes.golden", &doc);
}

// ─── 3. deterministic staged mtimes ─────────────────────────────────────────

#[test]
fn staged_file_mtimes_are_byte_identical() {
    let fx = build_staging_fixture();
    let staged = run_success_export(&fx);

    let mut doc = String::new();
    for dest in STAGED_DESTS {
        let mtime = std::fs::metadata(staged.dir.join(dest))
            .expect("stat staged file")
            .modified()
            .expect("staged mtime")
            .duration_since(UNIX_EPOCH)
            .expect("mtime after epoch");
        let _ = writeln!(
            doc,
            "{dest} mtime={}.{:09}",
            mtime.as_secs(),
            mtime.subsec_nanos(),
        );
    }
    assert_golden("staged_mtimes.golden", &doc);
}

// ─── 4. artifact + variable digests as staged ───────────────────────────────

#[test]
fn artifact_and_variable_digests_are_byte_identical() {
    let fx = build_staging_fixture();
    let staged = run_success_export(&fx);

    let mut doc = String::new();
    let _ = writeln!(doc, "digest = {}", staged.result.digest);
    let _ = writeln!(
        doc,
        "vars_digest = {}",
        staged.result.vars_digest.as_deref().unwrap_or("<none>")
    );
    assert_golden("artifact_digests.golden", &doc);
}

// ─── 5. manifest contents written during staging ────────────────────────────

#[test]
fn staging_manifest_is_byte_identical() {
    let fx = build_staging_fixture();
    let staged = run_success_export(&fx);

    let mut files: Vec<&_> = staged.result.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut doc = String::new();
    for f in files {
        let _ = writeln!(
            doc,
            "{} size={} mtime={} blake3={}",
            f.path.to_string_lossy().replace('\\', "/"),
            f.size,
            f.mtime,
            f.blake3,
        );
    }
    assert_golden("staging_manifest.golden", &doc);
}

// ─── 6. symlink rejection diagnostic ────────────────────────────────────────

#[test]
fn symlink_rejection_error_is_byte_identical() {
    let fx = build_staging_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy::default();
    let vars = BTreeMap::new();
    let leaves = [leaf("link", "deployed_copy")];

    let err = fx
        .backend
        .export_artifact(&ExportRequest {
            source: &sn("fixture"),
            url: &fx.url,
            commit: &fx.commit,
            root: None,
            policy: &policy,
            staging_dir: staging.path(),
            commit_time: COMMIT_TIME,
            template_opt_in: &TemplateOptIn::SuffixOnly,
            vars: &vars,
            leaves: &leaves,
        })
        .expect_err("staging a symlink under the default policy must be rejected");

    let rendered = format!("{err}\n");
    assert!(
        rendered.contains("deployed_copy"),
        "symlink rejection must name the deployed destination `deployed_copy` (distinct from \
         the source `link`), so a wrong-path-role regression cannot preserve the golden; got: \
         {rendered:?}"
    );
    assert_golden("symlink_rejection.golden", &rendered);
}

// ─── 7. template render error (strict-undefined variable) ───────────────────

#[test]
fn template_render_error_is_byte_identical() {
    let fx = build_staging_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy::default();
    let vars = BTreeMap::new();
    let leaves = [leaf("boom.txt.tmpl", "boom.txt")];

    let err = fx
        .backend
        .export_artifact(&ExportRequest {
            source: &sn("fixture"),
            url: &fx.url,
            commit: &fx.commit,
            root: None,
            policy: &policy,
            staging_dir: staging.path(),
            commit_time: COMMIT_TIME,
            template_opt_in: &TemplateOptIn::SuffixOnly,
            vars: &vars,
            leaves: &leaves,
        })
        .expect_err("a template referencing an undefined variable must fail strict rendering");

    assert_golden("template_error.golden", &format!("{err}\n"));
}

// ─── extended staging matrix (T003-extension: full relocated staging surface) ─

/// A second source repo carrying the shapes the base fixture omits: a file nested two
/// directories deep, a `.tmpl` whose loop exhausts the render fuel, two files a glob opt-in
/// renders (`app.conf`) or leaves verbatim (`notes.txt`), and a symlink whose target climbs
/// out of the deploy tree. Fixed content/author/date keep the commit deterministic.
fn build_extended_source_repo(root: &Path) {
    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "core.filemode", "true"]);

    write(&root.join("nested/deep/leaf.txt"), b"nested leaf body\n");
    write(
        &root.join("runaway.txt.tmpl"),
        b"{% for i in range(9999) %}{% for j in range(9999) %}{{ j }}{% endfor %}{% endfor %}\n",
    );
    write(&root.join("app.conf"), b"cfg={{ name }}\n");
    write(&root.join("notes.txt"), b"note {{ name }}\n");

    std::os::unix::fs::symlink("../../../outside", root.join("escape"))
        .expect("create escaping symlink fixture");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "extended fixture"]);
}

fn build_extended_fixture() -> StagingFixture {
    let src = TempDir::new().expect("src tempdir");
    build_extended_source_repo(src.path());

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
        .fetch(&sn("extended"), &url)
        .expect("fetch builds mirror");

    StagingFixture {
        _src: src,
        _git: git_dir,
        backend,
        url,
        commit,
    }
}

fn run_export(
    fx: &StagingFixture,
    source: &SourceName,
    policy: &ExportPolicy,
    staging_dir: &Path,
    template_opt_in: &TemplateOptIn,
    vars: &BTreeMap<String, String>,
    leaves: &[ExportLeaf],
) -> std::result::Result<ExportResult, SourceError> {
    fx.backend.export_artifact(&ExportRequest {
        source,
        url: &fx.url,
        commit: &fx.commit,
        root: None,
        policy,
        staging_dir,
        commit_time: COMMIT_TIME,
        template_opt_in,
        vars,
        leaves,
    })
}

// ─── 8. nested destinations materialize their parent directories ─────────────

#[test]
fn nested_destination_materializes_directories() {
    let fx = build_extended_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let vars = name_var();
    let result = run_export(
        &fx,
        &sn("extended"),
        &ExportPolicy::default(),
        staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("nested/deep/leaf.txt", "a/b/c.txt")],
    )
    .expect("nested destination stages");

    let staged = staging.path().join("a/b/c.txt");
    let bytes = std::fs::read(&staged).expect("read nested staged file");
    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "staged a/b/c.txt exists={} len={}",
        staged.exists(),
        bytes.len()
    );
    doc.push_str(&String::from_utf8(bytes).expect("staged body is utf8"));
    for f in &result.files {
        let _ = writeln!(
            doc,
            "manifest {} size={} blake3={}",
            f.path.to_string_lossy().replace('\\', "/"),
            f.size,
            f.blake3,
        );
    }
    assert_golden("nested_destination.golden", &doc);
}

// ─── 9. export creates its own (absent) staging directory ────────────────────

#[test]
fn export_creates_missing_staging_dir() {
    let fx = build_staging_fixture();
    let outer = TempDir::new().expect("staging tempdir");
    let staging_dir = outer.path().join("phora/stage/created");
    assert!(
        !staging_dir.exists(),
        "precondition: staging dir must be absent so the golden pins production's own creation"
    );
    let vars = name_var();
    let result = run_export(
        &fx,
        &sn("fixture"),
        &ExportPolicy::default(),
        &staging_dir,
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("plain.txt", "plain.txt")],
    )
    .expect("export creates its own staging dir");

    let bytes = std::fs::read(staging_dir.join("plain.txt")).expect("read staged file");
    let mut doc = String::new();
    let _ = writeln!(doc, "staging_dir_created={}", staging_dir.is_dir());
    let _ = writeln!(
        doc,
        "staged plain.txt len={} files={}",
        bytes.len(),
        result.files.len()
    );
    doc.push_str(&String::from_utf8(bytes).expect("staged body is utf8"));
    assert_golden("staging_dir_materialized.golden", &doc);
}

// ─── 10. deployed-name collision diagnostic ──────────────────────────────────

#[test]
fn deployed_name_collision_error_is_byte_identical() {
    let fx = build_staging_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let vars = BTreeMap::new();
    let err = run_export(
        &fx,
        &sn("fixture"),
        &ExportPolicy::default(),
        staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("plain.txt", "dup.txt"), leaf("run.sh", "dup.txt")],
    )
    .expect_err("two leaves mapping to the same dest must collide");

    let rendered = format!("{err}\n");
    assert!(
        rendered.contains("dup.txt")
            && rendered.contains("plain.txt")
            && rendered.contains("run.sh"),
        "collision diagnostic must name the deployed name and both source paths, got: {rendered:?}"
    );
    assert_golden("deployed_name_collision.golden", &rendered);
}

// ─── 11. runaway template exhausts the render fuel ───────────────────────────

#[test]
fn template_fuel_exhaustion_error_is_byte_identical() {
    let fx = build_extended_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let vars = name_var();
    let err = run_export(
        &fx,
        &sn("extended"),
        &ExportPolicy::default(),
        staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("runaway.txt.tmpl", "runaway.txt")],
    )
    .expect_err("a runaway template must exhaust the render fuel");

    let rendered = format!("{err}\n");
    assert!(
        rendered.contains("runaway.txt.tmpl"),
        "fuel-exhaustion render error must name the offending template path, got: {rendered:?}"
    );
    assert_golden("template_fuel_exhaustion.golden", &rendered);
}

// ─── 12. allow_symlinks=true materializes the link and frames its digest ─────

#[test]
fn allowed_symlink_materializes_and_frames_link_digest() {
    let fx = build_staging_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy {
        allow_symlinks: true,
        ..ExportPolicy::default()
    };
    let vars = BTreeMap::new();
    let result = run_export(
        &fx,
        &sn("fixture"),
        &policy,
        staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("link", "deployed_link")],
    )
    .expect("allow_symlinks=true stages the symlink");

    let target =
        std::fs::read_link(staging.path().join("deployed_link")).expect("staged path is a symlink");
    let mut doc = String::new();
    let _ = writeln!(doc, "link_target={}", target.to_string_lossy());
    let _ = writeln!(doc, "digest={}", result.digest);
    let _ = writeln!(
        doc,
        "vars_digest={}",
        result.vars_digest.as_deref().unwrap_or("<none>")
    );
    let _ = writeln!(doc, "manifest_files={}", result.files.len());
    assert_golden("allowed_symlink.golden", &doc);
}

// ─── 13. preserve_executable=false normalizes the staged exec bits ───────────

#[test]
fn preserve_executable_false_normalizes_exec_bits() {
    let fx = build_staging_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy {
        preserve_executable: false,
        ..ExportPolicy::default()
    };
    let vars = BTreeMap::new();
    let result = run_export(
        &fx,
        &sn("fixture"),
        &policy,
        staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("run.sh", "run.sh")],
    )
    .expect("export stages the executable with its exec bits normalized off");

    let mode = std::fs::metadata(staging.path().join("run.sh"))
        .expect("stat staged file")
        .permissions()
        .mode();
    let mut doc = String::new();
    let _ = writeln!(doc, "run.sh exec_bits={:03o}", mode & 0o111);
    let _ = writeln!(doc, "digest={}", result.digest);
    assert_golden("preserve_executable_false.golden", &doc);
}

// ─── 14. TemplateOptIn::Globs renders matched, passes unmatched ──────────────

#[test]
fn template_globs_render_matched_and_pass_unmatched() {
    let fx = build_extended_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let mut builder = globset::GlobSetBuilder::new();
    builder.add(globset::Glob::new("*.conf").expect("valid glob"));
    let opt_in = TemplateOptIn::Globs(builder.build().expect("build glob set"));
    let vars = name_var();
    let result = run_export(
        &fx,
        &sn("extended"),
        &ExportPolicy::default(),
        staging.path(),
        &opt_in,
        &vars,
        &[leaf("app.conf", "app.conf"), leaf("notes.txt", "notes.txt")],
    )
    .expect("glob opt-in stages both files");

    let mut doc = String::new();
    for dest in ["app.conf", "notes.txt"] {
        let bytes = std::fs::read(staging.path().join(dest)).expect("read staged file");
        let _ = writeln!(doc, "--- {dest} (len={}) ---", bytes.len());
        doc.push_str(&String::from_utf8(bytes).expect("staged body is utf8"));
    }
    let _ = writeln!(
        doc,
        "vars_digest={}",
        result.vars_digest.as_deref().unwrap_or("<none>")
    );
    assert_golden("template_globs.golden", &doc);
}

// ─── 15. TemplateOptIn::Disabled passes a would-render file through ──────────

#[test]
fn template_disabled_passes_through_would_render_bytes() {
    let fx = build_staging_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let vars = name_var();
    let result = run_export(
        &fx,
        &sn("fixture"),
        &ExportPolicy::default(),
        staging.path(),
        &TemplateOptIn::Disabled,
        &vars,
        &[leaf("greet.txt.tmpl", "greet.txt.tmpl")],
    )
    .expect("disabled templating stages the file verbatim");

    let bytes = std::fs::read(staging.path().join("greet.txt.tmpl")).expect("read staged file");
    let mut doc = String::new();
    let _ = writeln!(doc, "--- greet.txt.tmpl (len={}) ---", bytes.len());
    doc.push_str(&String::from_utf8(bytes).expect("staged body is utf8"));
    let _ = writeln!(
        doc,
        "vars_digest={}",
        result.vars_digest.as_deref().unwrap_or("<none>")
    );
    assert_golden("template_disabled.golden", &doc);
}

// ─── 16. vcs_opt_in gates `.git` destination components ──────────────────────

#[test]
fn vcs_opt_in_gates_dot_git_destinations() {
    let fx = build_staging_fixture();
    let vars = BTreeMap::new();
    let leaves = [leaf("plain.txt", ".git/plain.txt")];

    let filtered_staging = TempDir::new().expect("staging tempdir");
    let filtered = run_export(
        &fx,
        &sn("fixture"),
        &ExportPolicy::default(),
        filtered_staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &leaves,
    )
    .expect("default policy skips a .git destination without error");

    let opt_in_staging = TempDir::new().expect("staging tempdir");
    let opt_in_policy = ExportPolicy {
        vcs_opt_in: true,
        ..ExportPolicy::default()
    };
    let staged = run_export(
        &fx,
        &sn("fixture"),
        &opt_in_policy,
        opt_in_staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &leaves,
    )
    .expect("vcs_opt_in stages a .git destination");

    let mut doc = String::new();
    let _ = writeln!(
        doc,
        "default: files={} digest={} staged={}",
        filtered.files.len(),
        filtered.digest,
        filtered_staging.path().join(".git/plain.txt").exists(),
    );
    let _ = writeln!(
        doc,
        "vcs_opt_in: files={} digest={} staged={}",
        staged.files.len(),
        staged.digest,
        opt_in_staging.path().join(".git/plain.txt").exists(),
    );
    assert_golden("vcs_opt_in.golden", &doc);
}

// ─── 17. escaping symlink diagnostic (target climbs out of the deploy tree) ──

#[test]
fn escaping_symlink_error_is_byte_identical() {
    let fx = build_extended_fixture();
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy {
        allow_symlinks: true,
        ..ExportPolicy::default()
    };
    let vars = BTreeMap::new();
    let err = run_export(
        &fx,
        &sn("extended"),
        &policy,
        staging.path(),
        &TemplateOptIn::SuffixOnly,
        &vars,
        &[leaf("escape", "escape")],
    )
    .expect_err("a symlink whose target escapes the deploy tree must be rejected");

    let rendered = format!("{err}\n");
    assert!(
        rendered.contains("escape") && rendered.contains(".."),
        "escape diagnostic must name the link path and its climbing target, got: {rendered:?}"
    );
    assert_golden("symlink_escape.golden", &rendered);
}
