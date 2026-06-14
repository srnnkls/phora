use std::path::{Path, PathBuf};

use super::*;

fn raw_source(body: &str) -> Source {
    let toml = format!("version = 1\n\n[sources.s]\n{body}");
    toml::from_str::<Config>(&toml)
        .expect("raw source DTO deserializes")
        .sources
        .remove("s")
        .expect("source `s` present")
}

fn parse_remote(name: &str, body: &str) -> Result<ParsedSource> {
    ParsedSource::parse(name, &raw_source(body))
}

#[test]
fn shipped_example_configs_parse() {
    Config::parse(include_str!("../../phora.example.toml")).expect("phora.example.toml must parse");
    Config::parse(include_str!("../../phora.local.example.toml"))
        .expect("phora.local.example.toml must parse");
}

#[test]
fn parse_git_url_lands_on_remote_git() {
    let parsed = parse_remote("g", "git = \"https://github.com/me/x.git\"\n")
        .expect("a literal git URL parses to a typed source");
    assert!(
        matches!(&parsed.remote, Remote::Git(g) if g == "https://github.com/me/x.git"),
        "a `git = <url>` source must land on Remote::Git carrying the literal remote"
    );
    assert_eq!(parsed.mode(), SourceMode::Git);
}

#[test]
fn parse_git_localpath_alias_lands_on_local_with_git_mode_and_verbatim_resolution() {
    let parsed = parse_remote("g", "git = \"/home/me/dev/loqui\"\n")
        .expect("a `git = <localpath>` alias parses");
    assert_eq!(
        parsed.mode(),
        SourceMode::Git,
        "the git=<localpath> alias must still classify as SourceMode::Git"
    );
    assert_eq!(
        parsed
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("local alias resolves verbatim"),
        "/home/me/dev/loqui",
        "the git=<localpath> alias resolves the path verbatim"
    );
}

#[test]
fn parse_path_local_lands_on_remote_path() {
    let parsed = parse_remote("p", "path = \"/home/me/dev/loqui\"\n")
        .expect("a local `path` parses to a typed source");
    assert!(
        matches!(&parsed.remote, Remote::Path(p) if p == "/home/me/dev/loqui"),
        "a local `path` source must land on Remote::Path"
    );
    assert_eq!(
        parsed.mode(),
        SourceMode::Git,
        "Remote::Path classifies as Git"
    );
    assert_eq!(
        parsed
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("path local resolves verbatim"),
        "/home/me/dev/loqui"
    );
}

#[test]
fn parse_host_repo_lands_on_remote_host() {
    let parsed = parse_remote("t", "host = \"github\"\nrepo = \"srnnkls/tropos\"\n")
        .expect("host + repo parses to a typed forge source");
    assert!(
        matches!(&parsed.remote, Remote::Host { host, repo, .. } if host == "github" && repo == "srnnkls/tropos"),
        "host + repo must land on Remote::Host"
    );
    assert_eq!(parsed.mode(), SourceMode::Host);
}

#[test]
fn parse_host_path_alias_lands_on_remote_host() {
    let parsed = parse_remote("t", "host = \"github\"\npath = \"srnnkls/tropos\"\n")
        .expect("host + path forge alias parses");
    assert!(
        matches!(&parsed.remote, Remote::Host { host, repo, .. } if host == "github" && repo == "srnnkls/tropos"),
        "the host+path alias must land on Remote::Host with repo from the path alias"
    );
    assert_eq!(parsed.mode(), SourceMode::Host);
}

#[test]
fn parse_bare_repo_lands_on_remote_host_github() {
    let parsed =
        parse_remote("t", "repo = \"srnnkls/tropos\"\n").expect("bare repo parses to a forge");
    assert!(
        matches!(&parsed.remote, Remote::Host { host, repo, .. } if host == "github" && repo == "srnnkls/tropos"),
        "a bare `repo` must default the host to github"
    );
    assert_eq!(parsed.mode(), SourceMode::Host);
}

#[test]
fn parse_url_lands_on_remote_url_with_parsed_digest() {
    let parsed = parse_remote(
            "u",
            "url = \"https://example.com/foo.tar.gz\"\n\
             digest = \"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n",
        )
        .expect("a url + digest parses to a typed source");
    match &parsed.remote {
        Remote::Url { url, digest } => {
            assert_eq!(url, "https://example.com/foo.tar.gz");
            assert!(digest.is_some(), "the digest must be parsed at the edge");
        }
        other => panic!("expected Remote::Url, got {other:?}"),
    }
    assert_eq!(parsed.mode(), SourceMode::Url);
    assert_eq!(
        parsed.source_url(),
        Some("https://example.com/foo.tar.gz"),
        "source_url must expose the url for a Remote::Url"
    );
}

#[test]
fn parse_zero_kind_is_rejected_with_exactly_one_message() {
    let err = parse_remote("x", "branch = \"main\"\n")
        .expect_err("a mode-less source must be rejected by the typed parse");
    match err {
        Error::Config(msg) => assert!(
            msg.contains('x') && msg.contains("exactly one"),
            "zero-kind rejection must name the source and say `exactly one`, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn parse_multiple_kinds_is_rejected() {
    let err = parse_remote(
        "x",
        "git = \"https://github.com/me/x.git\"\nhost = \"github\"\nrepo = \"me/x\"\n",
    )
    .expect_err("multiple kinds must be rejected by the typed parse");
    assert!(matches!(err, Error::Config(_)));
}

#[test]
fn parse_url_with_branch_is_rejected_at_the_edge() {
    let err = parse_remote(
        "pkg",
        "url = \"https://example.com/foo.tar.gz\"\nbranch = \"main\"\n",
    )
    .expect_err("branch on a url source must be rejected at the typed parse");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg") && msg.contains("meaningless"),
            "url+branch rejection must name the source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn parse_url_with_root_is_rejected_at_the_edge() {
    let err = parse_remote(
        "pkg",
        "url = \"https://example.com/foo.tar.gz\"\nroot = \"sub\"\n",
    )
    .expect_err("root on a url source must be rejected at the typed parse");
    assert!(matches!(err, Error::Config(_)));
}

#[test]
fn parse_empty_url_is_rejected_at_the_edge() {
    let err = parse_remote("pkg", "url = \"\"\n")
        .expect_err("an empty url must be rejected at the typed parse");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg") && msg.contains("empty"),
            "empty-url rejection must name the source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn parse_host_without_repo_is_rejected_at_the_edge() {
    let err = parse_remote("t", "host = \"github\"\n")
        .expect_err("a host without a repo is incomplete and must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains('t') && msg.contains("incomplete"),
            "incomplete-forge rejection must name the source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

const EXAMPLE_TOML: &str = r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
root = "modules"

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
tag = "v2.1"
root = "configs"
include = ["editor", "lint"]
exclude = ["**/test/**", "**/*.bak"]

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
root = "languages"
allow_symlinks = false
preserve_executable = true

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]

[targets.vscode]
path = "~/.config/Code/User"
sources = ["dotfiles", "company-configs"]
layout = "flat"

[targets.cupcake-policies]
path = "~/.cupcake/policies/claude"
sources = ["loqui"]
layout = { type = "prefixed", separator = "/" }
"#;

fn tp(name: &str, source: &Source) -> ParsedSource {
    ParsedSource::parse(name, source).expect("source parses to typed form")
}

fn parse_source(toml_body: &str) -> Source {
    let toml =
        format!("version = 1\n\n[sources.s]\ngit = \"https://example.com/x.git\"\n{toml_body}");
    Config::parse(&toml)
        .expect("source toml parses")
        .sources
        .remove("s")
        .expect("source `s` present")
}

fn source(branch: Option<&str>, tag: Option<&str>, rev: Option<&str>) -> ParsedSource {
    use std::fmt::Write as _;
    let mut body = String::new();
    if let Some(b) = branch {
        let _ = writeln!(body, "branch = \"{b}\"");
    }
    if let Some(t) = tag {
        let _ = writeln!(body, "tag = \"{t}\"");
    }
    if let Some(r) = rev {
        let _ = writeln!(body, "rev = \"{r}\"");
    }
    ParsedSource::parse("s", &parse_source(&body)).expect("source parses to typed form")
}

fn target_of<'a>(cfg: &'a Config, name: &str) -> &'a Target {
    cfg.targets.get(name).expect("target present")
}

fn effective_layout(target: &Target) -> LayoutConfig {
    target.layout()
}

// ES-001: a target with no `sources` key receives NOTHING (implicit-all removed).

#[test]
fn resolve_sources_absent_key_receives_nothing() {
    let cfg = Config::parse(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
         [targets.t]\npath = \"~/x\"\n",
    )
    .expect("config with a no-key target parses");
    let resolved = target_of(&cfg, "t").resolve_sources(&cfg.sources);
    assert!(
        resolved.is_empty(),
        "a target with no `sources` key must resolve to NO sources, got {resolved:?}"
    );
}

#[test]
fn resolve_sources_explicit_list_is_verbatim() {
    let cfg = Config::parse(
        "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
         [targets.t]\npath = \"~/x\"\nsources = [\"a\"]\n",
    )
    .expect("config with an explicit-list target parses");
    let resolved = target_of(&cfg, "t").resolve_sources(&cfg.sources);
    let names: Vec<&str> = resolved.iter().map(|b| b.source).collect();
    assert_eq!(
        names,
        vec!["a"],
        "an explicit `sources` list resolves to exactly its listed names"
    );
}

// PAM-001: config parses from phora.toml

#[test]
fn parses_version_and_all_sections_from_example() {
    let cfg = Config::parse(EXAMPLE_TOML).expect("example toml should parse");
    assert_eq!(cfg.version, 1);
    assert_eq!(cfg.sources.len(), 3);
    assert_eq!(cfg.targets.len(), 3);
}

#[test]
fn parses_source_fields_from_example() {
    let cfg = Config::parse(EXAMPLE_TOML).expect("example toml should parse");

    let dotfiles = cfg.sources.get("dotfiles").expect("dotfiles source");
    assert_eq!(
        dotfiles.git.as_deref(),
        Some("https://github.com/me/dotfiles.git")
    );
    assert_eq!(dotfiles.branch.as_deref(), Some("main"));
    assert_eq!(dotfiles.root.as_deref(), Some(Path::new("modules")));

    let company = cfg
        .sources
        .get("company-configs")
        .expect("company-configs source");
    assert_eq!(company.tag.as_deref(), Some("v2.1"));
    let company = tp("company-configs", company);
    assert_eq!(company.includes(), ["editor", "lint"]);
    assert_eq!(company.excludes(), ["**/test/**", "**/*.bak"]);
}

#[test]
fn parses_target_sources_and_layout_from_example() {
    let cfg = Config::parse(EXAMPLE_TOML).expect("example toml should parse");

    let vscode = cfg.targets.get("vscode").expect("vscode target");
    let vscode_sources: Vec<&str> = vscode
        .sources
        .as_deref()
        .expect("vscode declares sources")
        .iter()
        .map(|b| match b {
            crate::config::Binding::Source(name) => name.as_str(),
            crate::config::Binding::Refined(r) => r.source.as_str(),
        })
        .collect();
    assert_eq!(vscode_sources, ["dotfiles", "company-configs"]);
    assert_eq!(
        effective_layout(vscode).artifact_path("loqui", "python"),
        PathBuf::from("python"),
        "flat layout drops the source prefix"
    );

    let cupcake = cfg
        .targets
        .get("cupcake-policies")
        .expect("cupcake-policies target");
    assert_eq!(
        effective_layout(cupcake).artifact_path("loqui", "python"),
        PathBuf::from("loqui/python"),
        "prefixed layout with `/` separator joins source and artifact"
    );
}

#[test]
fn parses_host_auth_token_config() {
    let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }
"#;
    let cfg = Config::parse(toml).expect("host toml should parse");
    let github = cfg.hosts.get("github").expect("github host");
    assert_eq!(
        github
            .remote
            .as_ref()
            .expect("remote present")
            .https_template(),
        Some("https://github.com/{owner}/{repo}.git")
    );
    match github.auth.as_ref().expect("auth config") {
        AuthConfig::Token { env } => assert_eq!(env, "GITHUB_TOKEN"),
        AuthConfig::Ssh { .. } => panic!("expected token auth, got ssh"),
    }
}

// PAM-002: refspec priority and export policy defaults

#[test]
fn refspec_defaults_to_main_branch() {
    assert!(matches!(
        source(None, None, None).refspec(),
        Refspec::Branch(b) if b == "main"
    ));
}

#[test]
fn refspec_uses_rev_when_only_rev_set() {
    let s = source(None, None, Some("abc123"));
    assert!(matches!(s.refspec(), Refspec::Rev(r) if r == "abc123"));
}

#[test]
fn refspec_uses_tag_when_only_tag_set() {
    let s = source(None, Some("v2.1"), None);
    assert!(matches!(s.refspec(), Refspec::Tag(t) if t == "v2.1"));
}

#[test]
fn refspec_uses_branch_when_only_branch_set() {
    let s = source(Some("dev"), None, None);
    assert!(matches!(s.refspec(), Refspec::Branch(b) if b == "dev"));
}

#[test]
fn export_policy_uses_spec_defaults() {
    let policy = source(None, None, None).export_policy();
    assert!(!policy.allow_symlinks);
    assert!(!policy.allow_submodules);
    assert!(policy.preserve_executable);
}

// PAM-003: layout path computation

#[test]
fn flat_layout_places_artifact_at_root() {
    let layout = LayoutConfig::default();
    assert_eq!(layout.kind, LayoutKind::Flat);
    assert_eq!(
        layout.artifact_path("loqui", "python"),
        PathBuf::from("python")
    );
}

#[test]
fn by_source_layout_nests_under_source_dir() {
    let layout: LayoutConfig = toml::from_str("layout = \"by-source\"")
        .map(|w: LayoutWrapper| w.layout)
        .expect("by-source layout parses");
    assert_eq!(
        layout.artifact_path("loqui", "python"),
        PathBuf::from("loqui").join("python")
    );
}

#[test]
fn prefixed_layout_table_uses_given_separator() {
    let layout: LayoutConfig =
        toml::from_str("layout = { type = \"prefixed\", separator = \"/\" }")
            .map(|w: LayoutWrapper| w.layout)
            .expect("prefixed layout parses");
    assert_eq!(
        layout.artifact_path("loqui", "python"),
        PathBuf::from("loqui/python")
    );
}

#[test]
fn prefixed_layout_defaults_separator_to_dash() {
    let layout: LayoutConfig = toml::from_str("layout = { type = \"prefixed\" }")
        .map(|w: LayoutWrapper| w.layout)
        .expect("prefixed layout parses");
    assert_eq!(
        layout.artifact_path("loqui", "python"),
        PathBuf::from("loqui-python")
    );
}

#[derive(Deserialize)]
struct LayoutWrapper {
    layout: LayoutConfig,
}

// PAM-004: effective-config merge

#[test]
fn merge_replaces_base_scalar_with_local() {
    let base = Config::parse(EXAMPLE_TOML).expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let loqui = effective.sources.get("loqui").expect("loqui source kept");
    assert_eq!(loqui.git.as_deref(), Some("/home/soeren/dev/loqui"));
    assert_eq!(loqui.branch.as_deref(), Some("main"));
    assert!(
        loqui.tag.is_none(),
        "local branch override must clear the base refspec group (tag)"
    );
    assert_eq!(
        loqui.root.as_deref(),
        Some(Path::new("languages")),
        "base-only field must survive when local does not set it"
    );
    assert!(matches!(tp("loqui", loqui).refspec(), Refspec::Branch(b) if b == "main"));
}

#[test]
fn merge_replaces_base_array_no_concatenation() {
    let base = Config::parse(EXAMPLE_TOML).expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
include = ["only-this"]
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let company = tp(
        "company-configs",
        effective
            .sources
            .get("company-configs")
            .expect("company-configs kept"),
    );
    assert_eq!(company.includes(), ["only-this"]);
}

#[test]
fn merge_explicit_empty_array_clears_base_array() {
    let base = Config::parse(EXAMPLE_TOML).expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
include = []
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let company = tp(
        "company-configs",
        effective
            .sources
            .get("company-configs")
            .expect("company-configs kept"),
    );
    assert!(
        company.includes().is_empty(),
        "an explicit empty `include = []` in local must replace (clear) the base array, \
             not be ignored as if unset"
    );
}

#[test]
fn merge_adds_local_only_source() {
    let base = Config::parse(EXAMPLE_TOML).expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.local-extra]
git = "/home/soeren/dev/extra"
branch = "main"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert!(effective.sources.contains_key("local-extra"));
    assert!(
        effective.sources.contains_key("dotfiles"),
        "base-only source must be kept"
    );
}

#[test]
fn merge_without_local_is_identity() {
    let base = Config::parse(EXAMPLE_TOML).expect("base parses");
    let effective = merge_configs(base, None);
    assert_eq!(effective.sources.len(), 3);
    assert_eq!(effective.targets.len(), 3);
    assert_eq!(effective.hosts.len(), 1);
    assert!(effective.hosts.contains_key("github"), "host survives");
    assert_eq!(
        effective
            .sources
            .get("loqui")
            .expect("loqui kept")
            .git
            .as_deref(),
        Some("https://github.com/srnnkls/loqui.git")
    );
    assert_eq!(
        effective
            .targets
            .get("neovim")
            .expect("neovim target kept")
            .path,
        PathBuf::from("~/.config/nvim")
    );
    assert_eq!(
        effective_layout(target_of(&effective, "cupcake-policies"))
            .artifact_path("loqui", "python"),
        PathBuf::from("loqui/python"),
        "identity merge preserves the prefixed `/` layout"
    );
}

#[test]
fn merge_path_only_target_override_preserves_base_layout() {
    let base = Config::parse(EXAMPLE_TOML).expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[targets.cupcake-policies]
path = "/local/override/policies"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let cupcake = target_of(&effective, "cupcake-policies");

    assert_eq!(
        cupcake.path,
        PathBuf::from("/local/override/policies"),
        "local path override must take effect"
    );
    assert_eq!(
        effective_layout(cupcake).artifact_path("loqui", "python"),
        PathBuf::from("loqui/python"),
        "a path-only override must NOT reset the base prefixed `/` layout to flat"
    );
}

#[test]
fn merge_partial_source_override_preserves_base_policy_flags() {
    let base = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
root = "languages"
allow_symlinks = true
preserve_executable = false
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let policy = tp("loqui", effective.sources.get("loqui").expect("loqui kept")).export_policy();

    assert!(
        policy.allow_symlinks,
        "git+branch-only override must NOT reset base allow_symlinks=true to default"
    );
    assert!(
        !policy.preserve_executable,
        "git+branch-only override must NOT reset base preserve_executable=false to default"
    );
}

#[test]
fn merge_host_auth_only_override_preserves_base_remote() {
    let base = Config::parse(
        r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[hosts.github]
auth = { type = "token", env = "GITHUB_TOKEN_WORK" }
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let github = effective.hosts.get("github").expect("github host kept");

    assert_eq!(
        github
            .remote
            .as_ref()
            .expect("remote present")
            .https_template(),
        Some("https://github.com/{owner}/{repo}.git"),
        "an auth-only local override must NOT clear the base remote"
    );
    match github.auth.as_ref().expect("auth config") {
        AuthConfig::Token { env } => assert_eq!(env, "GITHUB_TOKEN_WORK"),
        AuthConfig::Ssh { .. } => panic!("expected token auth, got ssh"),
    }
}

// PAM-005: validation

#[test]
fn unknown_auth_key_is_rejected() {
    let toml = r#"
version = 1

[hosts.github]
auth = { type = "token", env = "X", bogus = 1 }
"#;
    let err = Config::parse(toml).expect_err("unknown auth key must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("bogus"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn unknown_source_key_is_rejected() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
brunch = "main"
"#;
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "unknown source key must produce a config error"
    );
}

#[test]
fn unknown_top_level_key_is_rejected() {
    let toml = r#"
version = 1
bogus = "value"
"#;
    let err = Config::parse(toml).expect_err("unknown top-level key must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("bogus"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn unknown_target_key_is_rejected() {
    let toml = r#"
version = 1

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
destination = "elsewhere"
"#;
    let err = Config::parse(toml).expect_err("unknown target key must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("destination"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn unknown_host_key_is_rejected() {
    let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
proxy = "http://localhost"
"#;
    let err = Config::parse(toml).expect_err("unknown host key must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("proxy"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_with_branch_and_tag_is_rejected() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
tag = "v1.0"
"#;
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "specifying both branch and tag must be rejected"
    );
}

#[test]
fn source_with_tag_and_rev_is_rejected() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
tag = "v1.0"
rev = "abc123"
"#;
    assert!(matches!(Config::parse(toml), Err(Error::Config(_))));
}

#[test]
fn source_with_branch_and_rev_is_rejected() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
rev = "abc123"
"#;
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "specifying both branch and rev must be rejected"
    );
}

#[test]
fn invalid_layout_kind_is_rejected() {
    let toml = r#"
version = 1

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
layout = "fnord"
"#;
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "an unrecognized layout type must be rejected, not silently coerced to flat"
    );
}

#[test]
fn unknown_layout_table_key_is_rejected() {
    let toml = r#"
version = 1

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
layout = { type = "prefixed", seperator = "/" }
"#;
    let err = Config::parse(toml).expect_err("unknown layout key must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("seperator"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

// DLD-001: deploy mode field, merge, digest exclusion

fn deploy_of(cfg: &Config, source: &str) -> Option<DeployMode> {
    cfg.sources.get(source).expect("source present").deploy
}

#[test]
fn deploy_absent_is_copy_and_link_parses() {
    let copy_default = parse_source("");
    assert_eq!(
        copy_default.deploy.unwrap_or(DeployMode::Copy),
        DeployMode::Copy,
        "an absent `deploy` must resolve to the Copy default"
    );

    let linked = parse_source("deploy = \"link\"\n");
    assert_eq!(
        linked.deploy,
        Some(DeployMode::Link),
        "deploy = \"link\" must parse to DeployMode::Link"
    );

    let explicit_copy = parse_source("deploy = \"copy\"\n");
    assert_eq!(explicit_copy.deploy, Some(DeployMode::Copy));
}

#[test]
fn merge_local_deploy_override_replaces_base() {
    let base = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
deploy = "copy"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
deploy = "link"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        deploy_of(&effective, "loqui"),
        Some(DeployMode::Link),
        "a local `deploy = link` must override the base `deploy = copy`"
    );
}

#[test]
fn merge_partial_override_preserves_base_deploy() {
    let base = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
tag = "v1.0"
deploy = "link"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"
branch = "main"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        deploy_of(&effective, "loqui"),
        Some(DeployMode::Link),
        "a git+branch-only override that does not set deploy must keep the base `deploy = link`"
    );
}

#[test]
fn config_digest_ignores_deploy_for_lock_stability() {
    let without = tp(
        "s",
        &parse_source("root = \"languages\"\ninclude = [\"editor\"]\n"),
    );
    let with_link = tp(
        "s",
        &parse_source("root = \"languages\"\ninclude = [\"editor\"]\ndeploy = \"link\"\n"),
    );
    assert_eq!(
        with_link.config_digest(),
        without.config_digest(),
        "deploy mode does not change exported ODB content; it must be excluded from \
             config_digest or a link flip would invalidate the lock (source_matches, lock.rs:50)"
    );
}

#[test]
fn unknown_deploy_value_is_rejected_naming_it() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
deploy = "wormhole"
"#;
    let err = Config::parse(toml).expect_err("unknown deploy value must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("wormhole"),
            "error should name the offending deploy value, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_ref_on_link_source_is_rejected() {
    let cfg = Config::parse(
        "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\ndeploy = \"link\"\n\n\
         [targets.t]\npath = \"~/x\"\n\
         sources = [{ source = \"dotfiles\", tag = \"v1\" }]\n",
    )
    .expect("a link source with a tag-pinned binding still parses as a DTO");

    let err = cfg
        .validate()
        .expect_err("pinning a ref on a deploy=link source must be a config error");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("link"),
            "the error must name the source `dotfiles` and indicate the ref is meaningless on a \
             link source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_ref_on_copy_source_validates() {
    let cfg = Config::parse(
        "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
         [targets.t]\npath = \"~/x\"\n\
         sources = [{ source = \"dotfiles\", tag = \"v1\" }]\n",
    )
    .expect("a copy source with a tag-pinned binding parses");

    cfg.validate()
        .expect("a ref pin on a (default copy) source is allowed");
}

#[test]
fn valid_config_parses_ok() {
    assert!(
        Config::parse(EXAMPLE_TOML).is_ok(),
        "a single-refspec, no-unknown-keys config must parse cleanly"
    );
}

// HAS-001: host-aliased sources — host/path/protocol + Host.remote string-or-table

#[test]
fn host_remote_parses_as_single_string_template() {
    let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"
"#;
    let cfg = Config::parse(toml).expect("a string `remote` template must parse");
    let github = cfg.hosts.get("github").expect("github host");
    let remote = github.remote.as_ref().expect("remote present");
    assert_eq!(
        remote.https_template(),
        Some("https://github.com/{path}.git"),
        "a bare string `remote` is the https template"
    );
    assert_eq!(
        remote.ssh_template(),
        None,
        "a bare string `remote` carries no ssh shape"
    );
}

#[test]
fn host_remote_parses_as_https_ssh_table() {
    let toml = r#"
version = 1

[hosts.company]
remote = { https = "https://git.co/{path}.git", ssh = "git@git.co:{path}.git" }
"#;
    let cfg = Config::parse(toml).expect("a `{ https, ssh }` remote table must parse");
    let company = cfg.hosts.get("company").expect("company host");
    let remote = company.remote.as_ref().expect("remote present");
    assert_eq!(
        remote.https_template(),
        Some("https://git.co/{path}.git"),
        "the https key must be exposed"
    );
    assert_eq!(
        remote.ssh_template(),
        Some("git@git.co:{path}.git"),
        "the ssh key must be exposed"
    );
}

#[test]
fn host_remote_table_with_unknown_key_is_rejected_naming_it() {
    let toml = r#"
version = 1

[hosts.company]
remote = { https = "https://git.co/{path}.git", gopher = "x" }
"#;
    let err = Config::parse(toml).expect_err("an unknown key in the remote table must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("gopher"),
            "error should name the offending remote-table key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn host_remote_empty_table_is_rejected() {
    let toml = r"
version = 1

[hosts.company]
remote = {}
";
    let err = Config::parse(toml)
        .expect_err("an empty `remote = {}` table with no protocol keys must be rejected");
    match err {
        Error::Config(msg) => {
            let m = msg.to_lowercase();
            assert!(
                m.contains("company")
                    || m.contains("at least one")
                    || m.contains("protocol")
                    || m.contains("empty"),
                "empty-remote-table rejection must be a domain error explaining the \
                     missing protocol key (mention the host `company`, or \"at least one\"/\
                     \"protocol\"/\"empty\"), not a generic serde error, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn host_path_source_parses_and_exposes_fields() {
    let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
branch = "main"
"#;
    let cfg = Config::parse(toml).expect("a host+path source must parse");
    let tropos = cfg.sources.get("tropos").expect("tropos source");
    assert_eq!(tropos.host.as_deref(), Some("github"));
    assert_eq!(tropos.path.as_deref(), Some("srnnkls/tropos"));
    assert_eq!(tropos.branch.as_deref(), Some("main"));
    assert!(
        tropos.git.is_none(),
        "a host+path source must carry no literal git remote"
    );
}

#[test]
fn source_with_both_git_and_host_is_rejected_naming_source() {
    let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
git = "https://github.com/srnnkls/tropos.git"
host = "github"
path = "srnnkls/tropos"
"#;
    let err = Config::parse(toml)
        .expect_err("a source that sets both git and host must be rejected (mode exclusivity)");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("tropos"),
            "mode-exclusivity error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_with_git_and_path_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.tropos]
git = "https://github.com/srnnkls/tropos.git"
path = "srnnkls/tropos"
"#;
    let err = Config::parse(toml).expect_err(
        "a source that sets both `git` and `path` is dual-mode (path implies host-mode) \
             and must be rejected",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("tropos"),
            "mode-exclusivity error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_with_host_but_no_path_is_rejected_naming_source() {
    let toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
"#;
    let err = Config::parse(toml)
        .expect_err("a host source without a path must be rejected (incomplete mode group)");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("tropos"),
            "error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn top_level_protocol_ssh_parses_and_default_is_https() {
    let with_ssh = Config::parse(
        r#"
version = 1
protocol = "ssh"
"#,
    )
    .expect("a top-level protocol = \"ssh\" must parse");
    assert_eq!(
        with_ssh.protocol,
        Some(Protocol::Ssh),
        "top-level `protocol = ssh` must parse to Protocol::Ssh"
    );

    let with_https = Config::parse(
        r#"
version = 1
protocol = "https"
"#,
    )
    .expect("a top-level protocol = \"https\" must parse");
    assert_eq!(
        with_https.protocol,
        Some(Protocol::Https),
        "top-level `protocol = https` must parse to Protocol::Https (both enum arms reachable)"
    );

    let defaulted = Config::parse("version = 1\n").expect("omitting protocol must parse");
    assert!(
        defaulted.protocol.is_none(),
        "an omitted top-level protocol is None (https is the effective default downstream)"
    );
}

#[test]
fn merge_host_path_source_branch_only_override_preserves_mode_and_remote() {
    let base = Config::parse(
        r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
tag = "v1.0"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.tropos]
branch = "main"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let tropos = effective.sources.get("tropos").expect("tropos kept");
    assert_eq!(
        tropos.host.as_deref(),
        Some("github"),
        "a branch-only local override must NOT clear the base host (mode group is atomic)"
    );
    assert_eq!(
        tropos.path.as_deref(),
        Some("srnnkls/tropos"),
        "a branch-only local override must preserve the base path"
    );
    assert!(
        tropos.git.is_none(),
        "the host+path mode must not flip to literal-git on a partial override"
    );
    assert_eq!(
        tropos.branch.as_deref(),
        Some("main"),
        "the local branch override must take effect"
    );
    assert!(
        tropos.tag.is_none(),
        "the local branch override clears the base refspec group (tag)"
    );
}

#[test]
fn merge_local_source_referencing_base_only_host_validates_after_merge() {
    let base = Config::parse(
        r#"
version = 1

[hosts.company]
remote = "https://git.co/{path}.git"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.internal]
host = "company"
path = "team/sub/proj"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    effective.validate().expect(
        "a local source referencing a host defined only in the base must pass POST-MERGE \
             validation (the host is unknown per-file but known after merge)",
    );
}

#[test]
fn protocol_ssh_with_https_only_remote_fails_post_merge_validation() {
    let cfg = Config::parse(
        r#"
version = 1

[hosts.github]
remote = "https://github.com/{path}.git"

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
protocol = "ssh"
"#,
    )
    .expect("the document parses; the protocol/remote mismatch is a post-merge validation");
    let err = cfg
        .validate()
        .expect_err("protocol = ssh against an https-only remote must fail validation");
    match err {
        Error::Config(msg) => {
            assert!(
                msg.contains("tropos"),
                "validation error must name the offending source, got: {msg}"
            );
            assert!(
                msg.contains("github"),
                "validation error must name the offending host, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn unknown_host_reference_fails_post_merge_validation_naming_source_and_host() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.tropos]
host = "ghost"
path = "srnnkls/tropos"
"#,
    )
    .expect("a single-file source referencing an undefined host parses; validity is post-merge");
    let err = cfg
        .validate()
        .expect_err("a host with no built-in or [hosts] definition must fail validation");
    match err {
        Error::Config(msg) => {
            assert!(
                msg.contains("tropos"),
                "unknown-host error must name the source, got: {msg}"
            );
            assert!(
                msg.contains("ghost"),
                "unknown-host error must name the host, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn merge_configs_overlays_top_level_protocol() {
    let base = Config::parse(
        r#"
version = 1
protocol = "https"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1
protocol = "ssh"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        effective.protocol,
        Some(Protocol::Ssh),
        "merge_configs must overlay the top-level protocol (local wins)"
    );
}

#[test]
fn merge_configs_keeps_base_protocol_when_local_omits_it() {
    let base = Config::parse(
        r#"
version = 1
protocol = "ssh"
"#,
    )
    .expect("base parses");
    let local = Config::parse("version = 1\n").expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        effective.protocol,
        Some(Protocol::Ssh),
        "a local config that omits protocol must preserve the base protocol"
    );
}

#[test]
fn source_with_no_mode_is_allowed_as_partial_overlay() {
    let toml = r#"
version = 1

[sources.x]
branch = "main"
"#;
    let cfg = Config::parse(toml).expect(
        "a mode-less source fragment (no git, no host/path) must parse so a local override \
             like `[sources.x]\\nbranch = \"main\"` works as a partial overlay",
    );
    let x = cfg.sources.get("x").expect("x source");
    assert!(x.git.is_none(), "no literal git on a mode-less fragment");
    assert!(x.host.is_none(), "no host on a mode-less fragment");
    assert!(x.path.is_none(), "no path on a mode-less fragment");
    assert_eq!(
        x.branch.as_deref(),
        Some("main"),
        "the overlay field must survive parsing"
    );
}

#[test]
fn source_with_repo_and_no_host_defaults_to_github() {
    let toml = r#"
version = 1

[sources.tropos]
repo = "srnnkls/tropos"
"#;
    let cfg = Config::parse(toml)
        .expect("a source with `repo` but no `host` defaults host to github and parses");
    cfg.validate()
        .expect("a bare-repo source must validate (host defaults to github)");
    let tropos = tp(
        "tropos",
        cfg.sources.get("tropos").expect("tropos source present"),
    );
    assert_eq!(
        tropos
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("bare-repo source resolves against the built-in github forge"),
        "https://github.com/srnnkls/tropos.git",
        "an omitted `host` with `repo` set must default to github, not merely parse Ok"
    );
}

#[test]
fn host_source_with_protocol_matching_remote_passes_validation() {
    let cfg = Config::parse(
        r#"
version = 1

[hosts.company]
remote = { https = "https://git.co/{path}.git", ssh = "git@git.co:{path}.git" }

[sources.internal]
host = "company"
path = "team/sub/proj"
protocol = "ssh"
"#,
    )
    .expect("a host+path source with a matching protocol must parse");
    cfg.validate().expect(
        "protocol = ssh against a remote table that HAS an ssh key must pass validation \
             (guards against a validate() that always errors)",
    );
}

#[test]
fn protocol_https_with_ssh_only_remote_fails_validation() {
    let cfg = Config::parse(
        r#"
version = 1

[hosts.sshonly]
remote = { ssh = "git@h:{path}.git" }

[sources.repo]
host = "sshonly"
path = "o/r"
"#,
    )
    .expect("the document parses; the effective-protocol/remote mismatch is post-merge validation");
    let err = cfg.validate().expect_err(
        "a source whose effective protocol is the default https against an ssh-only remote \
             (no https template) must fail validation",
    );
    match err {
        Error::Config(msg) => {
            assert!(
                msg.contains("repo"),
                "validation error must name the offending source, got: {msg}"
            );
            assert!(
                msg.contains("sshonly"),
                "validation error must name the offending host, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn protocol_ssh_with_ssh_only_remote_passes_validation() {
    let cfg = Config::parse(
        r#"
version = 1

[hosts.sshonly]
remote = { ssh = "git@h:{path}.git" }

[sources.repo]
host = "sshonly"
path = "o/r"
protocol = "ssh"
"#,
    )
    .expect("a source against an ssh-only host with protocol = ssh must parse");
    cfg.validate().expect(
        "protocol = ssh against an ssh-only remote that HAS an ssh template must pass \
             validation (guards against an over-broad missing-template error)",
    );
}

#[test]
fn shipped_example_toml_parses_and_validates() {
    let cfg = Config::parse(include_str!("../../phora.example.toml"))
        .expect("the shipped phora.example.toml must parse");
    cfg.validate()
        .expect("the shipped phora.example.toml must pass post-merge validation");
}

// HTP-007: the shipped example must demonstrate a url source with an integrity digest

#[test]
fn shipped_example_toml_includes_a_url_source() {
    let cfg = Config::parse(include_str!("../../phora.example.toml"))
        .expect("the shipped phora.example.toml must parse");
    cfg.validate()
        .expect("the shipped phora.example.toml must pass post-merge validation");

    let parsed = cfg
        .parsed_sources()
        .expect("shipped example parses to typed form");
    let url_source = parsed
        .values()
        .find(|source| source.mode() == SourceMode::Url)
        .expect("the shipped phora.example.toml must demonstrate a url source");

    let url = url_source
        .source_url()
        .expect("a url source must expose its url via source_url()");
    assert!(
        url.starts_with("https://"),
        "the example url source must use an https url, got `{url}`"
    );

    assert!(
        url_source.digest().is_some(),
        "the example url source must carry a well-formed integrity `digest`"
    );
}

// PBR-008: the shipped example must teach per-binding refinement (aliasing via `as`)

#[test]
fn shipped_example_toml_demonstrates_a_refined_binding_alias() {
    let cfg = Config::parse(include_str!("../../phora.example.toml"))
        .expect("the shipped phora.example.toml must parse");
    cfg.validate()
        .expect("the shipped phora.example.toml must pass post-merge validation");

    let aliasing = cfg
        .targets
        .values()
        .flat_map(|target| target.sources.iter().flatten())
        .find(|binding| {
            matches!(binding, Binding::Refined(_)) && binding.identity() != binding.source()
        })
        .expect(
            "the shipped phora.example.toml must demonstrate a refined binding whose `as` \
             identity differs from its source (e.g. { source = \"dotfiles\", as = \"nvim\" })",
        );

    assert_ne!(
        aliasing.identity(),
        aliasing.source(),
        "the refined binding must alias the source to a distinct identity to teach the feature"
    );
}

// PTV-007: the shipped example must demonstrate per-target versioning

#[test]
fn example_toml_demonstrates_per_target_versioning() {
    let cfg = Config::parse(include_str!("../../phora.example.toml"))
        .expect("the shipped phora.example.toml must parse");
    cfg.validate()
        .expect("the shipped phora.example.toml must pass post-merge validation");

    let parsed = cfg
        .parsed_sources()
        .expect("shipped example parses to typed form");

    let has_two_version_pair = cfg.targets.values().any(|target| {
        let resolved = target.resolve_sources(&parsed);
        resolved.iter().enumerate().any(|(i, a)| {
            resolved[i + 1..].iter().any(|b| {
                a.source == b.source
                    && a.identity != b.identity
                    && format!("{:?}", a.effective_ref) != format!("{:?}", b.effective_ref)
            })
        })
    });

    assert!(
        has_two_version_pair,
        "the shipped phora.example.toml must demonstrate per-target versioning: ONE source \
         bound into a target TWICE under two DISTINCT `as` identities resolving to DISTINCT \
         effective refs (e.g. two tags). Without it the README/example claim is untruthful."
    );
}

// HAS-002: resolved_remote + single built-in forge registry

fn hosts_of(toml: &str) -> BTreeMap<String, Host> {
    Config::parse(toml).expect("hosts toml parses").hosts
}

fn source_of(toml: &str, name: &str) -> ParsedSource {
    let raw = Config::parse(toml)
        .expect("source toml parses")
        .sources
        .remove(name)
        .expect("named source present");
    ParsedSource::parse(name, &raw).expect("named source parses to typed form")
}

#[test]
fn resolved_remote_github_https_and_ssh_for_owner_repo_path() {
    let host_toml = r#"
version = 1

[hosts.github]
remote = { https = "https://github.com/{owner}/{repo}.git", ssh = "git@github.com:{owner}/{repo}.git" }
"#;
    let hosts = hosts_of(host_toml);
    let source = source_of(
        r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
        "tropos",
    );

    assert_eq!(
        source
            .resolved_remote(&hosts, Protocol::Https)
            .expect("https resolves"),
        "https://github.com/srnnkls/tropos.git",
        "https template must fill {{owner}}/{{repo}} from the path"
    );
    assert_eq!(
        source
            .resolved_remote(&hosts, Protocol::Ssh)
            .expect("ssh resolves"),
        "git@github.com:srnnkls/tropos.git",
        "ssh template must produce the scp-style remote"
    );
}

#[test]
fn resolved_remote_github_uses_builtin_when_no_user_host() {
    let source = source_of(
        r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
        "tropos",
    );
    let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

    let https = source
        .resolved_remote(&no_user_hosts, Protocol::Https)
        .expect("built-in github https resolves with no user hosts");
    assert_eq!(
        https, "https://github.com/srnnkls/tropos.git",
        "the built-in github forge must resolve EXACTLY without a user [hosts.github] def"
    );

    let ssh = source
        .resolved_remote(&no_user_hosts, Protocol::Ssh)
        .expect("built-in github ssh resolves with no user hosts");
    assert_eq!(
        ssh, "git@github.com:srnnkls/tropos.git",
        "the built-in github forge must ship the EXACT scp-style ssh shape"
    );
}

#[test]
fn resolved_remote_gitlab_subgroup_path_reconstructs_full_path() {
    let source = source_of(
        r#"
version = 1

[sources.internal]
host = "gitlab"
path = "group/sub/proj"
"#,
        "internal",
    );
    let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

    let https = source
        .resolved_remote(&no_user_hosts, Protocol::Https)
        .expect("gitlab subgroup https resolves");
    assert!(
        https.contains("group/sub/proj"),
        "a gitlab subgroup path must reconstruct fully via {{owner}}/{{repo}} ≡ {{path}}, \
             not collapse to the first/last segment, got: {https}"
    );

    let ssh = source
        .resolved_remote(&no_user_hosts, Protocol::Ssh)
        .expect("gitlab subgroup ssh resolves");
    assert!(
        ssh.contains("group/sub/proj"),
        "the ssh shape must also carry the full subgroup path, got: {ssh}"
    );
}

#[test]
fn resolved_remote_srht_uses_tilde_path_shape() {
    let source = source_of(
        r#"
version = 1

[sources.aerc]
host = "sr.ht"
repo = "~rjarry/aerc"
"#,
        "aerc",
    );
    let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

    let https = source
        .resolved_remote(&no_user_hosts, Protocol::Https)
        .expect("sr.ht https resolves via the built-in {path} template");
    assert!(
        https.contains("~rjarry/aerc"),
        "the built-in sr.ht template must use {{path}} verbatim so the ~user shape survives, \
             got: {https}"
    );
    assert!(
        https.contains('~'),
        "sr.ht resolved remote must retain the leading ~, got: {https}"
    );

    let ssh = source
        .resolved_remote(&no_user_hosts, Protocol::Ssh)
        .expect("sr.ht ssh resolves via the built-in {path} template");
    assert!(
        ssh.contains("~rjarry/aerc"),
        "the built-in sr.ht ssh template must also use {{path}} verbatim so the ~user shape \
             survives under ssh, got: {ssh}"
    );
    assert!(
        ssh.contains('~'),
        "sr.ht ssh resolved remote must retain the leading ~, got: {ssh}"
    );
}

#[test]
fn resolved_remote_user_host_overrides_builtin_github() {
    let host_toml = r#"
version = 1

[hosts.github]
remote = { https = "https://ghe.corp.example/{owner}/{repo}.git", ssh = "git@ghe.corp.example:{owner}/{repo}.git" }
"#;
    let hosts = hosts_of(host_toml);
    let source = source_of(
        r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
        "tropos",
    );

    let https = source
        .resolved_remote(&hosts, Protocol::Https)
        .expect("override resolves");
    assert_eq!(
        https, "https://ghe.corp.example/srnnkls/tropos.git",
        "a user [hosts.github] must override the built-in github forge in resolved_remote"
    );

    let ssh = source
        .resolved_remote(&hosts, Protocol::Ssh)
        .expect("override resolves under ssh");
    assert_eq!(
        ssh, "git@ghe.corp.example:srnnkls/tropos.git",
        "the user [hosts.github] ssh template must override the built-in github ssh shape too, \
             not fall back to git@github.com"
    );
}

#[test]
fn resolved_remote_git_mode_returns_literal_verbatim_ignoring_protocol() {
    let source = source_of(
        r#"
version = 1

[sources.dotfiles]
git = "https://example.com/me/dotfiles.git"
"#,
        "dotfiles",
    );
    let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

    assert_eq!(
        source
            .resolved_remote(&no_user_hosts, Protocol::Https)
            .expect("git-mode resolves"),
        "https://example.com/me/dotfiles.git",
        "a git-mode source returns its literal git verbatim under https"
    );
    assert_eq!(
        source
            .resolved_remote(&no_user_hosts, Protocol::Ssh)
            .expect("git-mode resolves under ssh too"),
        "https://example.com/me/dotfiles.git",
        "a git-mode source ignores protocol: the literal git is returned verbatim under ssh"
    );
}

#[test]
fn resolved_remote_unknown_host_errors() {
    let source = source_of(
        r#"
version = 1

[sources.tropos]
host = "ghost"
path = "srnnkls/tropos"
"#,
        "tropos",
    );
    let no_user_hosts: BTreeMap<String, Host> = BTreeMap::new();

    let err = source
        .resolved_remote(&no_user_hosts, Protocol::Https)
        .expect_err("an unknown host (no built-in, no user def) must error");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("ghost") && msg.contains("tropos"),
            "the unknown-host error must name BOTH the offending source and the host, \
                 consistent with HAS-001's validate() wording \
                 (`source `tropos` references unknown host `ghost``), got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn resolved_remote_ssh_without_ssh_template_errors() {
    let host_toml = r#"
version = 1

[hosts.github]
remote = "https://github.com/{owner}/{repo}.git"
"#;
    let hosts = hosts_of(host_toml);
    let source = source_of(
        r#"
version = 1

[sources.tropos]
host = "github"
path = "srnnkls/tropos"
"#,
        "tropos",
    );

    let err = source
        .resolved_remote(&hosts, Protocol::Ssh)
        .expect_err("protocol = ssh against an https-only remote must error");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("github") && (msg.contains("ssh") || msg.contains("template")),
            "the missing-template error must NAME the offending host AND indicate the missing \
                 ssh/template; an error that merely contains \"ssh\" without naming the host \
                 must fail this test, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn builtin_forges_ship_all_five_with_both_shapes() {
    let forges = builtin_forges();
    for name in ["github", "gitlab", "codeberg", "sr.ht", "bitbucket"] {
        let host = forges
            .get(name)
            .unwrap_or_else(|| panic!("built-in forge `{name}` must ship"));
        let remote = host
            .remote
            .as_ref()
            .unwrap_or_else(|| panic!("built-in forge `{name}` must carry a remote"));
        assert!(
            remote.https_template().is_some(),
            "built-in forge `{name}` must ship an https shape"
        );
        assert!(
            remote.ssh_template().is_some(),
            "built-in forge `{name}` must ship an ssh shape"
        );
    }
}

/// Build a `[sources.s]` document body with no implicit `git` line (unlike the
/// `parse_source` helper, which always injects a git remote and would make a
/// `url` source dual-mode).
fn parse_url_source(body: &str) -> Result<Source> {
    let toml = format!("version = 1\n\n[sources.s]\n{body}");
    Config::parse(&toml).map(|mut cfg| {
        cfg.sources
            .remove("s")
            .expect("source `s` present after parse")
    })
}

#[test]
fn url_only_source_parses_and_source_url_returns_it() {
    let s = parse_url_source("url = \"https://example.com/foo.tar.gz\"\n")
        .expect("a url-only source must parse");
    assert_eq!(
        tp("s", &s).source_url(),
        Some("https://example.com/foo.tar.gz"),
        "source_url() must return the configured url for a url-mode source"
    );
    assert!(
        s.git.is_none(),
        "a url-mode source carries no literal git remote"
    );
    assert!(
        s.host.is_none() && s.path.is_none(),
        "a url-mode source carries neither host nor path"
    );
}

#[test]
fn non_url_source_has_no_source_url() {
    let git_mode = tp("s", &parse_source(""));
    assert_eq!(
        git_mode.source_url(),
        None,
        "a git-mode source must not report a source_url()"
    );
}

#[test]
fn url_with_digest_parses_and_exposes_digest_string() {
    let s = parse_url_source(
            "url = \"https://example.com/foo.tar.gz\"\n\
             digest = \"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n",
        )
        .expect("a url source with a digest must parse");
    assert_eq!(
        s.digest.as_deref(),
        Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
        "the optional `digest` string must round-trip onto the Source"
    );
}

#[test]
fn source_with_url_and_git_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
git = "https://github.com/me/foo.git"
"#;
    let err = Config::parse(toml)
        .expect_err("a source that sets both `url` and `git` is dual-mode and must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "three-way mode-exclusivity error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_with_url_and_host_path_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
host = "github"
path = "me/foo"
"#;
    let err = Config::parse(toml).expect_err(
        "a source that sets both `url` and host+path is dual-mode and must be rejected",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "three-way mode-exclusivity error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn url_source_with_branch_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
branch = "main"
"#;
    let err = Config::parse(toml)
        .expect_err("`branch` is meaningless on a static url resource and must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "the url-vs-refspec error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn url_source_with_root_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
root = "subdir"
"#;
    let err = Config::parse(toml)
        .expect_err("`root` is meaningless on a pre-stripped url archive and must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "the url-vs-root error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn url_source_with_tag_is_rejected() {
    let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
tag = "v1.0"
"#;
    let err = Config::parse(toml)
        .expect_err("`tag` on a url source must be rejected (a static resource has no refspec)");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "the url-vs-refspec error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn url_source_with_rev_is_rejected() {
    let toml = r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
rev = "abc123"
"#;
    let err = Config::parse(toml)
        .expect_err("`rev` on a url source must be rejected (a static resource has no refspec)");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "the url-vs-refspec error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn merge_local_url_override_clears_base_git_refspec_and_root() {
    let base = Config::parse(
        r#"
version = 1

[sources.pkg]
git = "https://github.com/me/pkg.git"
branch = "main"
root = "subdir"
"#,
    )
    .expect("base parses");
    let local = parse_url_source("url = \"https://example.com/foo.tar.gz\"\n")
        .expect("local url source parses");
    let merged = base
        .sources
        .get("pkg")
        .expect("pkg present")
        .clone()
        .merged_with(local);

    assert_eq!(
        tp("pkg", &merged).source_url(),
        Some("https://example.com/foo.tar.gz"),
        "a local url override must switch the merged source into url mode"
    );
    assert!(merged.git.is_none(), "switching to url mode must clear git");
    assert!(
        merged.branch.is_none() && merged.tag.is_none() && merged.rev.is_none(),
        "switching to url mode must clear the stale base refspec"
    );
    assert!(
        merged.root.is_none(),
        "switching to url mode must clear the stale base root"
    );
}

#[test]
fn validate_rejects_url_source_with_stale_refspec() {
    let mut cfg = Config::parse(
        r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"
"#,
    )
    .expect("a url-only source parses; the stale refspec is injected post-parse");
    let pkg = cfg.sources.get_mut("pkg").expect("pkg present");
    pkg.branch = Some("main".to_owned());
    pkg.root = Some(PathBuf::from("subdir"));

    let err = cfg
        .validate()
        .expect_err("a url source carrying a stale `branch`/`root` must be rejected by validate()");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "validate() url-vs-refspec error must name the source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn empty_url_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.pkg]
url = ""
"#;
    let err = Config::parse(toml)
        .expect_err("an empty `url` is not a usable resource and must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg"),
            "empty-url error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn download_digest_parses_sha256_with_bytes() {
    use std::str::FromStr as _;
    let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let expected: [u8; 32] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
        0xcd, 0xef,
    ];
    let digest = crate::kernel::Digest::from_str(&format!("sha256:{hex}"))
        .expect("a sha256 digest must parse");
    assert_eq!(
        digest.bytes(),
        expected.as_slice(),
        "the decoded sha256 digest bytes must match the hex, not merely be non-empty"
    );
}

#[test]
fn download_digest_parses_blake3_with_bytes() {
    use std::str::FromStr as _;
    let hex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let digest = crate::kernel::Digest::from_str(&format!("blake3:{hex}"))
        .expect("a blake3 digest must parse");
    assert_eq!(
        digest.bytes().len(),
        32,
        "the decoded blake3 digest must be 32 bytes"
    );
}

#[test]
fn download_digest_rejects_unknown_algo_prefix() {
    use std::str::FromStr as _;
    let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert!(
        crate::kernel::Digest::from_str(&format!("md5:{hex}")).is_err(),
        "an unknown algo prefix (md5) must be rejected, not coerced to a known variant"
    );
    assert!(
        crate::kernel::Digest::from_str(hex).is_err(),
        "a bare hex string with no `<algo>:` prefix must be rejected"
    );
}

#[test]
fn download_digest_rejects_wrong_length_and_non_hex() {
    use std::str::FromStr as _;
    assert!(
        crate::kernel::Digest::from_str("sha256:abcd").is_err(),
        "a too-short hex body must be rejected (digest must be 32 bytes / 64 hex chars)"
    );
    assert!(
        crate::kernel::Digest::from_str(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdefff"
        )
        .is_err(),
        "a too-long hex body must be rejected"
    );
    assert!(
        crate::kernel::Digest::from_str(
            "blake3:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_err(),
        "non-hex characters must be rejected"
    );
}

#[test]
fn unified_digest_accepts_both_algos_with_strict_hex() {
    use std::str::FromStr as _;
    let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert!(
        crate::kernel::Digest::from_str(&format!("sha256:{hex}")).is_ok(),
        "the unified Digest accepts a strict 64-hex sha256 body"
    );
    assert!(
        crate::kernel::Digest::from_str(&format!("blake3:{hex}")).is_ok(),
        "the unified Digest accepts a strict 64-hex blake3 body"
    );
}

// HTP-005 A: Source::mode() and Refspec::None

fn config_source(name: &str, body: &str) -> ParsedSource {
    let toml = format!("version = 1\n\n[sources.{name}]\n{body}");
    let raw = Config::parse(&toml)
        .expect("source config parses")
        .sources
        .remove(name)
        .expect("named source present");
    ParsedSource::parse(name, &raw).expect("named source parses to typed form")
}

#[test]
fn mode_is_url_when_url_set() {
    let s = config_source("u", "url = \"https://example.com/pkg.tar.gz\"\n");
    assert_eq!(
        s.mode(),
        SourceMode::Url,
        "a source declaring `url` must classify as SourceMode::Url"
    );
}

#[test]
fn mode_is_git_when_literal_git_set() {
    let s = config_source("g", "git = \"https://github.com/me/repo.git\"\n");
    assert_eq!(
        s.mode(),
        SourceMode::Git,
        "a source declaring a literal `git` must classify as SourceMode::Git"
    );
}

#[test]
fn mode_is_host_when_host_path_set() {
    let s = config_source("h", "host = \"github\"\npath = \"me/repo\"\n");
    assert_eq!(
        s.mode(),
        SourceMode::Host,
        "a source declaring `host`/`path` must classify as SourceMode::Host"
    );
}

#[test]
fn mode_is_host_when_only_repo_set() {
    let s = config_source("h", "repo = \"me/repo\"\n");
    assert_eq!(
        s.mode(),
        SourceMode::Host,
        "a bare-`repo` source (host defaults to github) must classify as SourceMode::Host, \
             not Url and not Git"
    );
}

#[test]
fn refspec_is_none_for_url_source() {
    let s = config_source("u", "url = \"https://example.com/pkg.tar.gz\"\n");
    assert!(
        matches!(s.refspec(), Refspec::None),
        "a url source has no git ref; refspec() must be Refspec::None, never the \
             Branch(\"main\") default that would misroute resolve to a git branch"
    );
}

#[test]
fn refspec_none_displays_without_panicking() {
    let none = Refspec::None;
    assert_eq!(
        none.to_string(),
        "",
        "Display for Refspec::None must render (empty) without panicking; \
             resolved_remotes / lock formatting calls Display on it"
    );
}

#[test]
fn refspec_still_branch_tag_rev_for_git_source() {
    let branch = config_source("g", "git = \"https://x/y.git\"\nbranch = \"dev\"\n");
    assert!(matches!(branch.refspec(), Refspec::Branch(b) if b == "dev"));

    let tag = config_source("g", "git = \"https://x/y.git\"\ntag = \"v2\"\n");
    assert!(matches!(tag.refspec(), Refspec::Tag(t) if t == "v2"));

    let rev = config_source("g", "git = \"https://x/y.git\"\nrev = \"abc123\"\n");
    assert!(matches!(rev.refspec(), Refspec::Rev(r) if r == "abc123"));

    let default = config_source("g", "git = \"https://x/y.git\"\n");
    assert!(
        matches!(default.refspec(), Refspec::Branch(b) if b == "main"),
        "a git source with no explicit ref must still default to Branch(\"main\"); \
             Refspec::None must not leak into git sources"
    );
}

// ARCH-005: typed source-kind keys — `path` = LOCAL, forge key renamed to `repo`

fn parse_kind_source(name: &str, body: &str) -> Result<Source> {
    let toml = format!("version = 1\n\n[sources.{name}]\n{body}");
    Config::parse(&toml).map(|mut cfg| {
        cfg.sources
            .remove(name)
            .expect("named source present after parse")
    })
}

#[test]
fn path_key_is_a_local_source_resolving_to_the_local_path_verbatim() {
    let local = tp(
        "loqui",
        &parse_kind_source("loqui", "path = \"/home/me/dev/loqui\"\n")
            .expect("a `path = \"/abs/local\"` source must parse as a local source"),
    );
    assert_eq!(
        local
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("a local-path source must resolve"),
        "/home/me/dev/loqui",
        "a `path` source uses the local path as the remote verbatim, exactly like a \
             `git = \"/home/me/dev/loqui\"` source"
    );
}

#[test]
fn path_local_source_resolves_identically_to_git_localpath_alias() {
    let via_path = tp(
        "a",
        &parse_kind_source("a", "path = \"/home/me/dev/loqui\"\n").expect("local `path` parses"),
    );
    let via_git_alias = tp(
        "b",
        &parse_kind_source("b", "git = \"/home/me/dev/loqui\"\n")
            .expect("`git = <localpath>` alias parses"),
    );
    assert_eq!(
        via_path
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("path local resolves"),
        via_git_alias
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("git-alias local resolves"),
        "a local `path` source and its `git = <localpath>` alias must resolve to the same \
             local remote"
    );
}

#[test]
fn host_plus_repo_is_a_forge_source_resolving_to_the_github_remote() {
    let forge = tp(
        "tropos",
        &parse_kind_source("tropos", "host = \"github\"\nrepo = \"srnnkls/tropos\"\n")
            .expect("a host + repo forge source must parse"),
    );
    assert_eq!(
        forge
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("forge source resolves against the built-in github forge"),
        "https://github.com/srnnkls/tropos.git",
        "host = github + repo = srnnkls/tropos must resolve to the github https remote, \
             exactly as host + path did before the rename"
    );
}

#[test]
fn bare_repo_defaults_to_github_forge() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.tropos]
repo = "srnnkls/tropos"
"#,
    )
    .expect("a bare `repo` (no host) must parse, defaulting to github");
    cfg.validate()
        .expect("a bare-repo source must validate (host defaults to github)");
    let forge = tp("tropos", cfg.sources.get("tropos").expect("tropos present"));
    assert_eq!(
        forge
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("bare repo resolves against the built-in github forge"),
        "https://github.com/srnnkls/tropos.git",
        "a bare `repo = \"owner/repo\"` with no host must default to the github forge, \
             exactly as the OLD bare `path = \"owner/repo\"` shorthand did"
    );
}

#[test]
fn host_plus_path_is_a_deprecated_forge_alias() {
    let forge = tp(
        "tropos",
        &parse_kind_source("tropos", "host = \"github\"\npath = \"srnnkls/tropos\"\n")
            .expect("host + path (deprecated forge alias) must still parse"),
    );
    assert_eq!(
        forge
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("the host+path alias still resolves as a forge source"),
        "https://github.com/srnnkls/tropos.git",
        "host + path stays a forge alias: it must resolve to the forge remote, NOT be \
             treated as a local path under a host"
    );
}

#[test]
fn host_plus_path_alias_resolves_a_tilde_anchored_forge_owner() {
    let forge = tp(
        "aerc",
        &parse_kind_source("aerc", "host = \"sr.ht\"\npath = \"~rjarry/aerc\"\n")
            .expect("a host + path alias whose owner starts with ~ must still parse as forge"),
    );
    assert_eq!(
        forge
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("the ~-anchored host+path alias resolves as a forge source"),
        "https://git.sr.ht/~rjarry/aerc",
        "a forge `path` under a `host` is owner/repo regardless of shape: a ~user owner \
             must resolve via the forge template, never be reclassified as a local path"
    );
}

#[test]
fn bare_path_owner_repo_is_now_a_local_path_not_a_github_remote() {
    let local = tp(
        "x",
        &parse_kind_source("x", "path = \"owner/repo\"\n").expect(
            "the OLD github shorthand `path = \"owner/repo\"` must now parse as a LOCAL source",
        ),
    );
    assert_eq!(
        local
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("a bare-path local source resolves to the path verbatim"),
        "owner/repo",
        "INTENTIONAL BREAK: bare `path = \"owner/repo\"` is now a LOCAL relative path used \
             verbatim as the remote, NOT the github forge remote it used to expand to"
    );
    assert_ne!(
        local
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("resolves"),
        "https://github.com/owner/repo.git",
        "bare `path` must NOT resolve to a github remote URL any more"
    );
}

#[test]
fn git_url_and_url_keys_are_unchanged_for_their_kinds() {
    let git = tp(
        "g",
        &parse_kind_source("g", "git = \"https://github.com/me/x.git\"\n")
            .expect("a real git URL source still parses"),
    );
    assert_eq!(
        git.resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("git URL resolves verbatim"),
        "https://github.com/me/x.git",
        "a `git = <url>` remote source is unchanged: it resolves verbatim"
    );

    let url = tp(
        "u",
        &parse_url_source("url = \"https://example.com/foo.tar.gz\"\n")
            .expect("a url source still parses"),
    );
    assert_eq!(
        url.source_url(),
        Some("https://example.com/foo.tar.gz"),
        "a `url` source is unchanged: source_url() returns the configured url"
    );
}

#[test]
fn source_with_path_and_git_url_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.dual]
path = "/home/me/dev/x"
git = "https://github.com/me/x.git"
"#;
    let err = Config::parse(toml).expect_err(
        "a local `path` together with a `git = <url>` remote is dual-kind and must be rejected",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dual"),
            "the local-vs-git conflict error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn path_under_host_is_the_forge_alias_regardless_of_shape() {
    let forge = tp(
        "dual",
        &parse_kind_source("dual", "path = \"/home/me/dev/x\"\nhost = \"github\"\n").expect(
            "`host` + a local-looking `path` is the deprecated forge alias, not a conflict",
        ),
    );
    assert_eq!(
        forge.mode(),
        SourceMode::Host,
        "a `path` under a `host` is always the forge owner/repo alias: its shape is never \
             inspected, so it must not be reclassified as a local source"
    );
}

#[test]
fn source_with_path_and_repo_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.dual]
path = "/home/me/dev/x"
repo = "owner/repo"
"#;
    let err = Config::parse(toml).expect_err(
        "a local `path` together with a forge `repo` is dual-kind and must be rejected",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dual"),
            "the local-vs-forge conflict error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_with_repo_and_git_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.dual]
repo = "owner/repo"
git = "https://github.com/me/x.git"
"#;
    let err = Config::parse(toml).expect_err(
        "a forge `repo` together with a `git` remote is dual-kind and must be rejected",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dual"),
            "the forge-vs-git conflict error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_with_url_and_repo_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.dual]
url = "https://example.com/foo.tar.gz"
repo = "owner/repo"
"#;
    let err = Config::parse(toml)
        .expect_err("a `url` together with a forge `repo` is dual-kind and must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dual"),
            "the url-vs-forge conflict error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn host_without_repo_or_path_is_rejected_naming_source() {
    let toml = r#"
version = 1

[sources.incomplete]
host = "github"
"#;
    let err = Config::parse(toml).expect_err(
        "a forge `host` with neither `repo` nor a `path` alias is an incomplete forge group \
             and must be rejected",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("incomplete"),
            "the incomplete-forge error must name the offending source, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn merge_local_path_override_clears_stale_base_forge_repo_and_host() {
    let base = Config::parse(
        r#"
version = 1

[sources.loqui]
host = "github"
repo = "srnnkls/loqui"
tag = "v1.0"
"#,
    )
    .expect("base forge source parses");
    let local = parse_kind_source(
        "loqui",
        "path = \"/home/me/dev/loqui\"\nbranch = \"main\"\n",
    )
    .expect("local path override parses");

    let merged = base
        .sources
        .get("loqui")
        .expect("loqui present")
        .clone()
        .merged_with(local);

    assert_eq!(
        merged.path.as_deref(),
        Some("/home/me/dev/loqui"),
        "the local path override must switch the merged source into local-kind"
    );
    assert!(
        merged.repo.is_none(),
        "switching to a local `path` must clear the stale base forge `repo`"
    );
    assert!(
        merged.host.is_none(),
        "switching to a local `path` must clear the stale base forge `host`"
    );
    assert!(
        merged.git.is_none() && merged.url.is_none(),
        "a local-path override must not leave any other-kind remote set"
    );
    assert_eq!(
        tp("loqui", &merged)
            .resolved_remote(&BTreeMap::new(), Protocol::Https)
            .expect("merged local resolves"),
        "/home/me/dev/loqui",
        "after the override the merged source resolves to the local path, not a stale \
             github forge remote"
    );
}

#[test]
fn config_digest_is_unchanged_across_the_path_to_repo_rename() {
    let forge = tp(
            "tropos",
            &parse_kind_source(
                "tropos",
                "host = \"github\"\nrepo = \"srnnkls/tropos\"\nroot = \"languages\"\ninclude = [\"editor\"]\n",
            )
            .expect("repo-key forge source parses"),
        );
    let alias = tp(
            "tropos",
            &parse_kind_source(
                "tropos",
                "host = \"github\"\npath = \"srnnkls/tropos\"\nroot = \"languages\"\ninclude = [\"editor\"]\n",
            )
            .expect("path-alias forge source parses"),
        );
    assert_eq!(
        forge.config_digest(),
        alias.config_digest(),
        "config_digest hashes only include/exclude/root/policy; the `path` -> `repo` key \
             rename must NOT alter it, keeping lock identity byte-identical"
    );
}

#[test]
fn defaults_auto_target_parses_false() {
    let cfg = Config::parse("version = 1\n\n[defaults]\nauto_target = false\n")
        .expect("explicit auto_target = false parses");
    assert!(
        !cfg.defaults.auto_target(),
        "an explicit `auto_target = false` must read back as false"
    );
}

#[test]
fn defaults_absent_section_defaults_auto_target_true() {
    let cfg = Config::parse("version = 1\n").expect("config without [defaults] parses");
    assert!(
        cfg.defaults.auto_target(),
        "with no [defaults] section, auto_target must default to true"
    );
}

#[test]
fn defaults_present_without_key_defaults_auto_target_true() {
    let cfg =
        Config::parse("version = 1\n\n[defaults]\n").expect("an empty [defaults] section parses");
    assert!(
        cfg.defaults.auto_target(),
        "an empty [defaults] section leaves auto_target at its true default"
    );
}

#[test]
fn merge_local_auto_target_false_overrides_base() {
    let base = Config::parse("version = 1\n").expect("base parses");
    let local =
        Config::parse("version = 1\n\n[defaults]\nauto_target = false\n").expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert!(
        !effective.defaults.auto_target(),
        "local `auto_target = false` must override the base default of true"
    );
}

#[test]
fn merge_local_unset_auto_target_keeps_base() {
    let base =
        Config::parse("version = 1\n\n[defaults]\nauto_target = false\n").expect("base parses");
    let local = Config::parse("version = 1\n").expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert!(
        !effective.defaults.auto_target(),
        "a local config that does not set auto_target must not clobber the base value"
    );
}

// ARCH-015: source-key migration warnings (retrofit of ARCH-005 acceptance).
mod migration_warnings {

    use tempfile::TempDir;

    use crate::config::{Config, MigrationWarning};

    fn cfg(body: &str) -> Config {
        Config::parse(&format!("version = 1\n\n{body}")).expect("config parses")
    }

    /// Warnings for `body` resolved against a directory with NO local paths, so
    /// any `path = "owner/repo"` cannot exist as a local dir.
    fn warnings(body: &str) -> Vec<MigrationWarning> {
        let empty = TempDir::new().expect("empty base dir");
        cfg(body).migration_warnings(empty.path())
    }

    fn for_source<'a>(ws: &'a [MigrationWarning], name: &str) -> Vec<&'a MigrationWarning> {
        ws.iter().filter(|w| w.source() == name).collect()
    }

    #[test]
    fn git_localpath_alias_warns_and_suggests_path() {
        let ws = warnings("[sources.loqui]\ngit = \"/home/me/dev/loqui\"\n");
        let hit = for_source(&ws, "loqui");
        assert_eq!(
            hit.len(),
            1,
            "a `git = <localpath>` alias must emit exactly one migration warning, got: {ws:?}"
        );
        assert_eq!(
            hit[0].suggested_key(),
            "path",
            "the git=<localpath> alias warning must steer the user to the `path` key"
        );
        let line = hit[0].to_string();
        assert_eq!(
            line.lines().count(),
            1,
            "the deprecation warning must be a single line, got: {line:?}"
        );
        assert!(
            line.contains("loqui") && line.contains("path"),
            "the warning line must name the source and the new `path` key, got: {line:?}"
        );
    }

    #[test]
    fn git_url_form_is_silent() {
        let ws = warnings("[sources.x]\ngit = \"https://github.com/me/x.git\"\n");
        assert!(
            for_source(&ws, "x").is_empty(),
            "a real `git = <url>` remote is the canonical form and must NOT warn, got: {ws:?}"
        );
    }

    #[test]
    fn host_path_forge_alias_warns_and_suggests_repo() {
        let ws = warnings("[sources.tropos]\nhost = \"github\"\npath = \"srnnkls/tropos\"\n");
        let hit = for_source(&ws, "tropos");
        assert_eq!(
            hit.len(),
            1,
            "the `host` + `path` forge alias must emit exactly one migration warning, got: {ws:?}"
        );
        assert_eq!(
            hit[0].suggested_key(),
            "repo",
            "the host+path forge-alias warning must steer the user to the `repo` key"
        );
        let line = hit[0].to_string();
        assert_eq!(
            line.lines().count(),
            1,
            "the deprecation warning must be a single line, got: {line:?}"
        );
        assert!(
            line.contains("tropos") && line.contains("repo"),
            "the warning line must name the source and the new `repo` key, got: {line:?}"
        );
    }

    #[test]
    fn host_repo_forge_form_is_silent() {
        let ws = warnings("[sources.tropos]\nhost = \"github\"\nrepo = \"srnnkls/tropos\"\n");
        assert!(
            for_source(&ws, "tropos").is_empty(),
            "the canonical `host` + `repo` forge form must NOT warn, got: {ws:?}"
        );
    }

    #[test]
    fn bare_repo_form_is_silent() {
        let ws = warnings("[sources.tropos]\nrepo = \"srnnkls/tropos\"\n");
        assert!(
            for_source(&ws, "tropos").is_empty(),
            "the canonical bare `repo` form must NOT warn, got: {ws:?}"
        );
    }

    #[test]
    fn url_form_is_silent() {
        let ws = warnings("[sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n");
        assert!(
            for_source(&ws, "pkg").is_empty(),
            "a `url` source is canonical and must NOT warn, got: {ws:?}"
        );
    }

    #[test]
    fn bare_path_that_looks_like_github_shorthand_warns_to_use_bare_repo() {
        // exactly one '/', no leading '/', './' or '~', and the dir does NOT exist locally
        let ws = warnings("[sources.x]\npath = \"owner/repo\"\n");
        let hit = for_source(&ws, "x");
        assert_eq!(
            hit.len(),
            1,
            "a bare `path = \"owner/repo\"` that looks like the old github shorthand and does \
             not exist locally must emit exactly one meaning-changed hint, got: {ws:?}"
        );
        assert_eq!(
            hit[0].suggested_key(),
            "repo",
            "the shorthand-moved hint must steer the user to the bare `repo` key"
        );
        let line = hit[0].to_string();
        assert_eq!(
            line.lines().count(),
            1,
            "the meaning-changed hint must be a single line, got: {line:?}"
        );
        assert!(
            line.contains("repo") && line.contains('x'),
            "the hint must name the offending source and the `repo` key the shorthand moved to, got: {line:?}"
        );
    }

    #[test]
    fn bare_path_to_existing_owner_repo_dir_does_not_warn() {
        let base = TempDir::new().expect("base dir");
        std::fs::create_dir_all(base.path().join("owner/repo")).expect("create owner/repo dir");
        let ws = cfg("[sources.x]\npath = \"owner/repo\"\n").migration_warnings(base.path());
        assert!(
            for_source(&ws, "x").is_empty(),
            "a relative `path = \"owner/repo\"` that DOES exist as a local dir is a real local \
             source and must NOT trigger the shorthand hint, got: {ws:?}"
        );
    }

    #[test]
    fn bare_path_absolute_local_dir_does_not_warn() {
        let ws = warnings("[sources.x]\npath = \"/home/me/dev/loqui\"\n");
        assert!(
            for_source(&ws, "x").is_empty(),
            "an absolute local `path` (leading `/`) is unambiguously local and must NOT warn, \
             got: {ws:?}"
        );
    }

    #[test]
    fn bare_path_dotslash_relative_dir_does_not_warn() {
        let ws = warnings("[sources.x]\npath = \"./vendor/pkg\"\n");
        assert!(
            for_source(&ws, "x").is_empty(),
            "a `./`-anchored relative path is unambiguously local and must NOT warn, got: {ws:?}"
        );
    }

    #[test]
    fn bare_path_tilde_home_does_not_warn() {
        let ws = warnings("[sources.x]\npath = \"~/dev/loqui\"\n");
        assert!(
            for_source(&ws, "x").is_empty(),
            "a `~`-anchored home path is unambiguously local and must NOT warn, got: {ws:?}"
        );
    }

    #[test]
    fn bare_path_deep_relative_dir_does_not_warn() {
        // more than one '/' -> not the single-segment github shorthand shape
        let ws = warnings("[sources.x]\npath = \"vendor/group/pkg\"\n");
        assert!(
            for_source(&ws, "x").is_empty(),
            "a multi-segment relative `path` is not the old `owner/repo` shorthand and must \
             NOT trigger the hint, got: {ws:?}"
        );
    }

    #[test]
    fn each_offending_source_warns_exactly_once_per_parse_pass() {
        let ws = warnings(
            "[sources.a]\ngit = \"/home/me/dev/a\"\n\n\
             [sources.b]\nhost = \"github\"\npath = \"owner/b\"\n",
        );
        assert_eq!(
            for_source(&ws, "a").len(),
            1,
            "the git=<localpath> alias source `a` must warn exactly once, got: {ws:?}"
        );
        assert_eq!(
            for_source(&ws, "b").len(),
            1,
            "the host+path alias source `b` must warn exactly once, got: {ws:?}"
        );
    }

    #[test]
    fn a_fully_canonical_config_produces_no_warnings() {
        let ws = warnings(
            "[sources.remote]\ngit = \"https://github.com/me/x.git\"\n\n\
             [sources.forge]\nhost = \"github\"\nrepo = \"me/forge\"\n\n\
             [sources.bare]\nrepo = \"me/bare\"\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [sources.local]\npath = \"/abs/local/dir\"\n",
        );
        assert!(
            ws.is_empty(),
            "a config using only canonical keys must produce NO migration warnings, got: {ws:?}"
        );
    }
}

mod per_binding_refinement {
    use std::path::Path;

    use crate::config::target::{Binding, RefinedBinding, ResolvedBinding};
    use crate::config::{Config, Source, Target};
    use crate::error::Error;

    fn target(toml: &str, name: &str) -> Target {
        Config::parse(toml)
            .expect("config parses")
            .targets
            .remove(name)
            .expect("named target present")
    }

    fn sources(toml: &str) -> std::collections::BTreeMap<String, Source> {
        Config::parse(toml).expect("config parses").sources
    }

    fn bindings_of(t: &Target) -> &[Binding] {
        t.sources
            .as_deref()
            .expect("target declares an explicit sources list")
    }

    fn find_resolved<'a>(
        resolved: &'a [ResolvedBinding<'a>],
        identity: &str,
    ) -> &'a ResolvedBinding<'a> {
        resolved
            .iter()
            .find(|b| b.identity == identity)
            .unwrap_or_else(|| {
                panic!("a resolved binding with identity `{identity}` must be present")
            })
    }

    #[test]
    fn bare_string_source_parses_to_binding_source() {
        let t = target(
            "version = 1\n\n[targets.neovim]\npath = \"~/.config/nvim\"\nsources = [\"dotfiles\"]\n",
            "neovim",
        );
        let bindings = bindings_of(&t);
        assert_eq!(bindings.len(), 1, "exactly one binding in the list");
        match &bindings[0] {
            Binding::Source(name) => assert_eq!(
                name, "dotfiles",
                "a bare TOML string must deserialize to Binding::Source carrying the source name"
            ),
            other @ Binding::Refined(_) => {
                panic!("a bare string must be Binding::Source, got {other:?}")
            }
        }
    }

    #[test]
    fn refinement_table_parses_to_binding_refined_with_all_fields() {
        let t = target(
            "version = 1\n\n[targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"dotfiles\", as = \"nvim\", root = \"nvim\", \
             include = [\"init.lua\"], exclude = [\"**/*.bak\"] }]\n",
            "t",
        );
        let bindings = bindings_of(&t);
        assert_eq!(bindings.len(), 1, "exactly one binding in the list");
        match &bindings[0] {
            Binding::Refined(refined) => {
                let RefinedBinding {
                    source,
                    r#as,
                    root,
                    include,
                    exclude,
                    branch,
                    tag,
                    rev,
                    template: _,
                    map: _,
                } = refined.as_ref();
                assert_eq!(
                    source, "dotfiles",
                    "the `source` key carries the referenced source name"
                );
                assert_eq!(
                    r#as.as_deref(),
                    Some("nvim"),
                    "the `as` key carries the rename"
                );
                assert_eq!(
                    root.as_deref(),
                    Some(Path::new("nvim")),
                    "the `root` key is captured as a PathBuf"
                );
                assert_eq!(
                    include.as_deref(),
                    Some(["init.lua".to_string()].as_slice()),
                    "the `include` key is captured as a Vec<String>"
                );
                assert_eq!(
                    exclude.as_deref(),
                    Some(["**/*.bak".to_string()].as_slice()),
                    "the `exclude` key is captured as a Vec<String>"
                );
                assert_eq!(branch.as_deref(), None, "no `branch` set in this table");
                assert_eq!(tag.as_deref(), None, "no `tag` set in this table");
                assert_eq!(rev.as_deref(), None, "no `rev` set in this table");
            }
            other @ Binding::Source(_) => {
                panic!("a refinement table must be Binding::Refined, got {other:?}")
            }
        }
    }

    #[test]
    fn refinement_table_with_unknown_key_is_rejected_naming_it() {
        let toml = "version = 1\n\n[targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"dotfiles\", bogus = 1 }]\n";
        let err = Config::parse(toml).expect_err(
            "an unknown key in a refinement table must be rejected (deny_unknown_fields)",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("bogus"),
                "the unknown-refinement-key error must name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn resolve_overrides_root_independently_and_inherits_include_exclude() {
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\
            root = \"base-root\"\ninclude = [\"src-inc\"]\nexclude = [\"src-exc\"]\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [{ source = \"dotfiles\", root = \"override-root\" }]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "dotfiles");

        assert_eq!(b.source, "dotfiles", "the binding still names its source");
        assert_eq!(
            b.root,
            Some(Path::new("override-root")),
            "a binding that sets `root` must override the source's `root`"
        );
        assert_eq!(
            b.include,
            ["src-inc"],
            "an omitted binding `include` must inherit the source's include verbatim"
        );
        assert_eq!(
            b.exclude,
            ["src-exc"],
            "an omitted binding `exclude` must inherit the source's exclude verbatim"
        );
    }

    #[test]
    fn resolve_overrides_include_independently_and_inherits_root() {
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\
            root = \"base-root\"\ninclude = [\"src-inc\"]\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [{ source = \"dotfiles\", include = [\"only-binding\"] }]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "dotfiles");

        assert_eq!(
            b.include,
            ["only-binding"],
            "a binding that sets `include` must override the source's include (no concatenation)"
        );
        assert_eq!(
            b.root,
            Some(Path::new("base-root")),
            "an omitted binding `root` must inherit the source's root"
        );
    }

    #[test]
    fn identity_defaults_to_source_name_when_as_omitted() {
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
            [targets.t]\npath = \"~/x\"\nsources = [\"dotfiles\"]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        assert_eq!(
            resolved.len(),
            1,
            "one binding resolves to one ResolvedBinding"
        );
        assert_eq!(
            resolved[0].identity, "dotfiles",
            "a bare binding's identity must default to the source name when `as` is omitted"
        );
        assert_eq!(
            resolved[0].source, "dotfiles",
            "identity and source coincide for a bare binding"
        );
    }

    #[test]
    fn identity_uses_as_when_set() {
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [{ source = \"dotfiles\", as = \"nvim\" }]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = &resolved[0];
        assert_eq!(
            b.identity, "nvim",
            "a binding with `as = nvim` must take its identity from `as`, not the source name"
        );
        assert_eq!(
            b.source, "dotfiles",
            "the underlying source name is preserved independently of the identity"
        );
    }

    #[test]
    fn merge_string_only_sources_is_wholesale_replace_no_concatenation() {
        let base = Config::parse(
            "version = 1\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"a\", \"b\"]\n",
        )
        .expect("base parses");
        let local = Config::parse(
            "version = 1\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"c\"]\n",
        )
        .expect("local parses");

        let effective = crate::config::merge_configs(base, Some(local));
        let merged = effective.targets.get("t").expect("target kept");
        let names: Vec<&str> = bindings_of(merged)
            .iter()
            .map(|b| match b {
                Binding::Source(name) => name.as_str(),
                Binding::Refined(r) => r.source.as_str(),
            })
            .collect();
        assert_eq!(
            names,
            ["c"],
            "a local `sources` overlay must WHOLESALE-REPLACE the base list (no concatenation, \
             the base [\"a\", \"b\"] must be gone)"
        );
    }

    #[test]
    fn r9_golden_bare_string_config_resolves_to_source_identity_and_inherited_fields() {
        use crate::store::Registry as _;
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\
            root = \"modules\"\ninclude = [\"editor\"]\nexclude = [\"**/*.bak\"]\n\n\
            [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\nroot = \"languages\"\n\n\
            [targets.t]\npath = \"~/x\"\nsources = [\"dotfiles\", \"loqui\"]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let identities: Vec<&str> = resolved.iter().map(|b| b.identity).collect();
        assert_eq!(
            identities,
            ["dotfiles", "loqui"],
            "a bare-string config must resolve identities equal to the source names, in order"
        );

        let dotfiles = find_resolved(&resolved, "dotfiles");
        assert_eq!(
            dotfiles.source, "dotfiles",
            "a bare binding's source equals its identity"
        );
        assert_eq!(
            dotfiles.root,
            Some(Path::new("modules")),
            "with no binding-level refinement, root is inherited verbatim from the source"
        );
        assert_eq!(
            dotfiles.include,
            ["editor"],
            "with no binding-level refinement, include is inherited verbatim from the source"
        );
        assert_eq!(
            dotfiles.exclude,
            ["**/*.bak"],
            "with no binding-level refinement, exclude is inherited verbatim from the source"
        );

        let loqui = find_resolved(&resolved, "loqui");
        assert_eq!(
            loqui.root,
            Some(Path::new("languages")),
            "the second bare binding also inherits its source root verbatim"
        );
        assert!(
            loqui.include.is_empty() && loqui.exclude.is_empty(),
            "a source with no include/exclude resolves to empty slices for a bare binding"
        );

        // PBR-004 byte-identity: a bare binding (identity == source) must persist its
        // record at …/artifacts/<source>/<artifact>.toml with `source` == the source,
        // unchanged from the pre-PBR layout.
        let state = tempfile::TempDir::new().expect("state root");
        let reg = crate::store::FileRegistry::open(state.path().to_path_buf())
            .expect("open registry over tempdir");
        let rec = crate::store::RegistryRecord {
            version: 1,
            key: crate::store::ArtifactKey {
                target: "t".to_owned(),
                source: dotfiles.identity.to_owned(),
                artifact: "editor".to_owned(),
            },
            source: dotfiles.source.to_owned(),
            commit: "deadbeef".to_owned(),
            digest: "blake3:00".to_owned(),
            projected_at: "2026-01-31T12:34:56Z".to_owned(),
            layout: "flat".to_owned(),
            allow_symlinks: false,
            preserve_executable: true,
            files: vec![],
            linked: false,
            vars_digest: None,
        };
        reg.put(&rec).expect("put bare-binding record");

        let expected = state
            .path()
            .join("targets")
            .join("t")
            .join("artifacts")
            .join("dotfiles")
            .join("editor.toml");
        assert!(
            expected.is_file(),
            "a bare binding must persist at …/artifacts/<source>/<artifact>.toml \
             byte-identical to the pre-PBR layout, expected {}",
            expected.display()
        );
        assert_eq!(
            reg.get(&rec.key).expect("get").expect("present").source,
            "dotfiles",
            "a bare binding's persisted `source` equals its identity"
        );
    }

    #[test]
    fn resolve_overrides_exclude_independently_and_inherits_root_include() {
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\
            root = \"base-root\"\ninclude = [\"src-inc\"]\nexclude = [\"src-exc\"]\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [{ source = \"dotfiles\", exclude = [\"binding-exc\"] }]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "dotfiles");

        assert_eq!(
            b.exclude,
            ["binding-exc"],
            "a binding that sets `exclude` must override the source's exclude (no concatenation)"
        );
        assert_eq!(
            b.root,
            Some(Path::new("base-root")),
            "an omitted binding `root` must inherit the source's root verbatim"
        );
        assert_eq!(
            b.include,
            ["src-inc"],
            "an omitted binding `include` must inherit the source's include verbatim"
        );
    }

    #[test]
    fn merge_wholesale_replace_holds_when_base_has_refined_binding() {
        let base = Config::parse(
            "version = 1\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"a\", as = \"x\", root = \"r\" }, \"b\"]\n",
        )
        .expect("base parses");
        let local = Config::parse(
            "version = 1\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"c\"]\n",
        )
        .expect("local parses");

        let effective = crate::config::merge_configs(base, Some(local));
        let merged = effective.targets.get("t").expect("target kept");
        let names: Vec<&str> = bindings_of(merged)
            .iter()
            .map(|b| match b {
                Binding::Source(name) => name.as_str(),
                Binding::Refined(r) => r.source.as_str(),
            })
            .collect();
        assert_eq!(
            names,
            ["c"],
            "a local `sources` overlay must WHOLESALE-REPLACE a base list containing a refined \
             binding (no concatenation, the base entries must be gone regardless of variant)"
        );
    }

    #[test]
    fn resolve_combines_as_identity_with_field_override() {
        let toml = "version = 1\n\n\
            [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\
            root = \"base-root\"\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [{ source = \"dotfiles\", as = \"nvim\", root = \"override-root\" }]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "nvim");

        assert_eq!(
            b.identity, "nvim",
            "the binding's identity must come from `as` even when a field override is present"
        );
        assert_eq!(
            b.source, "dotfiles",
            "the underlying source name is preserved alongside the `as` rename"
        );
        assert_eq!(
            b.root,
            Some(Path::new("override-root")),
            "a binding with `as` set must still apply its own per-field `root` override"
        );
    }

    #[test]
    fn refinement_table_rejects_misplaced_source_level_key() {
        let toml = "version = 1\n\n[targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"dotfiles\", git = \"x\" }]\n";
        let err = Config::parse(toml).expect_err(
            "a source-level key (`git`) placed inside a refinement table must be rejected: \
             binding fields are not source fields",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("git"),
                "the misplaced-source-key error must name the offending key, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    fn config(toml: &str) -> Config {
        Config::parse(toml).expect("config parses")
    }

    #[test]
    fn validate_rejects_duplicate_bare_identity_within_a_target() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [targets.editor]\npath = \"~/.config\"\nsources = [\"dotfiles\", \"dotfiles\"]\n",
        );
        let err = cfg.validate().expect_err(
            "two bare `dotfiles` bindings collide on identity `dotfiles` within target `editor` \
             and must be rejected",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("editor"),
                    "the duplicate-identity error must name the offending target `editor`, got: {msg}"
                );
                assert!(
                    msg.contains("dotfiles"),
                    "the duplicate-identity error must name the colliding identity `dotfiles`, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_identity_collision_between_bare_and_table_forms() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [targets.editor]\npath = \"~/.config\"\n\
             sources = [\"dotfiles\", { source = \"dotfiles\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a table binding with no `as` defaults its identity to its source name `dotfiles`, \
             colliding with the bare `dotfiles` binding; this must be rejected even across forms",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("editor"),
                    "the collision error must name the target `editor`, got: {msg}"
                );
                assert!(
                    msg.contains("dotfiles"),
                    "the collision error must name the colliding identity `dotfiles`, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_allows_same_source_under_two_distinct_as_identities() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [targets.editor]\npath = \"~/.config\"\n\
             sources = [\
                { source = \"dotfiles\", as = \"nvim\" }, \
                { source = \"dotfiles\", as = \"tmux\" }\
             ]\n",
        );
        cfg.validate().expect(
            "two bindings of the SAME source carrying DISTINCT `as` identities (`nvim`, `tmux`) \
             are two slices into one target and must be VALID",
        );
    }

    #[test]
    fn validate_rejects_target_referencing_undefined_source_via_table_binding() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [targets.editor]\npath = \"~/.config\"\n\
             sources = [{ source = \"ghost\", as = \"x\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding whose `source` names no `[sources.*]` entry must be rejected by validate()",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("ghost"),
                "the undefined-source error must name the missing source `ghost`, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_target_referencing_undefined_source_via_bare_binding() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [targets.editor]\npath = \"~/.config\"\nsources = [\"ghost\"]\n",
        );
        let err = cfg.validate().expect_err(
            "a bare binding naming no `[sources.*]` entry must be rejected by validate(), \
             exactly as the table-form binding is",
        );
        match err {
            Error::Config(msg) => assert!(
                msg.contains("ghost"),
                "the undefined-source error must name the missing source `ghost`, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_distinct_sources_sharing_one_as_identity() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n\n\
             [targets.editor]\npath = \"~/.config\"\n\
             sources = [\
                { source = \"dotfiles\", as = \"x\" }, \
                { source = \"loqui\", as = \"x\" }\
             ]\n",
        );
        let err = cfg.validate().expect_err(
            "two DIFFERENT sources both claiming `as = x` in one target collide on identity `x` \
             and must be rejected",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("editor"),
                    "the identity-collision error must name the target `editor`, got: {msg}"
                );
                assert!(
                    msg.contains('x'),
                    "the identity-collision error must name the colliding identity `x`, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_allows_root_slice_on_git_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.dotfiles]\ngit = \"https://github.com/me/dotfiles.git\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"dotfiles\", root = \"sub\" }]\n",
        );
        cfg.validate().expect(
            "a binding that slices `root` on a GIT-backed source is valid (git sources carry a \
             tree to slice); the url-slice rejection must be specific to url-backed sources",
        );
    }

    #[test]
    fn validate_allows_plain_binding_to_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\" }]\n",
        );
        cfg.validate().expect(
            "a PLAIN binding to a url-backed source that sets no root/include/exclude must be \
             VALID; only sliced bindings on url sources are rejected",
        );
    }

    #[test]
    fn validate_rejects_root_slice_on_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\", root = \"sub\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that sets `root` on a url-backed source must be rejected: url sources \
             carry no tree to slice",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("pkg"),
                    "the url-slice error must name the offending source `pkg`, got: {msg}"
                );
                assert!(
                    msg.contains("root"),
                    "the url-slice error must name the rejected `root` refinement, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_include_slice_on_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\", include = [\"a\"] }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that sets `include` on a url-backed source must be rejected (url sources \
             reject slicing, mirroring the source-level rule)",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("pkg"),
                    "the url-slice error must name the offending source `pkg`, got: {msg}"
                );
                assert!(
                    msg.contains("include"),
                    "the url-slice error must name the rejected `include` refinement, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_exclude_slice_on_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\", exclude = [\"b\"] }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that sets `exclude` on a url-backed source must be rejected (url sources \
             reject slicing, mirroring the source-level rule)",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("pkg"),
                    "the url-slice error must name the offending source `pkg`, got: {msg}"
                );
                assert!(
                    msg.contains("exclude"),
                    "the url-slice error must name the rejected `exclude` refinement, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    // PTV-001: per-target version — binding ref overrides source ref, bare inherits.

    use crate::config::Refspec;

    #[test]
    fn resolve_binding_tag_overrides_source_branch_ref() {
        let toml = "version = 1\n\n\
            [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\nbranch = \"main\"\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [{ source = \"fzf\", as = \"canary\", tag = \"v0.56.0\" }]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "canary");

        match &b.effective_ref {
            Refspec::Tag(tag) => assert_eq!(
                tag, "v0.56.0",
                "a binding `tag` must REPLACE the source ref; effective_ref must be the binding's tag"
            ),
            other => panic!(
                "a binding-level `tag` must resolve effective_ref to Refspec::Tag, got {other:?} \
                 (this fails if the source's branch=main ref leaked through instead)"
            ),
        }
    }

    #[test]
    fn bare_binding_inherits_source_tag_ref() {
        let toml = "version = 1\n\n\
            [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\ntag = \"v0.55.0\"\n\n\
            [targets.t]\npath = \"~/x\"\nsources = [\"fzf\"]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "fzf");

        match &b.effective_ref {
            Refspec::Tag(tag) => assert_eq!(
                tag, "v0.55.0",
                "a bare binding must INHERIT the source ref; effective_ref must be the source's tag"
            ),
            other => panic!(
                "a bare binding must inherit the source's Refspec::Tag(v0.55.0), got {other:?}"
            ),
        }
    }

    #[test]
    fn resolve_binding_ref_precedence_is_rev_over_tag_over_branch() {
        // Built directly, not via Config::parse: PTV-002 makes >1 binding ref a validation error.
        let toml = "version = 1\n\n\
            [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\nbranch = \"main\"\n";
        let all = sources(toml);

        let t = Target {
            path: std::path::PathBuf::from("~/x"),
            layout: None,
            hooks: None,
            sources: Some(vec![Binding::Refined(Box::new(RefinedBinding {
                source: "fzf".to_owned(),
                r#as: Some("pinned".to_owned()),
                root: None,
                include: None,
                exclude: None,
                branch: Some("develop".to_owned()),
                tag: Some("v9.9.9".to_owned()),
                rev: Some("deadbeef".to_owned()),
                template: None,
                map: None,
            }))]),
        };

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "pinned");

        match &b.effective_ref {
            Refspec::Rev(rev) => assert_eq!(
                rev, "deadbeef",
                "with `rev`, `tag`, and `branch` all set on the binding, rev must win (rev > tag > branch)"
            ),
            other => panic!(
                "binding ref precedence must pick `rev` over `tag` and `branch`, got {other:?} \
                 (a Tag(v9.9.9) or Branch(develop) here would mean precedence is reversed)"
            ),
        }
    }

    #[test]
    fn resolve_binding_ref_precedence_is_tag_over_branch_when_rev_absent() {
        // Built directly, not via Config::parse: PTV-002 makes >1 binding ref a validation error.
        let toml = "version = 1\n\n\
            [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\nbranch = \"main\"\n";
        let all = sources(toml);

        let t = Target {
            path: std::path::PathBuf::from("~/x"),
            layout: None,
            hooks: None,
            sources: Some(vec![Binding::Refined(Box::new(RefinedBinding {
                source: "fzf".to_owned(),
                r#as: Some("pinned".to_owned()),
                root: None,
                include: None,
                exclude: None,
                branch: Some("develop".to_owned()),
                tag: Some("v9.9.9".to_owned()),
                rev: None,
                template: None,
                map: None,
            }))]),
        };

        let resolved = t.resolve_sources(&all);
        let b = find_resolved(&resolved, "pinned");

        match &b.effective_ref {
            Refspec::Tag(tag) => assert_eq!(
                tag, "v9.9.9",
                "with `tag` and `branch` set (no rev), tag must win (rev > tag > branch)"
            ),
            other => panic!(
                "binding ref precedence must pick `tag` over `branch` when rev is absent, got {other:?} \
                 (a Branch(develop) here would mean precedence is reversed)"
            ),
        }
    }

    #[test]
    fn two_refined_bindings_of_one_source_carry_distinct_effective_refs() {
        let toml = "version = 1\n\n\
            [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\nbranch = \"main\"\n\n\
            [targets.t]\npath = \"~/x\"\n\
            sources = [\
                { source = \"fzf\", as = \"stable\", tag = \"v0.55.0\" }, \
                { source = \"fzf\", as = \"canary\", tag = \"v0.56.0\" }\
            ]\n";
        let t = target(toml, "t");
        let all = sources(toml);

        let resolved = t.resolve_sources(&all);
        assert_eq!(
            resolved.len(),
            2,
            "two distinct refined bindings of one source must resolve to two ResolvedBindings"
        );

        let stable = find_resolved(&resolved, "stable");
        let canary = find_resolved(&resolved, "canary");

        assert_eq!(
            stable.source, "fzf",
            "both resolved bindings name the same underlying source"
        );
        assert_eq!(canary.source, "fzf");

        match (&stable.effective_ref, &canary.effective_ref) {
            (Refspec::Tag(s), Refspec::Tag(c)) => {
                assert_eq!(s, "v0.55.0", "the `stable` binding pins its own tag");
                assert_eq!(c, "v0.56.0", "the `canary` binding pins its own tag");
                assert_ne!(
                    s, c,
                    "two bindings of one source must carry DISTINCT effective_refs"
                );
            }
            other => panic!("both bindings must resolve to their own Refspec::Tag, got {other:?}"),
        }
    }

    // PTV-002: binding-level ref validation.

    #[test]
    fn validate_rejects_binding_tag_on_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\", as = \"x\", tag = \"v1\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that pins a `tag` on a url-backed source must be rejected: a static \
             url resource has no refspec to override",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("pkg"),
                    "the binding-ref-on-url error must name the offending source `pkg`, got: {msg}"
                );
                assert!(
                    msg.contains("`tag`"),
                    "the binding-ref-on-url error must name the SPECIFIC offending ref field \
                     `tag` (a disjunctive tag||branch||rev assertion lets a wrong-field-named \
                     mutation survive; the url-slice message style quotes fields in backticks, \
                     mod.rs:194), got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_binding_branch_on_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\", as = \"x\", branch = \"main\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that pins a `branch` on a url-backed source must be rejected, exactly as \
             a `tag` is: a static url resource has no refspec",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("pkg"),
                    "the binding-ref-on-url error must name the offending source `pkg`, got: {msg}"
                );
                assert!(
                    msg.contains("`branch`"),
                    "the binding-ref-on-url error must name the SPECIFIC offending ref field \
                     `branch` (a disjunctive assertion lets a wrong-field-named mutation survive; \
                     the url-slice message style quotes fields in backticks, mod.rs:194), \
                     got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_binding_rev_on_url_backed_source() {
        let cfg = config(
            "version = 1\n\n\
             [sources.pkg]\nurl = \"https://example.com/foo.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"pkg\", as = \"x\", rev = \"deadbeef\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that pins a `rev` on a url-backed source must be rejected, exactly as a \
             `tag` or `branch` is: a static url resource has no refspec",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("pkg"),
                    "the binding-ref-on-url error must name the offending source `pkg`, got: {msg}"
                );
                assert!(
                    msg.contains("`rev`"),
                    "the binding-ref-on-url error must name the SPECIFIC offending ref field \
                     `rev` (the requirement is branch/tag/rev — rev must be covered; the url-slice \
                     message style quotes fields in backticks, mod.rs:194), got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_more_than_one_ref_on_a_binding() {
        let cfg = config(
            "version = 1\n\n\
             [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"fzf\", as = \"x\", tag = \"v1\", branch = \"main\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that sets both `tag` and `branch` must be rejected: only one of \
             branch/tag/rev may pin a binding, mirroring the source-level rule in classify()",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("fzf"),
                    "the multi-ref error must name the offending source `fzf` \
                     (the prior `|| msg.contains('x')` single-char alternative is too weak — \
                     a stray 'x' anywhere in the message satisfies it), got: {msg}"
                );
                assert!(
                    msg.contains("branch") && msg.contains("tag"),
                    "the multi-ref error must name the conflicting ref fields (`branch`, `tag`), \
                     got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_tag_and_rev_on_a_binding() {
        let cfg = config(
            "version = 1\n\n\
             [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"fzf\", as = \"x\", tag = \"v1\", rev = \"deadbeef\" }]\n",
        );
        let err = cfg.validate().expect_err(
            "a binding that sets both `tag` and `rev` must be rejected: only one of \
             branch/tag/rev may pin a binding, so `rev` must participate in the one-ref-max rule",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("fzf"),
                    "the multi-ref error must name the offending source `fzf`, got: {msg}"
                );
                assert!(
                    msg.contains("`tag`") && msg.contains("`rev`"),
                    "the multi-ref error must name BOTH conflicting ref fields, backtick-quoted \
                     (`tag`, `rev`); a disjunctive or single-field assertion lets a mutation that \
                     drops `rev` from the rule survive, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_two_refs_of_one_source_without_distinct_as_naming_the_as_fix() {
        let cfg = config(
            "version = 1\n\n\
             [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [\
                { source = \"fzf\", tag = \"v0.55.0\" }, \
                { source = \"fzf\", tag = \"v0.56.0\" }\
             ]\n",
        );
        let err = cfg.validate().expect_err(
            "two bindings of `fzf` at different tags WITHOUT `as` collapse to identity `fzf` \
             and must be rejected as a collision",
        );
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("fzf"),
                    "the collision error must name the colliding identity `fzf`, got: {msg}"
                );
                assert!(
                    msg.contains("`as`"),
                    "the collision error must be actionable by naming the backtick-quoted `as` \
                     fix (substring \"as\" also matches \"has\"/\"please\"; a generic \
                     `more than once` message without the field-named fix hint is insufficient), \
                     got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn validate_allows_two_refs_of_one_source_with_distinct_as() {
        let cfg = config(
            "version = 1\n\n\
             [sources.fzf]\ngit = \"https://github.com/junegunn/fzf.git\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [\
                { source = \"fzf\", as = \"stable\", tag = \"v0.55.0\" }, \
                { source = \"fzf\", as = \"canary\", tag = \"v0.56.0\" }\
             ]\n",
        );
        cfg.validate().expect(
            "two bindings of `fzf` at different tags carrying DISTINCT `as` identities \
             (`stable`, `canary`) are two valid slices into one target and must not be rejected \
             by PTV-002's new checks",
        );
    }
}

// TPH-001: hook config schema — [targets.X.hooks] on_change + global [hooks] post_sync

fn on_change_of<'a>(cfg: &'a Config, target: &str) -> &'a [HookCommand] {
    target_of(cfg, target)
        .hooks
        .as_ref()
        .expect("target declares hooks")
        .on_change
        .as_deref()
        .expect("hooks declare on_change")
}

fn post_sync_of(cfg: &Config) -> &[HookCommand] {
    cfg.hooks
        .as_ref()
        .expect("global [hooks] table present")
        .post_sync
        .as_deref()
        .expect("post_sync declared")
}

fn runs(commands: &[HookCommand]) -> Vec<&str> {
    commands.iter().map(|c| c.run.as_str()).collect()
}

fn shells(commands: &[HookCommand]) -> Vec<Option<&str>> {
    commands.iter().map(|c| c.shell.as_deref()).collect()
}

#[test]
fn target_hooks_on_change_parses_single_string_as_one_command() {
    let cfg = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = "bat cache --build"
"#,
    )
    .expect("a target with a string `on_change` hook must parse");
    assert_eq!(
        runs(on_change_of(&cfg, "t")),
        ["bat cache --build"],
        "a single command string must normalize to a one-element command list"
    );
    assert_eq!(
        shells(on_change_of(&cfg, "t")),
        [None],
        "the string form is shorthand for `{{ run = ... }}` and carries no shell"
    );
}

#[test]
fn target_hooks_on_change_parses_list_in_declaration_order() {
    let cfg = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ["first --flag", "second"]
"#,
    )
    .expect("a target with a list `on_change` hook must parse");
    assert_eq!(
        runs(on_change_of(&cfg, "t")),
        ["first --flag", "second"],
        "list commands must keep declaration order"
    );
}

#[test]
fn hook_free_config_parses_with_no_hooks_anywhere() {
    let cfg = Config::parse(EXAMPLE_TOML).expect("the hook-free example toml must parse unchanged");
    assert!(
        cfg.hooks.is_none(),
        "an absent global [hooks] table must parse as None (global hook OFF by default)"
    );
    for (name, target) in &cfg.targets {
        assert!(
            target.hooks.is_none(),
            "target `{name}` declares no hooks and must carry none"
        );
    }
}

#[test]
fn global_hooks_post_sync_parses_string_with_when_always() {
    let cfg = Config::parse(
        r#"
version = 1

[hooks]
post_sync = "reload-everything"
when = "always"
"#,
    )
    .expect("a global [hooks] table with post_sync and when = \"always\" must parse");
    assert_eq!(runs(post_sync_of(&cfg)), ["reload-everything"]);
    assert_eq!(
        cfg.hooks
            .as_ref()
            .expect("global [hooks] table present")
            .when,
        HookWhen::Always,
        "an explicit `when = \"always\"` must be stored on the parsed config, not dropped"
    );
}

#[test]
fn global_hooks_post_sync_parses_list_in_declaration_order() {
    let cfg = Config::parse(
        r#"
version = 1

[hooks]
post_sync = ["first", "second"]
"#,
    )
    .expect("a global [hooks] table with a post_sync list and no `when` key must parse");
    assert_eq!(runs(post_sync_of(&cfg)), ["first", "second"]);
    assert_eq!(
        cfg.hooks
            .as_ref()
            .expect("global [hooks] table present")
            .when,
        HookWhen::Always,
        "an omitted `when` must default to always, the only valid value in v1"
    );
}

#[test]
fn merge_local_target_hooks_replace_base_hooks() {
    let base = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ["base-cmd"]
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = "local-cmd"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        runs(on_change_of(&effective, "t")),
        ["local-cmd"],
        "a local target `hooks` table must replace the base target's hooks wholesale \
         (per-key target merge, like `layout`)"
    );
}

#[test]
fn merge_path_only_target_override_preserves_base_hooks() {
    let base = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = "base-cmd"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[targets.t]
path = "/local/override"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        runs(on_change_of(&effective, "t")),
        ["base-cmd"],
        "a path-only local override must NOT clear the base target's hooks"
    );
}

#[test]
fn merge_local_empty_on_change_clears_base_hooks() {
    let base = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = "base-cmd"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = []
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert!(
        on_change_of(&effective, "t").is_empty(),
        "an explicit empty `on_change = []` in local must clear the base commands, \
         not be ignored as if unset (mirrors `include = []` clearing)"
    );
}

#[test]
fn merge_local_bare_hooks_section_clears_base_hooks() {
    let base = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = "base-cmd"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert!(
        target_of(&effective, "t")
            .hooks
            .as_ref()
            .expect("target carries the local hooks table")
            .on_change
            .is_none(),
        "a bare [targets.X.hooks] section (header, no on_change key) replaces the base \
         hooks wholesale, clearing base commands — matching the layout whole-replace semantic"
    );
}

#[test]
fn merge_global_hooks_local_wins() {
    let base = Config::parse(
        r#"
version = 1

[hooks]
post_sync = "base-reload"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[hooks]
post_sync = "local-reload"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        runs(post_sync_of(&effective)),
        ["local-reload"],
        "a local [hooks] table must override the base global hooks"
    );
}

#[test]
fn merge_keeps_base_global_hooks_when_local_omits_them() {
    let base = Config::parse(
        r#"
version = 1

[hooks]
post_sync = "base-reload"
"#,
    )
    .expect("base parses");
    let local = Config::parse("version = 1\n").expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        runs(post_sync_of(&effective)),
        ["base-reload"],
        "a local config without a [hooks] table must preserve the base global hooks"
    );
}

#[test]
fn unknown_target_hooks_key_is_rejected_naming_it() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_chnage = "x"
"#;
    let err =
        Config::parse(toml).expect_err("an unknown key in [targets.X.hooks] must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("on_chnage"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn unknown_global_hooks_key_is_rejected_naming_it() {
    let toml = r#"
version = 1

[hooks]
pre_sync = "x"
"#;
    let err = Config::parse(toml).expect_err(
        "an unknown key in the global [hooks] table must be rejected \
         (no `pre` hooks in v1)",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pre_sync"),
            "error should name the offending key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn empty_on_change_command_string_is_rejected() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ""
"#;
    let err = Config::parse(toml).expect_err("an empty `on_change` command must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.to_lowercase().contains("empty"),
            "empty-command rejection must say the command is empty, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn blank_command_in_on_change_list_is_rejected() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ["ok-cmd", ""]
"#;
    let err = Config::parse(toml)
        .expect_err("a blank command inside an `on_change` list must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.to_lowercase().contains("empty"),
            "blank-command rejection must say the command is empty, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn empty_post_sync_command_is_rejected() {
    let toml = r#"
version = 1

[hooks]
post_sync = ""
"#;
    let err = Config::parse(toml).expect_err("an empty `post_sync` command must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.to_lowercase().contains("empty"),
            "empty-command rejection must say the command is empty, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn global_hooks_when_rejects_unknown_value_naming_it() {
    let toml = r#"
version = 1

[hooks]
post_sync = "reload"
when = "on-change"
"#;
    let err = Config::parse(toml)
        .expect_err("only `when = \"always\"` is valid on the global [hooks] table in v1");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("on-change"),
            "error should name the offending `when` value, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn source_declaring_hooks_is_rejected_at_schema_level() {
    let toml = r#"
version = 1

[sources.s]
git = "https://example.com/x.git"

[sources.s.hooks]
on_change = "evil"
"#;
    let err = Config::parse(toml).expect_err(
        "INV-1: hook fields exist only on consumer config types — a `hooks` key on a \
         source must stay an unknown field",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("hooks"),
            "error should name the rejected `hooks` key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

// TPH-001: full mise hook value grammar — string | { run, shell } table | mixed array

#[test]
fn target_hooks_on_change_parses_table_with_run_and_shell() {
    let cfg = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "bat cache --build", shell = "bash -c" }
"#,
    )
    .expect("a target with a `{ run, shell }` table `on_change` hook must parse");
    assert_eq!(runs(on_change_of(&cfg, "t")), ["bat cache --build"]);
    assert_eq!(
        shells(on_change_of(&cfg, "t")),
        [Some("bash -c")],
        "the optional `shell` key must be parsed and carried on the command"
    );
}

#[test]
fn target_hooks_on_change_table_without_shell_carries_none() {
    let cfg = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "bat cache --build" }
"#,
    )
    .expect("a `{ run }` table without `shell` must parse (shell is optional)");
    assert_eq!(runs(on_change_of(&cfg, "t")), ["bat cache --build"]);
    assert_eq!(
        shells(on_change_of(&cfg, "t")),
        [None],
        "an omitted `shell` must parse as None (sh -c semantics are the dispatch default)"
    );
}

#[test]
fn target_hooks_on_change_parses_mixed_array_in_declaration_order() {
    let cfg = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ["first --flag", { run = "second", shell = "zsh -c" }, "third"]
"#,
    )
    .expect("an `on_change` array mixing strings and `{ run, shell }` tables must parse");
    assert_eq!(
        runs(on_change_of(&cfg, "t")),
        ["first --flag", "second", "third"],
        "mixed-array commands must keep declaration order"
    );
    assert_eq!(
        shells(on_change_of(&cfg, "t")),
        [None, Some("zsh -c"), None],
        "each element keeps its own form: string elements carry no shell, table elements do"
    );
}

#[test]
fn global_hooks_post_sync_parses_table_form() {
    let cfg = Config::parse(
        r#"
version = 1

[hooks]
post_sync = { run = "reload-everything", shell = "fish -c" }
"#,
    )
    .expect("a global `post_sync` in `{ run, shell }` table form must parse");
    assert_eq!(runs(post_sync_of(&cfg)), ["reload-everything"]);
    assert_eq!(shells(post_sync_of(&cfg)), [Some("fish -c")]);
}

#[test]
fn global_hooks_post_sync_parses_mixed_array_in_declaration_order() {
    let cfg = Config::parse(
        r#"
version = 1

[hooks]
post_sync = [{ run = "first", shell = "bash -c" }, "second"]
"#,
    )
    .expect("a global `post_sync` array mixing tables and strings must parse");
    assert_eq!(runs(post_sync_of(&cfg)), ["first", "second"]);
    assert_eq!(
        shells(post_sync_of(&cfg)),
        [Some("bash -c"), None],
        "each element keeps its own form: table elements carry their shell, string elements none"
    );
}

#[test]
fn hook_table_without_run_is_rejected_naming_run() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { shell = "bash -c" }
"#;
    let err =
        Config::parse(toml).expect_err("a hook table without `run` must be rejected (required)");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("run"),
            "error should name the missing `run` key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn hook_table_unknown_key_is_rejected_naming_it() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "x", cwd = "/tmp" }
"#;
    let err = Config::parse(toml).expect_err("an unknown key in a hook table must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("cwd"),
            "error should name the offending hook-table key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn empty_run_in_hook_table_is_rejected() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "" }
"#;
    let err = Config::parse(toml).expect_err("an empty `run` in a hook table must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.to_lowercase().contains("empty"),
            "empty-run rejection must say the command is empty, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn blank_run_in_hook_table_is_rejected() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "   " }
"#;
    let err =
        Config::parse(toml).expect_err("a whitespace-only `run` in a hook table must be rejected");
    match err {
        Error::Config(msg) => {
            let m = msg.to_lowercase();
            assert!(
                m.contains("empty") || m.contains("blank"),
                "blank-run rejection must say the command is empty/blank, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn empty_shell_in_hook_table_is_rejected() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "x", shell = "" }
"#;
    let err = Config::parse(toml).expect_err("an empty `shell` in a hook table must be rejected");
    match err {
        Error::Config(msg) => {
            let m = msg.to_lowercase();
            assert!(
                m.contains("shell") && (m.contains("empty") || m.contains("blank")),
                "empty-shell rejection must name `shell` and say it is empty, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn blank_shell_in_hook_table_is_rejected() {
    let toml = r#"
version = 1

[hooks]
post_sync = { run = "reload", shell = "  " }
"#;
    let err = Config::parse(toml)
        .expect_err("a whitespace-only `shell` in a hook table must be rejected");
    match err {
        Error::Config(msg) => {
            let m = msg.to_lowercase();
            assert!(
                m.contains("shell") && (m.contains("empty") || m.contains("blank")),
                "blank-shell rejection must name `shell` and say it is empty/blank, got: {msg}"
            );
        }
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn mixed_array_table_element_without_run_is_rejected() {
    let toml = r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ["ok-cmd", { shell = "bash -c" }]
"#;
    let err = Config::parse(toml).expect_err(
        "a table element without `run` inside a mixed array must be rejected \
         (each element validated by its own form's rules)",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("run"),
            "error should name the missing `run` key, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn mixed_array_table_element_with_empty_run_is_rejected() {
    let toml = r#"
version = 1

[hooks]
post_sync = [{ run = "" }, "ok-cmd"]
"#;
    let err = Config::parse(toml)
        .expect_err("a table element with an empty `run` inside a mixed array must be rejected");
    match err {
        Error::Config(msg) => assert!(
            msg.to_lowercase().contains("empty"),
            "empty-run rejection must say the command is empty, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn merge_local_table_form_target_hooks_replace_base_hooks() {
    let base = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = ["base-cmd"]
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[targets.t]
path = "~/x"

[targets.t.hooks]
on_change = { run = "local-cmd", shell = "bash -c" }
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        runs(on_change_of(&effective, "t")),
        ["local-cmd"],
        "a local table-form hook must replace the base target's hooks wholesale"
    );
    assert_eq!(
        shells(on_change_of(&effective, "t")),
        [Some("bash -c")],
        "the local table's `shell` must survive the merge"
    );
}

#[test]
fn merge_local_mixed_array_global_hooks_win() {
    let base = Config::parse(
        r#"
version = 1

[hooks]
post_sync = { run = "base-reload", shell = "fish -c" }
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[hooks]
post_sync = ["local-first", { run = "local-second", shell = "zsh -c" }]
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        runs(post_sync_of(&effective)),
        ["local-first", "local-second"],
        "a local mixed-array [hooks] value must override the base global hooks wholesale"
    );
    assert_eq!(
        shells(post_sync_of(&effective)),
        [None, Some("zsh -c")],
        "the local mixed array's per-element shells must survive the merge"
    );
}

// TPH-008: [vars] table + phora.local.toml per-key overlay (M003)

fn vars_of(cfg: &Config) -> &BTreeMap<String, String> {
    &cfg.vars
}

#[test]
fn vars_table_parses_flat_string_map() {
    let cfg = Config::parse(
        r#"
version = 1

[vars]
email = "soeren@code17.io"
editor = "nvim"
"#,
    )
    .expect("a flat [vars] string table must parse");
    assert_eq!(
        vars_of(&cfg).get("email").map(String::as_str),
        Some("soeren@code17.io"),
        "a [vars] key must parse to its string value"
    );
    assert_eq!(
        vars_of(&cfg).get("editor").map(String::as_str),
        Some("nvim")
    );
}

#[test]
fn vars_absent_parses_as_empty_map() {
    let cfg = Config::parse("version = 1\n").expect("a config with no [vars] table must parse");
    assert!(
        vars_of(&cfg).is_empty(),
        "an absent [vars] table must parse as an empty map, never an error"
    );
}

#[test]
fn vars_non_string_value_is_rejected() {
    let toml = r"
version = 1

[vars]
count = 3
";
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "a non-string [vars] value must be rejected: vars are a flat string map"
    );
}

#[test]
fn merge_local_vars_overlay_is_per_key_local_wins_omitted_preserved() {
    let base = Config::parse(
        r#"
version = 1

[vars]
email = "work@code17.io"
editor = "nvim"
host = "thinkpad"
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[vars]
email = "home@code17.io"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let vars = vars_of(&effective);
    assert_eq!(
        vars.get("email").map(String::as_str),
        Some("home@code17.io"),
        "a local [vars] key must override the matching base key (local wins)"
    );
    assert_eq!(
        vars.get("editor").map(String::as_str),
        Some("nvim"),
        "a base [vars] key the local omits must be PRESERVED, not blanked \
         (per-key overlay, not table-level replace — dotter #174)"
    );
    assert_eq!(
        vars.get("host").map(String::as_str),
        Some("thinkpad"),
        "every base key the local omits must survive the per-key overlay"
    );
}

#[test]
fn merge_local_only_vars_added_when_base_has_none() {
    let base = Config::parse("version = 1\n").expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[vars]
email = "home@code17.io"
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        vars_of(&effective).get("email").map(String::as_str),
        Some("home@code17.io"),
        "a local-only [vars] table must be adopted wholesale when the base has none"
    );
}

#[test]
fn merge_base_vars_kept_when_local_omits_the_table() {
    let base = Config::parse(
        r#"
version = 1

[vars]
email = "work@code17.io"
"#,
    )
    .expect("base parses");
    let local = Config::parse("version = 1\n").expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert_eq!(
        vars_of(&effective).get("email").map(String::as_str),
        Some("work@code17.io"),
        "a local config with no [vars] table must preserve the base vars entirely"
    );
}

// TPH-008: template opt-in (M001) — per-binding `template` glob list + `.tmpl` suffix

fn refined<'a>(cfg: &'a Config, target: &str) -> &'a RefinedBinding {
    target_of(cfg, target)
        .sources
        .as_deref()
        .expect("target declares sources")
        .iter()
        .find_map(|b| match b {
            Binding::Refined(r) => Some(r),
            Binding::Source(_) => None,
        })
        .expect("target declares a refined binding")
}

#[test]
fn binding_template_glob_list_parses() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = ["*.conf", "**/*.ini"] }]
"#,
    )
    .expect("a per-binding `template` glob list must parse");
    let opt_in = refined(&cfg, "t")
        .template
        .as_ref()
        .expect("the binding carries a template opt-in");
    assert!(
        opt_in.renders("app.conf"),
        "a file matching a `template` glob must opt into rendering"
    );
    assert!(
        opt_in.renders("nested/dir/settings.ini"),
        "a `**/*.ini` glob must match a nested path"
    );
    assert!(
        !opt_in.renders("notes.md"),
        "a file matching no template glob (and no .tmpl suffix) must NOT opt in"
    );
}

#[test]
fn tmpl_suffix_opts_in_by_default_without_a_glob_list() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles" }]
"#,
    )
    .expect("a refined binding with no `template` key must parse");
    let opt_in = refined(&cfg, "t").template_opt_in();
    assert!(
        opt_in.renders("foo.conf.tmpl"),
        "the `.tmpl` suffix convention is ON by default and must opt a file in \
         even when no `template` glob list is declared"
    );
    assert!(
        !opt_in.renders("foo.conf"),
        "a plain file with no .tmpl suffix and no glob must not opt in"
    );
}

#[test]
fn glob_or_suffix_either_renders() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = ["*.conf"] }]
"#,
    )
    .expect("a binding with a glob list still honours the .tmpl suffix");
    let opt_in = refined(&cfg, "t")
        .template
        .as_ref()
        .expect("the binding carries a template opt-in");
    assert!(
        opt_in.renders("app.conf"),
        "a file matching the glob renders (glob arm of glob-OR-suffix)"
    );
    assert!(
        opt_in.renders("other.txt.tmpl"),
        "a .tmpl file renders even when it matches no glob (suffix arm of glob-OR-suffix)"
    );
    assert!(
        !opt_in.renders("plain.txt"),
        "a file matching neither the glob nor the suffix must not render"
    );
}

#[test]
fn template_false_disables_the_tmpl_suffix_convention() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.vendor]
git = "https://github.com/vendor/tree.git"

[targets.t]
path = "~/x"
sources = [{ source = "vendor", template = false }]
"#,
    )
    .expect("`template = false` on a binding must parse");
    let opt_in = refined(&cfg, "t").template_opt_in();
    assert!(
        !opt_in.renders("literal.conf.tmpl"),
        "`template = false` is the escape hatch: a literal *.tmpl file in a \
         third-party tree must NOT be rendered or have its suffix stripped"
    );
    assert!(
        !opt_in.renders("anything.conf"),
        "`template = false` opts NOTHING into rendering"
    );
}

#[test]
fn deployed_name_strips_tmpl_suffix_only_for_rendered_files() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = ["*.conf"] }]
"#,
    )
    .expect("binding parses");
    let opt_in = refined(&cfg, "t")
        .template
        .as_ref()
        .expect("the binding carries a template opt-in");
    assert_eq!(
        opt_in.deployed_name("foo.conf.tmpl"),
        "foo.conf",
        "a .tmpl file deploys with the suffix stripped"
    );
    assert_eq!(
        opt_in.deployed_name("app.conf"),
        "app.conf",
        "a glob-matched file with no .tmpl suffix keeps its name verbatim"
    );
    assert_eq!(
        opt_in.deployed_name("plain.txt"),
        "plain.txt",
        "a non-rendered file keeps its name verbatim"
    );
}

#[test]
fn template_glob_list_with_bad_glob_is_rejected() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = ["["] }]
"#;
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "a malformed glob in a `template` list must be rejected at parse, \
         not surface as a panic at render time"
    );
}

#[test]
fn merge_local_binding_template_replaces_base_via_sources_replace() {
    let base = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = ["*.conf"] }]
"#,
    )
    .expect("base parses");
    let local = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = false }]
"#,
    )
    .expect("local parses");

    let effective = merge_configs(base, Some(local));
    let opt_in = refined(&effective, "t").template_opt_in();
    assert!(
        !opt_in.renders("app.conf.tmpl"),
        "a local `sources` list replaces the base wholesale, so a local \
         `template = false` must win over the base glob list"
    );
}

// TPH-008 / INV-8: a config using neither [vars] nor any template opt-in is
// schema-stable — the new fields default to absent/empty and opt nothing in.

#[test]
fn feature_free_config_has_empty_vars_and_no_template_fields() {
    // M001: the `.tmpl` opt-in is file-level not config-level, so INV-8 here is field-absence, NOT `!renders(".tmpl")`.
    let cfg =
        Config::parse(EXAMPLE_TOML).expect("the feature-free example toml must parse unchanged");
    assert!(
        vars_of(&cfg).is_empty(),
        "INV-8: a config with no [vars] table must carry an empty vars map"
    );
    for (name, target) in &cfg.targets {
        for binding in target.sources.iter().flatten() {
            if let Binding::Refined(refined) = binding {
                assert!(
                    refined.template.is_none(),
                    "INV-8: target `{name}` declares no `template` glob, so the field must be absent"
                );
            }
        }
    }
}

#[test]
fn default_opt_in_for_a_bare_source_binding_honours_only_the_suffix() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = ["dotfiles"]
"#,
    )
    .expect("a bare string-form binding parses");
    let binding = target_of(&cfg, "t")
        .sources
        .as_deref()
        .expect("declares sources")
        .first()
        .expect("one binding");
    let opt_in = binding.template_opt_in();
    assert!(
        opt_in.renders("foo.conf.tmpl"),
        "a bare `\"dotfiles\"` binding has no `template` key, so the default \
         .tmpl-suffix convention is ON"
    );
    assert!(
        !opt_in.renders("foo.conf"),
        "with no glob list a bare binding renders only .tmpl files"
    );
}

#[test]
fn bare_dot_tmpl_does_not_render_or_strip_to_empty() {
    let opt_in = TemplateOptIn::SuffixOnly;
    assert!(
        !opt_in.renders(".tmpl"),
        "a file literally named `.tmpl` has no stem, so the suffix convention must NOT opt it in"
    );
    assert_eq!(
        opt_in.deployed_name(".tmpl"),
        ".tmpl",
        "stripping the `.tmpl` suffix from `.tmpl` would yield an empty name; the path must be kept verbatim"
    );
}

#[test]
fn empty_template_glob_list_is_rejected() {
    let toml = r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = [] }]
"#;
    assert!(
        matches!(Config::parse(toml), Err(Error::Config(_))),
        "`template = []` is ambiguous with the default opt-in and must be rejected, \
         not silently behave like SuffixOnly"
    );
}

#[test]
fn template_on_a_url_source_is_rejected() {
    let toml = r#"
version = 1

[sources.blob]
url = "https://example.com/file.tar.gz"

[targets.t]
path = "~/x"
sources = [{ source = "blob", template = ["*.conf"] }]
"#;
    let cfg =
        Config::parse(toml).expect("the toml itself parses; url-slice rejection is in validate()");
    assert!(
        matches!(cfg.validate(), Err(Error::Config(_))),
        "a `template` glob on a single-file `url` source is meaningless and must be rejected, \
         matching how include/exclude/root are rejected on url sources"
    );
}

#[test]
fn classifier_renders_and_deployed_name_table() {
    let globs = {
        let cfg = Config::parse(
            r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", template = ["*.conf"] }]
"#,
        )
        .expect("globs binding parses");
        refined(&cfg, "t").template_opt_in()
    };
    let suffix = TemplateOptIn::SuffixOnly;
    let disabled = TemplateOptIn::Disabled;

    assert!(
        globs.renders("app.conf.tmpl") && globs.deployed_name("app.conf.tmpl") == "app.conf",
        "glob AND suffix both match: renders and strips one .tmpl level"
    );
    assert!(
        suffix.renders("app.conf.tmpl") && suffix.deployed_name("app.conf.tmpl") == "app.conf",
        "SuffixOnly: a .tmpl file renders and strips"
    );
    assert!(
        globs.renders("foo.tmpl.tmpl") && globs.deployed_name("foo.tmpl.tmpl") == "foo.tmpl",
        "stripping removes exactly one .tmpl level"
    );
    assert!(
        !suffix.renders(".tmpl") && suffix.deployed_name(".tmpl") == ".tmpl",
        "SuffixOnly: bare .tmpl neither renders nor strips to empty"
    );
    assert!(
        !globs.renders(".tmpl") && globs.deployed_name(".tmpl") == ".tmpl",
        "Globs: bare .tmpl neither renders nor strips to empty"
    );
    assert!(
        !disabled.renders("app.conf.tmpl")
            && disabled.deployed_name("app.conf.tmpl") == "app.conf.tmpl",
        "Disabled never renders nor strips"
    );
    assert!(
        globs.renders("a/b.conf"),
        "template globs use the same default separator semantics as include/exclude (`*.conf` spans separators)"
    );
}

fn map_config_err(map_body: &str) -> Error {
    let toml = format!(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{{ source = "dotfiles", {map_body} }}]
"#
    );
    let cfg = Config::parse(&toml).expect("a structurally valid `map` binding must parse");
    cfg.validate()
        .expect_err("a binding with an invalid `map` must be rejected at validation")
}

#[test]
fn binding_map_parses_and_is_visible_on_refined_and_resolved() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", map = { "AGENTS.md" = "CLAUDE.md", "nested/AGENTS.md" = "codex.md" } }]
"#,
    )
    .expect("a valid `map` binding must parse and pass load-time validation");

    let map = refined(&cfg, "t")
        .map
        .as_ref()
        .expect("the refined binding carries the `map`");
    assert_eq!(
        map.get("AGENTS.md").map(String::as_str),
        Some("CLAUDE.md"),
        "the map selector->dest entry must round-trip on RefinedBinding"
    );
    assert_eq!(
        map.get("nested/AGENTS.md").map(String::as_str),
        Some("codex.md"),
        "a key containing `/` (a nested source file) is allowed and must round-trip"
    );

    let resolved = target_of(&cfg, "t").resolve_sources(&cfg.sources);
    let binding = resolved
        .iter()
        .find(|b| b.source == "dotfiles")
        .expect("the dotfiles binding resolves");
    let resolved_map = binding
        .map
        .as_ref()
        .expect("the resolved binding carries the `map`");
    assert_eq!(
        resolved_map.get("AGENTS.md").map(String::as_str),
        Some("CLAUDE.md"),
        "`map` must be threaded onto ResolvedBinding at the resolve_binding site"
    );
    assert_eq!(
        resolved_map.get("nested/AGENTS.md").map(String::as_str),
        Some("codex.md"),
        "the resolved map must carry every entry, slashed keys included"
    );
}

#[test]
fn binding_map_with_include_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "CLAUDE.md" }, include = ["x"]"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("map") && msg.contains("include"),
            "map+include rejection must name the source and both fields, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_with_exclude_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "CLAUDE.md" }, exclude = ["x"]"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("map") && msg.contains("exclude"),
            "map+exclude rejection must name the source and both fields, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_value_with_slash_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "sub/CLAUDE.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("sub/CLAUDE.md"),
            "a nested map dest must be rejected and the message name the offending value, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_value_with_backslash_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "sub\\CLAUDE.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains(r"sub\CLAUDE.md"),
            "a map dest with a backslash must be rejected and the message name the offending value, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_value_dotdot_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = ".." }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains(".."),
            "a `..` map dest must be rejected and the message name the offending value, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_value_dot_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "." }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && (msg.contains("map") || msg.contains("AGENTS.md")),
            "a `.` map dest must be rejected and the message name the source and the map dest rule, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_value_absolute_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "/etc/passwd" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("/etc/passwd"),
            "an absolute map dest must be rejected and the message name the offending value, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_key_absolute_is_rejected() {
    let err = map_config_err(r#"map = { "/etc/passwd" = "CLAUDE.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("/etc/passwd"),
            "an absolute map key must be rejected and named, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_key_dotdot_escape_is_rejected() {
    let err = map_config_err(r#"map = { "../outside.md" = "CLAUDE.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains(".."),
            "a `..` map key escaping the root must be rejected and named, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_key_embedded_dotdot_escape_is_rejected() {
    let err = map_config_err(r#"map = { "a/../b" = "CLAUDE.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains(".."),
            "a `..` in any key component (not just leading) escaping the root must be rejected and named, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_on_url_source_is_rejected() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.pkg]
url = "https://example.com/foo.tar.gz"

[targets.t]
path = "~/x"
sources = [{ source = "pkg", map = { "AGENTS.md" = "CLAUDE.md" } }]
"#,
    )
    .expect("a structurally valid `map` binding on a url source must parse");
    let err = cfg.validate().expect_err(
        "a `map` on a url-backed source must be rejected: url sources carry no tree to alias",
    );
    match err {
        Error::Config(msg) => assert!(
            msg.contains("pkg") && msg.contains("map"),
            "the url-slice error must name the source `pkg` and the rejected `map`, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_key_with_slash_is_accepted() {
    let cfg = Config::parse(
        r#"
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"

[targets.t]
path = "~/x"
sources = [{ source = "dotfiles", map = { "a/b/AGENTS.md" = "CLAUDE.md" } }]
"#,
    )
    .expect("a map key containing `/` (a nested source file) must be accepted");
    let map = refined(&cfg, "t")
        .map
        .as_ref()
        .expect("the refined binding carries the `map`");
    assert_eq!(
        map.get("a/b/AGENTS.md").map(String::as_str),
        Some("CLAUDE.md"),
        "a slashed key naming a nested source file must round-trip"
    );

    let resolved = target_of(&cfg, "t").resolve_sources(&cfg.sources);
    let binding = resolved
        .iter()
        .find(|b| b.source == "dotfiles")
        .expect("the dotfiles binding resolves");
    let resolved_map = binding
        .map
        .as_ref()
        .expect("the resolved binding carries the `map`");
    assert_eq!(
        resolved_map.get("a/b/AGENTS.md").map(String::as_str),
        Some("CLAUDE.md"),
        "a slashed key must survive onto ResolvedBinding at the resolve_binding site"
    );
}

#[test]
fn binding_empty_map_is_rejected() {
    let err = map_config_err(r"map = {}");
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("map"),
            "an empty `map` table is ambiguous and must be rejected, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_empty_key_is_rejected() {
    let err = map_config_err(r#"map = { "" = "CLAUDE.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("map"),
            "an empty map key must be rejected and the message name the source and the map rule, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_empty_value_is_rejected() {
    let err = map_config_err(r#"map = { "AGENTS.md" = "" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && (msg.contains("AGENTS.md") || msg.contains("map")),
            "an empty map dest must be rejected and the message name the source and the offending key/rule, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

#[test]
fn binding_map_duplicate_dest_values_are_rejected() {
    let err = map_config_err(r#"map = { "A.md" = "OUT.md", "B.md" = "OUT.md" }"#);
    match err {
        Error::Config(msg) => assert!(
            msg.contains("dotfiles") && msg.contains("OUT.md"),
            "two keys mapping to one dest must be rejected, naming the source and the duplicated dest, got: {msg}"
        ),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}
