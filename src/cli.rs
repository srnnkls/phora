//! Command-line surface.

use std::collections::BTreeMap;
use std::path::Path;

use clap::{Parser, Subcommand};

use crate::config::{Config, Source};
use crate::error::{Error, Result};
use crate::matcher::PathMatcher;
use crate::paths::{ProjectId, phora_dir};
use crate::registry::{FileRegistry, Registry};

#[derive(Parser, Debug)]
#[command(name = "phora", version, about = "Git-based artifact package manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Parse a URL and add a source to the config.
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        root: Option<String>,
    },
    /// Fetch sources and project them to targets.
    Sync {
        #[arg(long)]
        prune: bool,
        #[arg(long)]
        force: bool,
    },
    /// Bump the lock to latest, then sync.
    Update { source: Option<String> },
    /// Show sources and deployment state.
    List {
        #[arg(long)]
        plan: bool,
    },
    /// Verify deployed files by hashing contents.
    Verify,
    /// Query the global registry.
    Where {
        #[arg(long)]
        digest: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        artifact: Option<String>,
        #[arg(long)]
        commit: Option<String>,
    },
    /// Permanently stop managing an artifact, keeping its files.
    Eject {
        artifact: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
    },
    /// Resume managing a previously ejected artifact.
    Uneject {
        artifact: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
    },
    /// Rebuild the global registry from lock and on-disk targets.
    RebuildRegistry,
    /// Debug include/exclude matching for a path.
    CheckMatch {
        #[arg(long)]
        source: String,
        path: String,
    },
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the command implementation will consume the parsed args"
)]
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Add { .. } => Err(Error::NotImplemented("add")),
        Command::Sync { .. } => Err(Error::NotImplemented("sync")),
        Command::Update { .. } => Err(Error::NotImplemented("update")),
        Command::List { .. } => Err(Error::NotImplemented("list")),
        Command::Verify => {
            let config = load_config()?;
            let mismatches = crate::sync::verify(&config, &open_project_registry()?)?;
            print_verify(&mismatches);
            if mismatches.is_empty() {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Command::Where {
            digest,
            source,
            artifact,
            commit,
        } => {
            let matches = where_cmd(
                &open_project_registry()?,
                &WhereFilter {
                    digest,
                    source,
                    artifact,
                    commit,
                },
            )?;
            print_where_matches(&matches);
            Ok(())
        }
        Command::Eject {
            artifact,
            source,
            target,
        } => {
            let config = load_config()?;
            crate::sync::eject(
                &config,
                &open_project_registry()?,
                &artifact,
                &source,
                &target,
            )?;
            println!("ejected {source}/{artifact} from {target} (files kept)");
            Ok(())
        }
        Command::Uneject {
            artifact,
            source,
            target,
        } => {
            let config = load_config()?;
            crate::sync::uneject(
                &config,
                &open_project_registry()?,
                &artifact,
                &source,
                &target,
            )?;
            println!("unejected {source}/{artifact} in {target}");
            Ok(())
        }
        Command::RebuildRegistry => Err(Error::NotImplemented("rebuild-registry")),
        Command::CheckMatch { source, path } => {
            let source = load_source(&source)?;
            let report = check_match_cmd(&source, &path);
            print_check_match(&source, &path, &report);
            Ok(())
        }
    }
}

fn open_project_registry() -> Result<FileRegistry> {
    let project = ProjectId::for_path(&std::env::current_dir()?)?;
    let state_root = phora_dir()?
        .join("state")
        .join("projects")
        .join(project.as_str());
    FileRegistry::open(state_root)
}

fn load_config() -> Result<Config> {
    let text = std::fs::read_to_string("phora.toml")
        .map_err(|e| Error::Config(format!("read phora.toml: {e}")))?;
    Config::parse(&text)
}

fn load_source(name: &str) -> Result<Source> {
    load_config()?
        .sources
        .remove(name)
        .ok_or_else(|| Error::Config(format!("source `{name}` not found in phora.toml")))
}

fn print_verify(mismatches: &[crate::sync::VerifyMismatch]) {
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

fn print_where_matches(matches: &[WhereMatch]) {
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

fn print_check_match(source: &Source, path: &str, report: &CheckMatchReport) {
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

/// Reverse-lookup filter over the registry: every `Some` field is an AND constraint.
#[derive(Debug, Default, Clone)]
pub struct WhereFilter {
    pub digest: Option<String>,
    pub source: Option<String>,
    pub artifact: Option<String>,
    pub commit: Option<String>,
}

impl WhereFilter {
    fn matches(&self, record: &crate::registry::RegistryRecord) -> bool {
        let eq = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        eq(&self.digest, &record.digest)
            && eq(&self.source, &record.key.source)
            && eq(&self.artifact, &record.key.artifact)
            && eq(&self.commit, &record.commit)
    }
}

/// One (source, artifact) deployment grouped across the targets it lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhereMatch {
    pub source: String,
    pub artifact: String,
    pub commit: String,
    pub digest: String,
    pub targets: Vec<String>,
}

/// Outcome of debugging include/exclude matching for a path under a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckMatchReport {
    pub artifact_allowed: bool,
    pub path_allowed: bool,
}

/// Filters the registry by the constraints in `filter`, grouping survivors by
/// (source, artifact) and listing the targets each is deployed to.
///
/// # Errors
///
/// Returns an error if the registry cannot be read.
pub fn where_cmd(registry: &dyn Registry, filter: &WhereFilter) -> Result<Vec<WhereMatch>> {
    let mut groups: BTreeMap<(String, String), WhereMatch> = BTreeMap::new();

    for record in registry.list_all()? {
        if !filter.matches(&record) {
            continue;
        }
        let entry = groups
            .entry((record.key.source.clone(), record.key.artifact.clone()))
            .or_insert_with(|| WhereMatch {
                source: record.key.source.clone(),
                artifact: record.key.artifact.clone(),
                commit: record.commit.clone(),
                digest: record.digest.clone(),
                targets: Vec::new(),
            });
        entry.targets.push(record.key.target.clone());
    }

    Ok(groups
        .into_values()
        .map(|mut m| {
            m.targets.sort();
            m.targets.dedup();
            m
        })
        .collect())
}

/// Reports artifact-level and path-level allow decisions for `path` under `source`.
#[must_use]
pub fn check_match_cmd(source: &Source, path: &str) -> CheckMatchReport {
    let Ok(matcher) = PathMatcher::new(source.includes(), source.excludes()) else {
        return CheckMatchReport {
            artifact_allowed: false,
            path_allowed: false,
        };
    };
    let artifact = path.split('/').next().unwrap_or(path);
    CheckMatchReport {
        artifact_allowed: matcher.allows_artifact(artifact),
        path_allowed: matcher.allows_path(Path::new(path), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ArtifactKey, FileRegistry, ManifestFile, RegistryRecord};
    use clap::CommandFactory;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    fn record(
        target: &str,
        source: &str,
        artifact: &str,
        commit: &str,
        digest: &str,
    ) -> RegistryRecord {
        RegistryRecord {
            version: 1,
            key: ArtifactKey {
                target: target.to_owned(),
                source: source.to_owned(),
                artifact: artifact.to_owned(),
            },
            commit: commit.to_owned(),
            digest: digest.to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![ManifestFile {
                path: PathBuf::from("python.json"),
                size: 12_345,
                mtime: 1_738_329_296,
                blake3: "9e8d7c6b5a4f3e2d".to_owned(),
            }],
        }
    }

    fn seeded_registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        reg.put(&record("nvim", "dotfiles", "init", "aaa111", "blake3:d1"))
            .expect("put nvim/dotfiles/init");
        reg.put(&record(
            "vscode",
            "dotfiles",
            "settings",
            "aaa111",
            "blake3:d2",
        ))
        .expect("put vscode/dotfiles/settings");
        reg.put(&record(
            "vscode",
            "company-configs",
            "python",
            "def456",
            "blake3:dpy",
        ))
        .expect("put vscode/company-configs/python");
        reg.put(&record(
            "agent-1",
            "company-configs",
            "python",
            "def456",
            "blake3:dpy",
        ))
        .expect("put agent-1/company-configs/python");
        (dir, reg)
    }

    fn source_with(include: &[&str], exclude: &[&str]) -> Source {
        use std::fmt::Write as _;
        let mut body = String::from("version = 1\n\n[sources.s]\ngit = \"https://x.git\"\n");
        if !include.is_empty() {
            let list = include
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(body, "include = [{list}]");
        }
        if !exclude.is_empty() {
            let list = exclude
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(body, "exclude = [{list}]");
        }
        crate::config::Config::parse(&body)
            .expect("source toml parses")
            .sources
            .remove("s")
            .expect("source `s` present")
    }

    fn find<'a>(matches: &'a [WhereMatch], source: &str, artifact: &str) -> Option<&'a WhereMatch> {
        matches
            .iter()
            .find(|m| m.source == source && m.artifact == artifact)
    }

    // where_cmd

    #[test]
    fn where_filters_by_source() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("dotfiles".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by source");

        assert!(
            matches.iter().all(|m| m.source == "dotfiles"),
            "every match must come from the requested source, got {matches:?}"
        );
        assert!(
            find(&matches, "dotfiles", "init").is_some(),
            "dotfiles/init must be present"
        );
        assert!(
            find(&matches, "dotfiles", "settings").is_some(),
            "dotfiles/settings must be present"
        );
        assert!(
            find(&matches, "company-configs", "python").is_none(),
            "company-configs must be excluded when filtering by source=dotfiles"
        );
    }

    #[test]
    fn where_filters_by_artifact() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            artifact: Some("python".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by artifact");

        assert!(
            matches.iter().all(|m| m.artifact == "python"),
            "only python artifacts must survive, got {matches:?}"
        );
        assert!(
            find(&matches, "company-configs", "python").is_some(),
            "company-configs/python must be present"
        );
    }

    #[test]
    fn where_filters_by_commit() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            commit: Some("aaa111".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by commit");

        assert!(
            matches.iter().all(|m| m.commit == "aaa111"),
            "only commit aaa111 records must survive, got {matches:?}"
        );
        assert!(
            find(&matches, "company-configs", "python").is_none(),
            "the def456 record must be filtered out by commit=aaa111"
        );
    }

    #[test]
    fn where_filters_by_digest() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            digest: Some("blake3:dpy".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where by digest");

        assert!(
            matches.iter().all(|m| m.digest == "blake3:dpy"),
            "only the matching digest must survive, got {matches:?}"
        );
        let m = find(&matches, "company-configs", "python")
            .expect("company-configs/python carries digest blake3:dpy");
        assert_eq!(m.digest, "blake3:dpy", "match must echo the queried digest");
    }

    #[test]
    fn where_combines_filters_with_and_semantics() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("dotfiles".to_owned()),
            artifact: Some("init".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where with source AND artifact");

        assert_eq!(
            matches.len(),
            1,
            "source=dotfiles AND artifact=init must select exactly one group, got {matches:?}"
        );
        assert!(
            find(&matches, "dotfiles", "init").is_some(),
            "the single match must be dotfiles/init"
        );
        assert!(
            find(&matches, "dotfiles", "settings").is_none(),
            "dotfiles/settings fails the artifact=init constraint"
        );
    }

    #[test]
    fn where_groups_a_shared_artifact_across_its_targets() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("company-configs".to_owned()),
            artifact: Some("python".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where company-configs/python");

        assert_eq!(
            matches.len(),
            1,
            "the two deployments of company-configs/python must collapse into one group"
        );
        let m = &matches[0];
        let mut targets = m.targets.clone();
        targets.sort();
        assert_eq!(
            targets,
            vec!["agent-1".to_owned(), "vscode".to_owned()],
            "the grouped match must list both targets the artifact deploys to"
        );
    }

    #[test]
    fn where_with_no_match_is_empty() {
        let (_dir, reg) = seeded_registry();
        let filter = WhereFilter {
            source: Some("nonexistent".to_owned()),
            ..WhereFilter::default()
        };

        let matches = where_cmd(&reg, &filter).expect("where with no matching source");

        assert!(
            matches.is_empty(),
            "a filter matching nothing yields an empty result, got {matches:?}"
        );
    }

    // check_match_cmd

    #[test]
    fn check_match_reports_artifact_allowed_for_included_name() {
        let source = source_with(&["editor"], &[]);

        let report = check_match_cmd(&source, "editor");

        assert!(
            report.artifact_allowed,
            "an artifact name on the include list must be reported as artifact-allowed"
        );
    }

    #[test]
    fn check_match_reports_artifact_not_allowed_for_unlisted_name() {
        let source = source_with(&["editor"], &[]);

        let report = check_match_cmd(&source, "vim");

        assert!(
            !report.artifact_allowed,
            "a name absent from a non-empty include list must be reported as not artifact-allowed"
        );
    }

    #[test]
    fn check_match_reports_path_excluded_for_bak_file() {
        let source = source_with(&[], &["**/*.bak"]);

        let report = check_match_cmd(&source, "editor/notes.bak");

        assert!(
            !report.path_allowed,
            "a path matching the `**/*.bak` exclude must be reported as not path-allowed"
        );
    }

    #[test]
    fn check_match_reports_path_allowed_for_non_excluded_file() {
        let source = source_with(&[], &["**/*.bak"]);

        let report = check_match_cmd(&source, "editor/init.lua");

        assert!(
            report.path_allowed,
            "a path not matching any exclude must be reported as path-allowed"
        );
    }

    #[test]
    fn check_match_distinguishes_artifact_and_path_outcomes() {
        let source = source_with(&["editor"], &["**/*.bak"]);

        let allowed = check_match_cmd(&source, "editor");
        assert!(
            allowed.artifact_allowed && allowed.path_allowed,
            "an included artifact name with no exclude hit must be allowed at both levels"
        );

        let mixed = check_match_cmd(&source, "editor/notes.bak");
        assert!(
            mixed.artifact_allowed,
            "the `editor` artifact stays allowed even when its path is excluded"
        );
        assert!(
            !mixed.path_allowed,
            "the path-level exclude must independently reject editor/notes.bak"
        );
        assert_ne!(
            mixed.artifact_allowed, mixed.path_allowed,
            "artifact-level and path-level outcomes must differ for editor/notes.bak"
        );
    }
}
