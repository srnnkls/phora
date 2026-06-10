use std::collections::BTreeMap;

use crate::config::{Config, DeployMode, ParsedSource, SourceMode};
use crate::error::Result;
use crate::kernel::{Selection, SourceName};
use crate::lock::{Lock, LockedSource, source_matches};
use crate::source::{SourceBackend, read_local_head};

use super::{effective_protocol, remote_for};

type RoutedSources = (Vec<(String, LockedSource)>, BTreeMap<String, String>);

pub(super) fn resolve_sources(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &dyn SourceBackend,
    force: bool,
) -> Result<RoutedSources> {
    let mut routed = Vec::new();
    let mut resolved_commits = BTreeMap::new();

    for (name, source) in parsed {
        let git = remote_for(remotes, name)?;
        let source_name = SourceName::new(name.clone());
        if source.deploy_mode() == DeployMode::Link {
            let commit = read_local_head(git)?;
            routed.push((
                name.clone(),
                LockedSource {
                    name: name.clone(),
                    git: git.to_owned(),
                    resolved: "link".to_owned(),
                    commit: commit.clone(),
                    digest: "link:".to_owned(),
                    config_digest: source.config_digest(),
                },
            ));
            resolved_commits.insert(name.clone(), commit);
            continue;
        }

        let locked = effective_lock.and_then(|l| l.find_source(name));
        let commit = match locked {
            Some(l)
                if source_matches(source, l, &config.hosts, effective_protocol(source, config))
                    && !force =>
            {
                l.commit.clone()
            }
            _ => {
                backend.fetch(&source_name, git)?;
                backend.resolve(&source_name, git, &source.refspec())?
            }
        };

        let selection = Selection::new(source.includes(), source.excludes())?;
        let digest = backend.compute_digest(
            &source_name,
            git,
            &commit,
            source.root.as_deref(),
            &selection,
        )?;

        let resolved = if source.mode() == SourceMode::Url {
            "url".to_owned()
        } else {
            source.refspec().to_string()
        };
        routed.push((
            name.clone(),
            LockedSource {
                name: name.clone(),
                git: git.to_owned(),
                resolved,
                commit: commit.clone(),
                digest,
                config_digest: source.config_digest(),
            },
        ));
        resolved_commits.insert(name.clone(), commit);
    }

    Ok((routed, resolved_commits))
}
