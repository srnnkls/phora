//! Terminal rendering for a sync run.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Mutex;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::sync::progress::{
    ArtifactId, FetchId, FetchOutcome, Phase, ProgressSink, Severity, SyncSummary,
};
use crate::sync::{AppliedChange, SkippedChange, SyncWarning};

const TICK: Duration = Duration::from_millis(80);

/// Whether live bars may draw, resolved once from the environment.
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

const TICKS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";

fn aggregate_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {prefix:>10.cyan.bold}  {bar:25.cyan/8} {pos}/{len}  {wide_msg}",
    )
    .expect("aggregate template compiles")
    .progress_chars("━╸━")
}

fn child_style() -> ProgressStyle {
    ProgressStyle::with_template("               {spinner:.cyan} {wide_msg}")
        .expect("child template compiles")
        .tick_chars(TICKS)
}

fn phase_style() -> ProgressStyle {
    ProgressStyle::with_template("  {prefix:>10.cyan.bold}  {spinner:.cyan} {wide_msg}")
        .expect("phase template compiles")
        .tick_chars(TICKS)
}

#[derive(Default)]
struct Bars {
    phase: Option<ProgressBar>,
    aggregate: Option<ProgressBar>,
    children: HashMap<String, ProgressBar>,
}

/// Draws live bars for the resolve and apply phases and captures the summary.
///
/// Only those two: hooks and both stdin prompts inherit this process's stderr,
/// and all of them run outside that window.
pub(super) struct TtySink {
    multi: MultiProgress,
    bars: Mutex<Bars>,
    summary: Mutex<Option<SyncSummary>>,
    planned_artifacts: Mutex<Option<u64>>,
}

impl TtySink {
    pub(super) fn new(mode: ProgressMode) -> Self {
        let multi = MultiProgress::new();
        if mode == ProgressMode::Quiet {
            multi.set_draw_target(ProgressDrawTarget::hidden());
        }
        Self {
            multi,
            bars: Mutex::new(Bars::default()),
            summary: Mutex::new(None),
            planned_artifacts: Mutex::new(None),
        }
    }

    pub(super) fn summary(&self) -> Option<SyncSummary> {
        self.summary.lock().expect("sink poisoned").clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Bars> {
        self.bars.lock().expect("sink poisoned")
    }

    fn open_phase(&self, label: &str) {
        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(phase_style());
        bar.set_prefix(label.to_owned());
        bar.enable_steady_tick(TICK);
        self.lock().phase = Some(bar);
    }

    fn open_aggregate(&self, label: &str, len: u64) {
        let bar = self.multi.add(ProgressBar::new(len));
        bar.set_style(aggregate_style());
        bar.set_prefix(label.to_owned());
        bar.enable_steady_tick(TICK);
        self.lock().aggregate = Some(bar);
    }

    fn advance(&self, message: &str) {
        if let Some(bar) = self.lock().aggregate.as_ref() {
            bar.inc(1);
            bar.set_message(message.to_owned());
        }
    }
}

fn artifact_label(change: &AppliedChange) -> String {
    let (verb, source, artifact) = match change {
        AppliedChange::Deployed {
            source, artifact, ..
        } => ("+", source, artifact),
        AppliedChange::Overwritten {
            source, artifact, ..
        }
        | AppliedChange::OverlayRewritten {
            source, artifact, ..
        } => ("~", source, artifact),
        AppliedChange::Ejected {
            source, artifact, ..
        } => ("=", source, artifact),
        AppliedChange::Removed {
            source, artifact, ..
        } => ("-", source, artifact),
    };
    format!("{verb} {source}/{artifact}")
}

impl ProgressSink for TtySink {
    fn phase_started(&self, phase: Phase) {
        match phase {
            Phase::Resolve => {}
            Phase::Apply => {
                let len = self.planned_artifacts.lock().expect("sink poisoned").take();
                self.open_aggregate("Deploying", len.unwrap_or(0));
            }
            Phase::Build => self.open_phase("Building"),
            Phase::Compose => self.open_phase("Composing"),
            Phase::Project => self.open_phase("Projecting"),
            Phase::Observe => self.open_phase("Checking"),
            Phase::Prune => self.open_phase("Pruning"),
            Phase::Hooks => self.open_phase("Hooks"),
        }
    }

    fn phase_finished(&self, _phase: Phase) {
        let mut bars = self.lock();
        if let Some(bar) = bars.phase.take() {
            bar.finish_and_clear();
        }
        if let Some(bar) = bars.aggregate.take() {
            bar.finish_and_clear();
        }
        for (_, bar) in bars.children.drain() {
            bar.finish_and_clear();
        }
    }

    fn resolve_planned(&self, groups: usize) {
        self.open_aggregate("Resolving", groups as u64);
    }

    fn fetch_started(&self, fetch: &FetchId) {
        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(child_style());
        bar.set_message(fetch.sources.join(", "));
        bar.enable_steady_tick(TICK);
        self.lock().children.insert(fetch.mirror.clone(), bar);
    }

    fn fetch_finished(&self, fetch: &FetchId, outcome: FetchOutcome) {
        if let Some(bar) = self.lock().children.remove(&fetch.mirror) {
            bar.finish_and_clear();
        }
        let verb = match outcome {
            FetchOutcome::Cached => "cached",
            FetchOutcome::Fetched => "fetched",
            FetchOutcome::Failed => "failed",
        };
        self.advance(&format!("{verb} {}", fetch.sources.join(", ")));
    }

    fn artifacts_planned(&self, artifacts: usize) {
        *self.planned_artifacts.lock().expect("sink poisoned") = Some(artifacts as u64);
    }

    fn artifact_applied(&self, change: &AppliedChange) {
        self.advance(&artifact_label(change));
    }

    fn artifact_skipped(&self, change: &SkippedChange) {
        let (source, artifact) = match change {
            SkippedChange::Conflict {
                source, artifact, ..
            }
            | SkippedChange::Failed {
                source, artifact, ..
            }
            | SkippedChange::ReadonlyOverlayRewrite {
                source, artifact, ..
            } => (source, artifact),
        };
        self.advance(&format!("! {source}/{artifact}"));
    }

    fn artifact_unchanged(&self, artifact: &ArtifactId) {
        self.advance(&format!("= {}/{}", artifact.source, artifact.artifact));
    }

    fn warning(&self, _warning: &SyncWarning) {}

    fn diagnostic(&self, severity: Severity, message: &str) {
        let prefix = match severity {
            Severity::Warn => "phora:",
            Severity::Error => "phora: error:",
        };
        let _ = self.multi.println(format!("{prefix} {message}"));
    }

    fn finished(&self, summary: &SyncSummary) {
        *self.summary.lock().expect("sink poisoned") = Some(summary.clone());
    }

    fn aborted(&self, _error: &str) {
        let _ = self.multi.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_sink_draws_nothing() {
        let sink = TtySink::new(ProgressMode::Quiet);
        assert!(sink.multi.is_hidden());
        sink.resolve_planned(3);
        sink.fetch_started(&FetchId {
            mirror: "m".to_owned(),
            sources: vec!["editor".to_owned()],
        });
        sink.fetch_finished(
            &FetchId {
                mirror: "m".to_owned(),
                sources: vec!["editor".to_owned()],
            },
            FetchOutcome::Cached,
        );
        sink.phase_finished(Phase::Resolve);
    }

    #[test]
    fn the_summary_survives_the_run() {
        let sink = TtySink::new(ProgressMode::Quiet);
        assert!(sink.summary().is_none());
        sink.finished(&SyncSummary {
            deployed: 2,
            ..SyncSummary::default()
        });
        assert_eq!(sink.summary().expect("summary captured").deployed, 2);
    }

    #[test]
    fn a_finished_phase_drops_every_bar() {
        let sink = TtySink::new(ProgressMode::Quiet);
        sink.resolve_planned(2);
        sink.fetch_started(&FetchId {
            mirror: "m".to_owned(),
            sources: vec!["a".to_owned()],
        });
        assert_eq!(sink.lock().children.len(), 1);
        sink.phase_finished(Phase::Resolve);
        let bars = sink.lock();
        assert!(bars.aggregate.is_none() && bars.phase.is_none() && bars.children.is_empty());
    }

    #[test]
    fn artifact_labels_carry_the_change_verb() {
        let deployed = AppliedChange::Deployed {
            target: "home".to_owned(),
            source: "editor".to_owned(),
            artifact: "init.lua".to_owned(),
        };
        assert_eq!(artifact_label(&deployed), "+ editor/init.lua");
        let removed = AppliedChange::Removed {
            target: "home".to_owned(),
            source: "editor".to_owned(),
            artifact: "old.lua".to_owned(),
            reason: crate::sync::model::RemovalReason::Pruned,
        };
        assert_eq!(artifact_label(&removed), "- editor/old.lua");
    }
}
