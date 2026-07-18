//! Pinning test for `scripts/arch-check.sh` — the module-dependency arch lint (T005).
//!
//! Invocation contract the script must honor and no assertion below can encode:
//! `scripts/arch-check.sh <SCAN_ROOT>` scans `<SCAN_ROOT>/src`, exiting non-zero on
//! any violation. Allowlists/baselines resolve relative to the script's own
//! directory, never to the scan root, so a scan root may be a bare copy of `src/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    manifest().join("scripts/arch-check.sh")
}

struct Run {
    ran: bool,
    success: bool,
    output: String,
}

impl Run {
    fn assert_pass(&self, ctx: &str) {
        assert!(
            self.ran,
            "arch-check.sh did not run ({ctx}) — the script is missing or not \
             executable at {}",
            script_path().display()
        );
        assert!(
            self.success,
            "arch-check.sh must EXIT 0 for {ctx}, but it failed:\n{}",
            self.output
        );
    }

    fn assert_fail(&self, ctx: &str) {
        assert!(
            self.ran,
            "arch-check.sh did not run ({ctx}) — the script is missing or not \
             executable at {}",
            script_path().display()
        );
        assert!(
            !self.success,
            "arch-check.sh must FAIL for {ctx}, but it exited 0:\n{}",
            self.output
        );
    }
}

fn run_arch_check(root: &Path) -> Run {
    match Command::new(script_path())
        .arg(root)
        .current_dir(manifest())
        .output()
    {
        Ok(out) => Run {
            ran: true,
            success: out.status.success(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        },
        Err(_) => Run {
            ran: false,
            success: false,
            output: String::new(),
        },
    }
}

struct Tree {
    _tmp: TempDir,
    root: PathBuf,
}

fn base_tree() -> Tree {
    let tmp = TempDir::new().expect("temp dir");
    let root = tmp.path().to_path_buf();
    copy_dir_all(&manifest().join("src"), &root.join("src"));
    Tree { _tmp: tmp, root }
}

impl Tree {
    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, body).expect("write fixture");
    }

    fn copy_real_src(&self, real_rel: &str, dst_rel: &str) {
        let body = fs::read(manifest().join(real_rel)).expect("read real source");
        let path = self.root.join(dst_rel);
        fs::write(&path, body).expect("write copied source");
    }

    fn check(&self) -> Run {
        run_arch_check(&self.root)
    }
}

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir dst");
    for entry in fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_all(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).expect("copy file");
        }
    }
}

const COMPLIANT_PROJECTION: &str = r"
use std::collections::BTreeMap;

use globset::GlobSet;
use unicode_normalization::UnicodeNormalization;

use crate::source::{SourceEntryKind, SourceEntryMeta, SourceInventory, SourcePath};
use crate::kernel::{ArtifactName, Commit, SourceName, TargetName};
use crate::error::Error;
use crate::diagnostic::Diagnostic;

use crate::projection::model;

pub fn build() -> BTreeMap<String, String> {
    BTreeMap::new()
}
";

const COMPLIANT_RECONCILE: &str = r"
use std::collections::BTreeMap;

use crate::projection::Projection;
use crate::sync::model::{ChangeSet, ObservedProjectState};

pub fn reconcile(_p: &Projection, _o: &ObservedProjectState) -> ChangeSet {
    let _ = BTreeMap::<String, String>::new();
    ChangeSet::default()
}
";

#[test]
#[cfg(unix)]
fn arch_check_script_exists_and_is_executable() {
    use std::os::unix::fs::PermissionsExt as _;

    let path = script_path();
    let meta = fs::metadata(&path)
        .unwrap_or_else(|_| panic!("scripts/arch-check.sh must exist at {}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "scripts/arch-check.sh must be executable (mode {:o})",
        meta.permissions().mode()
    );
}

#[test]
fn arch_check_passes_on_current_tree() {
    base_tree()
        .check()
        .assert_pass("the current tree: no projection/ or reconcile.rs, legacy I/O allow-listed");
}

#[test]
fn projection_direct_forbidden_imports_fail() {
    let forbidden = [
        ("config", "use crate::config::Config;"),
        ("std::fs", "use std::fs;"),
        ("serde", "use serde::Serialize;"),
        ("chrono", "use chrono::Utc;"),
        ("gix", "use gix::Repository;"),
        ("sync", "use crate::sync::plan::plan_target;"),
        ("source-io", "use crate::source::SourceStore;"),
        ("std::process", "use std::process::Command;"),
        ("cli-boundary", "use crate::cli::run;"),
        ("std::net", "use std::net::TcpStream;"),
        ("external-crate-not-on-allowlist", "use rand::Rng;"),
        ("source-io-git-backend", "use crate::source::GitBackend;"),
    ];
    for (label, import) in forbidden {
        let tree = base_tree();
        tree.write(
            "src/projection/build.rs",
            &format!("{import}\npub fn build() {{}}\n"),
        );
        tree.check()
            .assert_fail(&format!("projection importing forbidden `{label}`"));
    }
}

#[test]
fn projection_indirect_forbidden_import_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use crate::projection::helper;\npub fn build() { helper::run(); }\n",
    );
    tree.write(
        "src/projection/helper.rs",
        "use std::fs;\npub fn run() { let _ = fs::metadata(\"/\"); }\n",
    );
    tree.check()
        .assert_fail("projection whose own module pulls in an I/O-bearing dependency (indirect)");
}

#[test]
fn projection_compliant_file_passes() {
    let tree = base_tree();
    tree.write("src/projection/build.rs", COMPLIANT_PROJECTION);
    tree.write(
        "src/projection/model.rs",
        "use std::collections::BTreeMap;\npub type Model = BTreeMap<String, String>;\n",
    );
    tree.check().assert_pass(
        "a projection file importing only positive-allowlist items (pure source \
         value types, kernel identities, globset, unicode_normalization, crate \
         error/diagnostic, own modules, std non-I/O)",
    );
}

#[test]
fn projection_forbidden_token_in_comment_is_ignored() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        &format!(
            "// counter-example: use crate::config::Config; use std::fs;\n\
             /* also forbidden here: use chrono::Utc; */\n{COMPLIANT_PROJECTION}"
        ),
    );
    tree.write("src/projection/model.rs", "pub type Model = ();\n");
    tree.check()
        .assert_pass("forbidden identifiers that appear only inside comments");
}

#[test]
fn projection_forbidden_import_in_cfg_test_is_ignored() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        &format!(
            "{COMPLIANT_PROJECTION}\n\
             #[cfg(test)]\nmod tests {{\n    use crate::config::Config;\n    use std::fs;\n}}\n"
        ),
    );
    tree.write("src/projection/model.rs", "pub type Model = ();\n");
    tree.check()
        .assert_pass("forbidden imports confined to a #[cfg(test)] module");
}

#[test]
fn reconcile_importing_io_bearing_sync_module_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use crate::sync::target;\npub fn reconcile() { target::noop(); }\n",
    );
    tree.check()
        .assert_fail("reconcile.rs importing the I/O-bearing sync::target module");
}

#[test]
fn reconcile_importing_source_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use crate::source::SourceStore;\npub fn reconcile() {}\n",
    );
    tree.check().assert_fail("reconcile.rs importing source");
}

#[test]
fn reconcile_using_filesystem_method_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use std::fs;\npub fn reconcile() { let _ = fs::read(\"/etc/hosts\"); }\n",
    );
    tree.check()
        .assert_fail("reconcile.rs touching the filesystem via std::fs");
}

#[test]
fn reconcile_using_path_fs_method_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use std::path::Path;\n\
         pub fn reconcile() { let _ = Path::new(\"/etc/hosts\").exists(); }\n",
    );
    tree.check().assert_fail(
        "reconcile.rs reaching the filesystem via a Path fs-method (Path::exists) \
         with no std::fs import",
    );
}

#[test]
fn reconcile_using_path_metadata_method_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use std::path::Path;\n\
         pub fn reconcile() { let _ = Path::new(\"/etc/hosts\").metadata(); }\n",
    );
    tree.check().assert_fail(
        "reconcile.rs reaching the filesystem via a Path fs-method (Path::metadata) \
         with no std::fs import",
    );
}

#[test]
fn reconcile_compliant_file_passes() {
    let tree = base_tree();
    tree.write("src/sync/reconcile.rs", COMPLIANT_RECONCILE);
    tree.check()
        .assert_pass("reconcile.rs importing only projection + sync::model + std non-I/O");
}

const COMPLIANT_SOURCE: &str = r"
use std::collections::BTreeMap;

use crate::config::Refspec;
use crate::kernel::SourceName;
use crate::error::Error;

pub fn describe(_r: &Refspec, _n: &SourceName) -> BTreeMap<String, String> {
    let _ = Error::Sync(String::new());
    BTreeMap::new()
}
";

#[test]
fn source_direct_forbidden_imports_fail() {
    let forbidden = [
        ("projection", "use crate::projection::model::Projection;"),
        ("sync-staging", "use crate::sync::stage::stage_artifact;"),
        ("sync-target-path", "use crate::sync::TargetPath;"),
        ("manifest", "use crate::store::ManifestFile;"),
        ("registry", "use crate::store::RegistryRecord;"),
        ("template-policy", "use crate::config::TemplateOptIn;"),
    ];
    for (label, import) in forbidden {
        let tree = base_tree();
        tree.write(
            "src/source/probe.rs",
            &format!("{import}\npub fn probe() {{}}\n"),
        );
        tree.check().assert_fail(&format!(
            "source importing forbidden `{label}` (INV-2 staging clause)"
        ));
    }
}

#[test]
fn source_indirect_forbidden_import_fails() {
    let tree = base_tree();
    tree.write(
        "src/source/probe.rs",
        "use crate::source::probe_helper;\npub fn probe() { probe_helper::run(); }\n",
    );
    tree.write(
        "src/source/probe_helper.rs",
        "use crate::sync::stage::Renderer;\npub fn run() { let _ = Renderer; }\n",
    );
    tree.check().assert_fail(
        "a source submodule pulling in the sync staging module (INV-2 staging clause)",
    );
}

#[test]
fn source_compliant_file_passes() {
    let tree = base_tree();
    tree.write("src/source/probe.rs", COMPLIANT_SOURCE);
    tree.check().assert_pass(
        "a source file importing only config non-template values, kernel identities, the \
         crate error type, and std non-I/O — the INV-2 source clause must not over-forbid \
         source's legitimate config/kernel/std dependencies",
    );
}

#[test]
fn source_forbidden_import_in_cfg_test_is_ignored() {
    let tree = base_tree();
    tree.write(
        "src/source/probe.rs",
        &format!(
            "{COMPLIANT_SOURCE}\n\
             #[cfg(test)]\nmod tests {{\n    use crate::sync::stage::stage_artifact;\n\
             use crate::store::ManifestFile;\n}}\n"
        ),
    );
    tree.check()
        .assert_pass("forbidden source imports confined to a #[cfg(test)] module");
}

#[test]
fn source_fully_qualified_forbidden_crate_path_fails() {
    let bypasses = [
        (
            "crate::sync::stage body call, no use",
            "pub fn probe() { let _ = crate::sync::stage::stage_artifact(); }\n",
        ),
        (
            "crate::store body path, no use",
            "pub fn probe() { let _ = crate::store::ManifestFile::default(); }\n",
        ),
        (
            "crate :: sync (whitespace-separated ::)",
            "pub fn probe() { let _ = crate :: sync :: stage :: stage_artifact(); }\n",
        ),
        (
            "crate::projection body path, no use",
            "pub fn probe() { let _ = crate::projection::model::TargetProjection::default(); }\n",
        ),
        (
            "submodule-prefixed template policy import",
            "use crate::config::target::TemplateOptIn;\npub fn probe() {}\n",
        ),
    ];
    for (label, body) in bypasses {
        let tree = base_tree();
        tree.write("src/source/probe.rs", body);
        tree.check().assert_fail(&format!(
            "source reaching a forbidden module via `{label}` — full qualification and \
             submodule-prefixed paths must not bypass the INV-2 source denylist (the exact \
             FQ bypass class INV-1 closed in T007; the lint needs check_fq_crate over \
             src/source with prefix-robust patterns)"
        ));
    }
}

#[test]
fn source_fq_body_four_segment_leaf_evasion_fails() {
    let tree = base_tree();
    tree.write(
        "src/source/probe.rs",
        "pub fn probe() -> bool {\n    \
         matches!(crate::config::target::TemplateOptIn::Disabled, _)\n}\n",
    );
    tree.check().assert_fail(
        "source reaching the banned TemplateOptIn leaf via a FOUR-segment fully-qualified \
         body path (`crate::config::target::TemplateOptIn::Disabled`, no use statement) — \
         check_fq_crate's first-three-segments truncation yields `crate::config::target`, \
         which matches neither TemplateOptIn deny pattern, so the banned leaf slips through \
         exactly one submodule level deeper than the pinned patterns reach",
    );
}

#[test]
fn source_module_import_laundering_of_banned_leaf_fails() {
    let tree = base_tree();
    tree.write(
        "src/source/probe.rs",
        "use crate::config::target;\n\
         pub fn probe() -> bool {\n    matches!(target::TemplateOptIn::Disabled, _)\n}\n",
    );
    tree.check().assert_fail(
        "source importing the bare `crate::config::target` MODULE and naming the banned \
         leaf unqualified in the body (`target::TemplateOptIn`) — the use-scan sees only \
         the module path and the body path is not crate-anchored, so neither scan fires; \
         source has no legitimate need for the submodule path (its config items arrive via \
         the crate::config facade), so the bare module import itself must be denied",
    );
}

#[test]
fn source_importing_deploy_machinery_fails() {
    let tree = base_tree();
    tree.write(
        "src/source/probe.rs",
        "use crate::deploy::deploy_artifact;\npub fn probe() { let _ = deploy_artifact; }\n",
    );
    tree.check().assert_fail(
        "source importing crate::deploy machinery — deploy.rs is target-side apply/journal \
         code, an in-spirit INV-2 breach the denylist must close now rather than waiting \
         for T024 to dissolve deploy into sync (no live src/source file references \
         crate::deploy; this pins the door shut)",
    );
}

fn print_allowlist() -> Option<String> {
    match Command::new(script_path())
        .arg("--print-allowlist")
        .current_dir(manifest())
        .output()
    {
        Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        _ => None,
    }
}

#[test]
fn allowlist_is_exactly_the_phase_scoped_legacy_set() {
    let text = print_allowlist().expect(
        "arch-check.sh --print-allowlist must exit 0 and emit `path<TAB>expiry-task` \
         lines so the approved legacy set is inspectable and cannot silently grow",
    );

    let mut got: Vec<(String, String)> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (path, expiry) = line.split_once('\t').unwrap_or_else(|| {
                panic!("allowlist line is not `path<TAB>expiry-task`: {line:?}")
            });
            (path.trim().to_string(), expiry.trim().to_string())
        })
        .collect();
    got.sort();

    let mut want: Vec<(String, String)> = [
        ("src/deploy.rs", "T024"),
        ("src/store.rs", "T025"),
        ("src/source/archive.rs", "T016"),
        ("src/source/cache.rs", "T016"),
        ("src/source/git.rs", "T016"),
        ("src/source/http.rs", "T016"),
        ("src/source/import.rs", "T016"),
        ("src/source/mod.rs", "T016"),
        ("src/source/worktree.rs", "T016"),
        ("src/sync/transitive.rs", "T029"),
    ]
    .into_iter()
    .map(|(path, expiry)| (path.to_string(), expiry.to_string()))
    .collect();
    want.sort();

    assert_eq!(
        got, want,
        "the arch-check legacy allowlist must be exactly the phase-scoped set, each \
         tied to its expiry task; adding, dropping, or re-annotating an entry (the \
         allowlist may only ever shrink) must fail this pin"
    );
}

#[test]
fn legacy_deploy_exemption_does_not_generalize_to_a_new_file() {
    let tree = base_tree();
    tree.copy_real_src("src/deploy.rs", "src/deploy_leak.rs");
    tree.check()
        .assert_fail("a new non-exempt module performing deploy.rs's target-side I/O");
}

#[test]
fn legacy_store_exemption_does_not_generalize_to_a_new_file() {
    let tree = base_tree();
    tree.copy_real_src("src/store.rs", "src/store_leak.rs");
    tree.check()
        .assert_fail("a new non-exempt module performing store.rs's target-side I/O");
}

fn strip_yaml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (idx, &byte) in bytes.iter().enumerate() {
        match byte {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single
                && !in_double
                && (idx == 0 || bytes[idx - 1] == b' ' || bytes[idx - 1] == b'\t') =>
            {
                return &line[..idx];
            }
            _ => {}
        }
    }
    line
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn ci_run_commands(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut commands = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let code = strip_yaml_comment(lines[idx]);
        let trimmed = code.trim_start();
        let inline = trimmed
            .strip_prefix("- run:")
            .or_else(|| trimmed.strip_prefix("run:"));
        let Some(inline) = inline else {
            idx += 1;
            continue;
        };
        let key_indent = indent_of(code);
        let inline = inline.trim();
        if inline.starts_with('|') || inline.starts_with('>') {
            let mut block = String::new();
            idx += 1;
            while idx < lines.len() {
                let body = lines[idx];
                if body.trim().is_empty() {
                    idx += 1;
                    continue;
                }
                if indent_of(body) <= key_indent {
                    break;
                }
                block.push_str(strip_yaml_comment(body).trim());
                block.push('\n');
                idx += 1;
            }
            commands.push(block);
        } else {
            commands.push(inline.to_string());
            idx += 1;
        }
    }
    commands
}

#[test]
fn ci_run_command_parser_ignores_comment_mentions() {
    let yaml = "\
jobs:
  demo:
    steps:
      # a bare mention of arch-check.sh in a comment must not count
      - run: cargo test
      - run: |
          echo start
          ./scripts/arch-check.sh .
";
    let commands = ci_run_commands(yaml);
    assert!(
        commands.iter().any(|c| c.contains("arch-check.sh")),
        "a real `run:` step invoking arch-check.sh must be detected"
    );
    assert!(
        !commands.iter().any(|c| c.contains("a bare mention")),
        "YAML comments must be stripped from parsed run commands"
    );
}

#[test]
fn ci_workflow_invokes_arch_check() {
    let ci = fs::read_to_string(manifest().join(".github/workflows/ci.yml"))
        .expect(".github/workflows/ci.yml must exist");
    let commands = ci_run_commands(&ci);
    assert!(
        commands.iter().any(|cmd| cmd.contains("arch-check.sh")),
        ".github/workflows/ci.yml must invoke scripts/arch-check.sh from an actual \
         `run:` step from PR1 (a comment mention does not count); parsed run \
         commands were:\n{}",
        commands.join("\n---\n")
    );
}

#[test]
fn projection_as_renamed_std_io_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std::fs as f;\npub fn build() { let _ = f::read(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "projection aliasing std::fs via `use std::fs as f;` and reaching the \
         filesystem through the rename (`f::read`)",
    );
}

#[test]
fn reconcile_as_renamed_std_io_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use std::fs as f;\npub fn reconcile() { let _ = f::read(\"/etc/hosts\"); }\n",
    );
    tree.check().assert_fail(
        "reconcile.rs aliasing std::fs via `use std::fs as f;` and reading through \
         the rename (`f::read`)",
    );
}

#[test]
fn projection_grouped_std_io_import_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std::{fs, collections::BTreeMap};\n\
         pub fn build() -> BTreeMap<String, String> { let _ = fs::read(\"x\"); BTreeMap::new() }\n",
    );
    tree.check().assert_fail(
        "projection pulling std::fs inside a grouped `use std::{fs, collections::BTreeMap};`",
    );
}

#[test]
fn projection_grouped_std_non_io_import_passes() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std::{collections::BTreeMap, fmt};\n\
         pub fn build() -> BTreeMap<String, String> { let _ = fmt::Error; BTreeMap::new() }\n",
    );
    tree.check().assert_pass(
        "projection whose grouped std import lists only non-I/O std modules \
         (`use std::{collections::BTreeMap, fmt};`)",
    );
}

#[test]
fn projection_multiline_std_io_import_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std::\n    fs;\npub fn build() { let _ = fs::read(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "projection importing std::fs via a use statement split across lines \
         (`use std::\\n    fs;`)",
    );
}

#[test]
fn projection_fully_qualified_std_io_call_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "pub fn build() { let _ = std::fs::read(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "projection reaching the filesystem via a fully-qualified `std::fs::read` \
         call with no use statement",
    );
}

#[test]
fn reconcile_fully_qualified_std_io_call_fails() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "pub fn reconcile() { let _ = std::fs::read_to_string(\"/etc/hosts\"); }\n",
    );
    tree.check().assert_fail(
        "reconcile.rs reaching the filesystem via a fully-qualified \
         `std::fs::read_to_string` call with no use statement",
    );
}

#[test]
fn reconcile_super_io_sibling_import_fails() {
    for sibling in ["target", "state"] {
        let tree = base_tree();
        tree.write(
            "src/sync/reconcile.rs",
            &format!("use super::{sibling};\npub fn reconcile() {{ {sibling}::noop(); }}\n"),
        );
        tree.check().assert_fail(&format!(
            "reconcile.rs importing the I/O-bearing sync sibling `super::{sibling}`"
        ));
    }
}

#[test]
fn reconcile_super_sync_model_import_passes() {
    let tree = base_tree();
    tree.write(
        "src/sync/reconcile.rs",
        "use super::model::ChangeSet;\npub fn reconcile() -> ChangeSet { ChangeSet::default() }\n",
    );
    tree.check().assert_pass(
        "reconcile.rs importing the pure sync::model via `use super::model::ChangeSet;`",
    );
}

#[test]
fn inv3_new_top_level_io_file_fails_at_repo_root_scan() {
    let tree = base_tree();
    tree.write(
        "src/evil_leak.rs",
        "pub fn leak() { let _ = std::fs::remove_file(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "a NEW top-level module (src/evil_leak.rs — not sync/cli, not on the legacy \
         allowlist) performing filesystem I/O, scanned CI-style with the repo tree \
         root as the single argument",
    );
}

#[test]
fn inv3_sync_io_file_passes() {
    let tree = base_tree();
    tree.write(
        "src/sync/leak_ok.rs",
        "pub fn helper() { let _ = std::fs::remove_file(\"x\"); }\n",
    );
    tree.check()
        .assert_pass("a new sync/ module performing filesystem I/O (INV-3 permits sync)");
}

#[test]
fn inv3_cli_io_file_passes() {
    let tree = base_tree();
    tree.write(
        "src/cli/leak_ok.rs",
        "pub fn helper() { let _ = std::fs::remove_file(\"x\"); }\n",
    );
    tree.check()
        .assert_pass("a new cli/ module performing filesystem I/O (INV-3 permits cli)");
}

#[test]
fn projection_production_io_after_same_line_cfg_test_import_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "#[cfg(test)] use std::fs;\nuse std::fs;\npub fn build() { let _ = fs::read(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "projection whose production `use std::fs;` follows a same-line \
         `#[cfg(test)] use std::fs;` (the test-gated line must not swallow the \
         production statement)",
    );
}

#[test]
fn projection_forbidden_use_between_string_comment_delimiters_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "const OPEN: &str = \"/*\";\n\
         use std::fs;\n\
         const CLOSE: &str = \"*/\";\n\
         pub fn build() { let _ = (OPEN, CLOSE); }\n",
    );
    tree.check().assert_fail(
        "projection where a real `use std::fs;` sits between a `\"/*\"` and a `\"*/\"` \
         string literal (the literals must not be treated as comment delimiters that \
         erase the forbidden import between them)",
    );
}

#[test]
fn preprocess_failure_exits_two() {
    let tree = base_tree();
    let out = Command::new(script_path())
        .arg(&tree.root)
        .env("PERL5OPT", "-MThisModuleMustNotExist")
        .current_dir(manifest())
        .output()
        .unwrap_or_else(|_| {
            panic!(
                "arch-check.sh must be runnable at {}",
                script_path().display()
            )
        });
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "when the Perl preprocessor cannot start (PERL5OPT forces a missing module), \
         arch-check.sh must abort with exit 2 — a broken preprocessor cannot certify \
         any file as clean, so it must never print preprocessing failures and then \
         exit 0:\n{combined}"
    );
}

#[test]
fn projection_whole_crate_alias_std_io_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std as s;\npub fn build() { let _ = s::fs::read(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "projection renaming the whole `std` crate (`use std as s;`) and reaching the \
         filesystem through the alias chain (`s::fs::read`)",
    );
}

#[test]
fn projection_group_member_alias_std_io_fails() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std::{fs as f};\npub fn build() { let _ = f::read(\"x\"); }\n",
    );
    tree.check().assert_fail(
        "projection renaming a grouped member (`use std::{fs as f};`) and reading \
         through the rename (`f::read`)",
    );
}

#[test]
fn projection_non_io_alias_passes() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "use std::collections as c;\n\
         pub fn build() -> c::BTreeMap<String, String> { c::BTreeMap::new() }\n",
    );
    tree.check().assert_pass(
        "projection aliasing a non-I/O std module (`use std::collections as c;`) and \
         using it (`c::BTreeMap`) — an alias to a permitted module must not be \
         mistaken for filesystem access",
    );
}

#[test]
fn projection_fully_qualified_non_allowlisted_crate_call_fails() {
    let bypasses = [
        (
            "crate::config",
            "pub fn build() { let _ = crate::config::load(\"x\"); }\n",
        ),
        (
            "crate::source::GitBackend",
            "pub fn build() { let _ = crate::source::GitBackend::new(\"x\"); }\n",
        ),
        (
            "crate::sync",
            "pub fn build() { let _ = crate::sync::plan::plan_target(\"x\"); }\n",
        ),
        (
            "crate::store",
            "pub fn build() { let _ = crate::store::guard_git_fork(\"x\"); }\n",
        ),
    ];
    for (label, body) in bypasses {
        let tree = base_tree();
        tree.write("src/projection/build.rs", body);
        tree.check().assert_fail(&format!(
            "projection reaching a NON-allowlisted crate module via a fully-qualified body \
             call (`{label}::…`) with no use statement — full qualification must not bypass \
             the positive use-allowlist (INV-1 fully-qualified crate-path bypass)"
        ));
    }
}

#[test]
fn projection_whitespace_separated_fq_path_fails() {
    let bypasses = [
        (
            "crate :: sync (spaces around ::)",
            "pub fn build() { let _ = crate :: sync :: foo(); }\n",
        ),
        (
            "crate ::\\n config (newline-split ::)",
            "pub fn build() { let _ = crate ::\n    config :: load(\"x\"); }\n",
        ),
    ];
    for (label, body) in bypasses {
        let tree = base_tree();
        tree.write("src/projection/build.rs", body);
        tree.check().assert_fail(&format!(
            "projection reaching a NON-allowlisted crate module via a fully-qualified body \
             call whose `::` separators carry surrounding whitespace (`{label}`) — legal Rust \
             spacing/newlines around path separators must not bypass the INV-1 positive \
             use-allowlist"
        ));
    }
}

#[test]
fn projection_fully_qualified_allowlisted_kernel_identity_passes() {
    let tree = base_tree();
    tree.write(
        "src/projection/build.rs",
        "pub fn build() -> crate::kernel::TargetName {\n    \
         let _err: Option<crate::error::Error> = None;\n    \
         let _path: Option<crate::source::SourcePath> = None;\n    \
         crate::kernel::TargetName::from(\"x\")\n}\n",
    );
    tree.check().assert_pass(
        "projection referencing ALLOWLISTED leaves via fully-qualified body paths with no use \
         statement — a kernel identity (`crate::kernel::TargetName`), the crate error type \
         (`crate::error::Error`), and a pure source value type (`crate::source::SourcePath`), each \
         on projection_use_ok's positive allowlist — closing the FQ bypass must not over-reject the \
         same leaf allowlist the use-scan already permits",
    );
}

#[test]
fn arch_check_real_tree_root_scan_stays_green() {
    run_arch_check(&manifest()).assert_pass(
        "`bash scripts/arch-check.sh .` scanning the real repository tree — however the \
         INV-1 fully-qualified `crate::kernel::safe_relpath` bypass in projection/take.rs is \
         resolved (relocate the pure helper into projection, or extend the body-scan and admit \
         the leaf), the live tree must remain exit 0; safe_relpath itself must not be pinned \
         forbidden",
    );
}

fn print_infra() -> Option<String> {
    match Command::new(script_path())
        .arg("--print-infra")
        .current_dir(manifest())
        .output()
    {
        Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        _ => None,
    }
}

#[test]
fn legacy_infra_grandfather_set_is_pinned_exactly() {
    let text = print_infra().expect(
        "arch-check.sh --print-infra must exit 0 and emit one grandfathered-infra path \
         per line so the LEGACY_INFRA set is inspectable and cannot silently grow",
    );

    let mut got: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    got.sort();

    let want = vec!["src/main.rs".to_string()];

    assert_eq!(
        got, want,
        "after T011 moved src/archive.rs and src/http.rs into src/source/, the LEGACY_INFRA \
         grandfather set must be exactly src/main.rs; re-adding an entry (the set may only \
         ever shrink) must fail this pin"
    );
}
