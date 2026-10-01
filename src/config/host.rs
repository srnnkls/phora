//! Host registry: built-in forges, remote URL templates, and auth.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// The shipped forge registry: the single source of truth for built-in hosts.
#[must_use]
pub fn builtin_forges() -> BTreeMap<String, Host> {
    fn forge(https: &str, ssh: &str, web: &str) -> Host {
        Host {
            remote: Some(RemoteConfig {
                https: Some(https.to_owned()),
                ssh: Some(ssh.to_owned()),
            }),
            web: Some(web.to_owned()),
            auth: None,
        }
    }

    BTreeMap::from([
        (
            "github".to_owned(),
            forge(
                "https://github.com/{path}.git",
                "git@github.com:{path}.git",
                "https://github.com/{path}/blob/{commit}/{file}#L{line}",
            ),
        ),
        (
            "gitlab".to_owned(),
            forge(
                "https://gitlab.com/{path}.git",
                "git@gitlab.com:{path}.git",
                "https://gitlab.com/{path}/-/blob/{commit}/{file}#L{line}",
            ),
        ),
        (
            "codeberg".to_owned(),
            forge(
                "https://codeberg.org/{path}.git",
                "git@codeberg.org:{path}.git",
                "https://codeberg.org/{path}/src/commit/{commit}/{file}#L{line}",
            ),
        ),
        (
            "sr.ht".to_owned(),
            forge(
                "https://git.sr.ht/{path}",
                "git@git.sr.ht:{path}",
                "https://git.sr.ht/{path}/tree/{commit}/item/{file}#L{line}",
            ),
        ),
        (
            "bitbucket".to_owned(),
            forge(
                "https://bitbucket.org/{path}.git",
                "git@bitbucket.org:{path}.git",
                "https://bitbucket.org/{path}/src/{commit}/{file}#lines-{line}",
            ),
        ),
    ])
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    #[serde(default)]
    pub remote: Option<RemoteConfig>,
    /// A browsable file link; `remote`'s placeholders plus `{commit}`, `{file}`, `{line}`.
    #[serde(default)]
    pub web: Option<String>,
    pub auth: Option<AuthConfig>,
}

impl Host {
    #[must_use]
    pub(super) fn merged_with(mut self, local: Host) -> Host {
        if local.remote.is_some() {
            self.remote = local.remote;
        }
        if local.web.is_some() {
            self.web = local.web;
        }
        if local.auth.is_some() {
            self.auth = local.auth;
        }
        self
    }
}

/// A host's remote URL templates. A bare string is the https template; a table
/// carries explicit `https`/`ssh` keys. Templates support `{owner}`, `{repo}`,
/// `{path}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RemoteConfigRaw")]
pub struct RemoteConfig {
    https: Option<String>,
    ssh: Option<String>,
}

impl RemoteConfig {
    #[must_use]
    pub fn https_template(&self) -> Option<&str> {
        self.https.as_deref()
    }

    #[must_use]
    pub fn ssh_template(&self) -> Option<&str> {
        self.ssh.as_deref()
    }
}

enum RemoteConfigRaw {
    Simple(String),
    Table(RemoteTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteTable {
    https: Option<String>,
    ssh: Option<String>,
}

impl<'de> Deserialize<'de> for RemoteConfigRaw {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RawVisitor;

        impl<'de> serde::de::Visitor<'de> for RawVisitor {
            type Value = RemoteConfigRaw;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a remote URL string or a { https, ssh } table")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(RemoteConfigRaw::Simple(v.to_owned()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                RemoteTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(RemoteConfigRaw::Table)
            }
        }

        deserializer.deserialize_any(RawVisitor)
    }
}

impl From<RemoteConfigRaw> for RemoteConfig {
    fn from(raw: RemoteConfigRaw) -> Self {
        match raw {
            RemoteConfigRaw::Simple(https) => RemoteConfig {
                https: Some(https),
                ssh: None,
            },
            RemoteConfigRaw::Table(t) => RemoteConfig {
                https: t.https,
                ssh: t.ssh,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum AuthConfig {
    #[serde(rename = "ssh")]
    Ssh { key: Option<PathBuf> },
    #[serde(rename = "token")]
    Token { env: String },
}
