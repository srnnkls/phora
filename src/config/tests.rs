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
    let resolved = target_of(&cfg, "t").resolve_sources();
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
    let resolved = target_of(&cfg, "t").resolve_sources();
    assert_eq!(
        resolved,
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
    assert_eq!(
        vscode.sources.as_deref(),
        Some(["dotfiles".to_string(), "company-configs".to_string()].as_slice())
    );
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
    let cfg = Config::parse("version = 1\n\n[defaults]\n")
        .expect("an empty [defaults] section parses");
    assert!(
        cfg.defaults.auto_target(),
        "an empty [defaults] section leaves auto_target at its true default"
    );
}

#[test]
fn merge_local_auto_target_false_overrides_base() {
    let base = Config::parse("version = 1\n").expect("base parses");
    let local = Config::parse("version = 1\n\n[defaults]\nauto_target = false\n")
        .expect("local parses");

    let effective = merge_configs(base, Some(local));
    assert!(
        !effective.defaults.auto_target(),
        "local `auto_target = false` must override the base default of true"
    );
}

#[test]
fn merge_local_unset_auto_target_keeps_base() {
    let base = Config::parse("version = 1\n\n[defaults]\nauto_target = false\n")
        .expect("base parses");
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
