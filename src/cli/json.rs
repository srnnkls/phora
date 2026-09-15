//! NDJSON rendering of a sync run, one record per line on stdout.

use std::io::{BufWriter, Stdout, Write};
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::sync::progress::{
    ArtifactId, FetchId, FetchOutcome, Phase, ProgressSink, Severity, SyncSummary,
};
use crate::sync::{AppliedChange, SkippedChange, SyncWarning};

/// Writes one JSON object per line to stdout.
pub(super) struct JsonSink {
    out: Mutex<BufWriter<Stdout>>,
}

impl JsonSink {
    pub(super) fn new() -> Self {
        Self {
            out: Mutex::new(BufWriter::new(std::io::stdout())),
        }
    }

    pub(super) fn flush(&self) {
        let mut out = self.out.lock().expect("json sink poisoned");
        let _ = out.flush();
    }

    fn emit(&self, record: &Value) {
        let mut out = self.out.lock().expect("json sink poisoned");
        let _ = writeln!(out, "{record}");
    }
}

fn applied_parts(change: &AppliedChange) -> (&'static str, &str, &str, &str) {
    match change {
        AppliedChange::Deployed {
            target,
            source,
            artifact,
        } => ("deployed", target, source, artifact),
        AppliedChange::Overwritten {
            target,
            source,
            artifact,
        } => ("overwritten", target, source, artifact),
        AppliedChange::OverlayRewritten {
            target,
            source,
            artifact,
        } => ("overlay_rewritten", target, source, artifact),
        AppliedChange::Ejected {
            target,
            source,
            artifact,
        } => ("ejected", target, source, artifact),
        AppliedChange::Removed {
            target,
            source,
            artifact,
            ..
        } => ("removed", target, source, artifact),
    }
}

fn skipped_parts(change: &SkippedChange) -> (&'static str, &str, &str, &str, Option<&str>) {
    match change {
        SkippedChange::Conflict {
            target,
            source,
            artifact,
            ..
        } => ("conflict", target, source, artifact, None),
        SkippedChange::Failed {
            target,
            source,
            artifact,
            message,
        } => ("failed", target, source, artifact, Some(message.as_str())),
        SkippedChange::ReadonlyOverlayRewrite {
            target,
            source,
            artifact,
        } => ("readonly_overlay_rewrite", target, source, artifact, None),
    }
}

fn warning_kind(warning: &SyncWarning) -> &'static str {
    match warning {
        SyncWarning::Projection(_) => "projection",
        SyncWarning::MalformedTransitiveHooks { .. } => "malformed_transitive_hooks",
        SyncWarning::LinkPathNotPortable { .. } => "link_path_not_portable",
        SyncWarning::ReferenceMoved { .. } => "reference_moved",
        SyncWarning::OrphanedRecords { .. } => "orphaned_records",
        SyncWarning::PruneSkippedAfterFailures => "prune_skipped_after_failures",
        SyncWarning::PruneRefused { .. } => "prune_refused",
        SyncWarning::OrphanRecordPathUnknown { .. } => "orphan_record_path_unknown",
        SyncWarning::FastForwardKeptLive { .. } => "fast_forward_kept_live",
        SyncWarning::FastForwardDropped { .. } => "fast_forward_dropped",
        SyncWarning::CrossDeviceFallback { .. } => "cross_device_fallback",
        SyncWarning::ConflictModified { .. } => "conflict_modified",
        SyncWarning::ConflictForeign { .. } => "conflict_foreign",
        SyncWarning::UntrustedTransitiveHooks { .. } => "untrusted_transitive_hooks",
        SyncWarning::HistoryContentFilter { .. } => "history_content_filter",
    }
}

fn fetch_outcome_label(outcome: FetchOutcome) -> &'static str {
    match outcome {
        FetchOutcome::Cached => "cached",
        FetchOutcome::Fetched => "fetched",
        FetchOutcome::Failed => "failed",
    }
}

fn summary_record(summary: &SyncSummary) -> Value {
    json!({
        "type": "summary",
        "targets": summary.targets,
        "deployed": summary.deployed,
        "overwritten": summary.overwritten,
        "overlays_rewritten": summary.overlays_rewritten,
        "ejected": summary.ejected,
        "removed": summary.removed,
        "unchanged": summary.unchanged,
        "conflicts": summary.conflicts,
        "failures": summary.failures,
        "hooks_run": summary.hooks_run,
        "hooks_failed": summary.hooks_failed,
        "elapsed_ms": summary.elapsed.as_millis(),
    })
}

impl ProgressSink for JsonSink {
    fn phase_started(&self, phase: Phase) {
        self.emit(&json!({ "type": "phase_started", "phase": phase.label() }));
    }

    fn phase_finished(&self, phase: Phase) {
        self.emit(&json!({ "type": "phase_finished", "phase": phase.label() }));
    }

    fn resolve_planned(&self, groups: usize) {
        self.emit(&json!({ "type": "resolve_planned", "mirror_groups": groups }));
    }

    fn fetch_started(&self, fetch: &FetchId) {
        self.emit(&json!({
            "type": "fetch_started",
            "mirror": fetch.mirror,
            "sources": fetch.sources,
        }));
    }

    fn fetch_finished(&self, fetch: &FetchId, outcome: FetchOutcome) {
        self.emit(&json!({
            "type": "fetch_finished",
            "mirror": fetch.mirror,
            "sources": fetch.sources,
            "outcome": fetch_outcome_label(outcome),
        }));
    }

    fn artifacts_planned(&self, artifacts: usize) {
        self.emit(&json!({ "type": "artifacts_planned", "artifacts": artifacts }));
    }

    fn artifact_applied(&self, change: &AppliedChange) {
        let (change_kind, target, source, artifact) = applied_parts(change);
        self.emit(&json!({
            "type": "artifact_applied",
            "change": change_kind,
            "target": target,
            "source": source,
            "artifact": artifact,
        }));
    }

    fn artifact_skipped(&self, change: &SkippedChange) {
        let (reason, target, source, artifact, message) = skipped_parts(change);
        self.emit(&json!({
            "type": "artifact_skipped",
            "reason": reason,
            "target": target,
            "source": source,
            "artifact": artifact,
            "message": message,
        }));
    }

    fn artifact_unchanged(&self, artifact: &ArtifactId) {
        self.emit(&json!({
            "type": "artifact_unchanged",
            "target": artifact.target,
            "source": artifact.source,
            "artifact": artifact.artifact,
        }));
    }

    fn hook_finished(&self, outcome: &crate::sync::HookOutcome) {
        self.emit(&json!({
            "type": "hook_finished",
            "hook_id": outcome.hook_id,
            "command": outcome.command,
            "scope": match outcome.scope {
                crate::sync::HookScope::PreSync => "pre_sync",
                crate::sync::HookScope::PreDeploy => "pre_deploy",
                crate::sync::HookScope::OnChange => "on_change",
                crate::sync::HookScope::PostSync => "post_sync",
            },
            "status": match outcome.status {
                crate::sync::HookStatus::Success => "ok",
                crate::sync::HookStatus::Failure => "failed",
            },
        }));
    }

    fn warning(&self, warning: &SyncWarning) {
        self.emit(&json!({
            "type": "warning",
            "kind": warning_kind(warning),
            "message": super::render::format_sync_warning(warning),
        }));
    }

    fn diagnostic(&self, severity: Severity, message: &str) {
        self.emit(&json!({
            "type": "diagnostic",
            "severity": match severity {
                Severity::Warn => "warn",
                Severity::Error => "error",
            },
            "message": message,
        }));
    }

    fn finished(&self, summary: &SyncSummary) {
        self.emit(&summary_record(summary));
        self.flush();
    }

    fn aborted(&self, error: &str) {
        self.emit(&json!({ "type": "aborted", "error": error }));
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_applied_variant_has_a_distinct_change_label() {
        let one = |change| applied_parts(change).0;
        let named = |target: &str, source: &str, artifact: &str| {
            (target.to_owned(), source.to_owned(), artifact.to_owned())
        };
        let (t, s, a) = named("home", "editor", "init.lua");
        let labels = [
            one(&AppliedChange::Deployed {
                target: t.clone(),
                source: s.clone(),
                artifact: a.clone(),
            }),
            one(&AppliedChange::Overwritten {
                target: t.clone(),
                source: s.clone(),
                artifact: a.clone(),
            }),
            one(&AppliedChange::OverlayRewritten {
                target: t.clone(),
                source: s.clone(),
                artifact: a.clone(),
            }),
            one(&AppliedChange::Ejected {
                target: t.clone(),
                source: s.clone(),
                artifact: a.clone(),
            }),
            one(&AppliedChange::Removed {
                target: t,
                source: s,
                artifact: a,
                reason: crate::sync::model::RemovalReason::Pruned,
            }),
        ];
        let unique: std::collections::BTreeSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");
    }

    #[test]
    fn the_summary_record_is_a_flat_object_of_counts() {
        let record = summary_record(&SyncSummary {
            targets: 2,
            deployed: 3,
            unchanged: 4,
            elapsed: std::time::Duration::from_millis(1234),
            ..SyncSummary::default()
        });
        assert_eq!(record["type"], "summary");
        assert_eq!(record["targets"], 2);
        assert_eq!(record["deployed"], 3);
        assert_eq!(record["unchanged"], 4);
        assert_eq!(record["elapsed_ms"], 1234);
        assert!(
            record
                .as_object()
                .expect("object")
                .values()
                .all(|v| !v.is_object() && !v.is_array()),
            "the summary must stay flat for `jq` consumers: {record}"
        );
    }

    #[test]
    fn the_stripped_hook_notice_owns_the_untrusted_hooks_message() {
        let warning = SyncWarning::UntrustedTransitiveHooks { count: 2 };
        assert_eq!(warning_kind(&warning), "untrusted_transitive_hooks");
        assert!(super::super::render::format_sync_warning(&warning).is_none());
    }
}
