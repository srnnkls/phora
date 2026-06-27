//! The sole producer of user-facing CLI output (`println!`/`format!`).

use std::fmt::Write;

use crate::config::ParsedSource;
use crate::deploy::ArtifactState;
use crate::error::{Error, Result};
use crate::sync::{HookOutcome, HookScope, HookStatus, SyncState};

use super::query::{
    CheckMatchReport, ExplainBody, ExplainReport, OfferAttribution, PreviewPlan, SourceResolution,
    SourceRow, SourceSummary, TakeAttribution, TargetDetail, TargetListing, TargetRow, WhereFilter,
    WhereMatch,
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

#[must_use]
pub(crate) fn render_explain(report: &ExplainReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} under {}", report.source, report.target);
    match &report.body {
        ExplainBody::Path { path, offer, take } => {
            render_offer(&mut out, path, offer);
            if let Some(take) = take {
                render_take(&mut out, take);
            }
        }
        ExplainBody::Summary { leaves } => {
            if leaves.is_empty() {
                let _ = writeln!(out, "  offer is empty");
            }
            for leaf in leaves {
                let _ = write!(out, "  {}: ", leaf.leaf);
                render_take_inline(&mut out, &leaf.take);
            }
        }
    }
    for warning in &report.warnings {
        let _ = writeln!(out, "  warning: {warning}");
    }
    out
}

fn render_offer(out: &mut String, path: &str, offer: &OfferAttribution) {
    match offer {
        OfferAttribution::Allowed { include: Some(inc) } => {
            let _ = writeln!(out, "  offer: `{path}` allowed by include `{inc}`");
        }
        OfferAttribution::Allowed { include: None } => {
            let _ = writeln!(out, "  offer: `{path}` allowed by the implicit-full offer");
        }
        OfferAttribution::Vetoed { exclude } => {
            let _ = writeln!(out, "  offer: `{path}` vetoed by exclude `{exclude}`");
        }
        OfferAttribution::Outside { suggestions } => {
            let _ = writeln!(out, "  offer: `{path}` is outside the offer");
            if !suggestions.is_empty() {
                let _ = writeln!(out, "  did you mean: {}", suggestions.join(", "));
            }
        }
    }
}

fn render_take(out: &mut String, take: &TakeAttribution) {
    let _ = write!(out, "  take: ");
    render_take_inline(out, take);
}

fn render_take_inline(out: &mut String, take: &TakeAttribution) {
    match take {
        TakeAttribution::Identity { dest } => {
            let _ = writeln!(out, "kept at `{dest}`");
        }
        TakeAttribution::Renamed { src, dest } => {
            let _ = writeln!(out, "renamed `{src}` -> `{dest}`");
        }
        TakeAttribution::Collapsed { dir } => {
            let _ = writeln!(out, "collapsed into directory `{dir}`");
        }
        TakeAttribution::Dropped => {
            let _ = writeln!(out, "dropped by a narrowing take (not taken)");
        }
    }
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
        for group in &tp.warnings {
            render_binding_warnings(&mut out, group);
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
    let artifact = if let Some(src) = &entry.rename {
        format!("{src} -> {}", entry.artifact)
    } else if entry.collapsed {
        format!("{}/", entry.artifact)
    } else {
        entry.artifact.clone()
    };
    let _ = writeln!(
        out,
        "  {line} {artifact} -> {}",
        entry.destination.display()
    );
    for file in &entry.files {
        let suffix = if file.templated { " (templated)" } else { "" };
        let _ = writeln!(out, "    {}{suffix}", file.path.display());
    }
}

fn render_binding_warnings(out: &mut String, group: &crate::sync::BindingWarnings) {
    use crate::diagnostic::DID_YOU_MEAN;
    use crate::sync::PreviewWarning;
    for warning in &group.warnings {
        match warning {
            PreviewWarning::TakeNoMatch {
                pattern,
                suggestions,
            } => {
                let _ = writeln!(
                    out,
                    "  warning: {} take `{pattern}` matched no offered leaf",
                    group.identity
                );
                if !suggestions.is_empty() {
                    let _ = writeln!(out, "    {DID_YOU_MEAN} {}", suggestions.join(", "));
                }
            }
            PreviewWarning::CollapseBlocked { dir } => {
                let _ = writeln!(
                    out,
                    "  warning: {} `{dir}` could not collapse: a within-dir exclude forced \
                     per-leaf links",
                    group.identity
                );
            }
        }
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
        ArtifactState::Clean | ArtifactState::Revalidated { .. } => "✓ clean",
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
mod preview_render_tests {
    use std::path::PathBuf;

    use super::super::query::PreviewPlan;
    use super::{render_preview_json, render_preview_tree};
    use crate::sync::{
        BindingWarnings, PreviewEntry, PreviewFile, PreviewTargetPlan, PreviewWarning, SyncState,
    };

    fn synced(identity: &str, artifact: &str, dest: &str) -> PreviewEntry {
        PreviewEntry {
            identity: identity.to_string(),
            source: identity.to_string(),
            artifact: artifact.to_string(),
            commit: "c0ffeeba".to_string(),
            destination: PathBuf::from(dest),
            state: SyncState::Synced,
            files: Vec::new(),
            rename: None,
            collapsed: false,
        }
    }

    fn plan(target: PreviewTargetPlan) -> PreviewPlan {
        PreviewPlan {
            targets: vec![target],
        }
    }

    #[test]
    fn renamed_leaf_renders_src_to_dest_legibly() {
        let mut entry = synced("dots", "renamed.md", "/dst/renamed.md");
        entry.rename = Some("x.md".to_string());
        let tp = PreviewTargetPlan {
            target: "home".to_string(),
            entries: vec![entry],
            collisions: Vec::new(),
            warnings: Vec::new(),
        };
        let rendered = render_preview_tree(&plan(tp));
        assert!(
            rendered.contains("x.md -> renamed.md"),
            "a renamed leaf must render `src -> dest` so the rename is legible; got:\n{rendered}"
        );
    }

    #[test]
    fn collapsed_dir_is_marked_as_a_collapsed_artifact() {
        let mut entry = synced("dots", "d", "/dst/d");
        entry.collapsed = true;
        entry.files = vec![PreviewFile {
            path: PathBuf::from("a.md"),
            templated: false,
        }];
        let tp = PreviewTargetPlan {
            target: "home".to_string(),
            entries: vec![entry],
            collisions: Vec::new(),
            warnings: Vec::new(),
        };
        let rendered = render_preview_tree(&plan(tp));
        assert!(
            rendered.contains("d/"),
            "a collapsed dir must be legible as a directory artifact (`d/`); got:\n{rendered}"
        );
        assert!(
            rendered.contains("a.md"),
            "the `--files` enrichment must still list the collapsed dir's children; got:\n{rendered}"
        );
    }

    #[test]
    fn take_no_match_warning_renders_with_a_did_you_mean_suggestion() {
        let tp = PreviewTargetPlan {
            target: "home".to_string(),
            entries: Vec::new(),
            collisions: Vec::new(),
            warnings: vec![BindingWarnings {
                identity: "dots".to_string(),
                source: "dots".to_string(),
                warnings: vec![PreviewWarning::TakeNoMatch {
                    pattern: "init.lus".to_string(),
                    suggestions: vec!["init.lua".to_string()],
                }],
            }],
        };
        let rendered = render_preview_tree(&plan(tp));
        assert!(
            rendered.contains("warning") && rendered.contains("init.lus"),
            "a no-match-glob take must render a warning naming the unmatched pattern; got:\n{rendered}"
        );
        assert!(
            rendered.contains(crate::diagnostic::DID_YOU_MEAN) && rendered.contains("init.lua"),
            "the warning must render a `did you mean:` suggestion naming the nearest leaf; got:\n{rendered}"
        );
    }

    #[test]
    fn collapse_blocked_warning_renders_per_binding() {
        let tp = PreviewTargetPlan {
            target: "home".to_string(),
            entries: Vec::new(),
            collisions: Vec::new(),
            warnings: vec![BindingWarnings {
                identity: "dots".to_string(),
                source: "dots".to_string(),
                warnings: vec![PreviewWarning::CollapseBlocked {
                    dir: "d".to_string(),
                }],
            }],
        };
        let rendered = render_preview_tree(&plan(tp));
        assert!(
            rendered.contains("warning") && rendered.contains("`d`"),
            "a blocked collapse must render a clear per-binding warning naming the dir; got:\n{rendered}"
        );
    }

    #[test]
    fn json_carries_warnings_in_a_structured_form() {
        let tp = PreviewTargetPlan {
            target: "home".to_string(),
            entries: Vec::new(),
            collisions: Vec::new(),
            warnings: vec![BindingWarnings {
                identity: "dots".to_string(),
                source: "dots".to_string(),
                warnings: vec![PreviewWarning::TakeNoMatch {
                    pattern: "init.lus".to_string(),
                    suggestions: vec!["init.lua".to_string()],
                }],
            }],
        };
        let json = render_preview_json(&plan(tp)).expect("preview json serializes");
        let value: serde_json::Value = serde_json::from_str(&json).expect("json parses");
        let warning = &value["targets"][0]["warnings"][0];
        assert_eq!(
            warning["identity"], "dots",
            "the warning group must carry its binding identity in JSON; got:\n{json}"
        );
        let take = &warning["warnings"][0]["TakeNoMatch"];
        assert_eq!(
            take["pattern"], "init.lus",
            "the structured JSON must carry the unmatched pattern, not pre-rendered prose; got:\n{json}"
        );
        assert_eq!(
            take["suggestions"][0], "init.lua",
            "the structured JSON must carry the suggestions array; got:\n{json}"
        );
    }
}

#[cfg(test)]
mod explain_render_tests {
    use super::super::query::{ExplainBody, ExplainReport, OfferAttribution, TakeAttribution};
    use super::render_explain;

    fn report(body: ExplainBody, warnings: Vec<String>) -> ExplainReport {
        ExplainReport {
            target: "home".to_string(),
            source: "dots".to_string(),
            body,
            warnings,
        }
    }

    #[test]
    fn allowed_path_names_target_source_include_and_identity_take() {
        let rendered = render_explain(&report(
            ExplainBody::Path {
                path: "init.lua".to_string(),
                offer: OfferAttribution::Allowed {
                    include: Some("*.lua".to_string()),
                },
                take: Some(TakeAttribution::Identity {
                    dest: "init.lua".to_string(),
                }),
            },
            Vec::new(),
        ));
        for needle in ["dots", "home", "init.lua", "*.lua", "allowed", "kept"] {
            assert!(
                rendered.contains(needle),
                "the rendering must surface `{needle}`; got:\n{rendered}"
            );
        }
    }

    #[test]
    fn renamed_take_shows_src_to_dest_arrow() {
        let rendered = render_explain(&report(
            ExplainBody::Path {
                path: "x.md".to_string(),
                offer: OfferAttribution::Allowed { include: None },
                take: Some(TakeAttribution::Renamed {
                    src: "x.md".to_string(),
                    dest: "renamed.md".to_string(),
                }),
            },
            Vec::new(),
        ));
        assert!(
            rendered.contains("x.md") && rendered.contains("renamed.md") && rendered.contains("->"),
            "a rename must render `src -> dest`; got:\n{rendered}"
        );
    }

    #[test]
    fn outside_path_surfaces_did_you_mean_suggestion() {
        let rendered = render_explain(&report(
            ExplainBody::Path {
                path: "init.lus".to_string(),
                offer: OfferAttribution::Outside {
                    suggestions: vec!["init.lua".to_string()],
                },
                take: None,
            },
            Vec::new(),
        ));
        assert!(
            rendered.contains("outside") && rendered.contains("init.lua"),
            "an outside path must be reported with its nearest suggestion; got:\n{rendered}"
        );
    }

    #[test]
    fn warnings_are_surfaced() {
        let rendered = render_explain(&report(
            ExplainBody::Summary { leaves: Vec::new() },
            vec!["take glob `none/**` matched no offered leaf".to_string()],
        ));
        assert!(
            rendered.contains("warning") && rendered.contains("none/**"),
            "a carried plan warning must render; got:\n{rendered}"
        );
    }
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
