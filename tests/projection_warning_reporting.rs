//! A projection warning reaches the report once, including when a gate stops the deploy.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use phora::config::Config;
use phora::projection::diagnostic::ProjectionWarning;
use phora::source::GitBackend;
use phora::sync::state::FileStateStore;
use phora::sync::{
    Concurrency, ConflictPolicy, HookPolicy, LockSet, MovedPinPolicy, PrunePolicy, SourcePolicy,
    SyncOptions, SyncReport, SyncRequest, SyncWarning,
};

mod common;

fn git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sync_with_unmatched_take(pre_deploy: Option<&str>) -> (TempDir, SyncReport) {
    let root = TempDir::new().expect("fixture root");
    let origin = root.path().join("origin");
    std::fs::create_dir_all(origin.join("editor")).expect("create source tree");
    std::fs::write(origin.join("editor/init.lua"), b"-- init\n").expect("write source file");
    git(&origin, &["init", "-q", "-b", "main"]);
    git(
        &origin,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&origin, &["config", "user.name", "Fixture"]);
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-qm", "seed"]);

    let hooks = pre_deploy.map_or_else(String::new, |command| {
        format!("\n[targets.home.hooks]\npre_deploy = \"{command}\"\n")
    });
    let config = Config::parse(&format!(
        "version = 1\n\
         [sources.editor]\n\
         git = \"{}\"\n\
         branch = \"main\"\n\n\
         [targets.home]\n\
         path = \"{}\"\n\n\
         [targets.home.sources]\n\
         editor = {{ take = [\"editor/init.lua\", \"*.nomatch\"] }}\n\
         {hooks}",
        origin.display(),
        root.path().join("home").display()
    ))
    .expect("fixture config parses");
    config.validate().expect("fixture config validates");

    let registry = FileStateStore::open(root.path().join("state")).expect("open registry");
    let backend = GitBackend::new(root.path().join("cache"));
    let report = phora::sync::sync(
        &SyncRequest {
            base_config: &config,
            local_config: None,
            locks: LockSet::default(),
            options: SyncOptions {
                source_policy: SourcePolicy::Refresh,
                conflict_policy: ConflictPolicy::Refuse,
                prune_policy: PrunePolicy::KeepOrphans,
                hook_policy: if pre_deploy.is_some() {
                    HookPolicy::All
                } else {
                    HookPolicy::None
                },
                moved_pin_policy: MovedPinPolicy::Seal,
                concurrency: Concurrency::default(),
            },
            resolver: None,
            sink: phora::sync::progress::SILENT,
            trust_prompt: None,
        },
        &backend,
        &registry,
    )
    .expect("sync returns a report");
    (root, report)
}

fn unmatched_take_patterns(report: &SyncReport) -> Vec<String> {
    report
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            SyncWarning::Projection(ProjectionWarning::TakeNoMatchGlob(pattern)) => {
                Some(pattern.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn an_unmatched_take_pattern_is_reported_exactly_once() {
    let (_root, report) = sync_with_unmatched_take(None);
    assert_eq!(
        unmatched_take_patterns(&report),
        vec!["*.nomatch".to_owned()],
        "the warning must reach the report once, not once per collection: {:?}",
        report.warnings
    );
}

#[test]
fn a_pre_deploy_gate_does_not_swallow_the_projection_warning() {
    let (_root, report) = sync_with_unmatched_take(Some("exit 1"));
    assert_eq!(
        unmatched_take_patterns(&report),
        vec!["*.nomatch".to_owned()],
        "an aborted deploy still names the config mistake: {:?}",
        report.warnings
    );
    assert!(
        report.applied.is_empty(),
        "the gate must still stop the deploy: {:?}",
        report.applied
    );
}
