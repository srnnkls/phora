//! Human output for a sync run.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use console::Term;

use super::render::{self, Change, Fetched, Palette, RunContext, SyncView, TargetOutcome};
use crate::sync::progress::{
    ArtifactId, CommandRun, CommandSubject, FetchId, FetchOutcome, Phase, ProgressSink, Severity,
    SyncSummary,
};
use crate::sync::{AppliedChange, HookScope, SkippedChange, SyncWarning};

const TICK: Duration = Duration::from_millis(80);
const CACHED_LISTED: usize = 6;

/// Whether the status line may draw, resolved once from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProgressMode {
    Live,
    Quiet,
}

impl ProgressMode {
    /// `CI` is checked separately from the tty: a runner can hand out a pty and
    /// still strip cursor control from its captured log (jdx/mise#9249).
    pub(super) fn resolve(disabled_by_flag: bool) -> Self {
        let env = |key: &str| std::env::var_os(key).is_some_and(|value| !value.is_empty());
        if disabled_by_flag
            || env("PHORA_NO_PROGRESS")
            || env("CI")
            || std::env::var("TERM").is_ok_and(|term| term == "dumb")
            || !std::io::stderr().is_terminal()
        {
            Self::Quiet
        } else {
            Self::Live
        }
    }
}

struct Screen {
    live: bool,
    term: Term,
    view: SyncView,
    status: Option<(String, Instant)>,
    drawn: bool,
}

impl Screen {
    fn print(&mut self, text: &str) {
        self.erase();
        for line in text.lines() {
            let _ = self.term.write_line(line);
        }
        if text.is_empty() {
            let _ = self.term.write_line("");
        }
        self.draw();
    }

    fn set_status(&mut self, text: String, since: Instant) {
        self.status = Some((text, since));
        self.redraw();
    }

    fn clear_status(&mut self) {
        self.status = None;
        self.erase();
    }

    fn redraw(&mut self) {
        self.erase();
        self.draw();
    }

    fn erase(&mut self) {
        if self.drawn {
            let _ = self.term.clear_line();
            self.drawn = false;
        }
    }

    fn draw(&mut self) {
        let Some((text, since)) = self.status.as_ref().filter(|_| self.live) else {
            return;
        };
        let line = self.view.status(text, since.elapsed());
        let width = usize::from(self.term.size().1).saturating_sub(1);
        let _ = self
            .term
            .write_str(&console::truncate_str(&line, width, "…"));
        self.drawn = true;
    }
}

#[derive(Default)]
struct Resolve {
    started: Option<Instant>,
    planned: usize,
    done: usize,
    active: BTreeMap<String, (Vec<String>, Instant)>,
    finished: Vec<(String, Fetched, Duration)>,
}

#[derive(Default)]
struct Deploy {
    started: Option<Instant>,
    planned: usize,
    done: usize,
    targets: BTreeMap<String, TargetOutcome>,
    reported: bool,
}

#[derive(Default)]
struct Run {
    phase: Option<(Phase, Instant)>,
    section: Option<&'static str>,
    resolve: Resolve,
    deploy: Deploy,
    summary: Option<SyncSummary>,
}

/// Nothing else may write to the terminal while the status line is drawn.
pub(super) struct HumanSink {
    view: SyncView,
    screen: Arc<Mutex<Screen>>,
    run: Mutex<Run>,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl HumanSink {
    pub(super) fn new(mode: ProgressMode, context: RunContext) -> Self {
        let live = mode == ProgressMode::Live;
        let view = SyncView::new(Palette::stderr(), context);
        let screen = Arc::new(Mutex::new(Screen {
            live,
            term: Term::stderr(),
            view: view.clone(),
            status: None,
            drawn: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = live.then(|| {
            let screen = Arc::clone(&screen);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(TICK);
                    let mut screen = screen.lock().expect("screen poisoned");
                    if screen.status.is_some() {
                        screen.redraw();
                    }
                }
            })
        });
        let sink = Self {
            view,
            screen,
            run: Mutex::new(Run::default()),
            stop,
            ticker,
        };
        sink.print(&sink.view.header());
        sink
    }

    pub(super) fn summary(&self) -> Option<SyncSummary> {
        self.run().summary.clone()
    }

    fn run(&self) -> MutexGuard<'_, Run> {
        self.run.lock().expect("sink poisoned")
    }

    fn screen(&self) -> MutexGuard<'_, Screen> {
        self.screen.lock().expect("screen poisoned")
    }

    fn print(&self, text: &str) {
        self.screen().print(text);
    }

    fn print_in(&self, section: &'static str, text: &str) {
        let entered = {
            let mut run = self.run();
            let entered = run.section != Some(section);
            run.section = Some(section);
            entered
        };
        if entered {
            self.print(&self.view.section(section));
        }
        self.print(text);
    }

    fn phase_status(&self) {
        let phase = self.run().phase;
        match phase {
            Some((phase, since)) => self
                .screen()
                .set_status(render::status_for_phase(phase).to_owned(), since),
            None => self.screen().clear_status(),
        }
    }

    fn resolve_status(&self, run: &Run) {
        let resolve = &run.resolve;
        let active: Vec<&str> = resolve
            .active
            .values()
            .flat_map(|(sources, _)| sources)
            .map(|source| render::source_name(source))
            .collect();
        let text = render::status_for_resolve(resolve.done, resolve.planned, &active);
        let since = resolve.started.unwrap_or_else(Instant::now);
        self.screen().set_status(text, since);
    }

    fn deploy_status(&self, run: &Run, current: &str) {
        let deploy = &run.deploy;
        let text = render::status_for_deploy(deploy.done, deploy.planned, current);
        let since = deploy.started.unwrap_or_else(Instant::now);
        self.screen().set_status(text, since);
    }

    fn report_sources(&self) {
        let finished = std::mem::take(&mut self.run().resolve.finished);
        if finished.is_empty() {
            return;
        }
        let (cached, notable): (Vec<_>, Vec<_>) = finished
            .into_iter()
            .partition(|(_, how, _)| *how == Fetched::Cached);
        let mut lines: Vec<String> = notable
            .iter()
            .map(|(key, how, elapsed)| self.view.fetched(key, *how, *elapsed))
            .collect();
        if cached.len() > CACHED_LISTED {
            let keys: Vec<String> = cached.into_iter().map(|(key, _, _)| key).collect();
            lines.push(self.view.cached_summary(&keys));
        } else {
            lines.extend(
                cached
                    .iter()
                    .map(|(key, how, elapsed)| self.view.fetched(key, *how, *elapsed)),
            );
        }
        self.print_in("Sources", &lines.join("\n"));
    }

    fn report_targets(&self) {
        let text = {
            let mut run = self.run();
            let deploy = &mut run.deploy;
            if deploy.reported || deploy.started.is_none() {
                return;
            }
            deploy.reported = true;
            self.view.targets(&deploy.targets)
        };
        if !text.is_empty() {
            self.print_in("Targets", &text);
        }
    }

    fn record(&self, change: Change) {
        let mut run = self.run();
        let current = format!("{} {}", change.target, change.artifact);
        run.deploy.done += 1;
        run.deploy
            .targets
            .entry(change.target.clone())
            .or_default()
            .changes
            .push(change);
        self.deploy_status(&run, &current);
    }
}

impl Drop for HumanSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        self.screen().clear_status();
    }
}

impl ProgressSink for HumanSink {
    fn phase_started(&self, phase: Phase) {
        if phase == Phase::Hooks {
            self.report_targets();
        }
        let now = Instant::now();
        {
            let mut run = self.run();
            run.phase = Some((phase, now));
            match phase {
                Phase::Resolve => run.resolve.started = Some(now),
                Phase::Apply => run.deploy.started = run.deploy.started.or(Some(now)),
                _ => {}
            }
        }
        self.phase_status();
    }

    fn phase_finished(&self, phase: Phase) {
        self.run().phase = None;
        self.screen().clear_status();
        if phase == Phase::Resolve {
            self.report_sources();
        }
    }

    fn resolve_planned(&self, groups: usize) {
        let mut run = self.run();
        run.resolve.planned = groups;
        self.resolve_status(&run);
    }

    fn fetch_started(&self, fetch: &FetchId) {
        let mut run = self.run();
        run.resolve.active.insert(
            fetch.mirror.clone(),
            (fetch.sources.clone(), Instant::now()),
        );
        self.resolve_status(&run);
    }

    fn fetch_finished(&self, fetch: &FetchId, outcome: FetchOutcome) {
        let mut run = self.run();
        let resolve = &mut run.resolve;
        let elapsed = resolve
            .active
            .remove(&fetch.mirror)
            .map(|(_, started)| started.elapsed())
            .unwrap_or_default();
        resolve.done += 1;
        let how = match outcome {
            FetchOutcome::Cached => Fetched::Cached,
            FetchOutcome::Fetched => Fetched::Network,
            FetchOutcome::Failed => Fetched::Failed,
        };
        resolve.finished.extend(
            fetch
                .sources
                .iter()
                .map(|source| (source.clone(), how, elapsed)),
        );
        self.resolve_status(&run);
    }

    fn artifacts_planned(&self, artifacts: usize) {
        self.run().deploy.planned = artifacts;
    }

    fn artifact_applied(&self, change: &AppliedChange) {
        self.record(Change::applied(change));
    }

    fn artifact_skipped(&self, change: &SkippedChange) {
        self.record(Change::skipped(change));
    }

    fn artifact_unchanged(&self, artifact: &ArtifactId) {
        let mut run = self.run();
        run.deploy.done += 1;
        run.deploy
            .targets
            .entry(artifact.target.clone())
            .or_default();
        let current = format!("{} {}", artifact.target, artifact.artifact);
        self.deploy_status(&run, &current);
    }

    fn command_started(&self, subject: &CommandSubject, command: &str) {
        let text = render::status_for_command(subject, command);
        self.screen().set_status(text, Instant::now());
    }

    fn command_finished(&self, run: &CommandRun) {
        let section = match run.subject {
            CommandSubject::Build { .. } => "Sources",
            CommandSubject::Hook {
                scope: HookScope::PreSync,
            } => "Pre-sync hooks",
            CommandSubject::Hook {
                scope: HookScope::PreDeploy,
            } => "Pre-deploy hooks",
            CommandSubject::Hook { .. } => "Hooks",
        };
        self.print_in(section, &self.view.command_run(run));
        self.phase_status();
    }

    fn suspend(&self, interact: &mut dyn FnMut()) {
        let mut screen = self.screen();
        screen.erase();
        interact();
        screen.draw();
    }

    fn warning(&self, warning: &SyncWarning) {
        if matches!(warning, SyncWarning::BuildFailed { .. }) {
            self.print(&self.view.build_kept());
        }
    }

    fn diagnostic(&self, severity: Severity, message: &str) {
        self.print(&self.view.diagnostic(severity, message));
    }

    fn finished(&self, summary: &SyncSummary) {
        self.report_targets();
        self.screen().clear_status();
        self.run().summary = Some(summary.clone());
    }

    fn aborted(&self, _error: &str) {
        self.screen().clear_status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> HumanSink {
        HumanSink::new(ProgressMode::Quiet, RunContext::default())
    }

    #[test]
    fn the_summary_survives_the_run() {
        let sink = sink();
        assert!(sink.summary().is_none());
        sink.finished(&SyncSummary {
            deployed: 2,
            ..SyncSummary::default()
        });
        assert_eq!(sink.summary().expect("summary captured").deployed, 2);
    }

    #[test]
    fn resolve_collects_each_source_with_its_outcome() {
        let sink = sink();
        sink.phase_started(Phase::Resolve);
        sink.resolve_planned(2);
        for (mirror, outcome) in [("a", FetchOutcome::Fetched), ("b", FetchOutcome::Cached)] {
            let fetch = FetchId {
                mirror: mirror.to_owned(),
                sources: vec![mirror.to_owned()],
            };
            sink.fetch_started(&fetch);
            sink.fetch_finished(&fetch, outcome);
        }
        let run = sink.run();
        let outcomes: Vec<(&str, Fetched)> = run
            .resolve
            .finished
            .iter()
            .map(|(key, how, _)| (key.as_str(), *how))
            .collect();
        assert_eq!(outcomes, [("a", Fetched::Network), ("b", Fetched::Cached)]);
        assert!(run.resolve.active.is_empty());
    }

    #[test]
    fn targets_are_reported_once_and_group_changes() {
        let sink = sink();
        sink.phase_started(Phase::Apply);
        sink.artifact_applied(&AppliedChange::Deployed {
            target: "home".to_owned(),
            source: "editor".to_owned(),
            artifact: "init.lua".to_owned(),
        });
        sink.artifact_unchanged(&ArtifactId {
            target: "bin".to_owned(),
            source: "tools".to_owned(),
            artifact: "fd".to_owned(),
        });
        sink.phase_started(Phase::Hooks);
        let run = sink.run();
        assert!(run.deploy.reported);
        assert_eq!(run.deploy.targets["home"].changes.len(), 1);
        assert!(run.deploy.targets["bin"].changes.is_empty());
    }
}
