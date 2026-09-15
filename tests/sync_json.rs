//! `phora sync --json`: stdout is NDJSON, closed by exactly one terminal record.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod common;

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: std::path::PathBuf,
    xdg_cache: std::path::PathBuf,
    xdg_state: std::path::PathBuf,
}

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

fn build_fixture(extra_config: &str) -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let src = TempDir::new().expect("src tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");

    let src_path = src.path();
    std::fs::create_dir_all(src_path.join("editor")).expect("create source tree");
    std::fs::write(src_path.join("editor/init.lua"), b"-- init\n").expect("write source file");
    git(src_path, &["init", "-q", "-b", "main"]);
    git(
        src_path,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(src_path, &["config", "user.name", "Fixture"]);
    git(src_path, &["add", "-A"]);
    git(src_path, &["commit", "-qm", "fixture"]);

    let home_path = home.path().to_path_buf();
    let config = format!(
        "version = 1\n\n[sources.dotfiles]\npath = \"{src}\"\nbranch = \"main\"\n\n\
         [targets.home]\npath = \"{target}\"\nsources = [\"dotfiles\"]\nlayout = \"flat\"\n\
         {extra_config}",
        src = src_path.display(),
        target = home_path.join("deploy").display(),
    );
    std::fs::write(cwd.path().join("phora.toml"), config).expect("write config");

    Fixture {
        _home: home,
        _src: src,
        cwd,
        xdg_cache: home_path.join("xdg/cache"),
        xdg_state: home_path.join("xdg/state"),
        home_path,
    }
}

fn run(fixture: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env("XDG_CACHE_HOME", &fixture.xdg_cache)
        .env("XDG_STATE_HOME", &fixture.xdg_state)
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE")
        .output()
        .expect("phora binary runs")
}

fn parse_ndjson(stdout: &[u8]) -> Vec<Value> {
    let text = String::from_utf8(stdout.to_vec()).expect("stdout is utf-8");
    text.lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("stdout line is not JSON ({error}): {line:?}"))
        })
        .collect()
}

fn kinds(records: &[Value]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record["type"].as_str().expect("every record is typed"))
        .collect()
}

#[test]
fn every_stdout_line_is_json_and_the_last_is_the_summary() {
    let fixture = build_fixture("");
    let out = run(&fixture, &["sync", "--json"]);
    assert!(out.status.success(), "sync --json exits 0");

    let records = parse_ndjson(&out.stdout);
    assert!(!records.is_empty(), "the stream must not be empty");
    assert!(
        records
            .iter()
            .all(|record| record.as_object().is_some_and(|o| o.contains_key("type"))),
        "every record needs a `type` discriminator: {records:#?}"
    );

    let kinds = kinds(&records);
    assert_eq!(
        kinds.last(),
        Some(&"summary"),
        "the stream must close on a summary: {kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == "summary" || **k == "aborted")
            .count(),
        1,
        "exactly one terminal record per run: {kinds:?}"
    );
    for expected in ["phase_started", "resolve_planned", "artifact_applied"] {
        assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
    }

    let summary = records.last().expect("summary record");
    assert_eq!(summary["deployed"], 1);
    assert_eq!(summary["targets"], 1);
    assert!(
        summary["elapsed_ms"].is_number(),
        "the summary carries a duration: {summary}"
    );
}

#[test]
fn no_human_text_leaks_onto_the_json_stream() {
    let fixture = build_fixture("");
    let out = run(&fixture, &["sync", "--json"]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    assert!(
        !stdout.contains("sync complete") && !stdout.contains("synced 1 target"),
        "the human completion line must not share the machine stream: {stdout}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "no ANSI may reach the machine stream: {stdout:?}"
    );
}

#[test]
fn a_hook_report_goes_to_stderr_under_json() {
    let fixture = build_fixture("\n[targets.home.hooks]\non_change = \"true\"\n");
    let out = run(&fixture, &["sync", "--json"]);
    assert!(out.status.success(), "sync --json with a hook exits 0");

    let records = parse_ndjson(&out.stdout);
    assert!(
        kinds(&records).contains(&"hook_finished"),
        "the hook outcome belongs in the stream: {:?}",
        kinds(&records)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    assert!(
        !stdout.contains("hook "),
        "the rendered hook report must not corrupt the stream: {stdout}"
    );
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("on_change"),
        "the rendered hook report belongs on stderr: {stderr}"
    );
}

/// A config error predates the run, so it never opens a stream: plain stderr, exit 1.
#[test]
fn a_config_error_reports_on_stderr_without_opening_the_stream() {
    let fixture = build_fixture("\n[targets.home.hooks]\npost_sync = \"exit 3\"\n");
    let out = run(&fixture, &["sync", "--json"]);
    assert!(
        !out.status.success(),
        "an invalid config must exit non-zero"
    );
    assert!(
        out.stdout.is_empty(),
        "a pre-run error emits no partial stream: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("unknown field `post_sync`"),
        "the config error still reaches a human: {stderr}"
    );
}

#[test]
fn a_failed_run_still_closes_the_stream() {
    let fixture = build_fixture("\n[targets.home.hooks]\non_change = \"exit 3\"\n");
    let out = run(&fixture, &["sync", "--json"]);
    assert!(!out.status.success(), "a failing hook must exit non-zero");

    let records = parse_ndjson(&out.stdout);
    let kinds_seen = kinds(&records);
    assert_eq!(
        kinds_seen
            .iter()
            .filter(|k| **k == "summary" || **k == "aborted")
            .count(),
        1,
        "a failing run still closes its stream exactly once: {kinds_seen:?}"
    );
}
