use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;

use crate::config::{Config, DeployMode, ParsedSource, Refspec, SourceMode};
use crate::error::Result;
use crate::kernel::SourceName;
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
        let git = remote_for(remotes, &unit.name)?;
        if lock_hit(config, source, unit, effective_lock, force).is_some()
            && backend.mirror_ready(git)
        {
            continue;
        }
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

fn frozen_miss(name: &str, transitive: bool) -> crate::error::Error {
    let kind = if transitive {
        "transitive source"
    } else {
        "source"
    };
    crate::error::Error::Lock(format!(
        "{kind} `{name}` is not pinned in the lock; --frozen refuses to fetch or re-resolve"
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "resolving one unit threads config/parsed/remotes/lock/backend plus the force and frozen run flags"
)]
fn resolve_unit(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    instances: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &(dyn SourceBackend + Sync),
    force: bool,
    frozen: bool,
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
                instance: instances.get(&unit.name).cloned(),
            },
        }));
    }

    let discriminator = ref_discriminator(&unit.effective_ref, &source.refspec());
    let commit = match lock_hit(config, source, unit, effective_lock, force) {
        Some(l) => l.commit.clone(),
        None if frozen => {
            return Err(frozen_miss(&unit.name, instances.contains_key(&unit.name)));
        }
        None => backend.resolve(&source_name, git, &unit.effective_ref)?,
    };

    let digest = if source.mode() == SourceMode::Url {
        backend.compute_digest(&source_name, git, &commit, None, &[], &[])?
    } else {
        backend.compute_digest(
            &source_name,
            git,
            &commit,
            source.root.as_deref(),
            source.includes(),
            source.excludes(),
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
            instance: instances.get(&unit.name).cloned(),
        },
    }))
}

/// Default rayon pool size when `--jobs` is unset: one thread per unit, capped at
/// twice the core count so network-bound fetch overlaps I/O waits without unbounded
/// oversubscription of the CPU resolve/digest phase.
fn default_thread_count(units: usize, cores: usize) -> usize {
    units.min(2 * cores)
}

#[expect(
    clippy::too_many_arguments,
    reason = "resolution threads config/parsed/remotes/lock/backend plus the force, frozen, and jobs run flags"
)]
pub(super) fn resolve_sources(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    instances: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    backend: &(dyn SourceBackend + Sync),
    force: bool,
    frozen: bool,
    jobs: Option<usize>,
) -> Result<RoutedSources> {
    let units = resolution_units(config, parsed);

    let cores = std::thread::available_parallelism().map_or(8, std::num::NonZero::get);
    let threads = jobs
        .unwrap_or_else(|| default_thread_count(units.len(), cores))
        .max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| crate::error::Error::Source(e.to_string()))?;

    pool.install(|| -> Result<RoutedSources> {
        if !frozen {
            fetch_distinct_mirrors(
                config,
                parsed,
                remotes,
                effective_lock,
                backend,
                force,
                &units,
            )?;
        }

        let resolved: Vec<Option<Resolved>> = units
            .par_iter()
            .map(|unit| {
                resolve_unit(
                    config,
                    parsed,
                    remotes,
                    instances,
                    effective_lock,
                    backend,
                    force,
                    frozen,
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
        &BTreeMap::new(),
        effective_lock,
        backend,
        force,
        false,
        jobs,
    )
}

#[cfg(test)]
mod tests {
    use super::default_thread_count;

    #[test]
    fn default_thread_count_uses_one_thread_per_unit_below_cores() {
        assert_eq!(default_thread_count(5, 8), 5);
    }

    #[test]
    fn default_thread_count_uses_one_thread_per_unit_at_cores() {
        assert_eq!(default_thread_count(8, 8), 8);
    }

    #[test]
    fn default_thread_count_uses_one_thread_per_unit_between_cores_and_cap() {
        assert_eq!(default_thread_count(12, 8), 12);
    }

    #[test]
    fn default_thread_count_uses_one_thread_per_unit_at_twice_cores() {
        assert_eq!(default_thread_count(16, 8), 16);
    }

    #[test]
    fn default_thread_count_caps_at_twice_cores() {
        assert_eq!(default_thread_count(20, 8), 16);
    }

    #[test]
    fn default_thread_count_single_core_single_unit() {
        assert_eq!(default_thread_count(1, 1), 1);
    }

    #[test]
    fn default_thread_count_single_core_caps_at_two() {
        assert_eq!(default_thread_count(5, 1), 2);
    }

    #[test]
    fn default_thread_count_zero_units_returns_zero() {
        assert_eq!(default_thread_count(0, 8), 0);
    }
}
