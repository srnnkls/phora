use std::collections::{BTreeMap, BTreeSet};

use crate::config::{Config, DeployMode, ParsedSource, Refspec, SourceMode};
use crate::error::Result;
use crate::kernel::{Selection, SourceName};
use crate::lock::{Lock, LockedSource, encode_ref, entry_matches, ref_discriminator};
use crate::source::{SourceBackend, read_local_head};

use super::{effective_protocol, remote_for};

type RoutedSources = (
    Vec<(String, LockedSource)>,
    BTreeMap<(String, String), String>,
);

/// A distinct resolution unit: one (source, effective ref) pair to resolve and lock.
struct Unit {
    name: String,
    encoded_ref: String,
    effective_ref: Refspec,
}

fn resolution_units(config: &Config, parsed: &BTreeMap<String, ParsedSource>) -> Vec<Unit> {
    let mut by_key: BTreeMap<(String, String), Refspec> = BTreeMap::new();
    let mut bound: BTreeSet<String> = BTreeSet::new();
    for target in config.targets.values() {
        for binding in target.resolve_sources(parsed) {
            bound.insert(binding.source.to_owned());
            by_key.insert(
                (
                    binding.source.to_owned(),
                    encode_ref(&binding.effective_ref),
                ),
                binding.effective_ref.clone(),
            );
        }
    }
    for (name, source) in parsed {
        if bound.contains(name) {
            continue;
        }
        let r = source.refspec();
        by_key.insert((name.clone(), encode_ref(&r)), r);
    }
    by_key
        .into_iter()
        .map(|((name, encoded_ref), effective_ref)| Unit {
            name,
            encoded_ref,
            effective_ref,
        })
        .collect()
}

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
    let mut fetched: BTreeSet<String> = BTreeSet::new();

    for unit in resolution_units(config, parsed) {
        let Unit {
            name,
            encoded_ref,
            effective_ref,
        } = unit;
        let Some(source) = parsed.get(&name) else {
            continue;
        };
        let git = remote_for(remotes, &name)?;
        let source_name = SourceName::trusted(name.clone());

        if source.deploy_mode() == DeployMode::Link {
            if encoded_ref != encode_ref(&source.refspec()) {
                continue;
            }
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
                    r#ref: None,
                },
            ));
            resolved_commits.insert((name.clone(), encoded_ref), commit);
            continue;
        }

        let discriminator = ref_discriminator(&effective_ref, &source.refspec());
        let locked = effective_lock.and_then(|l| l.find_entry(&name, discriminator.as_deref()));
        let protocol = effective_protocol(source, config);
        let commit = match locked {
            Some(l)
                if entry_matches(source, &effective_ref, l, &config.hosts, protocol) && !force =>
            {
                l.commit.clone()
            }
            _ => {
                if fetched.insert(name.clone()) {
                    backend.fetch(&source_name, git)?;
                }
                backend.resolve(&source_name, git, &effective_ref)?
            }
        };

        let digest = if source.mode() == SourceMode::Url {
            let full = Selection::new(&[], &[])?;
            backend.compute_digest(&source_name, git, &commit, None, &full)?
        } else {
            let selection = Selection::new(source.includes(), source.excludes())?;
            backend.compute_digest(
                &source_name,
                git,
                &commit,
                source.root.as_deref(),
                &selection,
            )?
        };

        let resolved = if source.mode() == SourceMode::Url {
            "url".to_owned()
        } else {
            effective_ref.to_string()
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
                r#ref: discriminator,
            },
        ));
        resolved_commits.insert((name.clone(), encoded_ref), commit);
    }

    Ok((routed, resolved_commits))
}
