//! Lock file DTOs (`phora.lock`, `phora.local.lock`) and resolution matching.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::Source;

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
}

/// Effective lock merges base and local locks; local entries override base by name.
#[must_use]
pub fn merge_locks(base: &Lock, local: Option<&Lock>) -> Lock {
    let mut merged = base.clone();
    if let Some(local) = local {
        for local_source in &local.sources {
            merged.sources.retain(|s| s.name != local_source.name);
            merged.sources.push(local_source.clone());
        }
    }
    merged
}

/// Whether a locked resolution can be reused for `source`: identical git URL,
/// refspec, and export-affecting config digest.
#[must_use]
pub fn source_matches(source: &Source, locked: &LockedSource) -> bool {
    // HAS-005: compare NormalizedUrl/MirrorKey identity, not raw strings
    source.git.as_deref().unwrap_or_default() == locked.git
        && source.refspec().to_string() == locked.resolved
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
    use super::*;
    use crate::config::Config;

    fn locked(name: &str, git: &str, resolved: &str) -> LockedSource {
        LockedSource {
            name: name.to_owned(),
            git: git.to_owned(),
            resolved: resolved.to_owned(),
            commit: "c0ffee".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: "blake3:cfg".to_owned(),
        }
    }

    fn source_from(toml_body: &str) -> Source {
        let toml = format!("version = 1\n\n[sources.s]\n{toml_body}");
        Config::parse(&toml)
            .expect("source toml parses")
            .sources
            .remove("s")
            .expect("source `s` present")
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

    // PAM-042: source_matches

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
        };

        assert!(
            source_matches(&source, &locked),
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
        };

        assert!(
            !source_matches(&source, &locked),
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
        };

        assert!(
            !source_matches(&source, &locked),
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
        };

        assert_ne!(
            source.config_digest(),
            other.config_digest(),
            "changing include must change the config digest (guards the test premise)"
        );
        assert!(
            !source_matches(&source, &locked),
            "same git + refspec but changed export config must NOT reuse the locked commit"
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
}
