//! INV-9: one terminal record per run, and the phase/artifact events behind it.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use tempfile::TempDir;

use phora::config::Config;
use phora::source::GitBackend;
use phora::sync::progress::{FetchId, FetchOutcome, Phase, ProgressSink, SyncSummary};
use phora::sync::state::FileStateStore;
use phora::sync::{
    AppliedChange, ArtifactId, Concurrency, ConflictPolicy, HookPolicy, LockSet, MovedPinPolicy,
    PrunePolicy, SourcePolicy, SyncOptions, SyncRequest,
};

mod common;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    PhaseStarted(Phase),
    PhaseFinished(Phase),
    ResolvePlanned(usize),
    FetchStarted(String),
    FetchFinished(FetchOutcome),
    ArtifactsPlanned(usize),
    Applied(AppliedChange),
    Unchanged(ArtifactId),
    Finished(SyncSummary),
    Aborted(String),
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn events(&self) -> Vec<Event> {
        self.events.lock().expect("recorder poisoned").clone()
    }

    fn terminals(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| matches!(event, Event::Finished(_) | Event::Aborted(_)))
            .count()
    }

    fn summary(&self) -> SyncSummary {
        self.events()
            .into_iter()
            .find_map(|event| match event {
                Event::Finished(summary) => Some(summary),
                _ => None,
            })
            .expect("a completed run emits a summary")
    }

    fn push(&self, event: Event) {
        self.events.lock().expect("recorder poisoned").push(event);
    }
}

impl ProgressSink for Recorder {
    fn phase_started(&self, phase: Phase) {
        self.push(Event::PhaseStarted(phase));
    }
    fn phase_finished(&self, phase: Phase) {
        self.push(Event::PhaseFinished(phase));
    }
    fn resolve_planned(&self, groups: usize) {
        self.push(Event::ResolvePlanned(groups));
    }
    fn fetch_started(&self, fetch: &FetchId) {
        self.push(Event::FetchStarted(fetch.mirror.clone()));
    }
    fn fetch_finished(&self, _fetch: &FetchId, outcome: FetchOutcome) {
        self.push(Event::FetchFinished(outcome));
    }
    fn artifacts_planned(&self, artifacts: usize) {
        self.push(Event::ArtifactsPlanned(artifacts));
    }
    fn artifact_applied(&self, change: &AppliedChange) {
        self.push(Event::Applied(change.clone()));
    }
    fn artifact_unchanged(&self, artifact: &ArtifactId) {
        self.push(Event::Unchanged(artifact.clone()));
    }
    fn finished(&self, summary: &SyncSummary) {
        self.push(Event::Finished(summary.clone()));
    }
    fn aborted(&self, error: &str) {
        self.push(Event::Aborted(error.to_owned()));
    }
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

struct Fixture {
    _root: TempDir,
    config: Config,
    registry: FileStateStore,
    backend: GitBackend,
}

fn fixture() -> Fixture {
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

    let deploy_root = root.path().join("home");
    let config = Config::parse(&format!(
        "version = 1\n\
         [sources.editor]\n\
         git = \"{}\"\n\
         branch = \"main\"\n\n\
         [targets.home]\n\
         path = \"{}\"\n\n\
         [targets.home.sources]\n\
         editor = {{}}\n",
        origin.display(),
        deploy_root.display()
    ))
    .expect("fixture config parses");
    config.validate().expect("fixture config validates");

    let registry = FileStateStore::open(root.path().join("state")).expect("open registry");
    let backend = GitBackend::new(root.path().join("cache"));
    Fixture {
        _root: root,
        config,
        registry,
        backend,
    }
}

fn options() -> SyncOptions {
    SyncOptions {
        source_policy: SourcePolicy::Refresh,
        conflict_policy: ConflictPolicy::Refuse,
        prune_policy: PrunePolicy::KeepOrphans,
        hook_policy: HookPolicy::None,
        moved_pin_policy: MovedPinPolicy::Seal,
        concurrency: Concurrency::default(),
    }
}

fn run(fixture: &Fixture, sink: &dyn ProgressSink) -> phora::error::Result<()> {
    phora::sync::sync(
        &SyncRequest {
            base_config: &fixture.config,
            local_config: None,
            locks: LockSet::default(),
            options: options(),
            resolver: None,
            sink,
            trust_prompt: None,
        },
        &fixture.backend,
        &fixture.registry,
    )
    .map(|_| ())
}

#[test]
fn a_first_sync_reports_phases_a_fetch_and_one_terminal_record() {
    let fixture = fixture();
    let recorder = Recorder::default();
    run(&fixture, &recorder).expect("first sync succeeds");

    let events = recorder.events();
    for phase in [Phase::Compose, Phase::Resolve, Phase::Project, Phase::Apply] {
        assert!(
            events.contains(&Event::PhaseStarted(phase))
                && events.contains(&Event::PhaseFinished(phase)),
            "{phase:?} must open and close, got:\n{events:#?}"
        );
    }
    assert!(events.contains(&Event::ResolvePlanned(1)), "{events:#?}");
    assert!(events.contains(&Event::ArtifactsPlanned(1)), "{events:#?}");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::FetchStarted(_))),
        "{events:#?}"
    );
    assert!(
        events.contains(&Event::FetchFinished(FetchOutcome::Fetched)),
        "a cold cache must report a real fetch, got:\n{events:#?}"
    );
    assert_eq!(recorder.terminals(), 1);

    let summary = recorder.summary();
    assert_eq!(summary.deployed, 1);
    assert_eq!(summary.unchanged, 0);
    assert_eq!(summary.failures, 0);
    assert_eq!(summary.targets, 1);
}

#[test]
fn a_repeat_sync_reports_the_artifact_unchanged() {
    let fixture = fixture();
    run(&fixture, phora::sync::progress::SILENT).expect("first sync succeeds");

    let recorder = Recorder::default();
    run(&fixture, &recorder).expect("second sync succeeds");

    let events = recorder.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Unchanged(id)
            if id.target == "home" && id.source == "editor")),
        "an already-current artifact must report unchanged, got:\n{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Applied(_))),
        "a no-op sync applies nothing, got:\n{events:#?}"
    );
    assert_eq!(recorder.terminals(), 1);
    assert_eq!(recorder.summary().unchanged, 1);
}

#[test]
fn a_frozen_run_without_a_lock_emits_no_terminal_record_from_the_domain() {
    let fixture = fixture();
    let recorder = Recorder::default();
    let outcome = phora::sync::sync(
        &SyncRequest {
            base_config: &fixture.config,
            local_config: None,
            locks: LockSet::default(),
            options: SyncOptions {
                source_policy: SourcePolicy::Frozen,
                ..options()
            },
            resolver: None,
            sink: &recorder,
            trust_prompt: None,
        },
        &fixture.backend,
        &fixture.registry,
    );

    assert!(outcome.is_err(), "--frozen without a lock must fail");
    assert_eq!(
        recorder.terminals(),
        0,
        "the domain leaves the terminal record to the caller on the error path"
    );
}
