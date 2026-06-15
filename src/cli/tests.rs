use super::*;
use crate::store::{ArtifactKey, FileRegistry, ManifestFile, RegistryRecord};
use clap::CommandFactory;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn cli_definition_is_valid() {
    Cli::command().debug_assert();
}

#[test]
fn bind_parses_sources_target_and_flags() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "phora", "bind", "tools", "prompts", "--to", "claude", "--local",
    ])
    .expect("bind with sources, --to, --local must parse");
    let Command::Bind {
        sources, to, local, ..
    } = cli.command
    else {
        panic!("expected Command::Bind");
    };
    assert_eq!(sources, vec!["tools".to_owned(), "prompts".to_owned()]);
    assert_eq!(to, "claude");
    assert!(local, "--local must set local=true");
}

#[test]
fn bind_rejects_empty_source_list() {
    use clap::Parser;
    Cli::try_parse_from(["phora", "bind", "--to", "claude"])
        .expect_err("bind with no positional sources must be rejected (required=true)");
}

#[test]
fn bind_requires_to() {
    use clap::Parser;
    Cli::try_parse_from(["phora", "bind", "tools"])
        .expect_err("bind without --to must be rejected");
}

#[test]
fn unbind_parses_sources_target_and_local() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["phora", "unbind", "tools", "--from", "claude", "--local"])
        .expect("unbind with sources, --from, --local must parse");
    let Command::Unbind {
        sources,
        from,
        local,
    } = cli.command
    else {
        panic!("expected Command::Unbind");
    };
    assert_eq!(sources, vec!["tools".to_owned()]);
    assert_eq!(from, "claude");
    assert!(local, "--local must set local=true");
}

#[test]
fn unbind_rejects_empty_source_list() {
    use clap::Parser;
    Cli::try_parse_from(["phora", "unbind", "--from", "claude"])
        .expect_err("unbind with no positional sources must be rejected (required=true)");
}

#[test]
fn unbind_requires_from() {
    use clap::Parser;
    Cli::try_parse_from(["phora", "unbind", "tools"])
        .expect_err("unbind without --from must be rejected");
}

#[test]
fn state_label_renders_linked_artifact_as_linked() {
    assert_eq!(
        state_label(&ArtifactState::Linked),
        "linked",
        "`phora list` must label a Linked artifact `linked`"
    );
}

#[test]
fn sync_no_hooks_flag_parses_to_true() {
    use clap::Parser;
    let cli =
        Cli::try_parse_from(["phora", "sync", "--no-hooks"]).expect("sync --no-hooks must parse");
    let Command::Sync { no_hooks, .. } = cli.command else {
        panic!("expected Command::Sync");
    };
    assert!(no_hooks, "--no-hooks must set no_hooks=true");
}

#[test]
fn sync_without_no_hooks_flag_defaults_to_false() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["phora", "sync"]).expect("bare sync must parse");
    let Command::Sync { no_hooks, .. } = cli.command else {
        panic!("expected Command::Sync");
    };
    assert!(!no_hooks, "absent --no-hooks must default no_hooks=false");
}

#[test]
fn hook_report_lists_each_hook_with_scope_and_status() {
    use crate::sync::{HookOutcome, HookScope, HookStatus};
    let outcomes = vec![
        HookOutcome {
            hook_id: "fmt".to_owned(),
            command: "cargo fmt".to_owned(),
            scope: HookScope::OnChange,
            status: HookStatus::Success,
        },
        HookOutcome {
            hook_id: "reload".to_owned(),
            command: "systemctl reload".to_owned(),
            scope: HookScope::PostSync,
            status: HookStatus::Success,
        },
    ];
    let report = render::render_hook_report(&outcomes);
    assert!(
        report.contains("fmt"),
        "report must name the on_change hook by id: {report}"
    );
    assert!(
        report.contains("reload"),
        "report must name the post_sync hook by id: {report}"
    );
    assert!(
        report.contains("on_change"),
        "report must label the OnChange scope: {report}"
    );
    assert!(
        report.contains("post_sync"),
        "report must label the PostSync scope: {report}"
    );
}

#[test]
fn hook_report_surfaces_a_failed_hook_distinctly() {
    use crate::sync::{HookOutcome, HookScope, HookStatus};
    let outcomes = vec![
        HookOutcome {
            hook_id: "ok".to_owned(),
            command: "true".to_owned(),
            scope: HookScope::OnChange,
            status: HookStatus::Success,
        },
        HookOutcome {
            hook_id: "broken".to_owned(),
            command: "false".to_owned(),
            scope: HookScope::PostSync,
            status: HookStatus::Failure,
        },
    ];
    let report = render::render_hook_report(&outcomes);
    let broken_line = report
        .lines()
        .find(|l| l.contains("broken"))
        .unwrap_or_else(|| panic!("report must mention the failed hook: {report}"));
    assert!(
        broken_line.to_lowercase().contains("fail"),
        "the failed hook's line must be marked as a failure: {broken_line}"
    );
    let ok_line = report
        .lines()
        .find(|l| l.contains("ok"))
        .unwrap_or_else(|| panic!("report must mention the successful hook: {report}"));
    assert!(
        !ok_line.to_lowercase().contains("fail"),
        "a successful hook's line must not be marked as a failure: {ok_line}"
    );
}

#[test]
fn hook_report_is_empty_when_no_hooks_ran() {
    let report = render::render_hook_report(&[]);
    assert!(
        report.trim().is_empty(),
        "an empty hook-result set must produce no hook-related output: {report}"
    );
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
        source: source.to_owned(),
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
        linked: false,
        vars_digest: None,
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

fn source_with(include: &[&str], exclude: &[&str]) -> ParsedSource {
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
    let raw = crate::config::Config::parse(&body)
        .expect("source toml parses")
        .sources
        .remove("s")
        .expect("source `s` present");
    ParsedSource::parse("s", &raw).expect("source parses to typed form")
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

#[test]
fn where_marks_ejected_targets() {
    let (_dir, reg) = seeded_registry();
    reg.save_ejected(
        "nvim",
        &[crate::store::EjectedEntry {
            source: "dotfiles".to_owned(),
            artifact: "init".to_owned(),
            ejected_at: "2026-01-01T00:00:00Z".to_owned(),
        }],
    )
    .expect("eject nvim/dotfiles/init");

    let filter = WhereFilter {
        source: Some("dotfiles".to_owned()),
        artifact: Some("init".to_owned()),
        ..WhereFilter::default()
    };
    let matches = where_cmd(&reg, &filter).expect("where dotfiles/init");

    assert_eq!(matches.len(), 1, "one (source, artifact) group expected");
    assert_eq!(
        matches[0].targets,
        vec!["nvim (ejected)".to_owned()],
        "where must annotate the target an artifact was ejected from, got {:?}",
        matches[0].targets
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

// parse_add_url

fn no_hosts() -> BTreeMap<String, Host> {
    BTreeMap::new()
}

fn parse(input: &str) -> AddTarget {
    parse_add_url(input, &no_hosts()).unwrap_or_else(|e| panic!("parse `{input}`: {e}"))
}

#[test]
fn github_shorthand_yields_symbolic_github_source() {
    let parsed = parse("srnnkls/loqui");
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "a bare owner/repo shorthand must default to the symbolic github host, not an expanded git URL (got git={:?})",
        parsed.git
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("srnnkls/loqui"),
        "the shorthand must carry repo=srnnkls/loqui symbolically"
    );
    assert!(
        parsed.git.is_none(),
        "owner/repo must stay symbolic, not pre-expand to a github git URL, got {:?}",
        parsed.git
    );
    assert_eq!(
        parsed.name, "loqui",
        "default name is the repo segment, not the owner"
    );
    assert!(
        parsed.branch.is_none(),
        "a bare shorthand carries no branch"
    );
    assert!(parsed.root.is_none(), "a bare shorthand carries no root");
}

#[test]
fn github_shorthand_with_extra_path_becomes_root() {
    let parsed = parse("owner/repo/path/to/dir");
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "only owner/repo feed the symbolic host/path; the rest is the root"
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("owner/repo"),
        "the symbolic path is exactly owner/repo, with trailing segments split off as root"
    );
    assert!(
        parsed.git.is_none(),
        "a shorthand+path stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(
        parsed.root.as_deref(),
        Some("path/to/dir"),
        "trailing path segments beyond owner/repo become the source root"
    );
    assert_eq!(
        parsed.name, "repo",
        "default name is still the repo segment"
    );
    assert!(
        parsed.branch.is_none(),
        "a shorthand+path carries no branch"
    );
}

#[test]
fn domain_shorthand_yields_symbolic_github_source() {
    let parsed = parse("github.com/owner/repo");
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "a github.com/owner/repo domain shorthand must map to the symbolic host name `github`, not an expanded https URL (got git={:?})",
        parsed.git
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("owner/repo"),
        "the domain shorthand must carry repo=owner/repo symbolically"
    );
    assert!(
        parsed.git.is_none(),
        "a domain shorthand stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(parsed.name, "repo");
    assert!(
        parsed.branch.is_none(),
        "a domain shorthand carries no branch"
    );
    assert!(parsed.root.is_none(), "a domain shorthand carries no root");
}

#[test]
fn srht_domain_shorthand_maps_to_symbolic_srht_host() {
    let parsed = parse("git.sr.ht/~rjarry/aerc");
    assert_eq!(
        parsed.host.as_deref(),
        Some("sr.ht"),
        "the git.sr.ht DOMAIN must map to the forge NAME `sr.ht`, not the domain string"
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("~rjarry/aerc"),
        "the `~` owner segment must be preserved verbatim in the symbolic path"
    );
    assert!(
        parsed.git.is_none(),
        "a non-github domain shorthand stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(parsed.name, "aerc");
}

#[test]
fn full_https_url_stays_a_literal_git_remote() {
    let parsed = parse("https://github.com/owner/repo");
    assert_eq!(
        parsed.git.as_deref(),
        Some("https://github.com/owner/repo.git"),
        "a full https URL stays a LITERAL git remote (back-compat) with .git appended"
    );
    assert!(
        parsed.host.is_none() && parsed.repo.is_none(),
        "a literal scheme URL must NOT become a symbolic host/path source"
    );
    assert_eq!(parsed.name, "repo");
    assert!(parsed.branch.is_none());
    assert!(parsed.root.is_none());
}

#[test]
fn tree_url_stays_literal_and_extracts_branch_and_root() {
    let parsed = parse("https://github.com/company/configs/tree/main/editor");
    assert_eq!(
        parsed.git.as_deref(),
        Some("https://github.com/company/configs.git"),
        "a scheme URL stays a LITERAL git remote; the /tree/<ref>/<path> tail is stripped from it"
    );
    assert!(
        parsed.host.is_none() && parsed.repo.is_none(),
        "a tree URL is a literal scheme URL, not a symbolic host/path source"
    );
    assert_eq!(
        parsed.branch.as_deref(),
        Some("main"),
        "the segment after /tree/ is the branch"
    );
    assert_eq!(
        parsed.root.as_deref(),
        Some("editor"),
        "the segments after /tree/<ref>/ are the root"
    );
    assert_eq!(
        parsed.name, "configs",
        "name is the repo, not the path tail"
    );
}

#[test]
fn gitlab_domain_shorthand_maps_to_symbolic_gitlab_host() {
    let parsed = parse("gitlab.com/owner/repo");
    assert_eq!(
        parsed.host.as_deref(),
        Some("gitlab"),
        "the gitlab.com DOMAIN must map to the symbolic forge NAME `gitlab`, not github and not an expanded URL (got git={:?})",
        parsed.git
    );
    assert_eq!(parsed.repo.as_deref(), Some("owner/repo"));
    assert!(
        parsed.git.is_none(),
        "a gitlab domain shorthand stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(parsed.name, "repo");
    assert!(
        parsed.branch.is_none(),
        "a gitlab shorthand carries no branch"
    );
    assert!(parsed.root.is_none(), "a gitlab shorthand carries no root");
}

#[test]
fn codeberg_domain_shorthand_maps_to_symbolic_codeberg_host() {
    let parsed = parse("codeberg.org/owner/repo");
    assert_eq!(
        parsed.host.as_deref(),
        Some("codeberg"),
        "codeberg.org must map to the symbolic forge NAME `codeberg` via the SINGLE built-in forge registry"
    );
    assert_eq!(parsed.repo.as_deref(), Some("owner/repo"));
    assert!(
        parsed.git.is_none(),
        "a codeberg domain shorthand stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(parsed.name, "repo");
}

#[test]
fn bitbucket_domain_shorthand_maps_to_symbolic_bitbucket_host() {
    let parsed = parse("bitbucket.org/owner/repo");
    assert_eq!(
        parsed.host.as_deref(),
        Some("bitbucket"),
        "bitbucket.org must map to the symbolic forge NAME `bitbucket`; it can only resolve if \
             the forge registry derives from builtin_forges()"
    );
    assert_eq!(parsed.repo.as_deref(), Some("owner/repo"));
    assert!(
        parsed.git.is_none(),
        "a bitbucket domain shorthand stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(parsed.name, "repo");
}

#[test]
fn scp_ssh_url_is_kept_as_a_literal_git_remote() {
    let parsed = parse("git@github.com:owner/repo.git");
    assert_eq!(
        parsed.git.as_deref(),
        Some("git@github.com:owner/repo.git"),
        "an ssh scp-style URL is a literal git remote and must be preserved verbatim"
    );
    assert!(
        parsed.host.is_none() && parsed.repo.is_none(),
        "an scp literal must NOT become a symbolic host/path source"
    );
    assert_eq!(
        parsed.name, "repo",
        "the repo segment of an ssh URL (minus .git) is the default name"
    );
    assert!(parsed.branch.is_none(), "an ssh URL carries no branch");
    assert!(parsed.root.is_none(), "an ssh URL carries no root");
}

#[test]
fn custom_host_domain_shorthand_maps_to_symbolic_custom_host() {
    let mut hosts = BTreeMap::new();
    hosts.insert(
            "company".to_owned(),
            Config::parse(
                "version = 1\n\n[hosts.company]\nremote = \"ssh://git@git.company.com:2222/scm/{owner}/{repo}.git\"\n",
            )
            .expect("host toml parses")
            .hosts
            .remove("company")
            .expect("company host present"),
        );

    let parsed = parse_add_url("git.company.com/owner/repo", &hosts)
        .expect("custom host shorthand resolves");

    assert_eq!(
        parsed.host.as_deref(),
        Some("company"),
        "the custom host's DOMAIN (git.company.com) must map to its symbolic host NAME `company`, \
             not be expanded into the template URL"
    );
    assert_eq!(parsed.repo.as_deref(), Some("owner/repo"));
    assert!(
        parsed.git.is_none(),
        "a custom-host domain shorthand stays symbolic, got {:?}",
        parsed.git
    );
    assert_eq!(parsed.name, "repo");
    assert!(
        parsed.branch.is_none(),
        "a custom-host shorthand carries no branch"
    );
    assert!(
        parsed.root.is_none(),
        "a custom-host shorthand carries no root"
    );
}

// insert_source

const ADD_BASE: &str =
    "version = 1\n\n[sources.foo]\ngit = \"https://github.com/me/foo.git\"\nbranch = \"main\"\n";

fn lit(git: &str, branch: Option<&str>) -> AddTarget {
    AddTarget {
        name: String::new(),
        git: Some(git.to_owned()),
        host: None,
        repo: None,
        path: None,
        protocol: None,
        branch: branch.map(str::to_owned),
        root: None,
    }
}

#[test]
fn insert_source_preserves_existing_source_and_adds_new() {
    let out = insert_source(
        ADD_BASE,
        "loqui",
        &lit("https://github.com/srnnkls/loqui.git", None),
        None,
    )
    .expect("insert into base toml");

    let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");

    let foo = cfg
        .sources
        .get("foo")
        .expect("existing foo source survives");
    assert_eq!(
        foo.git.as_deref(),
        Some("https://github.com/me/foo.git"),
        "the pre-existing source must be untouched"
    );
    assert_eq!(
        foo.branch.as_deref(),
        Some("main"),
        "the pre-existing source's branch must be preserved"
    );

    let loqui = cfg.sources.get("loqui").expect("new loqui source added");
    assert_eq!(
        loqui.git.as_deref(),
        Some("https://github.com/srnnkls/loqui.git")
    );
    assert!(
        loqui.branch.is_none(),
        "no branch was passed, so no branch key must be emitted"
    );
    assert!(
        loqui.root.is_none(),
        "no root was passed, so no root key must be emitted"
    );
}

#[test]
fn insert_source_emits_branch_and_root_when_some() {
    let out = insert_source(
        ADD_BASE,
        "editor-config",
        &lit("https://github.com/company/configs.git", Some("main")),
        Some("editor"),
    )
    .expect("insert with branch and root");

    let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");

    let foo = cfg
        .sources
        .get("foo")
        .expect("pre-existing foo source survives the branch/root insert");
    assert_eq!(
        foo.git.as_deref(),
        Some("https://github.com/me/foo.git"),
        "the pre-existing source's git must be untouched when inserting a source with branch+root"
    );
    assert_eq!(
        foo.branch.as_deref(),
        Some("main"),
        "the pre-existing source's branch must be preserved"
    );

    let added = cfg
        .sources
        .get("editor-config")
        .expect("new editor-config source added");

    assert_eq!(
        added.git.as_deref(),
        Some("https://github.com/company/configs.git")
    );
    assert_eq!(
        added.branch.as_deref(),
        Some("main"),
        "a Some(branch) must be written as a branch key"
    );
    assert_eq!(
        added.root.as_deref(),
        Some(Path::new("editor")),
        "a Some(root) must be written as a root key"
    );
}

#[test]
fn insert_source_preserves_surrounding_text_verbatim() {
    let seeded =
        "# my comment\nversion = 1\n\n[sources.foo]\ngit = \"https://github.com/me/foo.git\"\n";

    let out = insert_source(
        seeded,
        "loqui",
        &lit("https://github.com/srnnkls/loqui.git", None),
        None,
    )
    .expect("insert into seeded toml");

    assert!(
        out.contains("# my comment\nversion = 1"),
        "the leading comment and version line must survive byte-for-byte (no reformatting), got:\n{out}"
    );
    assert!(
        out.contains("[sources.foo]\ngit = \"https://github.com/me/foo.git\""),
        "the existing [sources.foo] block must be present unchanged, not relocated or rewritten, got:\n{out}"
    );
    assert!(
        out.contains("[sources.loqui]"),
        "the new table must be inserted as [sources.loqui]"
    );

    let cfg = Config::parse(&out).expect("inserted text is valid phora.toml");
    let foo = cfg
        .sources
        .get("foo")
        .expect("existing foo source survives");
    assert_eq!(
        foo.git.as_deref(),
        Some("https://github.com/me/foo.git"),
        "re-parsing the output must yield the original foo git value"
    );
}

#[test]
fn insert_source_uses_standard_table_blocks_not_inline() {
    let first = insert_source(
        "version = 1\n",
        "loqui",
        &lit("https://github.com/srnnkls/loqui.git", None),
        None,
    )
    .expect("insert first source into a doc with no sources table");

    assert!(
        first.contains("[sources.loqui]"),
        "the new source must be a standard table header [sources.loqui], not an inline table, got:\n{first}"
    );
    assert!(
        first.contains("git = \"https://github.com/srnnkls/loqui.git\""),
        "the git key must be written on its own line under [sources.loqui], got:\n{first}"
    );
    assert!(
        !first.contains("sources = {"),
        "the sources table must not be rendered as an inline `sources = {{ ... }}` table, got:\n{first}"
    );

    let second = insert_source(
        &first,
        "editor",
        &lit("https://github.com/company/editor.git", None),
        None,
    )
    .expect("insert second source after the first");

    assert!(
        second.contains("[sources.loqui]"),
        "the first source must remain a standard [sources.loqui] block after a second insert, got:\n{second}"
    );
    assert!(
        second.contains("[sources.editor]"),
        "the second source must be its own standard [sources.editor] block, got:\n{second}"
    );
    assert!(
        !second.contains("sources = {"),
        "repeated inserts must stay as separate table blocks, never collapse into an inline table, got:\n{second}"
    );

    let cfg = Config::parse(&second).expect("block-form output is valid phora.toml");
    assert_eq!(
        cfg.sources
            .get("loqui")
            .expect("loqui source parses from block form")
            .git
            .as_deref(),
        Some("https://github.com/srnnkls/loqui.git")
    );
    assert_eq!(
        cfg.sources
            .get("editor")
            .expect("editor source parses from block form")
            .git
            .as_deref(),
        Some("https://github.com/company/editor.git")
    );
}

// ── HAS-004: add writes symbolic host/path (both writer paths) ──

use crate::source::Protocol;

/// Re-parses inserted text and returns the named source, asserting validity.
fn source_from(out: &str, name: &str) -> crate::config::Source {
    Config::parse(out)
        .unwrap_or_else(|e| panic!("inserted text must be valid phora.toml: {e}\n{out}"))
        .sources
        .remove(name)
        .unwrap_or_else(|| panic!("source `{name}` must be present in:\n{out}"))
}

#[test]
fn parse_colon_alias_yields_symbolic_github_source() {
    let parsed = parse("github:srnnkls/tropos");
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "`github:srnnkls/tropos` must parse to a symbolic source with host=github, not an expanded git URL (got git={:?})",
        parsed.git
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("srnnkls/tropos"),
        "the colon alias must carry repo=srnnkls/tropos symbolically"
    );
    assert_eq!(
        parsed.name, "tropos",
        "the default name is the repo segment of the alias"
    );
    assert!(
        parsed.git.is_none(),
        "a symbolic colon alias must NOT be pre-expanded into a literal git URL, got {:?}",
        parsed.git
    );
}

#[test]
fn parse_colon_alias_is_not_mistaken_for_scp() {
    let parsed = parse("github:owner/repo");
    assert!(
        parsed.git.is_none(),
        "`github:owner/repo` has no `@`, so it must NOT be dispatched to the scp-ssh path that keeps the literal string; git must be None, got {:?}",
        parsed.git
    );
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "the colon alias must resolve symbolically to host=github, not be treated as an scp remote"
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("owner/repo"),
        "the colon alias must carry repo=owner/repo, not swallow it into a literal scp string"
    );
}

#[test]
fn parse_gitlab_colon_alias_yields_symbolic_gitlab_source() {
    let parsed = parse("gitlab:owner/repo");
    assert_eq!(
        parsed.host.as_deref(),
        Some("gitlab"),
        "`gitlab:owner/repo` must parse to a symbolic source with host=gitlab"
    );
    assert_eq!(parsed.repo.as_deref(), Some("owner/repo"));
    assert!(
        parsed.git.is_none(),
        "a symbolic gitlab alias must not be pre-expanded"
    );
}

#[test]
fn colon_alias_splits_extra_path_into_root() {
    let parsed = parse("github:owner/repo/sub/dir");
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "only owner/repo feed the symbolic host/path; the rest is the root"
    );
    assert_eq!(
        parsed.repo.as_deref(),
        Some("owner/repo"),
        "the colon-alias path is exactly owner/repo, with trailing segments split off as root"
    );
    assert_eq!(
        parsed.root.as_deref(),
        Some("sub/dir"),
        "segments past owner/repo become the root, mirroring the slash shorthand"
    );
    assert!(
        parsed.git.is_none(),
        "a colon alias with extra path stays symbolic, got {:?}",
        parsed.git
    );
}

#[test]
fn colon_alias_empty_host_errors() {
    let err = parse_add_url(":owner/repo", &no_hosts())
        .expect_err("`:owner/repo` has an empty host and must be rejected");
    assert!(
        err.to_string().contains(":owner/repo"),
        "the error must name the offending input, got `{err}`"
    );
}

#[test]
fn colon_alias_empty_path_errors() {
    let err = parse_add_url("github:", &no_hosts())
        .expect_err("`github:` has an empty path and must be rejected");
    assert!(
        err.to_string().contains("github:"),
        "the error must name the offending input, got `{err}`"
    );
}

#[test]
fn parse_bare_owner_repo_defaults_to_symbolic_github() {
    let parsed = parse("owner/repo");
    assert_eq!(
        parsed.host.as_deref(),
        Some("github"),
        "a bare owner/repo shorthand must default to the symbolic github host"
    );
    assert_eq!(parsed.repo.as_deref(), Some("owner/repo"));
    assert!(
        parsed.git.is_none(),
        "a bare owner/repo must be symbolic, not expanded to a github git URL"
    );
}

#[test]
fn scp_ssh_url_stays_a_literal_git_remote_not_symbolic() {
    let parsed = parse("git@github.com:owner/repo.git");
    assert_eq!(
        parsed.git.as_deref(),
        Some("git@github.com:owner/repo.git"),
        "an scp-ssh remote (has `@` before `:`) is a literal git remote, preserved verbatim"
    );
    assert!(
        parsed.host.is_none() && parsed.repo.is_none(),
        "an scp literal must NOT be turned into a symbolic host/path source"
    );
}

#[test]
fn full_https_url_stays_a_literal_git_remote_not_symbolic() {
    let parsed = parse("https://github.com/owner/repo");
    assert_eq!(
        parsed.git.as_deref(),
        Some("https://github.com/owner/repo.git"),
        "a full scheme URL stays a literal git remote (back-compat)"
    );
    assert!(
        parsed.host.is_none() && parsed.repo.is_none(),
        "a literal scheme URL must NOT become a symbolic host/path source"
    );
}

// ── ARCH-005: symbolic forge add writes `host` + `repo` (not `host` + `path`) ──

#[test]
fn run_add_writer_writes_host_and_repo_for_symbolic_forge_add() {
    let parsed = parse("github:srnnkls/tropos");
    let out = insert_source_with_ref("version = 1\n", &parsed.name, &parsed, None, None, None)
        .expect("run_add's writer must accept a symbolic forge AddTarget");

    assert!(
        out.contains("repo = \"srnnkls/tropos\""),
        "a symbolic forge add must write `repo = \"srnnkls/tropos\"`, got:\n{out}"
    );
    assert!(
        !out.contains("path ="),
        "ARCH-005: a symbolic forge add must NOT emit a forge `path =` key any more, got:\n{out}"
    );
    assert!(
        !out.contains("git ="),
        "a symbolic forge add must not write an expanded `git =` key, got:\n{out}"
    );

    let src = source_from(&out, "tropos");
    assert_eq!(
        src.host.as_deref(),
        Some("github"),
        "run_add's writer must persist host = \"github\""
    );
    assert_eq!(
        src.repo.as_deref(),
        Some("srnnkls/tropos"),
        "run_add's writer must persist the forge key as repo = \"srnnkls/tropos\""
    );
    assert!(
        src.git.is_none(),
        "run_add's writer must not persist a literal git for a symbolic forge add"
    );
}

#[test]
fn run_add_writer_writes_symbolic_host_path_for_colon_alias() {
    let parsed = parse("github:srnnkls/tropos");
    let out = insert_source_with_ref("version = 1\n", &parsed.name, &parsed, None, None, None)
        .expect("run_add's writer must accept a symbolic AddTarget");

    assert!(
        out.contains("[sources.tropos]"),
        "run_add's writer must emit a [sources.tropos] table, got:\n{out}"
    );
    assert!(
        !out.contains("git ="),
        "a symbolic add must NOT write an expanded `git =` key (run_add writer), got:\n{out}"
    );

    let src = source_from(&out, "tropos");
    assert_eq!(
        src.host.as_deref(),
        Some("github"),
        "run_add's writer must persist host = \"github\""
    );
    assert_eq!(
        src.repo.as_deref(),
        Some("srnnkls/tropos"),
        "run_add's writer must persist repo = \"srnnkls/tropos\""
    );
    assert!(
        src.git.is_none(),
        "run_add's writer must not persist a literal git for a symbolic add"
    );
}

/// Serializes cwd-mutating tests: `run_add` reads/writes `phora.toml` in the
/// process cwd, so two such tests must not run concurrently.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `body` with the process cwd set to `dir`, restoring it on the way out
/// even on panic. Holds [`CWD_LOCK`]; never run alongside other cwd tests.
fn with_cwd<T>(dir: &std::path::Path, body: impl FnOnce() -> T) -> T {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev = std::env::current_dir().expect("read cwd");
    std::env::set_current_dir(dir).expect("enter temp cwd");
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    std::env::set_current_dir(&prev).expect("restore cwd");
    match out {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[test]
fn run_add_end_to_end_persists_symbolic_source_to_phora_toml() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");

    with_cwd(dir.path(), || {
        run_add(
            "github:srnnkls/tropos",
            &[],
            None,
            None,
            None,
            None,
            false,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add must succeed for a symbolic colon alias");
    });

    let written =
        std::fs::read_to_string(&toml_path).expect("run_add must write phora.toml in the cwd");
    assert!(
        !written.contains("git ="),
        "run_add's parse->writer glue must persist the SYMBOLIC source, never an expanded `git =`, got:\n{written}"
    );

    let src = source_from(&written, "tropos");
    assert_eq!(
        src.host.as_deref(),
        Some("github"),
        "run_add end-to-end must persist host = \"github\""
    );
    assert_eq!(
        src.repo.as_deref(),
        Some("srnnkls/tropos"),
        "run_add end-to-end must persist repo = \"srnnkls/tropos\""
    );
    assert!(
        src.git.is_none(),
        "run_add end-to-end must not persist a literal git for a symbolic add"
    );
}

#[test]
fn run_add_persists_local_path_source_for_absolute_dir() {
    let project = tempfile::TempDir::new().expect("temp project dir");
    let source_dir = tempfile::TempDir::new().expect("temp source dir");
    let toml_path = project.path().join("phora.toml");
    let source_path = source_dir.path().to_path_buf();
    let source_arg = source_path.to_str().expect("utf-8 source path");

    with_cwd(project.path(), || {
        run_add(
            source_arg,
            &[],
            None,
            None,
            None,
            None,
            false,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add must accept an absolute local path source");
    });

    let written =
        std::fs::read_to_string(&toml_path).expect("run_add must write phora.toml in the cwd");
    let canonical = std::fs::canonicalize(&source_path).expect("canonicalize source dir");
    let name = canonical
        .file_name()
        .expect("source dir has a basename")
        .to_string_lossy()
        .into_owned();
    let src = source_from(&written, &name);
    assert_eq!(
        src.path.as_deref(),
        Some(canonical.to_string_lossy().as_ref()),
        "an absolute local path must persist as a canonical `path =` source, got:\n{written}"
    );
    assert!(
        src.host.is_none() && src.repo.is_none(),
        "a local path source must not be misparsed as a forge host/repo, got:\n{written}"
    );
    assert!(
        src.git.is_none(),
        "a local path source uses `path =`, not `git =`, got:\n{written}"
    );
}

#[test]
fn run_add_to_target_persists_local_path_source() {
    let project = tempfile::TempDir::new().expect("temp project dir");
    let source_dir = tempfile::TempDir::new().expect("temp source dir");
    let toml_path = project.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n[targets.home]\npath = \"./out\"\nlayout = \"flat\"\nsources = []\n",
    )
    .expect("seed phora.toml with an existing target");
    let source_path = source_dir.path().to_path_buf();
    let source_arg = source_path.to_str().expect("utf-8 source path");

    with_cwd(project.path(), || {
        run_add(
            source_arg,
            &["home".to_owned()],
            None,
            None,
            None,
            None,
            false,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add --to must accept a local path source");
    });

    let written = std::fs::read_to_string(&toml_path).expect("run_add must rewrite phora.toml");
    let canonical = std::fs::canonicalize(&source_path).expect("canonicalize source dir");
    let name = canonical
        .file_name()
        .expect("source dir has a basename")
        .to_string_lossy()
        .into_owned();
    let src = source_from(&written, &name);
    assert!(
        src.path.is_some() && src.host.is_none() && src.repo.is_none() && src.git.is_none(),
        "`add --to <local path>` must persist a `path =` source bound to the target, got:\n{written}"
    );
}

#[test]
fn source_rm_command_scrubs_bindings_and_source_def_from_phora_toml() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\
         \n\
         [sources.dotfiles]\n\
         git = \"https://github.com/me/dotfiles.git\"\n\
         \n\
         [sources.loqui]\n\
         git = \"https://github.com/me/loqui.git\"\n\
         \n\
         [targets.editor]\n\
         path = \"~/.config/editor\"\n\
         sources = [\"dotfiles\", { source = \"dotfiles\", as = \"nvim\" }]\n\
         \n\
         [targets.shell]\n\
         path = \"~/.config/shell\"\n\
         sources = [\"loqui\"]\n",
    )
    .expect("seed phora.toml");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Source {
                cmd: SourceCmd::Rm {
                    name: "dotfiles".to_owned(),
                },
            },
        })
        .expect("`phora source rm dotfiles` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path)
        .expect("the command must leave phora.toml on disk in the cwd");
    let config = Config::parse(&written)
        .unwrap_or_else(|e| panic!("scrubbed phora.toml must re-parse: {e}\n{written}"));

    assert!(
        !config.sources.contains_key("dotfiles"),
        "`source rm dotfiles` must drop [sources.dotfiles], got:\n{written}"
    );
    assert!(
        config.sources.contains_key("loqui"),
        "`source rm dotfiles` must NOT touch the unrelated [sources.loqui], got:\n{written}"
    );

    let editor = config
        .targets
        .get("editor")
        .expect("target `editor` must survive a source rm");
    let editor_underlying: Vec<&str> = editor
        .sources
        .iter()
        .flatten()
        .map(crate::config::Binding::source)
        .collect();
    assert!(
        !editor_underlying.contains(&"dotfiles"),
        "every dotfiles binding (bare AND `as = \"nvim\"`) must be scrubbed from target `editor`, got bindings={editor_underlying:?}\n{written}"
    );

    let shell = config
        .targets
        .get("shell")
        .expect("target `shell` must be untouched by a source rm");
    let shell_underlying: Vec<&str> = shell
        .sources
        .iter()
        .flatten()
        .map(crate::config::Binding::source)
        .collect();
    assert_eq!(
        shell_underlying,
        vec!["loqui"],
        "the unrelated target `shell` must still bind exactly `loqui`, got bindings={shell_underlying:?}\n{written}"
    );
}

#[test]
fn insert_source_also_writes_symbolic_host_path_for_colon_alias() {
    let parsed = parse("gitlab:owner/repo");
    let out = insert_source("version = 1\n", &parsed.name, &parsed, None)
        .expect("the second writer must also accept a symbolic AddTarget");

    assert!(
        !out.contains("git ="),
        "both writers must agree: a symbolic add writes no `git =` key, got:\n{out}"
    );
    let src = source_from(&out, "repo");
    assert_eq!(
        src.host.as_deref(),
        Some("gitlab"),
        "insert_source must persist host = \"gitlab\" symbolically"
    );
    assert_eq!(src.repo.as_deref(), Some("owner/repo"));
    assert!(
        src.git.is_none(),
        "insert_source must not expand a symbolic add into a literal git"
    );
}

#[test]
fn symbolic_add_omits_default_protocol_and_writes_non_default() {
    let default_src = parse("github:srnnkls/tropos");
    let default_out = insert_source_with_ref(
        "version = 1\n",
        &default_src.name,
        &default_src,
        None,
        None,
        None,
    )
    .expect("write default-protocol symbolic source");
    assert!(
        !default_out.contains("protocol"),
        "a default (https) protocol must be omitted from the written table, got:\n{default_out}"
    );
    assert!(
        default_out.contains("host = \"github\"")
            && default_out.contains("repo = \"srnnkls/tropos\""),
        "the default-protocol silence is only meaningful if host+repo are still written, got:\n{default_out}"
    );

    let ssh_src = AddTarget {
        protocol: Some(Protocol::Ssh),
        ..parse("github:srnnkls/tropos")
    };
    let ssh_out =
        insert_source_with_ref("version = 1\n", &ssh_src.name, &ssh_src, None, None, None)
            .expect("write non-default-protocol symbolic source");
    assert!(
        ssh_out.contains("protocol = \"ssh\""),
        "a non-default protocol must be written literally as `protocol = \"ssh\"`, got:\n{ssh_out}"
    );
    assert!(
        ssh_out.contains("host = \"github\"") && ssh_out.contains("repo = \"srnnkls/tropos\""),
        "a non-default-protocol source must still carry its symbolic host+repo, got:\n{ssh_out}"
    );
    let src = source_from(&ssh_out, "tropos");
    assert_eq!(
        src.protocol,
        Some(Protocol::Ssh),
        "a non-default protocol on a symbolic source must round-trip through the parser, got:\n{ssh_out}"
    );
}

// ── write_locks / load_locks ───────────────────────────────────

use crate::lock::{Lock, LockedSource};
use crate::sync::Resolution;

fn lock_with(name: &str, git: &str, resolved: &str) -> Lock {
    Lock {
        version: 1,
        sources: vec![LockedSource {
            name: name.to_owned(),
            git: git.to_owned(),
            resolved: resolved.to_owned(),
            commit: "c0ffeec0ffee".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: "blake3:cfg".to_owned(),
            r#ref: None,
        }],
    }
}

#[test]
fn write_locks_base_only_writes_phora_lock_and_no_local_file() {
    let dir = TempDir::new().expect("locks dir");
    let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");

    write_locks(dir.path(), &base, None).expect("write base-only locks");

    assert!(
        dir.path().join("phora.lock").is_file(),
        "base-only write must create phora.lock"
    );
    assert!(
        !dir.path().join("phora.local.lock").exists(),
        "a base-only write (local=None) must NOT create phora.local.lock"
    );
}

#[test]
fn load_locks_round_trips_base_only_write() {
    let dir = TempDir::new().expect("locks dir");
    let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");

    write_locks(dir.path(), &base, None).expect("write base-only locks");
    let (loaded_base, loaded_local) = load_locks(dir.path()).expect("load locks");

    let loaded_base = loaded_base.expect("phora.lock present after a base write");
    assert!(
        loaded_local.is_none(),
        "no phora.local.lock on disk must load as None"
    );
    let src = loaded_base
        .find_source("dotfiles")
        .expect("the base source survives the round-trip");
    assert_eq!(
        src.git, "https://github.com/me/dotfiles.git",
        "round-tripped base lock must echo the source git URL"
    );
    assert_eq!(
        src.resolved, "main",
        "round-tripped base lock must echo the resolved refspec"
    );
    assert_eq!(
        loaded_base.sources.len(),
        1,
        "exactly the one written source must come back"
    );
}

#[test]
fn write_then_load_locks_round_trips_base_and_local() {
    let dir = TempDir::new().expect("locks dir");
    let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");
    let local = lock_with("loqui", "/home/soeren/dev/loqui", "dev");

    write_locks(dir.path(), &base, Some(&local)).expect("write base+local locks");

    assert!(
        dir.path().join("phora.lock").is_file(),
        "phora.lock must exist"
    );
    assert!(
        dir.path().join("phora.local.lock").is_file(),
        "a Some(local) write must create phora.local.lock"
    );

    let (loaded_base, loaded_local) = load_locks(dir.path()).expect("load both locks");
    assert!(
        loaded_base
            .expect("base present")
            .find_source("dotfiles")
            .is_some(),
        "base lock must round-trip its source"
    );
    let local = loaded_local.expect("local lock present when phora.local.lock exists");
    let loqui = local
        .find_source("loqui")
        .expect("local lock must round-trip its overridden source");
    assert_eq!(
        loqui.git, "/home/soeren/dev/loqui",
        "round-tripped local lock must echo the local checkout path"
    );
}

#[test]
fn write_locks_removes_stale_local_lock_when_local_is_none() {
    let dir = TempDir::new().expect("locks dir");
    let base = lock_with("dotfiles", "https://github.com/me/dotfiles.git", "main");
    let local = lock_with("loqui", "/home/soeren/dev/loqui", "dev");

    write_locks(dir.path(), &base, Some(&local)).expect("seed both locks");
    assert!(
        dir.path().join("phora.local.lock").is_file(),
        "premise: phora.local.lock must exist before the base-only rewrite"
    );

    write_locks(dir.path(), &base, None).expect("rewrite base-only");

    assert!(
        !dir.path().join("phora.local.lock").exists(),
        "a base-only rewrite (local=None) must DELETE the stale phora.local.lock"
    );
    let (_, loaded_local) = load_locks(dir.path()).expect("reload after stale removal");
    assert!(
        loaded_local.is_none(),
        "after the stale local lock is removed, load_locks must report no local lock"
    );
}

// ── list_statuses ──────────────────────────────────────────────

/// Writes `file` with `content` under `<target_dir>/<artifact>/` and returns a
/// [`ManifestFile`] whose size+mtime match what landed on disk, so a record built
/// from it reads Clean through `check_artifact_state`.
fn deploy_matching_file(
    target_dir: &Path,
    artifact: &str,
    file: &str,
    content: &[u8],
) -> ManifestFile {
    let artifact_dir = target_dir.join(artifact);
    std::fs::create_dir_all(&artifact_dir).expect("create artifact dir");
    let path = artifact_dir.join(file);
    std::fs::write(&path, content).expect("write deployed file");
    let meta = std::fs::metadata(&path).expect("stat deployed file");
    let mtime = meta
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs();
    ManifestFile {
        path: PathBuf::from(file),
        size: meta.len(),
        mtime,
        blake3: blake3::hash(content).to_hex().to_string(),
    }
}

fn record_for(
    target: &str,
    source: &str,
    artifact: &str,
    commit: &str,
    files: Vec<ManifestFile>,
) -> RegistryRecord {
    RegistryRecord {
        version: 1,
        key: ArtifactKey {
            target: target.to_owned(),
            source: source.to_owned(),
            artifact: artifact.to_owned(),
        },
        source: source.to_owned(),
        commit: commit.to_owned(),
        digest: "blake3:rec".to_owned(),
        projected_at: "2026-01-31T12:34:56Z".to_owned(),
        layout: "flat".to_owned(),
        allow_symlinks: false,
        preserve_executable: true,
        files,
        linked: false,
        vars_digest: None,
    }
}

fn config_one_flat_target(target: &str, source: &str, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \"https://example.com/x.git\"\nbranch = \"main\"\n\n\
             [targets.{target}]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"flat\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("one-target flat config parses")
}

fn config_two_flat_targets(
    target_a: &str,
    source_a: &str,
    path_a: &Path,
    target_b: &str,
    source_b: &str,
    path_b: &Path,
) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source_a}]\ngit = \"https://example.com/a.git\"\nbranch = \"main\"\n\n\
             [sources.{source_b}]\ngit = \"https://example.com/b.git\"\nbranch = \"main\"\n\n\
             [targets.{target_a}]\npath = \"{}\"\nsources = [\"{source_a}\"]\nlayout = \"flat\"\n\n\
             [targets.{target_b}]\npath = \"{}\"\nsources = [\"{source_b}\"]\nlayout = \"flat\"\n",
        path_a.display(),
        path_b.display(),
    );
    Config::parse(&toml).expect("two-target flat config parses")
}

fn status_for<'a>(
    listings: &'a [TargetListing],
    target: &str,
    artifact: &str,
) -> Option<&'a ArtifactStatus> {
    listings
        .iter()
        .find(|l| l.target == target)
        .and_then(|l| l.artifacts.iter().find(|a| a.artifact == artifact))
}

#[test]
fn list_statuses_reports_clean_for_matching_deployment() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

    let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
    reg.put(&record_for(
        "dest",
        "editor-src",
        "editor",
        "aaa111",
        vec![mf],
    ))
    .expect("seed registry record");

    let listings = list_statuses(&cfg, &reg).expect("list statuses");

    let st = status_for(&listings, "dest", "editor")
        .expect("the editor artifact must appear under target dest");
    assert_eq!(
        st.source, "editor-src",
        "the status row must carry the artifact's source"
    );
    assert!(
        st.state.contains('✓') || st.state.to_lowercase().contains("clean"),
        "a deployment whose files match its record must read Clean (✓), got state {:?}",
        st.state
    );
    assert!(
        !st.state.to_lowercase().contains("modified"),
        "a matching deployment must NOT be labelled modified, got {:?}",
        st.state
    );
}

#[test]
fn list_statuses_reports_modified_for_edited_deployment() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

    let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
    reg.put(&record_for(
        "dest",
        "editor-src",
        "editor",
        "aaa111",
        vec![mf],
    ))
    .expect("seed an accurate (would-be-Clean) registry record");

    // Record stays accurate; the deployed file drifts on disk (real user edit).
    std::fs::write(
        target_root.path().join("editor").join("init.lua"),
        b"-- init\nvim.opt.number = true\n",
    )
    .expect("edit deployed file on disk");

    let listings = list_statuses(&cfg, &reg).expect("list statuses");

    let st = status_for(&listings, "dest", "editor")
        .expect("the editor artifact must appear even when modified");
    assert!(
        st.state.to_lowercase().contains("modified"),
        "a deployment whose on-disk file differs from its record must read Modified, got {:?}",
        st.state
    );
    assert!(
        !st.state.contains('✓'),
        "a Modified artifact must NOT be shown as clean (✓), got {:?}",
        st.state
    );
}

#[test]
fn list_statuses_reports_ejected_for_ejected_artifact() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

    let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
    reg.put(&record_for(
        "dest",
        "editor-src",
        "editor",
        "aaa111",
        vec![mf],
    ))
    .expect("seed registry record");
    reg.save_ejected(
        "dest",
        &[crate::store::EjectedEntry {
            source: "editor-src".to_owned(),
            artifact: "editor".to_owned(),
            ejected_at: "2026-01-31T14:00:00Z".to_owned(),
        }],
    )
    .expect("mark editor ejected");

    let listings = list_statuses(&cfg, &reg).expect("list statuses");

    let st =
        status_for(&listings, "dest", "editor").expect("an ejected artifact must still be listed");
    assert!(
        st.state.to_lowercase().contains("ejected"),
        "an artifact in the target's ejected list must read Ejected, got {:?}",
        st.state
    );
}

#[test]
fn list_statuses_groups_by_target_and_names_source_and_artifact() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let root_a = TempDir::new().expect("target a root");
    let root_b = TempDir::new().expect("target b root");
    let cfg = config_two_flat_targets(
        "home",
        "editor-src",
        root_a.path(),
        "xdg",
        "snippets-src",
        root_b.path(),
    );

    let lua = deploy_matching_file(root_a.path(), "editor", "init.lua", b"-- init\n");
    reg.put(&record_for(
        "home",
        "editor-src",
        "editor",
        "aaa111",
        vec![lua],
    ))
    .expect("seed editor record under home");
    let json = deploy_matching_file(root_b.path(), "snippets", "py.json", b"{}\n");
    reg.put(&record_for(
        "xdg",
        "snippets-src",
        "snippets",
        "bbb222",
        vec![json],
    ))
    .expect("seed snippets record under xdg");

    let listings = list_statuses(&cfg, &reg).expect("list statuses");

    assert_eq!(
        listings.len(),
        2,
        "each configured target must get its own listing entry, got {listings:?}"
    );

    let home = listings
        .iter()
        .find(|l| l.target == "home")
        .expect("the home target must be present as its own grouping");
    let home_names: Vec<&str> = home.artifacts.iter().map(|a| a.artifact.as_str()).collect();
    assert_eq!(
        home_names,
        vec!["editor"],
        "the home group must carry only its own editor artifact, got {home_names:?}"
    );
    assert!(
        home.artifacts.iter().all(|a| a.source == "editor-src"),
        "every row in the home group must name the home source, got {:?}",
        home.artifacts
    );

    let xdg = listings
        .iter()
        .find(|l| l.target == "xdg")
        .expect("the xdg target must be present as its own grouping");
    let xdg_names: Vec<&str> = xdg.artifacts.iter().map(|a| a.artifact.as_str()).collect();
    assert_eq!(
        xdg_names,
        vec!["snippets"],
        "the xdg group must carry only its own snippets artifact, got {xdg_names:?}"
    );
    assert!(
        xdg.artifacts.iter().all(|a| a.source == "snippets-src"),
        "every row in the xdg group must name the xdg source, got {:?}",
        xdg.artifacts
    );

    assert!(
        !xdg_names.contains(&"editor"),
        "an artifact deployed under home must NOT leak into the xdg group, got {xdg_names:?}"
    );
    assert!(
        !home_names.contains(&"snippets"),
        "an artifact deployed under xdg must NOT leak into the home group, got {home_names:?}"
    );
}

// ── mapped-leaf observability (T8) ─────────────────────────────

/// Single file at `<target_dir>/<dest>` (no artifact dir) with size+mtime matched to disk so the mapped record reads Clean.
fn deploy_mapped_file(target_dir: &Path, dest: &str, content: &[u8]) -> ManifestFile {
    let path = target_dir.join(dest);
    std::fs::write(&path, content).expect("write mapped dest file");
    let meta = std::fs::metadata(&path).expect("stat mapped dest file");
    let mtime = meta
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs();
    ManifestFile {
        path: PathBuf::from(dest),
        size: meta.len(),
        mtime,
        blake3: blake3::hash(content).to_hex().to_string(),
    }
}

fn mapped_record(
    target: &str,
    source: &str,
    dest: &str,
    commit: &str,
    files: Vec<ManifestFile>,
) -> RegistryRecord {
    RegistryRecord {
        version: 1,
        key: ArtifactKey {
            target: target.to_owned(),
            source: source.to_owned(),
            artifact: dest.to_owned(),
        },
        source: source.to_owned(),
        commit: commit.to_owned(),
        digest: "blake3:map".to_owned(),
        projected_at: "2026-01-31T12:34:56Z".to_owned(),
        layout: crate::store::MAP_LAYOUT.to_owned(),
        allow_symlinks: false,
        preserve_executable: true,
        files,
        linked: false,
        vars_digest: None,
    }
}

fn config_one_by_source_target(target: &str, source: &str, target_path: &Path) -> Config {
    let toml = format!(
        "version = 1\n\n\
             [sources.{source}]\ngit = \"https://example.com/x.git\"\nbranch = \"main\"\n\n\
             [targets.{target}]\npath = \"{}\"\nsources = [\"{source}\"]\nlayout = \"by-source\"\n",
        target_path.display(),
    );
    Config::parse(&toml).expect("one-target by-source config parses")
}

#[test]
fn where_resolves_a_mapped_record_under_its_dest_name() {
    let dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
    reg.put(&mapped_record(
        "dest",
        "fzf-src",
        "fzf.zsh",
        "aaa111",
        vec![ManifestFile {
            path: PathBuf::from("fzf.zsh"),
            size: 10,
            mtime: 1,
            blake3: "x".to_owned(),
        }],
    ))
    .expect("seed mapped record");

    let filter = WhereFilter {
        artifact: Some("fzf.zsh".to_owned()),
        ..WhereFilter::default()
    };
    let matches = where_cmd(&reg, &filter).expect("where by mapped dest");

    let m = find(&matches, "fzf-src", "fzf.zsh")
        .expect("a mapped record must resolve under its dest name fzf.zsh");
    assert_eq!(
        m.targets,
        vec!["dest".to_owned()],
        "the mapped record must report the target it lands in, got {:?}",
        m.targets
    );
}

#[test]
fn list_statuses_reports_mapped_dest_path_without_layout_leak() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_by_source_target("dest", "fzf-src", target_root.path());

    let mf = deploy_mapped_file(target_root.path(), "fzf.zsh", b"# fzf\n");
    reg.put(&mapped_record(
        "dest",
        "fzf-src",
        "fzf.zsh",
        "aaa111",
        vec![mf],
    ))
    .expect("seed mapped record");

    let listings = list_statuses(&cfg, &reg).expect("list statuses");

    let st = status_for(&listings, "dest", "fzf.zsh")
        .expect("the mapped artifact must appear under target dest by its dest name");
    assert_eq!(
        st.source, "fzf-src",
        "the mapped status row must carry its source"
    );
    assert!(
        st.state.contains('✓') || st.state.to_lowercase().contains("clean"),
        "the mapped dest file at target/<dest> must read Clean (✓) — a layout-derived \
         path would miss it and read Missing, got {:?}",
        st.state
    );
    assert!(
        !st.state.to_lowercase().contains("missing"),
        "a mapped record must NOT be reported Missing — the by-source layout must not \
         leak onto the dest path, got {:?}",
        st.state
    );
}

#[test]
fn eject_keeps_mapped_file_and_marks_record_ejected() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_by_source_target("dest", "fzf-src", target_root.path());

    let mf = deploy_mapped_file(target_root.path(), "fzf.zsh", b"# fzf\n");
    reg.put(&mapped_record(
        "dest",
        "fzf-src",
        "fzf.zsh",
        "aaa111",
        vec![mf],
    ))
    .expect("seed mapped record");

    crate::sync::eject(&cfg, &reg, "fzf.zsh", "fzf-src", "dest").expect("eject mapped leaf");

    assert!(
        target_root.path().join("fzf.zsh").exists(),
        "eject must keep the mapped dest file on disk"
    );
    let listings = list_statuses(&cfg, &reg).expect("list statuses after eject");
    let st = status_for(&listings, "dest", "fzf.zsh")
        .expect("an ejected mapped artifact must still be listed");
    assert!(
        st.state.to_lowercase().contains("ejected"),
        "a mapped artifact in the target's ejected list must read Ejected, got {:?}",
        st.state
    );
}

#[test]
fn uneject_round_trips_a_mapped_record_back_to_managed() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_by_source_target("dest", "fzf-src", target_root.path());

    let mf = deploy_mapped_file(target_root.path(), "fzf.zsh", b"# fzf\n");
    reg.put(&mapped_record(
        "dest",
        "fzf-src",
        "fzf.zsh",
        "aaa111",
        vec![mf],
    ))
    .expect("seed mapped record");

    crate::sync::eject(&cfg, &reg, "fzf.zsh", "fzf-src", "dest").expect("eject mapped leaf");
    crate::sync::uneject(&cfg, &reg, "fzf.zsh", "fzf-src", "dest").expect("uneject mapped leaf");

    let listings = list_statuses(&cfg, &reg).expect("list statuses after uneject");
    let st = status_for(&listings, "dest", "fzf.zsh")
        .expect("an unejected mapped artifact must still be listed");
    assert!(
        !st.state.to_lowercase().contains("ejected"),
        "uneject must clear the ejected mark — the mapped record reads as managed again, got {:?}",
        st.state
    );
    assert!(
        st.state.contains('✓') || st.state.to_lowercase().contains("clean"),
        "after uneject the matching mapped dest file must read Clean (✓), got {:?}",
        st.state
    );
}

// ── resolution_from_char ───────────────────────────────────────

#[test]
fn resolution_from_char_maps_skip() {
    assert_eq!(
        resolution_from_char('s'),
        Some(Resolution::Skip),
        "`s` must map to Skip"
    );
}

#[test]
fn resolution_from_char_maps_overwrite() {
    assert_eq!(
        resolution_from_char('o'),
        Some(Resolution::Overwrite),
        "`o` must map to Overwrite"
    );
}

#[test]
fn resolution_from_char_maps_eject() {
    assert_eq!(
        resolution_from_char('e'),
        Some(Resolution::Eject),
        "`e` must map to Eject"
    );
}

#[test]
fn resolution_from_char_maps_abort() {
    assert_eq!(
        resolution_from_char('a'),
        Some(Resolution::Abort),
        "`a` must map to Abort"
    );
}

#[test]
fn resolution_from_char_rejects_unknown() {
    assert_eq!(
        resolution_from_char('x'),
        None,
        "an unrecognized prompt character must map to None, not a default Resolution"
    );
}

// `phora add --local` / `--symlink` (ALS-001): local-overlay path sources.

use crate::config::{DeployMode, Remote};

/// Parses the named source out of a written overlay/config into its typed form.
fn parsed_source_from(text: &str, name: &str) -> ParsedSource {
    let raw = Config::parse(text)
        .unwrap_or_else(|e| panic!("written toml must parse: {e}\n{text}"))
        .sources
        .remove(name)
        .unwrap_or_else(|| panic!("source `{name}` must be present in:\n{text}"));
    ParsedSource::parse(name, &raw)
        .unwrap_or_else(|e| panic!("source `{name}` must parse to typed form: {e}"))
}

#[test]
fn add_local_writes_path_local_path_to_phora_local_toml() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let src_dir = tempfile::TempDir::new().expect("temp source dir");
    let abspath = std::fs::canonicalize(src_dir.path()).expect("canonicalize source dir");

    with_cwd(dir.path(), || {
        run_add(
            src_dir.path().to_str().expect("utf8 source path"),
            &[],
            Some("mysrc".to_owned()),
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add --local must succeed for an existing dir");
    });

    let overlay = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("--local must write phora.local.toml in the cwd");
    let src = source_from(&overlay, "mysrc");
    assert_eq!(
        src.path.as_deref(),
        Some(abspath.to_string_lossy().as_ref()),
        "--local must persist path = <canonical abspath>, got:\n{overlay}"
    );
    assert!(
        src.deploy.is_none(),
        "--local (copy mode) must NOT write a deploy key, got:\n{overlay}"
    );
    assert!(
        !dir.path().join("phora.toml").exists(),
        "--local must not create or touch phora.toml"
    );
}

#[test]
fn add_symlink_writes_path_and_deploy_link_to_local_toml() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let src_dir = tempfile::TempDir::new().expect("temp source dir");
    let abspath = std::fs::canonicalize(src_dir.path()).expect("canonicalize source dir");

    with_cwd(dir.path(), || {
        run_add(
            src_dir.path().to_str().expect("utf8 source path"),
            &[],
            Some("linked".to_owned()),
            None,
            None,
            None,
            false,
            true,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add --symlink must succeed for an existing dir");
    });

    let overlay = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("--symlink must write phora.local.toml in the cwd");
    let src = source_from(&overlay, "linked");
    assert_eq!(
        src.path.as_deref(),
        Some(abspath.to_string_lossy().as_ref()),
        "--symlink must persist path = <canonical abspath>, got:\n{overlay}"
    );
    assert_eq!(
        src.deploy,
        Some(DeployMode::Link),
        "--symlink must persist deploy = \"link\", got:\n{overlay}"
    );
}

#[test]
fn add_local_infers_name_from_path_basename() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let parent = tempfile::TempDir::new().expect("temp parent dir");
    let src_dir = parent.path().join("widgets");
    std::fs::create_dir(&src_dir).expect("create named source dir");
    let abspath = std::fs::canonicalize(&src_dir).expect("canonicalize source dir");
    let basename = abspath
        .file_name()
        .expect("canonical path has a basename")
        .to_string_lossy()
        .into_owned();

    with_cwd(dir.path(), || {
        run_add(
            src_dir.to_str().expect("utf8 source path"),
            &[],
            None,
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add --local with no --name must succeed");
    });

    let overlay = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("--local must write phora.local.toml");
    let config = Config::parse(&overlay).expect("overlay must parse");
    assert!(
        config.sources.contains_key(&basename),
        "omitting --name must key the source by the canonical basename `{basename}`, got keys: {:?}",
        config.sources.keys().collect::<Vec<_>>()
    );
}

#[test]
fn add_symlink_implies_local_overlay_and_is_valid() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let src_dir = tempfile::TempDir::new().expect("temp source dir");
    let abspath = std::fs::canonicalize(src_dir.path()).expect("canonicalize source dir");

    with_cwd(dir.path(), || {
        run_add(
            src_dir.path().to_str().expect("utf8 source path"),
            &[],
            Some("app".to_owned()),
            None,
            None,
            None,
            false,
            true,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add --symlink must succeed");
    });

    let overlay = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("--symlink alone must write phora.local.toml");
    let parsed = parsed_source_from(&overlay, "app");
    assert_eq!(
        parsed.deploy_mode(),
        DeployMode::Link,
        "the typed overlay source must report deploy_mode == Link"
    );
    match &parsed.remote {
        Remote::Path(p) => assert_eq!(
            p.as_str(),
            abspath.to_string_lossy().as_ref(),
            "the typed overlay source must carry Remote::Path(<abspath>)"
        ),
        other => panic!("--symlink must produce a Remote::Path, got {other:?}"),
    }
}

#[test]
fn add_local_and_symlink_together_equals_symlink() {
    let src_dir = tempfile::TempDir::new().expect("temp source dir");
    let abspath = std::fs::canonicalize(src_dir.path()).expect("canonicalize source dir");

    let overlay_for = |local: bool, symlink: bool| -> String {
        let dir = tempfile::TempDir::new().expect("temp project dir");
        with_cwd(dir.path(), || {
            run_add(
                src_dir.path().to_str().expect("utf8 source path"),
                &[],
                Some("s".to_owned()),
                None,
                None,
                None,
                local,
                symlink,
                &config_edit::BindRefinement::default(),
            )
            .expect("run_add must not error");
        });
        std::fs::read_to_string(dir.path().join("phora.local.toml"))
            .expect("must write phora.local.toml")
    };

    let symlink_only = overlay_for(false, true);
    let both = overlay_for(true, true);

    let symlink_src = source_from(&symlink_only, "s");
    let both_src = source_from(&both, "s");

    assert_eq!(
        both_src.path.as_deref(),
        Some(abspath.to_string_lossy().as_ref()),
        "both-flags path must equal the canonical abspath, got:\n{both}"
    );
    assert_eq!(
        symlink_src.path.as_deref(),
        Some(abspath.to_string_lossy().as_ref()),
        "symlink-only path must equal the canonical abspath, got:\n{symlink_only}"
    );
    assert_eq!(
        both_src.deploy,
        Some(DeployMode::Link),
        "both-flags must deploy = \"link\", got:\n{both}"
    );
    assert_eq!(
        symlink_src.deploy,
        Some(DeployMode::Link),
        "symlink-only must deploy = \"link\", got:\n{symlink_only}"
    );
    assert_eq!(
        both_src.path, symlink_src.path,
        "local+symlink together must yield the same path source as --symlink alone"
    );
    assert_eq!(
        both_src.deploy, symlink_src.deploy,
        "local+symlink together must yield the same deploy as --symlink alone"
    );
}

#[test]
fn add_without_flags_still_writes_phora_toml() {
    let dir = tempfile::TempDir::new().expect("temp project dir");

    with_cwd(dir.path(), || {
        run_add(
            "github:srnnkls/tropos",
            &[],
            None,
            None,
            None,
            None,
            false,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add with no overlay flags must keep its remote behavior");
    });

    let written = std::fs::read_to_string(dir.path().join("phora.toml"))
        .expect("no-flags add must write phora.toml");
    let src = source_from(&written, "tropos");
    assert_eq!(
        src.host.as_deref(),
        Some("github"),
        "no-flags add must persist the remote host, got:\n{written}"
    );
    assert!(
        src.path.is_none(),
        "no-flags add must not write a path source, got:\n{written}"
    );
    assert!(
        !dir.path().join("phora.local.toml").exists(),
        "no-flags add must not create phora.local.toml"
    );
}

#[test]
fn add_local_canonicalizes_relative_path_to_absolute() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    std::fs::create_dir(dir.path().join("sub")).expect("create relative subdir");
    let expected = std::fs::canonicalize(dir.path().join("sub")).expect("canonicalize subdir");

    with_cwd(dir.path(), || {
        run_add(
            "sub",
            &[],
            Some("s".to_owned()),
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("run_add --local must accept a relative existing path");
    });

    let overlay = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("--local must write phora.local.toml");
    let src = source_from(&overlay, "s");
    let written = src.path.expect("a path source must be written");
    assert!(
        std::path::Path::new(&written).is_absolute(),
        "a relative input must be written as an absolute path, got `{written}`"
    );
    assert_eq!(
        written,
        expected.to_string_lossy(),
        "the written path must equal std::fs::canonicalize of the relative input"
    );
}

#[test]
fn add_local_errors_when_path_does_not_exist() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let missing = "does-not-exist-xyz";

    let err = with_cwd(dir.path(), || {
        run_add(
            missing,
            &[],
            Some("s".to_owned()),
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect_err("--local on a nonexistent path must error")
    });

    assert!(
        matches!(err, Error::Config(_)),
        "a missing local path must yield Error::Config, got {err:?}"
    );
    assert!(
        err.to_string().contains(missing),
        "the error message must name the offending path `{missing}`, got: {err}"
    );
    assert!(
        !dir.path().join("phora.local.toml").exists(),
        "a failed --local must not create phora.local.toml"
    );
    assert!(
        !dir.path().join("phora.toml").exists(),
        "a failed --local must not create phora.toml"
    );
}

#[test]
fn add_local_rejects_non_directory_path() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"not a dir").expect("create regular file");
    let file_str = file.to_str().expect("utf8 file path").to_owned();

    let err = with_cwd(dir.path(), || {
        run_add(
            &file_str,
            &[],
            Some("s".to_owned()),
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect_err("--local on a regular file must error")
    });

    assert!(
        matches!(err, Error::Config(_)),
        "a non-directory local path must yield Error::Config, got {err:?}"
    );
    assert!(
        err.to_string().contains(&file_str),
        "the error message must name the offending file path `{file_str}`, got: {err}"
    );
    assert!(
        !dir.path().join("phora.local.toml").exists(),
        "a failed --local must not create phora.local.toml"
    );
    assert!(
        !dir.path().join("phora.toml").exists(),
        "a failed --local must not create phora.toml"
    );
}

#[test]
fn add_local_preserves_siblings_and_replaces_same_name_in_overlay() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    std::fs::write(
        dir.path().join("phora.local.toml"),
        "version = 1\n\n[sources.other]\ngit = \"https://example.com/other.git\"\n",
    )
    .expect("seed an existing overlay with a sibling source");

    let first = tempfile::TempDir::new().expect("temp first source dir");
    let second = tempfile::TempDir::new().expect("temp second source dir");
    let second_abs = std::fs::canonicalize(second.path()).expect("canonicalize second dir");

    with_cwd(dir.path(), || {
        run_add(
            first.path().to_str().expect("utf8"),
            &[],
            Some("mine".to_owned()),
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("adding a new overlay source must succeed");
    });

    let after_add = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("overlay still present after add");
    let config = Config::parse(&after_add).expect("overlay parses after add");
    assert!(
        config.sources.contains_key("other"),
        "adding a new overlay source must preserve the sibling `other`, got:\n{after_add}"
    );
    assert!(
        config.sources.contains_key("mine"),
        "the new source `mine` must be present, got:\n{after_add}"
    );

    with_cwd(dir.path(), || {
        run_add(
            second.path().to_str().expect("utf8"),
            &[],
            Some("mine".to_owned()),
            None,
            None,
            None,
            true,
            false,
            &config_edit::BindRefinement::default(),
        )
        .expect("re-adding the same name must succeed");
    });

    let after_replace = std::fs::read_to_string(dir.path().join("phora.local.toml"))
        .expect("overlay present after replace");
    let replaced = source_from(&after_replace, "mine");
    assert_eq!(
        replaced.path.as_deref(),
        Some(second_abs.to_string_lossy().as_ref()),
        "re-adding the same name must replace its path with the new dir, got:\n{after_replace}"
    );
    assert!(
        Config::parse(&after_replace)
            .expect("overlay parses after replace")
            .sources
            .contains_key("other"),
        "replacing one source must leave the sibling `other` intact, got:\n{after_replace}"
    );
}

#[test]
fn add_symlink_overlay_overrides_base_source_after_merge() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    std::fs::write(
        dir.path().join("phora.toml"),
        "version = 1\n\n[sources.app]\nhost = \"github\"\nrepo = \"srnnkls/app\"\n",
    )
    .expect("seed base phora.toml with a forge source");

    let src_dir = tempfile::TempDir::new().expect("temp source dir");
    let abspath = std::fs::canonicalize(src_dir.path()).expect("canonicalize source dir");

    with_cwd(dir.path(), || {
        run_add(
            src_dir.path().to_str().expect("utf8"),
            &[],
            Some("app".to_owned()),
            None,
            None,
            None,
            false,
            true,
            &config_edit::BindRefinement::default(),
        )
        .expect("--symlink --name app must write the overlay");
    });

    let base = Config::parse(
        &std::fs::read_to_string(dir.path().join("phora.toml")).expect("base phora.toml present"),
    )
    .expect("base config parses");
    let overlay = Config::parse(
        &std::fs::read_to_string(dir.path().join("phora.local.toml"))
            .expect("overlay phora.local.toml present"),
    )
    .expect("overlay config parses");

    let merged = crate::config::merge_configs(base, Some(overlay));
    let effective = merged
        .sources
        .get("app")
        .expect("merged config must keep source `app`");
    assert_eq!(
        effective.path.as_deref(),
        Some(abspath.to_string_lossy().as_ref()),
        "the overlay path must win over the base forge source after merge"
    );
    assert_eq!(
        effective.deploy,
        Some(DeployMode::Link),
        "the overlay deploy = link must win after merge"
    );
}

// ── remove_source: scrub bindings out of every target ──────────

fn binding_sources(cfg: &Config, target: &str) -> Vec<String> {
    use crate::config::Binding;
    cfg.targets
        .get(target)
        .and_then(|t| t.sources.as_deref())
        .unwrap_or(&[])
        .iter()
        .map(|b| match b {
            Binding::Source(name) => name.clone(),
            Binding::Refined(r) => r.source.clone(),
        })
        .collect()
}

#[test]
fn remove_source_drops_bare_binding_of_named_source_from_target() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = [\"dotfiles\", \"loqui\"]\n";

    let out = remove_source(toml, "version = 1\n", "dotfiles")
        .expect("scrub the dotfiles source")
        .main;
    let cfg = Config::parse(&out).expect("scrubbed text is valid phora.toml");

    let remaining = binding_sources(&cfg, "editor");
    assert!(
        !remaining.iter().any(|s| s == "dotfiles"),
        "`phora source rm dotfiles` must drop the bare `dotfiles` binding from target `editor`, \
         got: {remaining:?}"
    );
    assert!(
        remaining.iter().any(|s| s == "loqui"),
        "a binding of a different source (`loqui`) must be left untouched, got: {remaining:?}"
    );
}

#[test]
fn remove_source_drops_aliased_table_binding_by_underlying_source_field() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\n\
        sources = [{ source = \"dotfiles\", as = \"nvim\" }, \"loqui\"]\n";

    let out = remove_source(toml, "version = 1\n", "dotfiles")
        .expect("scrub the dotfiles source")
        .main;
    let cfg = Config::parse(&out).expect("scrubbed text is valid phora.toml");

    let remaining = binding_sources(&cfg, "editor");
    assert!(
        !remaining.iter().any(|s| s == "dotfiles"),
        "`phora source rm dotfiles` must drop the aliased `{{ source = dotfiles, as = nvim }}` \
         binding by matching its underlying `source` field, not the `as` string `nvim`, so the \
         binding is not left orphaned; got: {remaining:?}"
    );
    assert!(
        remaining.iter().any(|s| s == "loqui"),
        "a binding of a different source (`loqui`) must survive the scrub, got: {remaining:?}"
    );
}

#[test]
fn remove_source_also_removes_the_source_definition() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = [\"dotfiles\"]\n";

    let out = remove_source(toml, "version = 1\n", "dotfiles")
        .expect("scrub the dotfiles source")
        .main;
    let cfg = Config::parse(&out).expect("scrubbed text is valid phora.toml");

    assert!(
        !cfg.sources.contains_key("dotfiles"),
        "`phora source rm dotfiles` must remove the `[sources.dotfiles]` definition itself"
    );
}

#[test]
fn remove_source_scrubs_every_target_and_leaves_others_intact() {
    let untouched_target = "[targets.solo]\npath = \"~/.solo\"\nsources = [\"loqui\"]\n";
    let toml = format!(
        "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = [\"dotfiles\", \"loqui\"]\n\n\
        [targets.shell]\npath = \"~/.shell\"\n\
        sources = [{{ source = \"dotfiles\", as = \"nvim\" }}, \"loqui\"]\n\n\
        {untouched_target}"
    );

    let out = remove_source(&toml, "version = 1\n", "dotfiles")
        .expect("scrub the dotfiles source")
        .main;
    let cfg = Config::parse(&out).expect("scrubbed text is valid phora.toml");

    assert!(
        !binding_sources(&cfg, "editor")
            .iter()
            .any(|s| s == "dotfiles"),
        "the bare dotfiles binding must be scrubbed from target `editor`"
    );
    assert!(
        !binding_sources(&cfg, "shell")
            .iter()
            .any(|s| s == "dotfiles"),
        "the aliased dotfiles binding must be scrubbed from the SECOND target `shell` too: \
         the scrub must sweep EVERY target, not just the first"
    );
    assert!(
        binding_sources(&cfg, "editor").iter().any(|s| s == "loqui"),
        "the loqui binding in `editor` must survive"
    );
    assert!(
        binding_sources(&cfg, "shell").iter().any(|s| s == "loqui"),
        "the loqui binding in `shell` must survive"
    );

    assert!(
        out.contains(untouched_target),
        "the loqui-only target `solo` binds no removed source and must be left byte-for-byte \
         unchanged, got:\n{out}"
    );
    assert!(
        !cfg.sources.contains_key("dotfiles"),
        "the `[sources.dotfiles]` definition itself must be removed"
    );
}

#[test]
fn source_rm_keeps_binding_whose_alias_matches_but_source_differs() {
    let toml = "version = 1\n\n\
        [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\n\
        sources = [{ source = \"loqui\", as = \"dotfiles\" }, \"dotfiles\"]\n";

    let out = remove_source(toml, "version = 1\n", "dotfiles")
        .expect("scrub the dotfiles source")
        .main;
    let cfg = Config::parse(&out).expect("scrubbed text is valid phora.toml");

    let remaining = binding_sources(&cfg, "editor");
    assert!(
        remaining.iter().any(|s| s == "loqui"),
        "`source rm dotfiles` must KEEP the `{{ source = loqui, as = dotfiles }}` binding: it is \
         matched by its underlying `source` (loqui), not its alias `dotfiles`, got: {remaining:?}"
    );
    assert!(
        !remaining.iter().any(|s| s == "dotfiles"),
        "the bare `dotfiles` binding (source = dotfiles) must be removed, got: {remaining:?}"
    );
}

// ── bind / unbind: per-binding refinement, mixed-array aware ────

fn binding_identities(cfg: &Config, target: &str) -> Vec<String> {
    cfg.targets
        .get(target)
        .and_then(|t| t.sources.as_deref())
        .unwrap_or(&[])
        .iter()
        .map(|b| b.identity().to_owned())
        .collect()
}

fn refined_binding(cfg: &Config, target: &str, identity: &str) -> crate::config::RefinedBinding {
    cfg.targets
        .get(target)
        .and_then(|t| t.sources.as_deref())
        .unwrap_or(&[])
        .iter()
        .find_map(|b| match b {
            crate::config::Binding::Refined(r) if b.identity() == identity => Some((**r).clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("target `{target}` must hold a TABLE binding with identity `{identity}`")
        })
}

#[test]
fn bind_with_refinement_flags_writes_a_table_entry() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = []\n";

    let out = config_edit::bind(
        toml,
        "editor",
        &["dotfiles".to_owned()],
        &config_edit::BindRefinement {
            r#as: Some("nvim".to_owned()),
            root: Some("nvim".to_owned()),
            include: Vec::new(),
            exclude: Vec::new(),
            ..config_edit::BindRefinement::default()
        },
    )
    .expect("bind with refinement flags must succeed")
    .text;

    let cfg = Config::parse(&out)
        .unwrap_or_else(|e| panic!("bind output must be valid phora.toml: {e}\n{out}"));
    let refined = refined_binding(&cfg, "editor", "nvim");
    assert_eq!(
        refined.source, "dotfiles",
        "ANY refinement flag must write a TABLE binding carrying `source = \"dotfiles\"`, got:\n{out}"
    );
    assert_eq!(
        refined.r#as.as_deref(),
        Some("nvim"),
        "`--as nvim` must land as `as = \"nvim\"` in the table binding, got:\n{out}"
    );
    assert_eq!(
        refined.root.as_deref(),
        Some(std::path::Path::new("nvim")),
        "`--root nvim` must land as `root = \"nvim\"` in the table binding, got:\n{out}"
    );
}

#[test]
fn bind_with_no_flags_writes_a_bare_string() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = []\n";

    let out = config_edit::bind(
        toml,
        "editor",
        &["dotfiles".to_owned()],
        &config_edit::BindRefinement::default(),
    )
    .expect("bind with no flags must succeed")
    .text;

    let cfg = Config::parse(&out)
        .unwrap_or_else(|e| panic!("bind output must be valid phora.toml: {e}\n{out}"));
    let editor = cfg
        .targets
        .get("editor")
        .expect("target editor survives bind");
    let bindings = editor.sources.as_deref().unwrap_or(&[]);
    assert!(
        matches!(bindings.first(), Some(crate::config::Binding::Source(s)) if s == "dotfiles"),
        "bind with NO refinement flags must append a BARE STRING, not a table, got:\n{out}"
    );
}

#[test]
fn bind_appends_into_an_array_that_already_holds_a_table_entry() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\n\
        sources = [{ source = \"dotfiles\", as = \"nvim\" }]\n";

    let out = config_edit::bind(
        toml,
        "editor",
        &["loqui".to_owned()],
        &config_edit::BindRefinement::default(),
    )
    .expect("bind must READ a mixed array holding a table entry WITHOUT erroring")
    .text;

    let cfg = Config::parse(&out)
        .unwrap_or_else(|e| panic!("bind output must be valid phora.toml: {e}\n{out}"));
    let identities = binding_identities(&cfg, "editor");
    assert!(
        identities.iter().any(|i| i == "nvim"),
        "the pre-existing aliased table binding (identity `nvim`) must be preserved, got: {identities:?}\n{out}"
    );
    assert!(
        identities.iter().any(|i| i == "loqui"),
        "the newly bound bare source `loqui` must be appended, got: {identities:?}\n{out}"
    );
}

#[test]
fn bind_as_with_multiple_sources_errors() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = []\n";

    let result = config_edit::bind(
        toml,
        "editor",
        &["dotfiles".to_owned(), "loqui".to_owned()],
        &config_edit::BindRefinement {
            r#as: Some("nvim".to_owned()),
            ..config_edit::BindRefinement::default()
        },
    );

    assert!(
        result.is_err(),
        "`--as` is a single identity; binding it across 2 sources is ambiguous and must error"
    );
}

#[test]
fn unbind_removes_the_aliased_entry_by_identity() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\n\
        sources = [\"dotfiles\", { source = \"dotfiles\", as = \"nvim\" }]\n";

    let out = config_edit::unbind(toml, "editor", &["nvim".to_owned()])
        .expect("unbind by identity must succeed")
        .text;

    let cfg = Config::parse(&out)
        .unwrap_or_else(|e| panic!("unbind output must be valid phora.toml: {e}\n{out}"));
    let identities = binding_identities(&cfg, "editor");
    assert!(
        !identities.iter().any(|i| i == "nvim"),
        "`unbind nvim` must drop the ALIASED `{{ source = dotfiles, as = nvim }}` binding by IDENTITY, got: {identities:?}\n{out}"
    );
    assert!(
        identities.iter().any(|i| i == "dotfiles"),
        "the bare `dotfiles` binding (identity `dotfiles`) must survive unbinding the alias `nvim`, got: {identities:?}\n{out}"
    );
}

#[test]
fn string_only_bind_then_unbind_is_byte_identical_to_today() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = []\n";

    let bound = config_edit::bind(
        toml,
        "editor",
        &["dotfiles".to_owned()],
        &config_edit::BindRefinement::default(),
    )
    .expect("string-only bind must succeed")
    .text;
    assert!(
        bound.contains("sources = [\"dotfiles\"]"),
        "a string-only bind must produce the same bare-string array as today, got:\n{bound}"
    );

    let unbound = config_edit::unbind(&bound, "editor", &["dotfiles".to_owned()])
        .expect("string-only unbind must succeed")
        .text;
    assert_eq!(
        unbound, toml,
        "bind then unbind of a single bare string must round-trip BYTE-IDENTICAL to the original config"
    );
}

#[test]
fn bind_reads_mixed_array_and_dedups_by_identity_without_erroring() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\n\
        sources = [\"dotfiles\", { source = \"dotfiles\", as = \"nvim\" }]\n";

    let out = config_edit::bind(
        toml,
        "editor",
        &["dotfiles".to_owned()],
        &config_edit::BindRefinement::default(),
    )
    .expect(
        "bind must READ a mixed string|table array WITHOUT erroring (table entries are now VALID); \
         the old `non-string entry errors` behavior is gone",
    )
    .text;

    let cfg = Config::parse(&out)
        .unwrap_or_else(|e| panic!("bind output must be valid phora.toml: {e}\n{out}"));
    let identities = binding_identities(&cfg, "editor");
    assert_eq!(
        identities,
        vec!["dotfiles".to_owned(), "nvim".to_owned()],
        "re-binding bare `dotfiles` must dedup by IDENTITY: the existing bare `dotfiles` and aliased \
         `nvim` (identity) both survive, with no duplicate `dotfiles` appended, got: {identities:?}\n{out}"
    );
}

#[test]
fn bind_with_zero_sources_is_rejected_by_clap() {
    let result = Cli::command().try_get_matches_from(["phora", "bind", "--to", "editor"]);
    assert!(
        result.is_err(),
        "`phora bind --to T` with no positional sources must be a clap usage error, not a silent no-op write"
    );
}

#[test]
fn unbind_with_zero_identities_is_rejected_by_clap() {
    let result = Cli::command().try_get_matches_from(["phora", "unbind", "--from", "editor"]);
    assert!(
        result.is_err(),
        "`phora unbind --from T` with no positional identities must be a clap usage error"
    );
}

#[test]
fn bind_root_on_url_source_errors_and_leaves_file_untouched() {
    let toml = "version = 1\n\n\
        [sources.fonts]\nurl = \"https://example.com/fonts.tar.gz\"\n\n\
        [targets.editor]\npath = \"~/.config\"\nsources = []\n";

    let result = config_edit::bind(
        toml,
        "editor",
        &["fonts".to_owned()],
        &config_edit::BindRefinement {
            root: Some("sub".to_owned()),
            ..config_edit::BindRefinement::default()
        },
    );

    assert!(
        result.is_err(),
        "binding `--root` onto a `url` source writes a config `validate()` rejects; \
         bind must validate the edited document and refuse, not return a poisoned text"
    );
}

#[test]
fn bind_bare_when_table_entry_exists_preserves_table_and_reports_unchanged() {
    let toml = "version = 1\n\n\
        [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
        [targets.editor]\npath = \"~/.config\"\n\
        sources = [{ source = \"dotfiles\", root = \"nvim\" }]\n";

    let result = config_edit::bind(
        toml,
        "editor",
        &["dotfiles".to_owned()],
        &config_edit::BindRefinement::default(),
    )
    .expect("a bare re-bind of an existing identity is a valid no-op, not an error");

    assert!(
        !result.changed,
        "a bare re-bind that preserves an existing TABLE entry changes nothing and must report `changed = false`"
    );

    let cfg = Config::parse(&result.text)
        .unwrap_or_else(|e| panic!("bind output must be valid phora.toml: {e}\n{}", result.text));
    let refined = refined_binding(&cfg, "editor", "dotfiles");
    assert_eq!(
        refined.root.as_deref(),
        Some(std::path::Path::new("nvim")),
        "the bare re-bind must PRESERVE the existing table entry's `root`, not downgrade it to a bare string, got:\n{}",
        result.text
    );
}

// ── PBR-007: `add --to T` threads per-binding refinement ────

#[test]
fn add_to_with_refinement_flags_writes_source_and_table_binding() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: Some("nvim".to_owned()),
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: Some("nvim".to_owned()),
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("`add --to editor --as nvim --root nvim` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    let src = source_from(&written, "tropos");
    assert_eq!(
        src.host.as_deref(),
        Some("github"),
        "`add --to` must still write the source into [sources.*], got:\n{written}"
    );
    assert_eq!(
        src.repo.as_deref(),
        Some("srnnkls/tropos"),
        "`add --to` must persist the symbolic repo, got:\n{written}"
    );

    let refined = refined_binding(&cfg, "editor", "nvim");
    assert_eq!(
        refined.source, "tropos",
        "`add --to editor` with refinement flags must write a TABLE binding carrying \
         `source = \"tropos\"`, got:\n{written}"
    );
    assert_eq!(
        refined.r#as.as_deref(),
        Some("nvim"),
        "`--as nvim` on add must land as `as = \"nvim\"` in the binding, got:\n{written}"
    );
    assert_eq!(
        refined.root.as_deref(),
        Some(std::path::Path::new("nvim")),
        "`--root nvim` on `add --to` must land as the BINDING `root = \"nvim\"`, got:\n{written}"
    );
}

#[test]
fn add_to_with_no_refinement_flags_writes_a_bare_string_binding() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("`add --to editor` with no refinement flags must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    let editor = cfg
        .targets
        .get("editor")
        .expect("target editor survives add --to");
    let bindings = editor.sources.as_deref().unwrap_or(&[]);
    assert!(
        matches!(bindings.first(), Some(crate::config::Binding::Source(s)) if s == "tropos"),
        "`add --to editor` with NO refinement flags must append a BARE STRING `\"tropos\"`, \
         not a table, got:\n{written}"
    );
}

#[test]
fn add_as_with_multiple_targets_errors() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n\n\
         [targets.shell]\npath = \"~/.config/shell\"\nsources = []\n",
    )
    .expect("seed phora.toml with two empty targets");

    let result = with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned(), "shell".to_owned()],
                r#as: Some("nvim".to_owned()),
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
    });

    assert!(
        result.is_err(),
        "`--as` sets a single binding identity; spreading it across TWO `--to` targets is \
         ambiguous and must error"
    );
}

#[test]
fn add_to_a_single_target_with_as_is_the_happy_path() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: Some("nvim".to_owned()),
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect(
            "`add` takes ONE url, so a single `--to` + `--as` is the happy path and must succeed",
        );
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));
    let refined = refined_binding(&cfg, "editor", "nvim");
    assert_eq!(
        refined.source, "tropos",
        "a single-source `add --to editor --as nvim` must bind `source = \"tropos\"` as `nvim`, got:\n{written}"
    );
}

#[test]
fn bare_add_without_to_does_not_touch_targets() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: Vec::new(),
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("bare `add` with no `--to` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    let src = source_from(&written, "tropos");
    assert_eq!(
        src.host.as_deref(),
        Some("github"),
        "bare add must still write the source into [sources.*], got:\n{written}"
    );
    let editor = cfg
        .targets
        .get("editor")
        .expect("target editor must survive a bare add");
    assert!(
        editor.sources.as_deref().unwrap_or(&[]).is_empty(),
        "bare `add` (no `--to`) must NOT bind into any target — `editor.sources` must stay empty, got:\n{written}"
    );
}

#[test]
fn add_to_with_root_scopes_the_binding_not_the_source() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: Some("nvim".to_owned()),
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: Some("nvim".to_owned()),
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("`add --to editor --as nvim --root nvim` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    let src = source_from(&written, "tropos");
    assert!(
        src.root.is_none(),
        "`--root` with `--to` scopes the BINDING; the SOURCE must stay pure provenance with NO \
         root, got: {:?}\n{written}",
        src.root
    );

    let refined = refined_binding(&cfg, "editor", "nvim");
    assert_eq!(
        refined.root.as_deref(),
        Some(std::path::Path::new("nvim")),
        "`--root` with `--to` must land on the BINDING `root = \"nvim\"`, got:\n{written}"
    );
}

#[test]
fn add_to_with_url_embedded_root_sets_the_source_root() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos/editor".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("`add github:owner/repo/subdir --to editor` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    let src = source_from(&written, "tropos");
    assert_eq!(
        src.root.as_deref(),
        Some(std::path::Path::new("editor")),
        "a URL-embedded root is provenance and must root the SOURCE even with `--to`, got: {:?}\n{written}",
        src.root
    );

    let editor = cfg
        .targets
        .get("editor")
        .expect("target editor must survive the add");
    assert!(
        binding_sources(&cfg, "editor")
            .iter()
            .any(|s| s == "tropos"),
        "`add --to editor` must bind the source into target `editor`, got:\n{written}"
    );
    let binding = editor
        .sources
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find(|b| b.identity() == "tropos")
        .expect("target editor must hold a `tropos` binding");
    assert!(
        matches!(binding, crate::config::Binding::Source(_)),
        "with no explicit `--root`, the binding carries no refinement, got:\n{written}"
    );
}

#[test]
fn bare_add_with_root_still_sets_the_source_root() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(&toml_path, "version = 1\n").expect("seed phora.toml");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: Some("nvim".to_owned()),
                local: false,
                symlink: false,
                to: Vec::new(),
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("bare `add --root` (no `--to`) must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let src = source_from(&written, "tropos");
    assert_eq!(
        src.root.as_deref(),
        Some(std::path::Path::new("nvim")),
        "a bare `add --root` (no `--to`) must STILL set the SOURCE root, got:\n{written}"
    );
}

#[test]
fn add_as_without_to_errors() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(&toml_path, "version = 1\n").expect("seed phora.toml");

    let result = with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: Vec::new(),
                r#as: Some("nvim".to_owned()),
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
    });

    assert!(
        result.is_err(),
        "`--as` with no `--to` target binds nothing and silently drops the identity; it must error"
    );
}

#[test]
fn add_local_with_to_errors() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let src_dir = tempfile::TempDir::new().expect("temp source dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    let src_path = src_dir.path().to_string_lossy().into_owned();
    let result = with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: src_path,
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: true,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
    });

    assert!(
        result.is_err(),
        "`--local` overlays do not support `--to`/refinement; pairing them must error rather than \
         silently dropping the binding"
    );
}

#[test]
fn add_to_multiple_targets_writes_a_binding_in_each() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n\n\
         [targets.shell]\npath = \"~/.config/shell\"\nsources = []\n",
    )
    .expect("seed phora.toml with two empty targets");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned(), "shell".to_owned()],
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("`add --to editor --to shell` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    for target in ["editor", "shell"] {
        let bindings = cfg
            .targets
            .get(target)
            .and_then(|t| t.sources.as_deref())
            .unwrap_or(&[]);
        assert!(
            matches!(bindings.first(), Some(crate::config::Binding::Source(s)) if s == "tropos"),
            "`add --to {target}` must append a bare-string `tropos` binding, got:\n{written}"
        );
    }
}

#[test]
fn add_to_with_include_exclude_writes_arrays_on_the_binding() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    std::fs::write(
        &toml_path,
        "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n",
    )
    .expect("seed phora.toml with an empty target");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: None,
                include: vec!["*.lua".to_owned()],
                exclude: vec![".git".to_owned()],
            },
        })
        .expect("`add --to editor --include --exclude` must succeed");
    });

    let written = std::fs::read_to_string(&toml_path).expect("add must leave phora.toml on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));

    let refined = refined_binding(&cfg, "editor", "tropos");
    assert_eq!(
        refined.include.as_deref(),
        Some(&["*.lua".to_owned()][..]),
        "`--include \"*.lua\"` must land as the BINDING include array, got:\n{written}"
    );
    assert_eq!(
        refined.exclude.as_deref(),
        Some(&[".git".to_owned()][..]),
        "`--exclude \".git\"` must land as the BINDING exclude array, got:\n{written}"
    );
}

#[test]
fn add_to_target_without_sources_array_creates_the_array_and_binds() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    let seed = "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\n";
    std::fs::write(&toml_path, seed).expect("seed phora.toml with a target lacking `sources`");

    with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["editor".to_owned()],
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
        .expect("`add --to editor` must materialize a missing `sources` array, not error");
    });

    let written = std::fs::read_to_string(&toml_path).expect("phora.toml must remain on disk");
    let cfg = Config::parse(&written)
        .unwrap_or_else(|e| panic!("add output must be valid phora.toml: {e}\n{written}"));
    let bindings = cfg
        .targets
        .get("editor")
        .and_then(|t| t.sources.as_deref())
        .unwrap_or(&[]);
    assert!(
        matches!(bindings.first(), Some(crate::config::Binding::Source(s)) if s == "tropos"),
        "binding into a target with no `sources` key must create the array with `tropos`, got:\n{written}"
    );
}

#[test]
fn add_to_nonexistent_target_errors_and_leaves_file_untouched() {
    let dir = tempfile::TempDir::new().expect("temp project dir");
    let toml_path = dir.path().join("phora.toml");
    let seed = "version = 1\n\n\
         [targets.editor]\npath = \"~/.config/editor\"\nsources = []\n";
    std::fs::write(&toml_path, seed).expect("seed phora.toml");

    let result = with_cwd(dir.path(), || {
        run(Cli {
            command: Command::Add {
                url: "github:srnnkls/tropos".to_owned(),
                name: None,
                branch: None,
                tag: None,
                root: None,
                local: false,
                symlink: false,
                to: vec!["does-not-exist".to_owned()],
                r#as: None,
                include: Vec::new(),
                exclude: Vec::new(),
            },
        })
    });

    assert!(
        result.is_err(),
        "`add --to does-not-exist` must error on a missing target"
    );
    let written = std::fs::read_to_string(&toml_path).expect("phora.toml must remain on disk");
    assert_eq!(
        written, seed,
        "a failed `add --to` must leave phora.toml unchanged (no orphan source write)"
    );
}

fn source_names_opt(target: &crate::config::Target) -> Option<Vec<String>> {
    target
        .sources
        .as_ref()
        .map(|bindings| bindings.iter().map(|b| b.source().to_owned()).collect())
}

// ── CLI-002 / CLI-003: source + target namespace parsing ───────────

#[test]
fn source_add_parses_into_source_namespace() {
    let cli = Cli::try_parse_from(["phora", "source", "add", "github:me/dots"])
        .expect("`source add <url>` must parse");
    let Command::Source {
        cmd: SourceCmd::Add { url, .. },
    } = cli.command
    else {
        panic!("`source add` must route to Command::Source(SourceCmd::Add)");
    };
    assert_eq!(
        url, "github:me/dots",
        "the positional url must reach SourceCmd::Add.url"
    );
}

#[test]
fn source_add_carries_same_args_as_top_level_add() {
    let argv = ["github:me/dots", "--branch", "dev", "--root", "sub"];

    let top = Cli::try_parse_from(["phora", "add"].into_iter().chain(argv))
        .expect("top-level add parses");
    let Command::Add {
        url: top_url,
        name: top_name,
        branch: top_branch,
        tag: top_tag,
        root: top_root,
        local: top_local,
        symlink: top_symlink,
        ..
    } = top.command
    else {
        panic!("expected Command::Add");
    };

    let sub = Cli::try_parse_from(["phora", "source", "add"].into_iter().chain(argv))
        .expect("source add parses");
    let Command::Source {
        cmd:
            SourceCmd::Add {
                url,
                name,
                branch,
                tag,
                root,
                local,
                symlink,
            },
    } = sub.command
    else {
        panic!("expected SourceCmd::Add");
    };

    assert_eq!(url, top_url, "url must match top-level `add`");
    assert_eq!(name, top_name, "name must match top-level `add`");
    assert_eq!(branch, top_branch, "branch must match top-level `add`");
    assert_eq!(tag, top_tag, "tag must match top-level `add`");
    assert_eq!(root, top_root, "root must match top-level `add`");
    assert_eq!(local, top_local, "local must match top-level `add`");
    assert_eq!(symlink, top_symlink, "symlink must match top-level `add`");
}

#[test]
fn source_rm_parses_name() {
    let cli = Cli::try_parse_from(["phora", "source", "rm", "dotfiles"])
        .expect("`source rm <name>` must parse");
    let Command::Source {
        cmd: SourceCmd::Rm { name },
    } = cli.command
    else {
        unreachable!("expected SourceCmd::Rm");
    };
    assert_eq!(name, "dotfiles", "the positional name must reach Rm.name");
    assert!(
        Cli::try_parse_from(["phora", "source", "rm", "dotfiles", "--local"]).is_err(),
        "--local must be rejected: `source rm` scrubs both files, so file-addressing is meaningless"
    );
}

#[test]
fn source_list_parses() {
    let cli = Cli::try_parse_from(["phora", "source", "list"]).expect("`source list` must parse");
    assert!(
        matches!(
            cli.command,
            Command::Source {
                cmd: SourceCmd::List
            }
        ),
        "`source list` must route to SourceCmd::List, got {cli:?}"
    );
}

#[test]
fn source_show_requires_a_name() {
    Cli::try_parse_from(["phora", "source", "show"])
        .expect_err("`source show` with no name must be a parse error");
    let cli = Cli::try_parse_from(["phora", "source", "show", "dotfiles"])
        .expect("`source show <name>` must parse");
    assert!(
        matches!(
            &cli.command,
            Command::Source {
                cmd: SourceCmd::Show { name }
            } if name == "dotfiles"
        ),
        "`source show <name>` must carry the required name, got {cli:?}"
    );
}

#[test]
fn target_add_requires_path() {
    Cli::try_parse_from(["phora", "target", "add", "nvim"])
        .expect_err("`target add <name>` without --path must be a clap-level error");
    let cli = Cli::try_parse_from(["phora", "target", "add", "nvim", "--path", "~/.config/nvim"])
        .expect("`target add <name> --path <p>` must parse");
    assert!(
        matches!(
            &cli.command,
            Command::Target {
                cmd: TargetCmd::Add { name, path, layout: None, local: false }
            } if name == "nvim" && path == "~/.config/nvim"
        ),
        "target add must carry name + required path with default layout/local, got {cli:?}"
    );
}

#[test]
fn target_add_accepts_layout_and_local() {
    let cli = Cli::try_parse_from([
        "phora",
        "target",
        "add",
        "nvim",
        "--path",
        "~/x",
        "--layout",
        "by-source",
        "--local",
    ])
    .expect("`target add` with --layout and --local must parse");
    let Command::Target {
        cmd: TargetCmd::Add { layout, local, .. },
    } = cli.command
    else {
        panic!("expected TargetCmd::Add");
    };
    assert_eq!(
        layout.as_deref(),
        Some("by-source"),
        "--layout must reach Add.layout as a string"
    );
    assert!(local, "--local must reach Add.local");
}

#[test]
fn target_rm_parses_name_and_local() {
    let cli = Cli::try_parse_from(["phora", "target", "rm", "nvim", "--local"])
        .expect("`target rm <name> --local` must parse");
    assert!(
        matches!(
            &cli.command,
            Command::Target {
                cmd: TargetCmd::Rm { name, local: true }
            } if name == "nvim"
        ),
        "target rm must carry name + local, got {cli:?}"
    );
}

#[test]
fn target_list_parses() {
    let cli = Cli::try_parse_from(["phora", "target", "list"]).expect("`target list` must parse");
    assert!(
        matches!(
            cli.command,
            Command::Target {
                cmd: TargetCmd::List
            }
        ),
        "`target list` must route to TargetCmd::List, got {cli:?}"
    );
}

#[test]
fn target_show_requires_a_name() {
    Cli::try_parse_from(["phora", "target", "show"])
        .expect_err("`target show` with no name must be a parse error");
    let cli = Cli::try_parse_from(["phora", "target", "show", "nvim"])
        .expect("`target show <name>` must parse");
    assert!(
        matches!(
            &cli.command,
            Command::Target {
                cmd: TargetCmd::Show { name }
            } if name == "nvim"
        ),
        "`target show <name>` must carry the required name, got {cli:?}"
    );
}

// ── CLI-002: source rm + source show pure helpers ──────────────────

#[test]
fn source_rm_helper_scrubs_both_files_and_target_arrays() {
    let main = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
         [targets.A]\npath = \"~/a\"\nsources = [\"dotfiles\", \"other\"]\n\n\
         [sources.other]\ngit = \"h\"\n";
    let local = "version = 1\n\n[targets.B]\npath = \"~/b\"\nsources = [\"dotfiles\"]\n";

    let result =
        config_edit::remove_source(main, local, "dotfiles").expect("source rm removes dotfiles");

    let main_cfg = Config::parse(&result.main).expect("scrubbed main parses");
    assert!(
        !main_cfg.sources.contains_key("dotfiles"),
        "`source rm dotfiles` must drop [sources.dotfiles] from phora.toml"
    );
    assert_eq!(
        source_names_opt(main_cfg.targets.get("A").expect("target A survives")),
        Some(vec!["other".to_owned()]),
        "dotfiles must be scrubbed from [targets.A].sources, leaving only other"
    );
    let local_cfg = Config::parse(&result.local).expect("scrubbed local parses");
    assert_eq!(
        source_names_opt(local_cfg.targets.get("B").expect("target B survives")),
        Some(vec![]),
        "dotfiles must be scrubbed from [targets.B].sources in phora.local.toml too"
    );
}

#[test]
fn source_rm_helper_unknown_name_errors() {
    let err = config_edit::remove_source("version = 1\n", "version = 1\n", "ghost")
        .expect_err("`source rm ghost` must error when ghost is undefined");
    assert!(
        matches!(err, Error::Config(msg) if msg.contains("ghost")),
        "removing an undefined source must Err naming the source"
    );
}

fn config_with_targets(toml: &str) -> Config {
    Config::parse(toml).expect("config under test parses")
}

#[test]
fn source_summary_lists_only_targets_that_bind_it_explicitly() {
    let cfg = config_with_targets(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
         [targets.binds_a]\npath = \"~/x\"\nsources = [\"a\"]\n\n\
         [targets.binds_b]\npath = \"~/y\"\nsources = [\"b\"]\n",
    );

    let summary = source_summary(&cfg, "a").expect("source a is defined");
    assert_eq!(summary.name, "a", "the summary must echo the source name");
    assert_eq!(
        summary.targets,
        vec!["binds_a".to_owned()],
        "only the target whose explicit sources list contains `a` must be listed"
    );
}

#[test]
fn source_summary_no_key_target_receives_nothing() {
    let cfg = config_with_targets(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
         [targets.everything]\npath = \"~/x\"\n",
    );

    let summary_a = source_summary(&cfg, "a").expect("source a defined");
    let summary_b = source_summary(&cfg, "b").expect("source b defined");
    assert!(
        !summary_a.targets.contains(&"everything".to_owned()),
        "a target with no `sources` key must NOT receive source a, got {:?}",
        summary_a.targets
    );
    assert!(
        !summary_b.targets.contains(&"everything".to_owned()),
        "the same no-key target must NOT receive source b either, got {:?}",
        summary_b.targets
    );
}

#[test]
fn source_summary_unknown_name_errors() {
    let cfg = config_with_targets("version = 1\n\n[sources.a]\ngit = \"g\"\n");
    let err = source_summary(&cfg, "ghost").expect_err("an undefined source must error");
    assert!(
        matches!(err, Error::Config(msg) if msg.contains("ghost")),
        "source show on an undefined name must Err naming it"
    );
}

#[test]
fn targets_receiving_only_lists_explicit_binders() {
    let cfg = config_with_targets(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
         [targets.no_key]\npath = \"~/x\"\n\n\
         [targets.explicit_b]\npath = \"~/y\"\nsources = [\"b\"]\n",
    );

    let receiving_a = targets_receiving(&cfg, "a");
    assert!(
        !receiving_a.contains(&"no_key".to_owned()),
        "a target with no `sources` key must NOT receive a, got {receiving_a:?}"
    );
    assert!(
        !receiving_a.contains(&"explicit_b".to_owned()),
        "a target that explicitly binds only b must NOT receive a, got {receiving_a:?}"
    );

    let receiving_b = targets_receiving(&cfg, "b");
    assert!(
        receiving_b.contains(&"explicit_b".to_owned()),
        "the explicit binder of b must receive b, got {receiving_b:?}"
    );
    assert!(
        !receiving_b.contains(&"no_key".to_owned()),
        "the no-key target must NOT receive b, got {receiving_b:?}"
    );
}

#[test]
fn source_listing_rows_carry_name_remote_and_refspec() {
    let cfg = config_with_targets(
        "version = 1\n\n[sources.dots]\ngit = \"https://example.com/d.git\"\nbranch = \"dev\"\n",
    );

    let rows = source_listing(&cfg).expect("source list over the merged config");
    let row = rows
        .iter()
        .find(|r| r.name == "dots")
        .expect("the dots source must appear as a row");
    assert_eq!(
        row.remote, "https://example.com/d.git",
        "a literal git source's remote must be the verbatim url"
    );
    assert_eq!(
        row.refspec, "dev",
        "the row must carry the source's refspec (branch dev)"
    );
}

#[test]
fn source_listing_uses_per_source_protocol_override_under_https_default() {
    let cfg = config_with_targets(
        "version = 1\nprotocol = \"https\"\n\n\
         [sources.dots]\nhost = \"github\"\nrepo = \"o/r\"\nprotocol = \"ssh\"\n",
    );

    let rows = source_listing(&cfg).expect("source list over the merged config");
    let row = rows
        .iter()
        .find(|r| r.name == "dots")
        .expect("the dots source must appear as a row");
    assert_eq!(
        row.remote, "git@github.com:o/r.git",
        "a source's own ssh protocol must win over the global https default"
    );
}

// ── CLI-003: target list + target show pure helpers ────────────────

#[test]
fn target_listing_derives_explicit_vs_all_resolution_mode() {
    let cfg = config_with_targets(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n\
         [targets.explicit]\npath = \"~/e\"\nsources = [\"a\"]\n\n\
         [targets.implicit]\npath = \"~/i\"\n",
    );

    let rows = target_listing(&cfg);
    let explicit = rows
        .iter()
        .find(|r| r.name == "explicit")
        .expect("explicit target row present");
    assert_eq!(
        explicit.resolution,
        SourceResolution::Explicit(vec!["a".to_owned()]),
        "a target with a sources key must resolve as Explicit with its listed names"
    );
    let implicit = rows
        .iter()
        .find(|r| r.name == "implicit")
        .expect("no-key target row present");
    assert_eq!(
        implicit.resolution,
        SourceResolution::Explicit(vec![]),
        "a target with no sources key resolves to the empty explicit set (receives nothing)"
    );
}

#[test]
fn target_detail_no_key_target_binds_nothing() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let cfg = config_with_targets(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
         [targets.everything]\npath = \"~/x\"\n",
    );

    let detail = target_detail(&cfg, &reg, "everything").expect("target everything is defined");
    assert!(
        detail.bound_sources.is_empty(),
        "a target with no `sources` key binds NO sources, got {:?}",
        detail.bound_sources
    );
}

#[test]
fn target_detail_reports_per_artifact_deployment_state() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let target_root = TempDir::new().expect("target root");
    let cfg = config_one_flat_target("dest", "editor-src", target_root.path());

    let mf = deploy_matching_file(target_root.path(), "editor", "init.lua", b"-- init\n");
    reg.put(&record_for(
        "dest",
        "editor-src",
        "editor",
        "aaa111",
        vec![mf],
    ))
    .expect("seed a clean record");

    let detail = target_detail(&cfg, &reg, "dest").expect("target dest is defined");
    let editor = detail
        .artifacts
        .iter()
        .find(|a| a.artifact == "editor")
        .expect("the editor artifact must appear in target detail");
    assert_eq!(
        editor.source, "editor-src",
        "the artifact row must name its source"
    );
    assert!(
        editor.state.contains('✓') || editor.state.to_lowercase().contains("clean"),
        "a matching deployment must read Clean in target show, got {:?}",
        editor.state
    );
}

#[test]
fn target_detail_unknown_name_errors() {
    let state_dir = TempDir::new().expect("state root");
    let reg = FileRegistry::open(state_dir.path().to_path_buf()).expect("open registry");
    let cfg = config_with_targets("version = 1\n\n[targets.real]\npath = \"~/x\"\n");
    let err = target_detail(&cfg, &reg, "ghost")
        .expect_err("target show on an undefined name must error");
    assert!(
        matches!(err, Error::Config(msg) if msg.contains("ghost")),
        "target show on an undefined target must Err naming it"
    );
}

#[test]
fn target_has_deployed_artifacts_is_true_when_records_exist() {
    let (_dir, reg) = seeded_registry();
    assert!(
        target_has_deployed_artifacts(&reg, "nvim").expect("predicate reads the registry"),
        "a target with registry records must report having deployed artifacts (warning path)"
    );
}

#[test]
fn target_has_deployed_artifacts_is_false_for_clean_target() {
    let (_dir, reg) = seeded_registry();
    assert!(
        !target_has_deployed_artifacts(&reg, "never-deployed").expect("predicate reads registry"),
        "a target with no registry records must report no deployed artifacts (no warning)"
    );
}

// CLI-005: add/rm sugar — clap surface + atomic desugar (pure helper).

#[test]
fn add_parses_repeatable_to_targets() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "phora",
        "add",
        "https://github.com/me/dots.git",
        "--to",
        "a",
        "--to",
        "b",
    ])
    .expect("add with repeated --to must parse");
    let Command::Add { url, to, .. } = cli.command else {
        panic!("expected Command::Add");
    };
    assert_eq!(url, "https://github.com/me/dots.git");
    assert_eq!(
        to,
        vec!["a".to_owned(), "b".to_owned()],
        "repeated --to must collect into to == [a, b]"
    );
}

#[test]
fn add_without_to_parses_empty_targets() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["phora", "add", "https://github.com/me/dots.git"])
        .expect("add with no --to must parse");
    let Command::Add { to, .. } = cli.command else {
        panic!("expected Command::Add");
    };
    assert!(to.is_empty(), "no --to must leave targets empty");
}

#[test]
fn add_without_url_is_rejected() {
    use clap::Parser;
    Cli::try_parse_from(["phora", "add"])
        .expect_err("bare `add` with no url must be a clap missing-arg error");
}

#[test]
fn rm_parses_name() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["phora", "rm", "dotfiles"]).expect("rm <name> must parse");
    let Command::Rm { name } = cli.command else {
        panic!("expected Command::Rm");
    };
    assert_eq!(name, "dotfiles");
}

#[test]
fn rm_rejects_local_flag() {
    use clap::Parser;
    Cli::try_parse_from(["phora", "rm", "dotfiles", "--local"])
        .expect_err("the `rm` alias must not accept --local (cross-file scrub is file-unsafe)");
}

#[test]
fn rm_routes_to_same_scrub_as_source_rm() {
    // Routing parity: `Command::Rm` and `source rm` both delegate to
    // config_edit::remove_source, which scrubs the source from both files.
    let main = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
         [targets.A]\npath = \"~/a\"\nsources = [\"dotfiles\"]\n";
    let local = "version = 1\n\n\
         [targets.B]\npath = \"~/b\"\nsources = [\"dotfiles\"]\n";

    let result = config_edit::remove_source(main, local, "dotfiles")
        .expect("remove_source scrubs dotfiles from both files");

    let main_cfg = Config::parse(&result.main).expect("scrubbed main is valid toml");
    let local_cfg = Config::parse(&result.local).expect("scrubbed local is valid toml");
    assert!(
        !main_cfg.sources.contains_key("dotfiles"),
        "the [sources.dotfiles] table must be gone from main"
    );
    assert_eq!(
        source_names_opt(&main_cfg.targets["A"]),
        Some(vec![]),
        "dotfiles must be scrubbed from [targets.A].sources in main"
    );
    assert_eq!(
        source_names_opt(&local_cfg.targets["B"]),
        Some(vec![]),
        "dotfiles must be scrubbed from [targets.B].sources in local"
    );
}

// Pure desugar helper: a single config-text string in, final text or whole Err out.

struct RejectAll;

impl MissingTargetDecider for RejectAll {
    fn decide(&self, _name: &str, _default_path: &str) -> MissingTarget {
        MissingTarget::Reject
    }
}

struct CreateAtDefault;

impl MissingTargetDecider for CreateAtDefault {
    fn decide(&self, _name: &str, default_path: &str) -> MissingTarget {
        MissingTarget::Create {
            path: default_path.to_owned(),
        }
    }
}

struct CreateAt(&'static str);

impl MissingTargetDecider for CreateAt {
    fn decide(&self, _name: &str, _default_path: &str) -> MissingTarget {
        MissingTarget::Create {
            path: self.0.to_owned(),
        }
    }
}

const TWO_EXPLICIT_TARGETS: &str = "version = 1\n\n\
     [targets.A]\npath = \"~/a\"\nsources = [\"existing\"]\n\n\
     [targets.B]\npath = \"~/b\"\nsources = [\"existing\"]\n\n\
     [sources.existing]\ngit = \"e\"\n";

const NO_KEY_TARGET: &str = "version = 1\n\n\
     [targets.A]\npath = \"~/a\"\n\n\
     [sources.existing]\ngit = \"e\"\n";

#[test]
fn add_with_binds_appends_to_both_targets_and_inserts_source() {
    let out = add_with_binds(
        TWO_EXPLICIT_TARGETS,
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["A".to_owned(), "B".to_owned()],
        &config_edit::BindRefinement::default(),
        &RejectAll,
    )
    .expect("binding dots to two explicit targets must succeed atomically");

    let cfg = Config::parse(&out).expect("returned text is valid phora.toml");
    assert!(
        cfg.sources.contains_key("dots"),
        "[sources.dots] must be inserted"
    );
    assert_eq!(
        source_names_opt(&cfg.targets["A"]),
        Some(vec!["existing".to_owned(), "dots".to_owned()]),
        "dots must be appended to target A's sources"
    );
    assert_eq!(
        source_names_opt(&cfg.targets["B"]),
        Some(vec!["existing".to_owned(), "dots".to_owned()]),
        "dots must be appended to target B's sources"
    );
}

#[test]
fn add_with_binds_missing_target_errs_with_no_partial_text() {
    let result = add_with_binds(
        TWO_EXPLICIT_TARGETS,
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["A".to_owned(), "ghost".to_owned()],
        &config_edit::BindRefinement::default(),
        &RejectAll,
    );
    let err = result.expect_err("a nonexistent target must fail the whole command");
    assert!(
        matches!(&err, Error::Config(msg) if msg.contains("ghost") && msg.contains("phora target add")),
        "the error must name the missing target and point at `phora target add`, \
         and by returning Err the helper produces no partial text"
    );
}

#[test]
fn add_with_binds_no_key_target_creates_list() {
    let out = add_with_binds(
        NO_KEY_TARGET,
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["A".to_owned()],
        &config_edit::BindRefinement::default(),
        &RejectAll,
    )
    .expect("binding to a no-key target must succeed, creating its sources list");

    let cfg = Config::parse(&out).expect("returned text is valid phora.toml");
    assert_eq!(
        source_names_opt(&cfg.targets["A"]),
        Some(vec!["dots".to_owned()]),
        "binding via add sugar to a no-key target must create sources = [\"dots\"]"
    );
}

#[test]
fn run_target_add_rejects_path_unsafe_name() {
    let err = run_target_add("a/b", "~/x", None, false)
        .expect_err("a target name containing `/` must be rejected before any write");
    assert!(
        err.to_string().contains("unsafe path component"),
        "name validation must propagate the kernel path-traversal guard error, got: {err}"
    );
}

#[test]
fn add_with_binds_no_targets_equals_plain_source_insert() {
    let base = "version = 1\n";
    let source = lit("https://github.com/me/dots.git", None);
    let out = add_with_binds(
        base,
        "dots",
        &source,
        None,
        None,
        None,
        &[],
        &config_edit::BindRefinement::default(),
        &RejectAll,
    )
    .expect("zero targets must behave as a plain source upsert");

    let expected = config_edit::upsert_source(base, "dots", &source, None, None, None)
        .expect("baseline plain source upsert");
    assert_eq!(
        out, expected,
        "adding with zero targets must produce byte-identical text to a plain source insert"
    );
}

#[test]
fn add_to_default_target_creates_flat_default_and_binds() {
    let out = add_to_default_target(
        "version = 1\n",
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
    )
    .expect("auto-target add must create [targets.default] and bind the source");

    let cfg = Config::parse(&out).expect("returned text is valid phora.toml");
    assert!(
        cfg.sources.contains_key("dots"),
        "[sources.dots] must be inserted"
    );
    let default = &cfg.targets["default"];
    assert_eq!(
        default.path,
        PathBuf::from("."),
        "the auto-created default target must live at the project root"
    );
    assert_eq!(
        default.layout().kind,
        LayoutKind::Flat,
        "the auto-created default target must use the flat layout"
    );
    assert_eq!(
        source_names_opt(default),
        Some(vec!["dots".to_owned()]),
        "the source must be bound into [targets.default].sources"
    );
}

#[test]
fn add_to_default_target_second_call_extends_without_clobbering() {
    let first = add_to_default_target(
        "version = 1\n",
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
    )
    .expect("first auto-target add");
    let second = add_to_default_target(
        &first,
        "tools",
        &lit("https://github.com/me/tools.git", None),
        None,
        None,
        None,
    )
    .expect("second auto-target add reuses the existing default target");

    let cfg = Config::parse(&second).expect("returned text is valid phora.toml");
    assert_eq!(
        source_names_opt(&cfg.targets["default"]),
        Some(vec!["dots".to_owned(), "tools".to_owned()]),
        "a second source must be appended, not clobber the existing default binding"
    );
    let again = add_to_default_target(
        &second,
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
    )
    .expect("re-adding an existing source");
    let cfg = Config::parse(&again).expect("valid");
    assert_eq!(
        source_names_opt(&cfg.targets["default"]),
        Some(vec!["dots".to_owned(), "tools".to_owned()]),
        "re-binding a present source must dedup, not duplicate"
    );
}

#[test]
fn add_with_binds_to_named_target_never_touches_default() {
    let base = "version = 1\n\n[targets.A]\npath = \"~/a\"\n\n[sources.existing]\ngit = \"e\"\n";
    let out = add_with_binds(
        base,
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["A".to_owned()],
        &config_edit::BindRefinement::default(),
        &RejectAll,
    )
    .expect("--to routing binds only the named target");

    let cfg = Config::parse(&out).expect("valid");
    assert!(
        !cfg.targets.contains_key("default"),
        "routing to a named target must never materialize [targets.default]"
    );
    assert_eq!(
        source_names_opt(&cfg.targets["A"]),
        Some(vec!["dots".to_owned()]),
        "the source must be bound to the named target only"
    );
}

#[test]
fn add_with_binds_create_decider_materializes_flat_default_path_target() {
    let base = "version = 1\n";
    let out = add_with_binds(
        base,
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["staging".to_owned()],
        &config_edit::BindRefinement::default(),
        &CreateAtDefault,
    )
    .expect("a Create decider must create the missing target and bind");

    let cfg = Config::parse(&out).expect("valid");
    let staging = &cfg.targets["staging"];
    assert_eq!(
        staging.path,
        PathBuf::from("./staging"),
        "an empty-input Create must use the default ./<name> path"
    );
    assert_eq!(
        staging.layout().kind,
        LayoutKind::Flat,
        "an interactively created target must use the flat layout"
    );
    assert_eq!(
        source_names_opt(staging),
        Some(vec!["dots".to_owned()]),
        "the source must be bound to the freshly created target"
    );
}

#[test]
fn add_with_binds_create_decider_honors_entered_path() {
    let out = add_with_binds(
        "version = 1\n",
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["staging".to_owned()],
        &config_edit::BindRefinement::default(),
        &CreateAt("~/custom/staging"),
    )
    .expect("a Create decider with a typed path must honor it");

    let cfg = Config::parse(&out).expect("valid");
    assert_eq!(
        cfg.targets["staging"].path,
        PathBuf::from("~/custom/staging"),
        "a typed path must override the default ./<name>"
    );
}

#[test]
fn add_with_binds_reject_decider_errors_with_hint_and_no_partial_text() {
    let result = add_with_binds(
        "version = 1\n",
        "dots",
        &lit("https://github.com/me/dots.git", None),
        None,
        None,
        None,
        &["staging".to_owned()],
        &config_edit::BindRefinement::default(),
        &RejectAll,
    );
    let err = result.expect_err("a Reject decider must fail the whole command");
    assert!(
        matches!(&err, Error::Config(msg) if msg.contains("staging") && msg.contains("phora target add")),
        "rejecting a missing target must error naming it with the `phora target add` hint"
    );
}

#[test]
fn add_tag_writes_source_level_ref() {
    let out = config_edit::upsert_source(
        "version = 1\n",
        "fzf",
        &lit("https://github.com/junegunn/fzf.git", None),
        None,
        Some("v1.0"),
        None,
    )
    .expect("upsert a source with a source-level tag");

    let cfg = Config::parse(&out).expect("output parses");
    let source = &cfg.sources["fzf"];
    assert_eq!(
        source.tag.as_deref(),
        Some("v1.0"),
        "`add --tag` must keep writing the ref at the SOURCE level under [sources.fzf]"
    );
}

fn locked_split(name: &str, r#ref: Option<&str>) -> LockedSource {
    LockedSource {
        name: name.to_owned(),
        git: "https://github.com/junegunn/fzf.git".to_owned(),
        resolved: "deadbeef".to_owned(),
        commit: "deadbeefcafe".to_owned(),
        digest: "blake3:artifact".to_owned(),
        config_digest: "blake3:cfg".to_owned(),
        r#ref: r#ref.map(str::to_owned),
    }
}

#[test]
fn drop_one_removes_all_ref_splits_of_that_source() {
    let mut lock = Lock {
        version: 1,
        sources: vec![
            locked_split("fzf", None),
            locked_split("fzf", Some("tag:v0.56.0")),
            locked_split("other", None),
        ],
    };

    drop_sources(Some(&mut lock), &DropSources::One("fzf".to_owned()));

    assert!(
        lock.sources.iter().all(|s| s.name != "fzf"),
        "dropping `fzf` must remove BOTH its default entry and its tag:v0.56.0 split, got {:?}",
        lock.sources
    );
    assert!(
        lock.sources.iter().any(|s| s.name == "other"),
        "an unrelated source must survive a targeted drop"
    );
}
// ── `phora preview` CLI parsing ───────────────────────

use clap::Parser as _;

/// Destructures `cli`'s command into a `Command::Preview`, panicking otherwise.
fn preview_of(cli: Cli) -> (Option<String>, Option<String>, bool, bool) {
    match cli.command {
        Command::Preview {
            source,
            target,
            files,
            json,
        } => (source, target, files, json),
        other => panic!("expected Command::Preview, got {other:?}"),
    }
}

#[test]
fn preview_parses_all_selectors_and_flags() {
    let cli = Cli::try_parse_from([
        "phora", "preview", "--source", "s", "--target", "t", "--files", "--json",
    ])
    .expect("`phora preview --source s --target t --files --json` must parse");

    let (source, target, files, json) = preview_of(cli);
    assert_eq!(
        source.as_deref(),
        Some("s"),
        "--source must populate Command::Preview.source"
    );
    assert_eq!(
        target.as_deref(),
        Some("t"),
        "--target must populate Command::Preview.target"
    );
    assert!(files, "--files must set the files flag");
    assert!(json, "--json must set the json flag");
}

#[test]
fn bare_preview_defaults_selectors_to_none_and_flags_to_false() {
    let cli = Cli::try_parse_from(["phora", "preview"]).expect("a bare `phora preview` must parse");

    let (source, target, files, json) = preview_of(cli);
    assert!(
        source.is_none(),
        "a bare preview must leave --source unset, got {source:?}"
    );
    assert!(
        target.is_none(),
        "a bare preview must leave --target unset, got {target:?}"
    );
    assert!(!files, "a bare preview must default --files to false");
    assert!(!json, "a bare preview must default --json to false");
}

#[test]
fn preview_selectors_are_long_flags_not_positionals() {
    let positional = Cli::try_parse_from(["phora", "preview", "some-source"]);
    assert!(
        positional.is_err(),
        "`phora preview <name>` must be rejected: the source selector is the long flag --source, \
             not a positional argument"
    );
}

#[expect(
    clippy::unwrap_used,
    reason = "fixture git setup fails loudly in tests"
)]
fn git_init_with_template(dir: &std::path::Path, body: &[u8]) -> String {
    let run = |args: &[&str]| {
        let _serial = crate::store::guard_git_fork();
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00 +0000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-b", "main", "."]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::create_dir_all(dir.join("editor")).unwrap();
    std::fs::write(dir.join("editor/motd.tmpl"), body).unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
    dir.to_string_lossy().into_owned()
}

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

#[test]
fn rebuild_over_merged_vars_agrees_with_deployed_vars_digest() {
    let source = TempDir::new().expect("source repo dir");
    let url = git_init_with_template(source.path(), b"hello {{ greeting }}!\n");

    let project = TempDir::new().expect("project dir");
    let target = TempDir::new().expect("target parent dir");
    let target_path = target.path().join("dest");
    std::fs::write(
        project.path().join("phora.toml"),
        format!(
            "version = 1\n\n[vars]\ngreeting = \"base\"\n\n\
             [sources.ed]\ngit = \"{url}\"\nbranch = \"main\"\n\n\
             [targets.home]\npath = \"{}\"\nsources = [\"ed\"]\nlayout = \"flat\"\n",
            target_path.display(),
        ),
    )
    .expect("write phora.toml");
    std::fs::write(
        project.path().join("phora.local.toml"),
        "version = 1\n\n[vars]\ngreeting = \"local\"\n",
    )
    .expect("write phora.local.toml");

    let state = TempDir::new().expect("state root");
    let cache = TempDir::new().expect("cache root");
    let key = ArtifactKey {
        target: "home".to_owned(),
        source: "ed".to_owned(),
        artifact: "editor".to_owned(),
    };

    let (deployed_vd, rebuilt_vd) = with_cwd(project.path(), || {
        let _serial = crate::store::STATE_LOCK_SERIAL
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _state = EnvVarGuard::set("XDG_STATE_HOME", state.path());
        let _cache = EnvVarGuard::set("XDG_CACHE_HOME", cache.path());

        super::sync::run_sync(false, false, false, None).expect("merged-vars deploy succeeds");

        let motd = target_path.join("editor").join("motd");
        assert_eq!(
            std::fs::read(&motd).expect("rendered motd deployed"),
            b"hello local!\n",
            "premise: the local [vars] overlay (greeting=local) must win over base; the deploy \
             renders motd with the MERGED greeting"
        );

        let reg = open_project_registry().expect("open project registry");
        let deployed_vd = reg
            .get(&key)
            .expect("registry get")
            .expect("deploy recorded the artifact")
            .vars_digest;
        assert!(
            deployed_vd.is_some(),
            "premise: a templated artifact's deployed record must stamp a vars_digest"
        );

        reg.remove(&key).expect("drop the record to force rebuild");

        super::sync::run_rebuild_registry().expect("rebuild reconstructs the dropped record");

        let reg = open_project_registry().expect("reopen project registry");
        let rebuilt_vd = reg
            .get(&key)
            .expect("registry get after rebuild")
            .expect("rebuild reconstructed the record")
            .vars_digest;
        (deployed_vd, rebuilt_vd)
    });

    assert_eq!(
        rebuilt_vd, deployed_vd,
        "rebuild-registry must reconcile against the EFFECTIVE merged base+local vars (the same \
         config the deploy used), so the reconstructed vars_digest equals the deployed one; \
         reconciling against base-only vars stamps a divergent digest"
    );
}
