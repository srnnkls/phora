//! The sole producer of user-facing CLI output (`println!`/`format!`).

use crate::config::ParsedSource;
use crate::projection::ArtifactState;

use super::query::{CheckMatchReport, TargetListing, WhereMatch};

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

pub(super) fn state_label(state: &ArtifactState) -> &'static str {
    match state {
        ArtifactState::Clean => "✓ clean",
        ArtifactState::Modified { .. } => "modified",
        ArtifactState::Foreign => "foreign",
        ArtifactState::Missing => "missing",
        ArtifactState::Ejected => "ejected",
        ArtifactState::Linked => "linked",
    }
}
