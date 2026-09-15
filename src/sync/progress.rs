//! Observation port for a synchronization run; rendering lives at the CLI edge.

use super::hooks::HookOutcome;
use super::request::{AppliedChange, SkippedChange, SyncWarning};

/// A stage of the run, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Compose,
    Resolve,
    Project,
    /// Runs a second time when a `pre_deploy` hook mutated the workspace.
    Observe,
    Apply,
    Prune,
    Hooks,
}

impl Phase {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Compose => "compose",
            Self::Resolve => "resolve",
            Self::Project => "project",
            Self::Observe => "observe",
            Self::Apply => "apply",
            Self::Prune => "prune",
            Self::Hooks => "hooks",
        }
    }
}

/// One mirror group; sources sharing a mirror resolve serially inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchId {
    pub mirror: String,
    pub sources: Vec<String>,
}

/// Whether a mirror group needed the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    Cached,
    Fetched,
    Failed,
}

/// An artifact's place in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactId {
    pub target: String,
    pub source: String,
    pub artifact: String,
}

/// How loudly a diagnostic should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warn,
    Error,
}

/// Terminal tally for the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub targets: usize,
    pub deployed: usize,
    pub overwritten: usize,
    pub overlays_rewritten: usize,
    pub ejected: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub failures: usize,
    pub sources_cached: usize,
    pub sources_fetched: usize,
    pub hooks_run: usize,
    pub hooks_failed: usize,
    pub elapsed: std::time::Duration,
}

/// Receives run events as they happen.
pub trait ProgressSink: Sync {
    fn phase_started(&self, _phase: Phase) {}
    fn phase_finished(&self, _phase: Phase) {}

    fn resolve_planned(&self, _groups: usize) {}
    fn fetch_started(&self, _fetch: &FetchId) {}
    fn fetch_finished(&self, _fetch: &FetchId, _outcome: FetchOutcome) {}

    fn artifacts_planned(&self, _artifacts: usize) {}
    fn artifact_applied(&self, _change: &AppliedChange) {}
    fn artifact_skipped(&self, _change: &SkippedChange) {}
    fn artifact_unchanged(&self, _artifact: &ArtifactId) {}

    fn hook_started(&self, _hook_id: &str) {}
    fn hook_finished(&self, _outcome: &HookOutcome) {}

    fn warning(&self, _warning: &SyncWarning) {}
    fn diagnostic(&self, _severity: Severity, _message: &str) {}

    fn finished(&self, _summary: &SyncSummary) {}
    fn aborted(&self, _error: &str) {}
}

/// Discards every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct SilentSink;

impl ProgressSink for SilentSink {}

/// The sink used wherever none was supplied.
pub const SILENT: &SilentSink = &SilentSink;

#[cfg(test)]
pub(crate) mod recording {
    use std::sync::Mutex;

    use super::{
        AppliedChange, ArtifactId, FetchId, FetchOutcome, HookOutcome, Phase, ProgressSink,
        Severity, SkippedChange, SyncSummary, SyncWarning,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Event {
        PhaseStarted(Phase),
        PhaseFinished(Phase),
        ResolvePlanned(usize),
        FetchStarted(FetchId),
        FetchFinished(FetchId, FetchOutcome),
        ArtifactsPlanned(usize),
        Applied(AppliedChange),
        Skipped(SkippedChange),
        Unchanged(ArtifactId),
        HookStarted(String),
        HookFinished(HookOutcome),
        Warning(SyncWarning),
        Diagnostic(Severity, String),
        Finished(SyncSummary),
        Aborted(String),
    }

    #[derive(Debug, Default)]
    pub(crate) struct RecordingSink {
        events: Mutex<Vec<Event>>,
    }

    impl RecordingSink {
        pub(crate) fn events(&self) -> Vec<Event> {
            self.events.lock().expect("sink poisoned").clone()
        }

        pub(crate) fn terminal_count(&self) -> usize {
            self.events()
                .iter()
                .filter(|event| matches!(event, Event::Finished(_) | Event::Aborted(_)))
                .count()
        }

        fn push(&self, event: Event) {
            self.events.lock().expect("sink poisoned").push(event);
        }
    }

    impl ProgressSink for RecordingSink {
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
            self.push(Event::FetchStarted(fetch.clone()));
        }

        fn fetch_finished(&self, fetch: &FetchId, outcome: FetchOutcome) {
            self.push(Event::FetchFinished(fetch.clone(), outcome));
        }

        fn artifacts_planned(&self, artifacts: usize) {
            self.push(Event::ArtifactsPlanned(artifacts));
        }

        fn artifact_applied(&self, change: &AppliedChange) {
            self.push(Event::Applied(change.clone()));
        }

        fn artifact_skipped(&self, change: &SkippedChange) {
            self.push(Event::Skipped(change.clone()));
        }

        fn artifact_unchanged(&self, artifact: &ArtifactId) {
            self.push(Event::Unchanged(artifact.clone()));
        }

        fn hook_started(&self, hook_id: &str) {
            self.push(Event::HookStarted(hook_id.to_owned()));
        }

        fn hook_finished(&self, outcome: &HookOutcome) {
            self.push(Event::HookFinished(outcome.clone()));
        }

        fn warning(&self, warning: &SyncWarning) {
            self.push(Event::Warning(warning.clone()));
        }

        fn diagnostic(&self, severity: Severity, message: &str) {
            self.push(Event::Diagnostic(severity, message.to_owned()));
        }

        fn finished(&self, summary: &SyncSummary) {
            self.push(Event::Finished(summary.clone()));
        }

        fn aborted(&self, error: &str) {
            self.push(Event::Aborted(error.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_sink_swallows_every_event() {
        let sink: &dyn ProgressSink = SILENT;
        sink.phase_started(Phase::Apply);
        sink.resolve_planned(3);
        sink.diagnostic(Severity::Warn, "ignored");
        sink.finished(&SyncSummary::default());
    }

    #[test]
    fn phase_labels_are_stable() {
        let labels: Vec<_> = [
            Phase::Compose,
            Phase::Resolve,
            Phase::Project,
            Phase::Observe,
            Phase::Apply,
            Phase::Prune,
            Phase::Hooks,
        ]
        .into_iter()
        .map(Phase::label)
        .collect();
        assert_eq!(
            labels,
            [
                "compose", "resolve", "project", "observe", "apply", "prune", "hooks"
            ]
        );
    }

    #[test]
    fn recording_sink_preserves_order_and_counts_one_terminal() {
        use recording::{Event, RecordingSink};

        let sink = RecordingSink::default();
        sink.phase_started(Phase::Resolve);
        sink.resolve_planned(2);
        sink.phase_finished(Phase::Resolve);
        sink.finished(&SyncSummary::default());

        assert_eq!(
            sink.events(),
            vec![
                Event::PhaseStarted(Phase::Resolve),
                Event::ResolvePlanned(2),
                Event::PhaseFinished(Phase::Resolve),
                Event::Finished(SyncSummary::default()),
            ]
        );
        assert_eq!(sink.terminal_count(), 1);
    }

    #[test]
    fn a_shared_sink_records_from_many_threads() {
        use recording::RecordingSink;

        let sink = RecordingSink::default();
        let shared: &dyn ProgressSink = &sink;
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..16 {
                        shared.phase_started(Phase::Resolve);
                    }
                });
            }
        });
        assert_eq!(sink.events().len(), 128);
    }
}
