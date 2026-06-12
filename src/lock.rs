//! Lock file DTOs (`phora.lock`, `phora.local.lock`) and resolution matching.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::{Host, ParsedSource, Protocol, Refspec, SourceMode};
use crate::source::NormalizedUrl;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lock {
    pub version: u32,
    pub sources: Vec<LockedSource>,
}

impl Lock {
    #[must_use]
    pub fn find_source(&self, name: &str) -> Option<&LockedSource> {
        self.sources.iter().find(|s| s.name == name)
    }

    #[must_use]
    pub fn find_entry(&self, name: &str, r#ref: Option<&str>) -> Option<&LockedSource> {
        self.sources
            .iter()
            .find(|s| s.name == name && s.r#ref.as_deref() == r#ref)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSource {
    pub name: String,
    pub git: String,
    pub resolved: String,
    pub commit: String,
    pub digest: String,
    /// Hash of export-affecting config; lets sync detect config changes that alter
    /// export output without a commit change.
    pub config_digest: String,
    /// Kind-tagged effective ref, set only when this entry overrides the source's
    /// default refspec; `None` for the canonical entry so bare locks stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
}

/// Kind-tagged so `Branch("x")` and `Tag("x")` never collide.
#[must_use]
pub fn encode_ref(r: &Refspec) -> String {
    match r {
        Refspec::Branch(s) => format!("branch:{s}"),
        Refspec::Tag(s) => format!("tag:{s}"),
        Refspec::Rev(s) => format!("rev:{s}"),
        Refspec::None => "url".to_owned(),
    }
}

/// The `ref` field value for an entry: `None` when the effective ref equals the
/// source's default, else the kind-tagged override.
#[must_use]
pub fn ref_discriminator(effective_ref: &Refspec, source_default: &Refspec) -> Option<String> {
    let encoded = encode_ref(effective_ref);
    (encoded != encode_ref(source_default)).then_some(encoded)
}

/// Effective lock merges base and local locks; local entries override base by name.
#[must_use]
pub fn merge_locks(base: &Lock, local: Option<&Lock>) -> Lock {
    let mut merged = base.clone();
    if let Some(local) = local {
        for local_source in &local.sources {
            merged
                .sources
                .retain(|s| !(s.name == local_source.name && s.r#ref == local_source.r#ref));
            merged.sources.push(local_source.clone());
        }
    }
    merged
}

/// Whether a locked resolution can be reused for `source`: same remote identity,
/// refspec, and export-affecting config digest. A source that fails to resolve
/// never matches.
#[must_use]
pub fn source_matches(
    source: &ParsedSource,
    locked: &LockedSource,
    hosts: &BTreeMap<String, Host>,
    protocol: Protocol,
) -> bool {
    entry_matches(source, &source.refspec(), locked, hosts, protocol)
}

/// Like [`source_matches`] but checks `effective_ref` against the lock's `resolved`,
/// so a ref-overriding binding reuses only its own (source, ref) entry.
#[must_use]
pub fn entry_matches(
    source: &ParsedSource,
    effective_ref: &Refspec,
    locked: &LockedSource,
    hosts: &BTreeMap<String, Host>,
    protocol: Protocol,
) -> bool {
    if let (SourceMode::Url, Some(url)) = (source.mode(), source.source_url()) {
        // Url identity = url + config_digest; the synthetic commit is content-addressed, so no refspec/remote comparison.
        return NormalizedUrl::parse(url) == NormalizedUrl::parse(&locked.git)
            && source.config_digest() == locked.config_digest;
    }

    let Ok(resolved) = source.resolved_remote(hosts, protocol) else {
        return false;
    };
    // Protocol-independent: https/ssh/literal forms of one repo share an identity.
    NormalizedUrl::parse(&resolved) == NormalizedUrl::parse(&locked.git)
        && effective_ref.to_string() == locked.resolved
        && source.config_digest() == locked.config_digest
}

/// Routes resolved sources into the base lock and the local lock (`None` when no
/// source name appears in `local_override_names`).
#[must_use]
pub fn split_locks(
    resolved: Vec<(String, LockedSource)>,
    local_override_names: &BTreeSet<String>,
) -> (Lock, Option<Lock>) {
    let mut base = Vec::new();
    let mut local = Vec::new();
    for (name, locked) in resolved {
        if local_override_names.contains(&name) {
            local.push(locked);
        } else {
            base.push(locked);
        }
    }
    let base_lock = Lock {
        version: 1,
        sources: base,
    };
    let local_lock = (!local.is_empty()).then_some(Lock {
        version: 1,
        sources: local,
    });
    (base_lock, local_lock)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::{Config, Host, Protocol};
    use crate::source::NormalizedUrl;

    fn no_hosts() -> BTreeMap<String, Host> {
        BTreeMap::new()
    }

    fn hosts_from(toml: &str) -> BTreeMap<String, Host> {
        Config::parse(toml).expect("hosts toml parses").hosts
    }

    fn locked(name: &str, git: &str, resolved: &str) -> LockedSource {
        LockedSource {
            name: name.to_owned(),
            git: git.to_owned(),
            resolved: resolved.to_owned(),
            commit: "c0ffee".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: "blake3:cfg".to_owned(),
            r#ref: None,
        }
    }

    fn source_from(toml_body: &str) -> ParsedSource {
        let toml = format!("version = 1\n\n[sources.s]\n{toml_body}");
        let raw = Config::parse(&toml)
            .expect("source toml parses")
            .sources
            .remove("s")
            .expect("source `s` present");
        ParsedSource::parse("s", &raw).expect("source parses to typed form")
    }

    // PAM-009: lock TOML round-trip

    #[test]
    fn lock_round_trips_through_toml() {
        let lock = Lock {
            version: 1,
            sources: vec![
                locked("dotfiles", "https://github.com/me/dotfiles.git", "main"),
                locked(
                    "company-configs",
                    "https://github.com/company/shared-configs.git",
                    "v2.1",
                ),
            ],
        };

        let text = toml::to_string(&lock).expect("lock serializes to toml");
        let parsed: Lock = toml::from_str(&text).expect("lock deserializes from toml");

        assert_eq!(parsed.version, lock.version);
        assert_eq!(parsed.sources.len(), lock.sources.len());
        let a = &parsed.sources[0];
        let b = &lock.sources[0];
        assert_eq!(a.name, b.name);
        assert_eq!(a.git, b.git);
        assert_eq!(a.resolved, b.resolved);
        assert_eq!(a.commit, b.commit);
        assert_eq!(a.digest, b.digest);
        assert_eq!(a.config_digest, b.config_digest);
    }

    #[test]
    fn lock_toml_uses_sources_array_of_tables_with_spec_field_names() {
        let lock = Lock {
            version: 1,
            sources: vec![locked(
                "dotfiles",
                "https://github.com/me/dotfiles.git",
                "main",
            )],
        };

        let text = toml::to_string(&lock).expect("lock serializes to toml");

        assert!(text.contains("version = 1"), "version field, got:\n{text}");
        assert!(
            text.contains("[[sources]]"),
            "sources must be an array-of-tables, got:\n{text}"
        );
        for field in [
            "name",
            "git",
            "resolved",
            "commit",
            "digest",
            "config_digest",
        ] {
            assert!(
                text.contains(&format!("{field} =")),
                "missing `{field}` field in:\n{text}"
            );
        }
    }

    // PAM-010: merge_locks (regression guard)

    #[test]
    fn merge_locks_local_overrides_base_by_name() {
        let base = Lock {
            version: 1,
            sources: vec![locked(
                "loqui",
                "https://github.com/srnnkls/loqui.git",
                "v1.0",
            )],
        };
        let local = Lock {
            version: 1,
            sources: vec![locked("loqui", "/home/soeren/dev/loqui", "main")],
        };

        let merged = merge_locks(&base, Some(&local));

        let loqui = merged.find_source("loqui").expect("loqui kept");
        assert_eq!(loqui.git, "/home/soeren/dev/loqui");
        assert_eq!(loqui.resolved, "main");
        assert_eq!(
            merged.sources.iter().filter(|s| s.name == "loqui").count(),
            1,
            "override replaces, does not duplicate"
        );
    }

    #[test]
    fn merge_locks_keeps_base_only_and_adds_local_only() {
        let base = Lock {
            version: 1,
            sources: vec![locked(
                "dotfiles",
                "https://github.com/me/dotfiles.git",
                "main",
            )],
        };
        let local = Lock {
            version: 1,
            sources: vec![locked("extra", "/home/soeren/dev/extra", "main")],
        };

        let merged = merge_locks(&base, Some(&local));

        assert!(merged.find_source("dotfiles").is_some(), "base-only kept");
        assert!(merged.find_source("extra").is_some(), "local-only added");
    }

    #[test]
    fn source_matches_when_git_refspec_and_config_digest_all_agree() {
        let source =
            source_from("git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "dotfiles".to_owned(),
            git: "https://github.com/me/dotfiles.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "identical git + refspec + config_digest must reuse the lock"
        );
    }

    #[test]
    fn source_does_not_match_when_git_differs() {
        let source =
            source_from("git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "dotfiles".to_owned(),
            git: "https://github.com/OTHER/dotfiles.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "different git URL must not match"
        );
    }

    #[test]
    fn source_does_not_match_when_refspec_differs() {
        let source =
            source_from("git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "dotfiles".to_owned(),
            git: "https://github.com/me/dotfiles.git".to_owned(),
            resolved: "develop".to_owned(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "different resolved refspec must not match"
        );
    }

    #[test]
    fn source_does_not_match_when_config_digest_differs_despite_same_git_and_refspec() {
        let source = source_from(
            "git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\ninclude = [\"editor\"]\n",
        );
        let other = source_from(
            "git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\ninclude = [\"lint\"]\n",
        );
        let locked = LockedSource {
            name: "dotfiles".to_owned(),
            git: "https://github.com/me/dotfiles.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: other.config_digest(),
            r#ref: None,
        };

        assert_ne!(
            source.config_digest(),
            other.config_digest(),
            "changing include must change the config digest (guards the test premise)"
        );
        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "same git + refspec but changed export config must NOT reuse the locked commit"
        );
    }

    #[test]
    fn literal_git_lock_still_matches_literal_git_source() {
        let source =
            source_from("git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "dotfiles".to_owned(),
            // lock git lacks `.git`: raw-string compare fails, NormalizedUrl matches.
            git: "https://github.com/me/dotfiles".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a literal-git lock must still match its literal-git source under \
             normalized-identity comparison"
        );
    }

    #[test]
    fn host_path_source_matches_equivalent_literal_github_lock() {
        let source =
            source_from("host = \"github\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "tropos".to_owned(),
            git: "https://github.com/srnnkls/tropos.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a host+path source resolving (via built-in github) to the locked literal \
             github URL must match, so sync suppresses the fetch"
        );
    }

    #[test]
    fn lock_written_https_still_matches_source_resolved_as_ssh() {
        let source =
            source_from("host = \"github\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\n");

        let ssh_remote = source
            .resolved_remote(&no_hosts(), Protocol::Ssh)
            .expect("symbolic github ssh resolves");
        assert!(
            ssh_remote.starts_with("git@github.com:"),
            "test premise: the ssh resolution must be the scp-style form, got {ssh_remote}"
        );

        let locked = LockedSource {
            name: "tropos".to_owned(),
            git: "https://github.com/srnnkls/tropos.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert_ne!(
            ssh_remote, locked.git,
            "premise: the raw ssh and https strings differ — a raw-string compare would reject this"
        );
        assert_eq!(
            NormalizedUrl::parse(&ssh_remote),
            NormalizedUrl::parse(&locked.git),
            "premise: both forms normalize to one identity"
        );

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Ssh),
            "an https-written lock must still match the same repo resolved as ssh: \
             flipping protocol must not force a refetch"
        );
    }

    #[test]
    fn source_does_not_match_when_remote_identity_differs() {
        let source =
            source_from("host = \"github\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "tropos".to_owned(),
            git: "https://github.com/srnnkls/OTHER-REPO.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a different repo identity must not match, even under normalized comparison"
        );
    }

    #[test]
    fn source_does_not_match_when_remote_cannot_resolve() {
        let source =
            source_from("host = \"nonesuch\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "tropos".to_owned(),
            git: "https://github.com/srnnkls/tropos.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a source whose remote cannot be resolved must not match any lock"
        );
    }

    #[test]
    fn host_override_resolution_flows_into_source_matches() {
        let hosts = hosts_from(
            "version = 1\n\n[hosts.github]\nremote = { https = \"https://ghe.corp/{path}.git\", \
             ssh = \"git@ghe.corp:{path}.git\" }\n",
        );
        let source =
            source_from("host = \"github\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\n");

        let enterprise_lock = LockedSource {
            name: "tropos".to_owned(),
            git: "https://ghe.corp/srnnkls/tropos.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &enterprise_lock, &hosts, Protocol::Https),
            "the [hosts.github] override must flow into resolution so the source matches the \
             ghe.corp lock"
        );

        let builtin_lock = LockedSource {
            git: "https://github.com/srnnkls/tropos.git".to_owned(),
            ..enterprise_lock.clone()
        };
        assert!(
            !source_matches(&source, &builtin_lock, &hosts, Protocol::Https),
            "with the override active the source resolves to ghe.corp, so a builtin-github lock \
             must NOT match — proving the hosts arg drove resolution, not the builtin"
        );
    }

    #[test]
    fn existing_literal_git_lock_deserializes_and_matches() {
        let toml = "\
name = \"dotfiles\"
git = \"https://github.com/me/dotfiles.git\"
resolved = \"main\"
commit = \"abc123\"
digest = \"blake3:artifact\"
config_digest = \"PLACEHOLDER\"
";
        let source =
            source_from("git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\n");
        let toml = toml.replace("PLACEHOLDER", &source.config_digest());

        let locked: LockedSource =
            toml::from_str(&toml).expect("an existing literal-git lock must still deserialize");

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "an existing lock written with only today's fields must still match its source — \
             no version bump, no new required field"
        );
    }

    #[test]
    fn host_path_source_does_not_match_when_config_digest_differs() {
        let source = source_from(
            "host = \"github\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\ninclude = [\"editor\"]\n",
        );
        let other = source_from(
            "host = \"github\"\npath = \"srnnkls/tropos\"\nbranch = \"main\"\ninclude = [\"lint\"]\n",
        );
        let locked = LockedSource {
            name: "tropos".to_owned(),
            git: "https://github.com/srnnkls/tropos.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: other.config_digest(),
            r#ref: None,
        };

        assert_ne!(
            source.config_digest(),
            other.config_digest(),
            "changing include must change the config digest (guards the test premise)"
        );
        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a symbolic host+path source whose remote + refspec match but whose config_digest \
             differs must NOT reuse the lock"
        );
    }

    // HTP-006: url-source lock identity (no resolved_remote, no refspec)

    /// A url-mode `Source`: only `url` set, so `mode()` is `SourceMode::Url` and
    /// `resolved_remote` fabricates a bogus github url that must NOT be consulted.
    fn url_source(url: &str) -> ParsedSource {
        source_from(&format!("url = \"{url}\"\n"))
    }

    #[test]
    fn url_source_matches_lock_with_same_url_and_config_digest() {
        let source = url_source("https://example.com/p.tar.gz");
        assert_eq!(
            source.mode(),
            crate::config::SourceMode::Url,
            "premise: the source must classify as a url source"
        );
        let locked = LockedSource {
            name: "s".to_owned(),
            git: "https://example.com/p.tar.gz".to_owned(),
            resolved: "url".to_owned(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a url source whose locked git equals its url and whose config_digest agrees must \
             reuse the lock — comparison must ignore resolved_remote (which fabricates a github \
             url) and the refspec"
        );
    }

    #[test]
    fn url_source_does_not_match_when_url_differs() {
        let source = url_source("https://example.com/p.tar.gz");
        let locked = LockedSource {
            name: "s".to_owned(),
            git: "https://example.com/OTHER.tar.gz".to_owned(),
            resolved: "url".to_owned(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a different download url must not match, even though both are url sources"
        );
    }

    #[test]
    fn url_source_does_not_match_when_config_digest_differs() {
        let source = url_source("https://example.com/p.tar.gz");
        let other = source_from("url = \"https://example.com/p.tar.gz\"\nallow_symlinks = true\n");
        let locked = LockedSource {
            name: "s".to_owned(),
            git: "https://example.com/p.tar.gz".to_owned(),
            resolved: "url".to_owned(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: other.config_digest(),
            r#ref: None,
        };

        assert_ne!(
            source.config_digest(),
            other.config_digest(),
            "changing allow_symlinks must change the config digest (guards the test premise)"
        );
        assert!(
            !source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "same url but a changed export config must NOT reuse the locked url import"
        );
    }

    #[test]
    fn git_source_matching_is_unchanged_by_url_branch() {
        let source =
            source_from("git = \"https://github.com/me/dotfiles.git\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "dotfiles".to_owned(),
            git: "https://github.com/me/dotfiles.git".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "adding a url branch to source_matches must not regress git matching: a git source \
             with agreeing remote + refspec + config_digest still matches"
        );
    }

    // ARCH-005: local `path` source has the same lock identity as its `git=<localpath>` alias

    #[test]
    fn local_path_source_matches_lock_keyed_by_the_local_path() {
        let source = source_from("path = \"/home/me/dev/loqui\"\nbranch = \"main\"\n");
        let locked = LockedSource {
            name: "loqui".to_owned(),
            git: "/home/me/dev/loqui".to_owned(),
            resolved: source.refspec().to_string(),
            commit: "abc123".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: source.config_digest(),
            r#ref: None,
        };

        assert!(
            source_matches(&source, &locked, &no_hosts(), Protocol::Https),
            "a local `path` source must reuse a lock whose `git` field is that same local path, \
             keeping lock identity byte-identical with the `git = <localpath>` alias"
        );
    }

    #[test]
    fn local_path_and_git_localpath_alias_produce_identical_lock_git_field() {
        let via_path = source_from("path = \"/home/me/dev/loqui\"\nbranch = \"main\"\n");
        let via_git = source_from("git = \"/home/me/dev/loqui\"\nbranch = \"main\"\n");
        assert_eq!(
            via_path
                .resolved_remote(&no_hosts(), Protocol::Https)
                .expect("path local resolves"),
            via_git
                .resolved_remote(&no_hosts(), Protocol::Https)
                .expect("git-alias local resolves"),
            "the resolved remote written into the lock `git` field must be identical whether the \
             local source is declared via `path` or the `git = <localpath>` alias"
        );
    }

    // PTV-004: merge dedups by (name, resolved ref), not name alone

    #[test]
    fn merge_locks_dedups_ref_split_source_by_name_and_ref_not_name_alone() {
        let git = "https://github.com/junegunn/fzf.git";
        let base = Lock {
            version: 1,
            sources: vec![
                LockedSource {
                    name: "fzf".to_owned(),
                    git: git.to_owned(),
                    resolved: "v0.55.0".to_owned(),
                    commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    digest: "blake3:v55".to_owned(),
                    config_digest: "blake3:cfg".to_owned(),
                    r#ref: Some("tag:v0.55.0".to_owned()),
                },
                LockedSource {
                    name: "fzf".to_owned(),
                    git: git.to_owned(),
                    resolved: "v0.56.0".to_owned(),
                    commit: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                    digest: "blake3:v56".to_owned(),
                    config_digest: "blake3:cfg".to_owned(),
                    r#ref: Some("tag:v0.56.0".to_owned()),
                },
            ],
        };
        // Local overrides only the v0.56.0 split (e.g. repointed at a local checkout).
        let local = Lock {
            version: 1,
            sources: vec![LockedSource {
                name: "fzf".to_owned(),
                git: "/home/me/dev/fzf".to_owned(),
                resolved: "v0.56.0".to_owned(),
                commit: "cccccccccccccccccccccccccccccccccccccccc".to_owned(),
                digest: "blake3:local56".to_owned(),
                config_digest: "blake3:cfg".to_owned(),
                r#ref: Some("tag:v0.56.0".to_owned()),
            }],
        };

        let merged = merge_locks(&base, Some(&local));

        let fzf: Vec<&LockedSource> = merged.sources.iter().filter(|s| s.name == "fzf").collect();
        assert_eq!(
            fzf.len(),
            2,
            "merge must dedup by (name, ref): the v0.55.0 base split survives and only the \
             v0.56.0 split is replaced — a name-only dedup wrongly collapses to one entry, got {fzf:?}"
        );

        let v55 = fzf
            .iter()
            .find(|s| s.resolved == "v0.55.0")
            .expect("the un-overridden v0.55.0 base split must survive the merge");
        assert_eq!(
            v55.commit, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "the surviving v0.55.0 split must keep its base commit, untouched by the local override"
        );

        let v56 = fzf
            .iter()
            .find(|s| s.resolved == "v0.56.0")
            .expect("the v0.56.0 split must still be present after override");
        assert_eq!(
            v56.git, "/home/me/dev/fzf",
            "the local override must REPLACE the matching (name, v0.56.0) split"
        );
        assert_eq!(
            v56.commit, "cccccccccccccccccccccccccccccccccccccccc",
            "the replaced v0.56.0 split must carry the local override's commit"
        );
    }

    // PAM-011: split_locks

    #[test]
    fn split_locks_without_overrides_yields_no_local_lock() {
        let resolved = vec![
            (
                "dotfiles".to_owned(),
                locked("dotfiles", "https://github.com/me/dotfiles.git", "main"),
            ),
            (
                "loqui".to_owned(),
                locked("loqui", "https://github.com/srnnkls/loqui.git", "v1.0"),
            ),
        ];

        let (base, local) = split_locks(resolved, &BTreeSet::new());

        assert_eq!(base.version, 1, "base lock version is 1");
        assert_eq!(base.sources.len(), 2, "all sources land in the base lock");
        assert!(local.is_none(), "no overrides => no local lock");
    }

    #[test]
    fn split_locks_routes_overrides_to_local_lock_only() {
        let resolved = vec![
            (
                "dotfiles".to_owned(),
                locked("dotfiles", "https://github.com/me/dotfiles.git", "main"),
            ),
            (
                "loqui".to_owned(),
                locked("loqui", "/home/soeren/dev/loqui", "main"),
            ),
        ];
        let overrides: BTreeSet<String> = ["loqui".to_owned()].into_iter().collect();

        let (base, local) = split_locks(resolved, &overrides);

        assert!(
            base.find_source("dotfiles").is_some(),
            "non-overridden in base"
        );
        assert!(
            base.find_source("loqui").is_none(),
            "overridden source must NOT appear in the committed base lock"
        );

        let local = local.expect("override present => local lock exists");
        assert_eq!(local.version, 1, "local lock version is 1");
        assert!(
            local.find_source("loqui").is_some(),
            "override in local lock"
        );
        assert!(
            local.find_source("dotfiles").is_none(),
            "non-overridden source must NOT appear in the local lock"
        );
    }

    // Guards existing_literal_git_lock_deserializes_and_matches: a future optional lock field (PTV-004) must skip-serialize so bare locks stay byte-identical.
    #[test]
    fn bare_locked_source_serializes_only_existing_fields() {
        let lock = Lock {
            version: 1,
            sources: vec![locked(
                "dotfiles",
                "https://github.com/me/dotfiles.git",
                "main",
            )],
        };

        let text = toml::to_string(&lock).expect("lock serializes to toml");

        let value: toml::Value = toml::from_str(&text).expect("serialized lock re-parses as toml");
        let sources = value
            .get("sources")
            .and_then(toml::Value::as_array)
            .expect("serialized lock has a [[sources]] array");
        assert_eq!(sources.len(), 1, "fixture has exactly one locked source");
        let source = sources[0]
            .as_table()
            .expect("the single [[sources]] entry is a table");

        let mut keys: Vec<&str> = source.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "commit",
                "config_digest",
                "digest",
                "git",
                "name",
                "resolved"
            ],
            "a bare lock (no per-binding ref) must serialize EXACTLY this key set; any extra key \
             means a new optional lock field was not skip-serialized when absent, so old configs \
             would not lock byte-identically, got:\n{text}"
        );
    }
}
