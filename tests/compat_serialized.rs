//! T002 compatibility baselines (INV-4): byte-exact serialized-format fixtures
//! pinned on unmoved `main` so every serialized surface stays byte-identical
//! through the source → projection → sync refactor.
//!
//! Content-addressed tokens (source commit, digests, per-file blake3,
//! mirror/project keys of stable inputs, deterministic mtimes) are pinned RAW,
//! since holding those constant is exactly what INV-4 asserts. Only the two
//! genuinely per-run-varying identifiers of this fixture — the project-id
//! (blake3 of the random cwd path) and the mirror key (blake3 of the random
//! source path) — plus wall-clock timestamps and temp paths, are normalized,
//! each by its own literal value so an unrelated 16-hex token is never scrubbed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::str::FromStr as _;

use phora::config::transitive::{FetchNode, Instance};
use phora::config::{Config, TemplateOptIn, admit_transitive_hooks, hook_preimage};
use phora::kernel::SourceName;
use phora::lock::{CandidateHookRecord, LOCK_SCHEMA_VERSION, Lock, TrustedHook};
use phora::projection::model::{
    BindingProjection, BindingProjectionInput, CollapsePreference, LayoutSpec, LayoutStyle,
    Materialization, MaterializationPolicy, OfferSpec, ResolvedSourceRef, TakeSpec,
    TargetProjection, TemplatePolicy,
};
use phora::source::{
    ExportLeaf, ExportPolicy, ExportRequest, GitBackend, MirrorKey, NormalizedUrl,
    SourceBackend as _, SourceInventory, vars_digest,
};
use phora::store::{FileRegistry, Registry as _};
use tempfile::TempDir;

mod common;

/// A `compute_digest` selection case: label, optional root, include globs, exclude globs.
type DigestCase<'a> = (&'a str, Option<&'a str>, &'a [&'a str], &'a [&'a str]);

/// The unmoved baseline this whole file is pinned against (INV-4).
const BASELINE_COMMIT: &str = "92c784e3b14496be25dcecc8d4500e32b52b1c50";

// ─── golden-fixture harness ────────────────────────────────────────────────

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat/serialized")
}

/// `git rev-parse HEAD`, or `None` when git is unavailable / not a repo.
fn current_head() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
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

fn tracked_worktree_is_dirty() -> Option<String> {
    let out = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let dirty = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!dirty.is_empty()).then_some(dirty)
}

fn regenerate_golden(path: &Path, actual: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "PHORA_UPDATE_GOLDEN must never be set while CI is set: a golden fixture may not \
         self-bless in CI. Unset PHORA_UPDATE_GOLDEN and commit regenerated fixtures explicitly."
    );
    let head = current_head().unwrap_or_else(|| {
        panic!(
            "refusing to regenerate T002 baselines: `git rev-parse HEAD` did not succeed \
             (git unavailable or not a repo); baselines must be captured on {BASELINE_COMMIT}"
        )
    });
    assert_eq!(
        head, BASELINE_COMMIT,
        "refusing to regenerate T002 baselines off the pinned commit: HEAD is {head}, \
         baselines must be captured on {BASELINE_COMMIT}"
    );
    assert!(
        tracked_worktree_is_dirty().is_none(),
        "refusing to regenerate T002 baselines against a dirty worktree: tracked files carry \
         uncommitted modifications, so the capture would not reflect the pinned baseline {BASELINE_COMMIT}. \
         Commit or stash tracked changes first (untracked test files are expected and fine)."
    );
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
        "golden fixture missing: {} (regenerate the T002 baseline with PHORA_UPDATE_GOLDEN=1)",
        path.display()
    );
    let expected = std::fs::read_to_string(&path).expect("read golden fixture");
    assert_eq!(
        actual,
        expected,
        "serialized bytes drifted from the pinned baseline {}",
        path.display()
    );
}

// ─── shared fixture helpers ────────────────────────────────────────────────

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

/// A source repo built with fixed content, author, and dates so its commit id
/// and every derived digest are deterministic constants across runs.
fn build_source_repo(root: &Path) {
    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);

    write(&root.join("editor/init.lua"), b"-- init\n");
    write(&root.join("editor/lua/opts.lua"), b"return {}\n");
    write(&root.join("lint/rules.toml"), b"[rules]\n");
    write(&root.join("README.md"), b"loose root file\n");
    write(&root.join(".config/settings.json"), b"{\"k\":1}\n");

    git(root, &["add", "editor", "lint", "README.md", ".config"]);
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
    fn write_config(&self, body: &str) {
        write(&self.cwd.path().join("phora.toml"), body.as_bytes());
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

    /// The single per-project registry directory name under the state root — the
    /// path-hash project-id blake3(cwd). `None` until a run has created it.
    fn project_id(&self) -> Option<String> {
        let base = self.xdg_state.join("phora").join("projects");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        if dirs.len() != 1 {
            return None;
        }
        dirs.pop()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    }

    /// The mirror-directory key for this fixture's source path (blake3 of the
    /// normalized URL, first 16 hex) — derived exactly as production derives it.
    fn mirror_key(&self) -> String {
        MirrorKey::from_url(&NormalizedUrl::parse(&self.src_path.to_string_lossy()))
            .as_str()
            .to_owned()
    }

    /// Scrub the per-run temp paths and this fixture's two varying ids (project-id,
    /// mirror key) each by its own literal value, leaving every other 16-hex token
    /// (commits, digests, unrelated mirror keys, hook discriminators) byte-identical.
    fn normalize(&self, raw: &str) -> String {
        let mut s = raw.to_owned();
        for (needle, tag) in [
            (self.xdg_cache.to_string_lossy().into_owned(), "<XDG_CACHE>"),
            (self.xdg_state.to_string_lossy().into_owned(), "<XDG_STATE>"),
            (self.target_path.to_string_lossy().into_owned(), "<TARGET>"),
            (self.home_path.to_string_lossy().into_owned(), "<HOME>"),
            (self.src_path.to_string_lossy().into_owned(), "<SRC>"),
            (self.cwd.path().to_string_lossy().into_owned(), "<CWD>"),
        ] {
            s = s.replace(&needle, tag);
        }
        if let Some(project) = self.project_id() {
            s = s.replace(&project, "<PROJECT>");
        }
        s = s.replace(&self.mirror_key(), "<MIRROR>");
        normalize_iso_timestamps(&s)
    }

    fn registry_dir(&self) -> PathBuf {
        let base = self.xdg_state.join("phora").join("projects");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
            .expect("read projects base")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        assert_eq!(
            dirs.len(),
            1,
            "expected exactly one registry dir, got {dirs:?}"
        );
        dirs.pop().expect("one registry dir")
    }
}

fn snapshot(out: &Output) -> String {
    format!(
        "exit: {}\n--- stdout ---\n{}--- stderr ---\n{}",
        out.status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string()),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

fn assert_success(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx} must succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn normalize_iso_timestamps(s: &str) -> String {
    let b = s.as_bytes();
    let digits = |start: usize, n: usize| {
        start + n <= b.len() && b[start..start + n].iter().all(u8::is_ascii_digit)
    };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let stamp = digits(i, 4)
            && b.get(i + 4) == Some(&b'-')
            && digits(i + 5, 2)
            && b.get(i + 7) == Some(&b'-')
            && digits(i + 8, 2)
            && b.get(i + 10) == Some(&b'T')
            && digits(i + 11, 2)
            && b.get(i + 13) == Some(&b':')
            && digits(i + 14, 2)
            && b.get(i + 16) == Some(&b':')
            && digits(i + 17, 2);
        if stamp {
            let mut j = i + 19;
            if b.get(j) == Some(&b'.') {
                j += 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let end = if b.get(j) == Some(&b'Z') {
                Some(j + 1)
            } else if matches!(b.get(j), Some(&(b'+' | b'-')))
                && digits(j + 1, 2)
                && b.get(j + 3) == Some(&b':')
                && digits(j + 4, 2)
            {
                Some(j + 6)
            } else {
                None
            };
            if let Some(end) = end {
                for &byte in &b[i..i + 19] {
                    out.push(if byte.is_ascii_digit() {
                        'N'
                    } else {
                        byte as char
                    });
                }
                if b.get(i + 19) == Some(&b'.') {
                    out.push_str(".NNNNNN");
                }
                for &byte in &b[j..end] {
                    out.push(if byte.is_ascii_digit() {
                        'N'
                    } else {
                        byte as char
                    });
                }
                i = end;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// Sorted by relative path so the concatenated dump is order-independent.
fn dump_toml_tree(root: &Path) -> String {
    fn collect(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, base, out);
            } else if path.extension().is_some_and(|e| e == "toml") {
                let rel = path
                    .strip_prefix(base)
                    .expect("under base")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("read toml")));
            }
        }
    }
    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort();
    let mut doc = String::new();
    for (rel, body) in files {
        doc.push_str("--- ");
        doc.push_str(&rel);
        doc.push_str(" ---\n");
        doc.push_str(&body);
        if !body.ends_with('\n') {
            doc.push('\n');
        }
    }
    doc
}

const GIT_SOURCE_CONFIG: &str = "\
version = 1

[sources.dotfiles]
path = \"__SRC__\"
branch = \"main\"
include = [\"editor\", \"lint\"]

[targets.home]
path = \"__TARGET__\"
sources = [\"dotfiles\"]
layout = \"flat\"
";

fn git_source_config(fx: &Fixture) -> String {
    GIT_SOURCE_CONFIG
        .replace("__SRC__", &fx.src_path.to_string_lossy())
        .replace("__TARGET__", &fx.target_path.to_string_lossy())
}

// ─── normalization self-test (harness invariant, not a golden) ──────────────

#[test]
fn normalize_scrubs_only_fixture_ids_and_preserves_unrelated_hex() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync (to materialize a project-id)");

    let project = fx.project_id().expect("a project-id after sync");
    let mirror = fx.mirror_key();
    let unrelated_content_addressed_hex = "0123456789abcdef";
    let commit_40hex = "ca94c83b3a51aab8dea8315a9baa986e178c599d";
    assert_ne!(
        project, unrelated_content_addressed_hex,
        "self-test precondition"
    );
    assert_ne!(
        mirror, unrelated_content_addressed_hex,
        "self-test precondition"
    );

    let raw = format!(
        "projects/{project}/x mirror={mirror}.git other={unrelated_content_addressed_hex} commit={commit_40hex}\n"
    );
    let normalized = fx.normalize(&raw);

    assert_eq!(
        normalized,
        format!(
            "projects/<PROJECT>/x mirror=<MIRROR>.git other={unrelated_content_addressed_hex} commit={commit_40hex}\n"
        ),
        "normalize must scrub only the project-id and mirror key, leaving unrelated \
         content-addressed 16-hex tokens (and the 40-hex commit) byte-identical"
    );
}

#[test]
fn normalize_iso_timestamps_masks_fractions_fixed_width_and_keeps_offset_shape() {
    assert_eq!(
        normalize_iso_timestamps("at 2026-07-17T00:31:44.156086+00:00 done"),
        "at NNNN-NN-NNTNN:NN:NN.NNNNNN+NN:NN done",
    );
    assert_eq!(
        normalize_iso_timestamps("2026-07-17T00:31:44.156Z"),
        normalize_iso_timestamps("2026-07-17T00:31:44.156086231Z"),
        "chrono's AutoSi emits 0/3/6/9 fractional digits per value; every present \
         fraction must mask to one fixed-width token or the goldens flake on \
         trailing-zero micros"
    );
    assert_eq!(
        normalize_iso_timestamps("2026-07-17T00:31:44.156Z"),
        "NNNN-NN-NNTNN:NN:NN.NNNNNNZ",
    );
    assert_eq!(
        normalize_iso_timestamps("2026-07-17T00:31:44Z"),
        "NNNN-NN-NNTNN:NN:NNZ",
        "an absent fraction stays absent; only a present fraction masks fixed-width"
    );
    assert_ne!(
        normalize_iso_timestamps("2026-07-17T00:31:44Z"),
        normalize_iso_timestamps("2026-07-17T00:31:44+00:00"),
        "a Z↔+00:00 offset drift must still break the golden rather than hide"
    );
}

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

// ─── lock ──────────────────────────────────────────────────────────────────

#[test]
fn lock_serialized_is_byte_identical() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");

    let lock = std::fs::read_to_string(fx.cwd.path().join("phora.lock")).expect("read phora.lock");
    assert_golden("lock.toml.golden", &fx.normalize(&lock));
}

// ─── registry record + metadata ────────────────────────────────────────────

#[test]
fn registry_records_serialized_are_byte_identical() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");

    let dump = dump_toml_tree(&fx.registry_dir());
    assert_golden("registry_records.golden", &fx.normalize(&dump));
}

// ─── ejection (recorded in target metadata) ─────────────────────────────────

#[test]
fn registry_metadata_after_ejection_is_byte_identical() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");
    assert_success(
        &fx.run(&[
            "eject", "editor", "--source", "dotfiles", "--target", "home",
        ]),
        "eject editor",
    );

    let dump = dump_toml_tree(&fx.registry_dir());
    assert_golden("registry_after_eject.golden", &fx.normalize(&dump));
}

// ─── orphan (a record whose config target is gone) ──────────────────────────

#[test]
fn orphan_report_is_byte_identical() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(
        &fx.run(&["sync"]),
        "initial sync deploys editor + lint under target home",
    );

    // sync::is_orphan keys on target absence, not include narrowing: dropping the
    // whole `[targets.home]` block is what turns the first sync's records orphan.
    let without_target = git_source_config(&fx)
        .split_once("[targets.home]")
        .expect("config has a [targets.home] section")
        .0
        .trim_end()
        .to_owned()
        + "\n";
    fx.write_config(&without_target);

    let out = fx.run(&["sync"]);
    assert_golden("orphan_report.golden", &fx.normalize(&snapshot(&out)));
}

// ─── preview: projection tree + JSON ────────────────────────────────────────

#[test]
fn preview_projection_tree_is_byte_identical() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");

    let out = fx.run(&["preview"]);
    assert_success(&out, "preview");
    assert_golden("projection_tree.golden", &fx.normalize(&snapshot(&out)));
}

#[test]
fn preview_json_is_byte_identical() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");

    let out = fx.run(&["preview", "--json"]);
    assert_success(&out, "preview --json");
    assert_golden("preview.json.golden", &fx.normalize(&snapshot(&out)));
}

// ─── diagnostics ────────────────────────────────────────────────────────────

#[test]
fn source_error_diagnostic_is_byte_identical() {
    let fx = build_fixture();
    // A fixed bogus path yields a fully deterministic source-resolution diagnostic.
    fx.write_config(
        "version = 1\n\n[sources.dotfiles]\npath = \"/nonexistent/phora-compat-fixture\"\n\
         branch = \"main\"\ninclude = [\"editor\"]\n\n[targets.home]\npath = \"__TARGET__\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n"
            .replace("__TARGET__", &fx.target_path.to_string_lossy())
            .as_str(),
    );

    let out = fx.run(&["sync"]);
    assert!(
        !out.status.success(),
        "sync against a nonexistent source path must fail with a diagnostic"
    );
    assert_golden(
        "diagnostic_source_error.golden",
        &fx.normalize(&snapshot(&out)),
    );
}

// ─── config bytes (the toml_edit writer behind `phora bind`) ────────────────

#[test]
fn config_writer_bytes_are_byte_identical() {
    let fx = build_fixture();
    fx.write_config(
        "version = 1\n\n[sources.dotfiles]\npath = \"__SRC__\"\nbranch = \"main\"\n\
         include = [\"editor\"]\n\n[targets.home]\npath = \"__TARGET__\"\nlayout = \"flat\"\n\
         sources = []\n"
            .replace("__SRC__", &fx.src_path.to_string_lossy())
            .replace("__TARGET__", &fx.target_path.to_string_lossy())
            .as_str(),
    );

    assert_success(
        &fx.run(&["bind", "dotfiles", "--to", "home"]),
        "bind dotfiles to home",
    );

    let written =
        std::fs::read_to_string(fx.cwd.path().join("phora.toml")).expect("read phora.toml");
    assert_golden("config_bytes.golden", &fx.normalize(&written));
}

// ─── source, file, and variable digests (pure, offline) ─────────────────────

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
}

struct DigestFixture {
    _src: TempDir,
    _git_dir: TempDir,
    backend: GitBackend,
    url: String,
    commit: String,
}

fn build_digest_fixture() -> DigestFixture {
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

    DigestFixture {
        _src: src,
        _git_dir: git_dir,
        backend,
        url,
        commit,
    }
}

#[test]
fn source_and_file_digests_are_byte_identical() {
    let fx = build_digest_fixture();
    let to_vec = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let digest = |root: Option<&str>, include: &[&str], exclude: &[&str]| {
        fx.backend
            .compute_digest(
                &sn("fixture"),
                &fx.url,
                &fx.commit,
                root.map(Path::new),
                &to_vec(include),
                &to_vec(exclude),
            )
            .expect("compute_digest succeeds")
    };

    let cases: [DigestCase; 5] = [
        ("full-tree", None, &[], &[]),
        ("exclude-root-loose", None, &[], &["/README.md"]),
        ("exclude-dotfile-dir", None, &[], &[".config/**"]),
        ("editor-subtree", None, &["editor/**"], &[]),
        ("rooted-dotfile", Some(".config"), &[], &[]),
    ];

    let mut doc = String::new();
    doc.push_str("[aggregate-selection-digest]\n");
    for (label, root, include, exclude) in cases {
        doc.push_str(label);
        doc.push_str(" = ");
        doc.push_str(&digest(root, include, exclude));
        doc.push('\n');
    }

    let commit_time = fx
        .backend
        .commit_time(&sn("fixture"), &fx.url, &fx.commit)
        .expect("commit_time");
    let leaf_paths = fx
        .backend
        .list_source_leaves(&sn("fixture"), &fx.url, &fx.commit, None)
        .expect("list source leaves");
    let leaves: Vec<ExportLeaf> = leaf_paths
        .iter()
        .map(|p| ExportLeaf {
            source: PathBuf::from(p),
            dest: PathBuf::from(p),
        })
        .collect();
    let staging = TempDir::new().expect("staging tempdir");
    let policy = ExportPolicy::default();
    let vars: BTreeMap<String, String> = BTreeMap::new();
    let export = fx
        .backend
        .export_artifact(&ExportRequest {
            source: &sn("fixture"),
            url: &fx.url,
            commit: &fx.commit,
            root: None,
            policy: &policy,
            staging_dir: staging.path(),
            commit_time,
            template_opt_in: &TemplateOptIn::SuffixOnly,
            vars: &vars,
            leaves: &leaves,
        })
        .expect("export_artifact succeeds");

    doc.push_str("\n[artifact-digest]\n");
    doc.push_str("digest = ");
    doc.push_str(&export.digest);
    doc.push('\n');
    doc.push_str("vars_digest = ");
    doc.push_str(export.vars_digest.as_deref().unwrap_or("<none>"));
    doc.push('\n');

    doc.push_str("\n[per-file-blake3]\n");
    let mut files = export.files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for f in &files {
        let _ = writeln!(
            doc,
            "{} size={} mtime={} blake3={}",
            f.path.to_string_lossy().replace('\\', "/"),
            f.size,
            f.mtime,
            f.blake3,
        );
    }

    assert_golden("source_digests.golden", &doc);
}

#[test]
fn variable_digests_are_byte_identical() {
    let cases: [(&str, &[(&str, &str)]); 3] = [
        ("empty", &[]),
        ("single", &[("editor", "nvim")]),
        (
            "multi",
            &[("editor", "nvim"), ("shell", "zsh"), ("theme", "dark")],
        ),
    ];
    let mut doc = String::new();
    for (label, pairs) in cases {
        let vars: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        doc.push_str(label);
        doc.push_str(" = ");
        doc.push_str(&vars_digest(&vars));
        doc.push('\n');
    }
    assert_golden("variable_digests.golden", &doc);
}

// ─── cache/mirror addressing SCHEME/format (M12) ────────────────────────────

#[test]
fn cache_mirror_addressing_scheme_is_byte_identical() {
    // Pins the url → mirror-directory-name mapping (normalization + BLAKE3 key),
    // i.e. the addressing SCHEME/format — not any concrete cache file set.
    let urls = [
        "https://github.com/user/repo.git",
        "https://github.com/user/repo",
        "git@github.com:user/repo.git",
        "https://GitHub.com/User/Repo.git",
        "https://gitlab.com/group/sub/project.git",
        "ssh://git@example.com/team/tool.git",
    ];
    let mut doc = String::new();
    for url in urls {
        let normalized = NormalizedUrl::parse(url);
        let key = MirrorKey::from_url(&normalized);
        doc.push_str(url);
        doc.push_str("\n  normalized = ");
        doc.push_str(normalized.as_str());
        doc.push_str("\n  mirror_dir = ");
        doc.push_str(key.as_str());
        doc.push_str(".git\n");
    }
    assert_golden("cache_mirror_addressing.golden", &doc);
}

// ─── URL synthetic commit (deterministic, content-addressed) ────────────────

/// Serve `body` at `http://127.0.0.1:<ephemeral>/src.tar` for the lifetime of
/// the test process. Content-addressed synthetic import makes the resulting
/// commit id independent of the (random) port.
fn serve_bytes_forever(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut scratch = [0u8; 2048];
            let _ = stream.read(&mut scratch);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/src.tar")
}

#[test]
fn url_synthetic_commit_is_byte_identical() {
    // Two top-level dirs so `strip_single_top_level` leaves the tree untouched.
    let staging = TempDir::new().expect("staging tempdir");
    write(&staging.path().join("a/x.txt"), b"alpha\n");
    write(&staging.path().join("b/y.txt"), b"beta\n");
    let tar_path = staging.path().join("src.tar");
    let tar = Command::new("tar")
        .current_dir(staging.path())
        .args(["-cf", "src.tar", "a", "b"])
        .status()
        .expect("tar runs");
    assert!(tar.success(), "tar must build the fixture archive");
    let tar_bytes = std::fs::read(&tar_path).expect("read tar");

    let url = serve_bytes_forever(tar_bytes);

    let fx = build_fixture();
    fx.write_config(
        "version = 1\n\n[sources.web]\nurl = \"__URL__\"\ninclude = [\"a\", \"b\"]\n\n\
         [targets.home]\npath = \"__TARGET__\"\nsources = [\"web\"]\nlayout = \"flat\"\n"
            .replace("__URL__", &url)
            .replace("__TARGET__", &fx.target_path.to_string_lossy())
            .as_str(),
    );
    assert_success(&fx.run(&["sync"]), "sync url source");

    let lock = std::fs::read_to_string(fx.cwd.path().join("phora.lock")).expect("read phora.lock");
    let commit = lock
        .lines()
        .find_map(|l| l.trim().strip_prefix("commit = "))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .expect("lock records a synthetic commit for the url source");
    assert!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "synthetic commit must be a 40-hex git id, got {commit:?}"
    );
    assert_golden("url_synthetic_commit.golden", &format!("{commit}\n"));
}

// ─── hooks: identity/discriminator, trust-lock variants, success recording ──────

/// A fixed dep commit so every commit-bound preimage in the hook goldens is constant.
const HOOK_DEP_COMMIT: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

/// Admits one Shell and one Exec candidate from a fixed opaque `[targets.editor.hooks]`
/// payload, so each hook's identity (`command_discriminator`-derived `hook_id`) and its
/// commit-bound preimage are derived by production, then pinned.
fn transitive_hook_candidates() -> Vec<phora::config::CandidateHook> {
    let opaque: toml::Value = toml::from_str(
        "[editor]\non_change = [\"./install.sh\", { cmd = [\"setup\", \"--yes\"] }]\n",
    )
    .expect("opaque hooks payload parses as toml");
    let node = FetchNode::new("https://github.com/dep/nvim.git", "main", HOOK_DEP_COMMIT);
    let instance = Instance::new("root", "editor", "anchor", node);
    let (candidates, diagnostics) =
        admit_transitive_hooks(&opaque, "editor", "ns%1%editor", &instance);
    assert!(
        diagnostics.is_empty(),
        "the fixture hooks payload must admit cleanly, got: {diagnostics:?}"
    );
    assert_eq!(
        candidates.len(),
        2,
        "the fixture must yield exactly one Shell and one Exec candidate"
    );
    candidates
}

#[test]
fn hook_identity_and_command_discriminator_serialized_are_byte_identical() {
    let candidates = transitive_hook_candidates();
    let mut doc = String::new();
    for c in &candidates {
        let _ = writeln!(doc, "dep_instance = {}", c.dep_instance);
        let _ = writeln!(doc, "hook_id = {}", c.hook_id);
        let _ = writeln!(doc, "command = {}", c.command.display());
        let _ = writeln!(
            doc,
            "on_change_preimage = {}",
            hook_preimage(&c.command, "on_change", HOOK_DEP_COMMIT)
        );
        let _ = writeln!(
            doc,
            "pre_deploy_preimage = {}",
            hook_preimage(&c.command, "pre_deploy", HOOK_DEP_COMMIT)
        );
        doc.push('\n');
    }
    assert_golden("hook_identity.golden", &doc);
}

#[test]
fn hook_trust_lock_variants_serialized_are_byte_identical() {
    let candidates = transitive_hook_candidates();
    let trusted = &candidates[0];
    let candidate = &candidates[1];
    let lock = Lock {
        version: LOCK_SCHEMA_VERSION,
        sources: Vec::new(),
        trusted_hooks: vec![TrustedHook {
            dep_instance: trusted.dep_instance.clone(),
            hook_id: trusted.hook_id.clone(),
            preimage: hook_preimage(&trusted.command, "on_change", HOOK_DEP_COMMIT),
            approved_at: "2026-01-31T12:34:56+00:00".to_owned(),
            source: "editor".to_owned(),
            commit: HOOK_DEP_COMMIT.to_owned(),
        }],
        candidate_hooks: vec![CandidateHookRecord {
            dep_instance: candidate.dep_instance.clone(),
            hook_id: candidate.hook_id.clone(),
            preimage: hook_preimage(&candidate.command, "on_change", HOOK_DEP_COMMIT),
            command: candidate.command.display(),
            source: "editor".to_owned(),
            commit: HOOK_DEP_COMMIT.to_owned(),
        }],
    };
    let text = toml::to_string(&lock).expect("hook-trust lock serializes to toml");
    assert_golden("hook_trust_lock.toml.golden", &text);
}

#[test]
fn hook_success_state_serialized_is_byte_identical() {
    let root = TempDir::new().expect("state tempdir");
    let registry = FileRegistry::open(root.path().join("state")).expect("open registry");
    let set: std::collections::BTreeSet<String> = ["blake3:aaaa1111", "blake3:bbbb2222"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    registry
        .record_hook_success("home", "home#0123456789abcdef#fedcba9876543210", &set)
        .expect("record hook success");

    let meta = std::fs::read_to_string(root.path().join("state/targets/home/meta.toml"))
        .expect("record_hook_success wrote the target meta");
    assert_golden("hook_success_state.golden", &meta);
}

// ─── hooks: environment construction + dedup (subprocess, via `sync`) ────────────

#[test]
fn hook_environment_construction_is_byte_identical() {
    let fx = build_fixture();
    write(
        &fx.cwd.path().join("dump-env.sh"),
        b"#!/bin/sh\n\
          printf 'PHORA_TARGET=%s\\n' \"$PHORA_TARGET\" > hookenv.txt\n\
          printf 'PHORA_CHANGED_NAMES=%s\\n' \"$PHORA_CHANGED_NAMES\" >> hookenv.txt\n\
          printf 'PHORA_CHANGED=%s\\n' \"$PHORA_CHANGED\" >> hookenv.txt\n",
    );
    fx.write_config(&format!(
        "{}\n[targets.home.hooks]\non_change = \"sh dump-env.sh\"\n",
        git_source_config(&fx)
    ));
    assert_success(&fx.run(&["sync"]), "sync fires the on_change hook");

    let captured = std::fs::read_to_string(fx.cwd.path().join("hookenv.txt"))
        .expect("the on_change hook captured its PHORA_* environment");
    assert_golden("hook_environment.golden", &fx.normalize(&captured));
}

#[test]
fn hook_dedup_behavior_is_byte_identical() {
    let fx = build_fixture();
    write(
        &fx.cwd.path().join("count.sh"),
        b"#!/bin/sh\nprintf 'ran\\n' >> invocations.txt\n",
    );
    fx.write_config(&format!(
        "{}\n[targets.home.hooks]\non_change = [\"sh count.sh\", \"sh count.sh\"]\n",
        git_source_config(&fx)
    ));
    assert_success(&fx.run(&["sync"]), "sync fires the deduped on_change hooks");

    let runs = std::fs::read_to_string(fx.cwd.path().join("invocations.txt"))
        .map_or(0, |s| s.lines().count());
    assert_golden("hook_dedup.golden", &format!("invocations = {runs}\n"));
}

// ─── T010: the moved projection paths vs the T002 goldens ───────────────────

fn read_golden(name: &str) -> String {
    std::fs::read_to_string(golden_dir().join(name)).expect("read golden fixture")
}

fn snapshot_stdout(snapshot: &str) -> &str {
    let marker = "--- stdout ---\n";
    let start = snapshot.find(marker).expect("snapshot has a stdout marker") + marker.len();
    let end = snapshot
        .rfind("--- stderr ---")
        .expect("snapshot has a stderr marker");
    &snapshot[start..end]
}

fn preview_json_artifact_commit_pairs(json: &str) -> Vec<(String, String)> {
    let doc: serde_json::Value = serde_json::from_str(json).expect("preview json parses");
    let field = |entry: &serde_json::Value, key: &str| {
        entry[key]
            .as_str()
            .unwrap_or_else(|| panic!("entry carries a string `{key}`"))
            .to_owned()
    };
    doc["targets"][0]["entries"]
        .as_array()
        .expect("preview json has entries")
        .iter()
        .map(|entry| (field(entry, "artifact"), field(entry, "commit")))
        .collect()
}

fn golden_tree_commit_dest_pairs() -> Vec<(String, String)> {
    read_golden("projection_tree.golden")
        .lines()
        .filter_map(|line| {
            let (head, dest) = line.split_once(" -> <TARGET>/")?;
            let commit = head.trim_start().split_once('@')?.1.split_once(' ')?.0;
            Some((commit.to_owned(), dest.to_owned()))
        })
        .collect()
}

fn golden_lock_commit() -> String {
    read_golden("lock.toml.golden")
        .lines()
        .find_map(|line| line.strip_prefix("commit = "))
        .expect("the lock golden records a resolved commit")
        .trim_matches('"')
        .to_owned()
}

fn golden_registry_leaf_paths(artifact: &str) -> Vec<String> {
    let text = read_golden("registry_records.golden");
    let header = format!("--- targets/home/artifacts/dotfiles/{artifact}.toml ---\n");
    let (_, rest) = text
        .split_once(&header)
        .unwrap_or_else(|| panic!("the registry golden has a section for `{artifact}`"));
    let mut paths: Vec<String> = rest
        .split("\n--- ")
        .next()
        .expect("a split yields a first chunk")
        .lines()
        .filter_map(|line| line.strip_prefix("path = "))
        .map(|value| value.trim_matches('"').to_owned())
        .collect();
    paths.sort();
    paths
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

fn moved_path_projection(fx: &Fixture, layout_override: Option<LayoutSpec>) -> TargetProjection {
    let cfg = Config::parse(&git_source_config(fx)).expect("fixture config parses");
    let parsed = cfg.parsed_sources().expect("fixture sources parse");
    let target = &cfg.targets["home"];
    let commit = fixture_source_commit(fx);

    let bindings = target.resolve_sources(&parsed);
    assert_eq!(
        bindings.len(),
        1,
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
    let layout = layout_override.unwrap_or_else(|| LayoutSpec::from(&target.layout()));
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
    phora::projection::build::project_target("home", &[input]).expect("the fixture projects")
}

fn destinations_of(projection: &TargetProjection) -> Vec<String> {
    projection
        .artifacts
        .iter()
        .map(|artifact| artifact.destination.as_str().to_owned())
        .collect()
}

#[test]
fn moved_projection_output_equals_projection_tree_and_artifact_identity_goldens() {
    let fx = build_fixture();
    let projection = moved_path_projection(&fx, None);

    let tree = golden_tree_commit_dest_pairs();
    assert!(
        !tree.is_empty(),
        "the projection-tree golden pins at least one artifact"
    );
    let destinations = destinations_of(&projection);
    assert_eq!(
        destinations,
        tree.iter()
            .map(|(_, dest)| dest.clone())
            .collect::<Vec<_>>(),
        "the moved projection's target-relative destinations must equal the T002 \
         projection-tree golden"
    );

    assert_eq!(projection.bindings.len(), 1, "one binding projects");
    let binding = &projection.bindings[0];
    let lock_commit = golden_lock_commit();
    assert_eq!(
        binding.commit, lock_commit,
        "the moved projection's resolved commit must equal the lock golden's artifact identity"
    );
    for (abbreviated, _) in &tree {
        assert_eq!(
            abbreviated,
            &lock_commit[..abbreviated.len()],
            "the tree golden abbreviates that same commit"
        );
    }

    let entries =
        preview_json_artifact_commit_pairs(snapshot_stdout(&read_golden("preview.json.golden")));
    assert_eq!(
        phora::projection::build::projected_artifact_keys(binding),
        entries
            .iter()
            .map(|(artifact, _)| artifact.clone())
            .collect::<Vec<_>>(),
        "the moved projected_artifact_keys must equal the preview-json golden's artifact keys"
    );
    for (artifact, commit) in &entries {
        assert_eq!(
            commit, &binding.commit,
            "preview-json golden entry `{artifact}` carries the projected commit"
        );
    }

    for artifact in &binding.artifacts {
        let key = artifact.materialization.published_key();
        let mut leaf_destinations: Vec<String> = artifact
            .leaves
            .iter()
            .map(|leaf| leaf.destination.as_str().to_owned())
            .collect();
        leaf_destinations.sort();
        let golden_paths = golden_registry_leaf_paths(key);
        assert!(
            !golden_paths.is_empty(),
            "the registry golden pins leaves for `{key}`"
        );
        assert_eq!(
            leaf_destinations, golden_paths,
            "artifact `{key}`: the moved projection's leaf set must equal the registry \
             golden's manifest paths"
        );
    }

    let perturbed = moved_path_projection(
        &fx,
        Some(LayoutSpec::new(LayoutStyle::BySource, String::new())),
    );
    assert_ne!(
        destinations_of(&perturbed),
        destinations,
        "a divergent projection (by-source layout) must not satisfy the golden equality"
    );
}

#[test]
fn preview_sync_and_prune_share_one_projected_artifact_identity_set() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");
    let out = fx.run(&["preview", "--json"]);
    assert_success(&out, "preview --json");

    let previewed: BTreeSet<String> =
        preview_json_artifact_commit_pairs(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .map(|(artifact, _)| artifact)
            .collect();

    let artifacts_dir = fx.registry_dir().join("targets/home/artifacts/dotfiles");
    let synced: BTreeSet<String> = std::fs::read_dir(&artifacts_dir)
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

    let projection = moved_path_projection(&fx, None);
    let prune_protected: BTreeSet<String> = projection
        .bindings
        .iter()
        .flat_map(phora::projection::build::projected_artifact_keys)
        .collect();

    let expected: BTreeSet<String> = ["editor", "lint"].map(str::to_owned).into_iter().collect();
    assert_eq!(
        previewed, expected,
        "preview derives the projected artifact identities"
    );
    assert_eq!(
        synced, expected,
        "sync manages the projected artifact identities"
    );
    assert_eq!(
        prune_protected, expected,
        "the keys prune protects are the moved projection's projected_artifact_keys"
    );
}

#[test]
fn deploy_joins_the_target_root_with_moved_target_relative_destinations() {
    let fx = build_fixture();
    fx.write_config(&git_source_config(&fx));
    assert_success(&fx.run(&["sync"]), "sync");

    let projection = moved_path_projection(&fx, None);
    assert!(
        !projection.artifacts.is_empty(),
        "the fixture projects artifacts"
    );
    for artifact in &projection.artifacts {
        let dest = artifact.destination.as_str();
        assert!(
            !dest.starts_with('/'),
            "the moved projection keeps `{dest}` target-relative; joining the root is sync's job"
        );
        let deployed = fx.target_path.join(dest);
        match &artifact.materialization {
            Materialization::Leaf(_) => assert!(
                deployed.is_file(),
                "deploy must materialize the leaf by joining the target root with `{dest}`"
            ),
            Materialization::CollapsedDir { .. } => {
                assert!(
                    deployed.is_dir(),
                    "deploy must materialize the dir by joining the target root with `{dest}`"
                );
                for leaf in &artifact.leaves {
                    let file = deployed.join(leaf.destination.as_str());
                    assert!(
                        file.is_file(),
                        "deploy must materialize leaf `{}` under the joined destination",
                        file.display()
                    );
                }
            }
        }
    }
}

#[test]
fn sync_facade_projection_exports_are_the_moved_symbols() {
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

    let via_facade: BindingProjection =
        phora::sync::project_binding(&input).expect("facade projects");
    let direct = phora::projection::build::project_binding(&input).expect("direct path projects");
    assert_eq!(
        via_facade, direct,
        "facade and moved path yield one projection"
    );

    let facade_keys: fn(&BindingProjection) -> Vec<String> = phora::sync::projected_artifact_keys;
    assert_eq!(
        facade_keys(&direct),
        phora::projection::build::projected_artifact_keys(&direct),
        "facade and moved projected_artifact_keys agree"
    );
    assert_eq!(
        phora::projection::build::projected_artifact_keys(&direct),
        vec!["d".to_owned()],
        "the wholly-taken dir projects as the collapsed key `d`"
    );

    let moved_destination: phora::projection::model::TargetPath =
        direct.artifacts[0].destination.clone();
    let facade_destination: phora::sync::TargetPath = moved_destination;
    assert_eq!(facade_destination.as_str(), "d");
}
