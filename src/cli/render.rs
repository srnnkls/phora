//! The sole producer of user-facing CLI output (`println!`/`format!`).

use std::fmt::Write;

use crate::config::ParsedSource;
use crate::deploy::ArtifactState;
use crate::error::{Error, Result};
use crate::sync::{HookOutcome, HookScope, HookStatus, SyncState};

use super::query::{
    CheckMatchReport, PreviewPlan, SourceResolution, SourceRow, SourceSummary, TargetDetail,
    TargetListing, TargetRow, WhereFilter, WhereMatch,
};

#[must_use]
pub(super) fn render_hook_report(outcomes: &[HookOutcome]) -> String {
    let mut out = String::new();
    for outcome in outcomes {
        let scope = match outcome.scope {
            HookScope::OnChange => "on_change",
            HookScope::PostSync => "post_sync",
        };
        let status = match outcome.status {
            HookStatus::Success => "ok",
            HookStatus::Failure => "failed",
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
    print!("{}", format_listings(listings));
}

#[must_use]
pub(super) fn format_listings(listings: &[TargetListing]) -> String {
    let mut out = String::new();
    if listings.is_empty() {
        let _ = writeln!(out, "No targets configured.");
        return out;
    }
    for listing in listings {
        let _ = writeln!(out, "{}:", listing.target);
        if listing.artifacts.is_empty() {
            let _ = writeln!(out, "  (nothing deployed — run `phora sync`)");
            continue;
        }
        for artifact in &listing.artifacts {
            let _ = writeln!(
                out,
                "  {}/{}  {}",
                artifact.source, artifact.artifact, artifact.state
            );
        }
    }
    out
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

pub(super) fn print_where_matches(matches: &[WhereMatch], filter: &WhereFilter) {
    print!("{}", format_where_matches(matches, filter));
}

#[must_use]
pub(super) fn format_where_matches(matches: &[WhereMatch], filter: &WhereFilter) -> String {
    let mut out = String::new();
    if matches.is_empty() {
        match where_filter_description(filter) {
            Some(desc) => {
                let _ = writeln!(out, "No deployed artifacts match {desc}.");
            }
            None => {
                let _ = writeln!(out, "No deployed artifacts yet.");
            }
        }
        let _ = writeln!(
            out,
            "Run `phora sync` to deploy, or `phora preview` to see the plan."
        );
        return out;
    }
    for m in matches {
        let commit = m.commit.get(..8).unwrap_or(&m.commit);
        let _ = writeln!(
            out,
            "Artifact: {}/{} (commit {commit}, digest {})",
            m.source, m.artifact, m.digest
        );
        for target in &m.targets {
            let _ = writeln!(out, "  - {target}");
        }
    }
    out
}

/// A human phrase for the active `where` filter constraints, or `None` when unfiltered.
fn where_filter_description(filter: &WhereFilter) -> Option<String> {
    let parts: Vec<String> = [
        ("source", filter.source.as_deref()),
        ("artifact", filter.artifact.as_deref()),
        ("commit", filter.commit.as_deref()),
        ("digest", filter.digest.as_deref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.map(|v| format!("{label} `{v}`")))
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
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
        let suffix = if file.templated { " (templated)" } else { "" };
        let _ = writeln!(out, "    {}{suffix}", file.path.display());
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

/// Summarizes what a dep's root `phora.toml` would contribute if imported: its
/// targets (with relative paths), any stripped/inert hooks, and the `imports` +
/// `phora trust` opt-in. Empty for a manifest declaring neither targets nor sources.
#[must_use]
pub(super) fn render_add_contribution(
    name: &str,
    manifest: &crate::config::transitive::TransitiveManifest,
) -> String {
    if manifest.targets.is_empty() && manifest.sources.is_empty() {
        return String::new();
    }

    let hooked: std::collections::BTreeSet<&str> = manifest
        .hooks()
        .and_then(toml::Value::as_table)
        .map(|t| t.keys().map(String::as_str).collect())
        .unwrap_or_default();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Source '{name}' ships a phora.toml that would contribute:"
    );
    for (target, config) in &manifest.targets {
        let _ = writeln!(out, "  [targets.{target}] -> {}", config.path.display());
        if hooked.contains(target.as_str()) {
            let _ = writeln!(
                out,
                "    note: carries a hook, stripped and inert until you `phora trust` it"
            );
        }
    }
    let _ = writeln!(
        out,
        "Opt in with `imports = [\"{name}\"]`, then `phora trust` to admit any hooks."
    );
    out
}

#[cfg(test)]
mod contribution_tests {
    use crate::config::transitive::TransitiveManifest;

    fn manifest(text: &str) -> TransitiveManifest {
        TransitiveManifest::parse(text).expect("dep manifest parses")
    }

    #[test]
    fn contribution_summary_lists_targets_with_relative_paths() {
        let dep = manifest(
            "version = 1\n\n\
             [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
             [targets.editor]\npath = \"config/nvim\"\n\n\
             [targets.shell]\npath = \"config/zsh\"\n",
        );

        let summary = super::render_add_contribution("dots", &dep);

        assert!(
            summary.contains("editor") && summary.contains("config/nvim"),
            "the summary must list target `editor` with its relative path `config/nvim`, got:\n{summary}"
        );
        assert!(
            summary.contains("shell") && summary.contains("config/zsh"),
            "the summary must list target `shell` with its relative path `config/zsh`, got:\n{summary}"
        );
    }

    #[test]
    fn contribution_summary_strips_hooks_and_notes_they_are_inert() {
        let dep = manifest(
            "version = 1\n\n\
             [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
             [targets.editor]\npath = \"config/nvim\"\n\n\
             [targets.editor.hooks]\non_change = \"./install.sh\"\n",
        );

        let summary = super::render_add_contribution("dots", &dep);

        assert!(
            summary.to_lowercase().contains("hook"),
            "a dep declaring a per-target hook must be surfaced as carrying a stripped hook, got:\n{summary}"
        );
        assert!(
            summary.to_lowercase().contains("inert")
                || summary.to_lowercase().contains("trust")
                || summary.to_lowercase().contains("stripped"),
            "the summary must note the hook is stripped/inert until trusted, got:\n{summary}"
        );
        assert!(
            !summary.contains("./install.sh"),
            "the contribution summary must NOT echo the hook command verbatim (it stays opaque/inert), got:\n{summary}"
        );
    }

    #[test]
    fn contribution_summary_suggests_the_imports_and_trust_opt_in() {
        let dep = manifest(
            "version = 1\n\n\
             [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
             [targets.editor]\npath = \"config/nvim\"\n",
        );

        let summary = super::render_add_contribution("dots", &dep);

        assert!(
            summary.contains("imports"),
            "the summary must point at the `imports = [...]` opt-in path, got:\n{summary}"
        );
        assert!(
            summary.contains("trust"),
            "the summary must point at `phora trust` as the explicit opt-in, got:\n{summary}"
        );
    }

    #[test]
    fn contribution_summary_is_empty_for_a_manifest_with_no_targets_or_sources() {
        let dep = manifest("version = 1\n");

        let summary = super::render_add_contribution("dots", &dep);

        assert!(
            !summary.contains("imports") && !summary.to_lowercase().contains("trust"),
            "a non-transitive dep (no targets/sources) must NOT produce a contribution/opt-in summary, got:\n{summary}"
        );
    }
}
