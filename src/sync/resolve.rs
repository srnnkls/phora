use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;

use crate::config::{Config, DeployMode, ParsedSource, Refspec, SourceMode};
use crate::error::Result;
use crate::kernel::{Selection, SourceName};
use crate::lock::{Lock, LockedSource, encode_ref, entry_matches, ref_discriminator};
use crate::source::{MirrorKey, NormalizedUrl, SourceBackend, read_local_head};

use super::{effective_protocol, remote_for};

pub type RoutedSources = (
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

/// Outcome of resolving one unit, carrying its source-routing entry plus the
/// `(name, encoded_ref) -> commit` pair for the resolved-commits map.
struct Resolved {
    name: String,
    encoded_ref: String,
    commit: String,
    locked: LockedSource,
}

/// Concurrent fetches into one bare mirror corrupt it, so fetches sharing a
/// [`MirrorKey`] run serially while distinct keys fetch in parallel. Git-mode
/// fetch is idempotent (one per key); url-mode fetch validates each source's own
/// integrity digest, so every url-mode source must fetch — deduping by key would
/// silently bypass the second source's digest pin.
fn fetch_distinct_mirrors(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &(dyn SourceBackend + Sync),
    force: bool,
    units: &[Unit],
) -> Result<()> {
    let mut groups: BTreeMap<String, Vec<(SourceName, String)>> = BTreeMap::new();
    for unit in units {
        let Some(source) = parsed.get(&unit.name) else {
            continue;
        };
        if source.deploy_mode() == DeployMode::Link {
            continue;
        }
        if lock_hit(config, source, unit, effective_lock, force).is_some() {
            continue;
        }
        let git = remote_for(remotes, &unit.name)?;
        let key = MirrorKey::from_url(&NormalizedUrl::parse(git))
            .as_str()
            .to_owned();
        let fetches = groups.entry(key).or_default();
        if source.mode() == SourceMode::Url || fetches.is_empty() {
            fetches.push((SourceName::trusted(unit.name.clone()), git.to_owned()));
        }
    }

    groups.into_par_iter().try_for_each(|(_key, fetches)| {
        for (name, git) in &fetches {
            backend.fetch(name, git)?;
        }
        Ok(())
    })
}

/// The lock entry that lets a unit skip fetch+resolve, or `None` when a fetch is
/// required (no matching entry, or `force`).
fn lock_hit<'l>(
    config: &Config,
    source: &ParsedSource,
    unit: &Unit,
    effective_lock: Option<&'l Lock>,
    force: bool,
) -> Option<&'l LockedSource> {
    if force {
        return None;
    }
    let discriminator = ref_discriminator(&unit.effective_ref, &source.refspec());
    let protocol = effective_protocol(source, config);
    effective_lock
        .and_then(|l| l.find_entry(&unit.name, discriminator.as_deref()))
        .filter(|l| entry_matches(source, &unit.effective_ref, l, &config.hosts, protocol))
}

fn resolve_unit(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &(dyn SourceBackend + Sync),
    force: bool,
    unit: &Unit,
) -> Result<Option<Resolved>> {
    let Some(source) = parsed.get(&unit.name) else {
        return Ok(None);
    };
    let git = remote_for(remotes, &unit.name)?;
    let source_name = SourceName::trusted(unit.name.clone());

    if source.deploy_mode() == DeployMode::Link {
        if unit.encoded_ref != encode_ref(&source.refspec()) {
            return Ok(None);
        }
        let commit = read_local_head(git)?;
        return Ok(Some(Resolved {
            name: unit.name.clone(),
            encoded_ref: unit.encoded_ref.clone(),
            commit: commit.clone(),
            locked: LockedSource {
                name: unit.name.clone(),
                git: git.to_owned(),
                resolved: "link".to_owned(),
                commit,
                digest: "link:".to_owned(),
                config_digest: source.config_digest(),
                r#ref: None,
            },
        }));
    }

    let discriminator = ref_discriminator(&unit.effective_ref, &source.refspec());
    let commit = match lock_hit(config, source, unit, effective_lock, force) {
        Some(l) => l.commit.clone(),
        None => backend.resolve(&source_name, git, &unit.effective_ref)?,
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
        unit.effective_ref.to_string()
    };
    Ok(Some(Resolved {
        name: unit.name.clone(),
        encoded_ref: unit.encoded_ref.clone(),
        commit: commit.clone(),
        locked: LockedSource {
            name: unit.name.clone(),
            git: git.to_owned(),
            resolved,
            commit,
            digest,
            config_digest: source.config_digest(),
            r#ref: discriminator,
        },
    }))
}

pub(super) fn resolve_sources(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &(dyn SourceBackend + Sync),
    force: bool,
    jobs: Option<usize>,
) -> Result<RoutedSources> {
    let units = resolution_units(config, parsed);

    let threads = jobs.unwrap_or_else(|| units.len().min(8)).max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| crate::error::Error::Source(e.to_string()))?;

    pool.install(|| -> Result<RoutedSources> {
        fetch_distinct_mirrors(
            config,
            parsed,
            remotes,
            effective_lock,
            backend,
            force,
            &units,
        )?;

        let resolved: Vec<Option<Resolved>> = units
            .par_iter()
            .map(|unit| {
                resolve_unit(
                    config,
                    parsed,
                    remotes,
                    effective_lock,
                    backend,
                    force,
                    unit,
                )
            })
            .collect::<Result<Vec<_>>>()?;

        let mut routed = Vec::new();
        let mut resolved_commits = BTreeMap::new();
        for entry in resolved.into_iter().flatten() {
            resolved_commits.insert((entry.name.clone(), entry.encoded_ref), entry.commit);
            routed.push((entry.name, entry.locked));
        }
        Ok((routed, resolved_commits))
    })
}

#[cfg(feature = "bench")]
pub fn resolve_sources_for_bench(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &(dyn SourceBackend + Sync),
    force: bool,
    jobs: Option<usize>,
) -> Result<RoutedSources> {
    resolve_sources(
        config,
        parsed,
        remotes,
        effective_lock,
        backend,
        force,
        jobs,
    )
}
