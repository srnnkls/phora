//! The sole producer of user-facing CLI output (`println!`/`format!`).

use std::fmt::Write;

use crate::config::ParsedSource;
use crate::deploy::ArtifactState;
use crate::error::{Error, Result};
use crate::sync::{HookOutcome, HookScope, HookStatus, SyncState};

use super::query::{
    CheckMatchReport, PreviewPlan, SourceResolution, SourceRow, SourceSummary, TargetDetail,
    TargetListing, TargetRow, WhereMatch,
};

pub(crate) fn render_hook_report(outcomes: &[HookOutcome]) -> String {
    let mut out = String::new();
    for outcome in outcomes {
        let scope = match outcome.scope {
            HookScope::OnChange => "on_change",
            HookScope::PostSync => "post_sync",
        };
        let status = match outcome.status {
            HookStatus::Success => "ok",
            HookStatus::Failure => "FAILED",
        };
        let _ = writeln!(
            out,
            "hook {} [{scope}] `{}` {status}",
            outcome.hook_id, outcome.command
        );
    }
    out
}

pub(super) fn print_listings(listings: &[TargetListing]) {
    for listing in listings {
        println!("{}:", listing.target);
        for artifact in &listing.artifacts {
            println!(
                "  {}/{}  {}",
                artifact.source, artifact.artifact, artifact.state
            );
        }
    }
}

pub(super) fn print_verify(mismatches: &[crate::sync::VerifyMismatch]) {
    use crate::sync::VerifyReason;
    if mismatches.is_empty() {
        println!("all verified");
        return;
    }
    for m in mismatches {
        let reason = match &m.reason {
            VerifyReason::Missing => "missing".to_owned(),
            VerifyReason::ContentMismatch { .. } => "content mismatch".to_owned(),
        };
        println!(
            "{}/{}: {} ({reason})",
            m.key.source,
            m.key.artifact,
            m.path.display()
        );
    }
}

pub(super) fn print_where_matches(matches: &[WhereMatch]) {
    for m in matches {
        let commit = m.commit.get(..8).unwrap_or(&m.commit);
        println!(
            "Artifact: {}/{} (commit {commit}, digest {})",
            m.source, m.artifact, m.digest
        );
        for target in &m.targets {
            println!("  - {target}");
        }
    }
}

pub(super) fn print_check_match(source: &ParsedSource, path: &str, report: &CheckMatchReport) {
    let artifact = path.split('/').next().unwrap_or(path);
    println!(
        "artifact `{artifact}`: {}",
        allow_label(report.artifact_allowed)
    );
    println!("path `{path}`: {}", allow_label(report.path_allowed));
    println!("include: {:?}", source.includes());
    println!("exclude: {:?}", source.excludes());
}

fn allow_label(allowed: bool) -> &'static str {
    if allowed { "allowed" } else { "excluded" }
}

pub(super) fn print_source_rows(rows: &[SourceRow]) {
    for row in rows {
        println!("{}  {}  {}", row.name, row.remote, row.refspec);
    }
}

pub(super) fn print_source_summary(summary: &SourceSummary) {
    println!("{}", summary.name);
    println!("  remote: {}", summary.remote);
    println!("  refspec: {}", summary.refspec);
    if summary.targets.is_empty() {
        println!("  deployed to: (none)");
    } else {
        println!("  deployed to: {}", summary.targets.join(", "));
    }
}

pub(super) fn print_target_rows(rows: &[TargetRow]) {
    for row in rows {
        let SourceResolution::Explicit(names) = &row.resolution;
        let sources = names.join(", ");
        println!("{}  {}  [{sources}]", row.name, row.path);
    }
}

pub(super) fn print_target_detail(detail: &TargetDetail) {
    println!("{}", detail.name);
    println!("  path: {}", detail.path);
    println!("  sources: {}", detail.bound_sources.join(", "));
    for artifact in &detail.artifacts {
        println!(
            "  {}/{}  {}",
            artifact.source, artifact.artifact, artifact.state
        );
    }
}

pub(super) fn print_source_removed(name: &str) {
    println!("Removed source '{name}'");
}

pub(super) fn print_target_added(name: &str, path: &str) {
    println!("Added target '{name}': {path}");
}

pub(super) fn print_target_removed(name: &str) {
    println!("Removed target '{name}'");
}

pub(super) fn print_added_to_default(name: &str, description: &str) {
    println!("Added source '{name}': {description}");
    println!("  bound to default");
}

pub(super) fn print_added_declared(name: &str, description: &str) {
    println!("Added source '{name}': {description}");
    println!("  declared only; bind it with `phora bind {name} --to <target>`");
}

pub(super) fn print_added_and_bound(name: &str, description: &str, targets: &[String]) {
    println!("Added source '{name}': {description}");
    println!("  bound to {}", targets.join(", "));
}

pub(super) fn print_bound(sources: &[String], target: &str) {
    println!("Bound {} to '{target}'", sources.join(", "));
}

pub(super) fn print_bind_unchanged(sources: &[String], target: &str) {
    println!(
        "Bindings in '{target}' already up to date: {}",
        sources.join(", ")
    );
}

pub(super) fn print_unbound(sources: &[String], target: &str) {
    println!("Unbound {} from '{target}'", sources.join(", "));
}

pub(super) fn warn_target_rm_deployed(name: &str) {
    eprintln!(
        "phora: target `{name}` still has deployed artifacts registered; \
         run `phora sync --prune` to remove them"
    );
}

pub(super) fn warn_unbind_tombstone(target: &str) {
    eprintln!("phora: {}", super::bind::unbind_tombstone_warning(target));
}

/// Renders the preview plan as an indented per-target tree for terminal output.
#[must_use]
pub(crate) fn render_preview_tree(plan: &PreviewPlan) -> String {
    let mut out = String::new();
    for tp in &plan.targets {
        let _ = writeln!(out, "{}", tp.target);
        for entry in &tp.entries {
            match entry.state {
                SyncState::Synced => render_synced_entry(&mut out, entry),
                SyncState::NotLocked => {
                    let _ = writeln!(out, "  {} — not locked", entry.identity);
                }
                SyncState::NeedsSync => {
                    let _ = writeln!(out, "  {} — needs sync", entry.identity);
                }
                SyncState::LinkWorkingTreeGone => {
                    let _ = writeln!(out, "  {} — link working tree gone", entry.identity);
                }
            }
        }
        for collision in &tp.collisions {
            let _ = writeln!(
                out,
                "  collision: {} from {}",
                collision.artifact,
                collision.sources.join(", ")
            );
        }
    }
    out
}

fn render_synced_entry(out: &mut String, entry: &crate::sync::PreviewEntry) {
    let line = if entry.commit == "link" {
        format!("{}@link", entry.identity)
    } else {
        let short = entry.commit.get(..8).unwrap_or(&entry.commit);
        format!("{}@{short}", entry.identity)
    };
    let _ = writeln!(
        out,
        "  {line} {} -> {}",
        entry.artifact,
        entry.destination.display()
    );
    for file in &entry.files {
        let _ = writeln!(out, "    {}", file.display());
    }
}

/// Returns the preview plan as pretty-printed JSON.
///
/// # Errors
/// Errors if serialization fails.
pub(crate) fn render_preview_json(plan: &PreviewPlan) -> Result<String> {
    serde_json::to_string_pretty(&PreviewPlanJson {
        targets: &plan.targets,
    })
    .map_err(|e| Error::Sync(format!("serialize preview json: {e}")))
}

#[derive(serde::Serialize)]
struct PreviewPlanJson<'a> {
    targets: &'a [crate::sync::PreviewTargetPlan],
}

pub(super) fn state_label(state: &ArtifactState) -> &'static str {
    match state {
        ArtifactState::Clean => "✓ clean",
        ArtifactState::Outdated => "outdated",
        ArtifactState::Modified { .. } => "modified",
        ArtifactState::Foreign => "foreign",
        ArtifactState::Missing => "missing",
        ArtifactState::Ejected => "ejected",
        ArtifactState::Linked => "linked",
    }
}
