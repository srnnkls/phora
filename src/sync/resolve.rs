use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;

use crate::config::{Config, DeployMode, ParsedSource, Refspec, SourceMode};
use crate::error::Result;
use crate::lock::{Lock, LockedSource, encode_ref, entry_matches, ref_discriminator};
use crate::projection::offer::OfferSelection;
use crate::source::{
    Commit, MirrorKey, NormalizedUrl, ResolvePolicy, ResolveRequest, ResolvedRevision,
    ResolvedSource, RevisionSpec, SnapshotId, SourceDirectoryEntry, SourceEntry, SourceError,
    SourceInventory, SourceLocation, SourceName, SourcePath, SourceStore, digest_snapshot,
};

use super::{effective_protocol, remote_for};

type SourceResult<T> = std::result::Result<T, SourceError>;

pub struct RoutedSources {
    pub locks: Vec<(String, LockedSource)>,
    pub commits: BTreeMap<(String, String), String>,
    pub resolved: ResolvedSourceMap,
}

pub type ResolvedSourceMap = BTreeMap<(String, String), ResolvedSource>;

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
    source: ResolvedSource,
}

/// Units sharing a mirror resolve serially; distinct mirrors resolve in parallel.
/// URL units remain distinct within a group because each source's integrity pin
/// must validate its own download.
fn resolution_groups<'a>(
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    units: &'a [Unit],
) -> Result<Vec<Vec<&'a Unit>>> {
    let mut groups: BTreeMap<String, Vec<&Unit>> = BTreeMap::new();
    for unit in units {
        if !parsed.contains_key(&unit.name) {
            continue;
        }
        let git = remote_for(remotes, &unit.name)?;
        let key = MirrorKey::from_url(&NormalizedUrl::parse(git))
            .as_str()
            .to_owned();
        groups.entry(key).or_default().push(unit);
    }
    Ok(groups.into_values().collect())
}

fn revision_spec(refspec: &Refspec) -> Result<RevisionSpec> {
    Ok(match refspec {
        Refspec::Branch(branch) => RevisionSpec::Branch(branch.clone()),
        Refspec::Tag(tag) => RevisionSpec::Tag(tag.clone()),
        Refspec::Rev(commit) => RevisionSpec::Commit(commit.parse::<Commit>()?),
        Refspec::Default => RevisionSpec::Default,
        Refspec::None => RevisionSpec::None,
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
    store: &dyn SourceStore,
    force: bool,
    frozen: bool,
    unit: &Unit,
    mirror_refreshed: &mut bool,
) -> Result<Option<Resolved>> {
    let Some(source) = parsed.get(&unit.name) else {
        return Ok(None);
    };
    let git = remote_for(remotes, &unit.name)?;

    let lock_entry = lock_hit(config, source, unit, effective_lock, force);
    if lock_entry.is_none() && frozen && source.deploy_mode() != DeployMode::Link {
        return Err(frozen_miss(&unit.name, instances.contains_key(&unit.name)));
    }

    let request = resolve_request(source, unit, git, lock_entry)?;
    let source_resolution = resolve_source(
        store,
        &request,
        source,
        lock_entry,
        frozen,
        mirror_refreshed,
    )?;

    if source.deploy_mode() == DeployMode::Link {
        if unit.encoded_ref != encode_ref(&source.refspec()) {
            return Ok(None);
        }
        let commit = match &source_resolution.revision {
            ResolvedRevision::WorktreeHead(Some(head)) => head.to_string(),
            ResolvedRevision::WorktreeHead(None) => "link".to_owned(),
            ResolvedRevision::Commit(_) => {
                return Err(crate::error::Error::Source(format!(
                    "link source {} resolved to a git snapshot",
                    unit.name
                )));
            }
        };
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
            source: source_resolution,
        }));
    }

    let discriminator = ref_discriminator(&unit.effective_ref, &source.refspec());
    let commit = match &source_resolution.revision {
        ResolvedRevision::Commit(commit) => commit.to_string(),
        ResolvedRevision::WorktreeHead(_) => {
            return Err(crate::error::Error::Source(format!(
                "copy source {} resolved to a worktree snapshot",
                unit.name
            )));
        }
    };

    let digest = match lock_entry {
        Some(locked) if locked.commit == commit => locked.digest.clone(),
        _ => selected_source_digest(store, &source_resolution, source)?,
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
        source: source_resolution,
    }))
}

fn resolve_request(
    source: &ParsedSource,
    unit: &Unit,
    git: &str,
    lock_entry: Option<&LockedSource>,
) -> Result<ResolveRequest> {
    let revision = match lock_entry {
        Some(locked) => RevisionSpec::Commit(locked.commit.parse::<Commit>()?),
        None if source.deploy_mode() == DeployMode::Link => RevisionSpec::Default,
        None => revision_spec(&unit.effective_ref)?,
    };
    let location = match source.deploy_mode() {
        DeployMode::Link => SourceLocation::Worktree { root: git.into() },
        DeployMode::Copy if source.mode() == SourceMode::Url => SourceLocation::Url {
            url: git.to_owned(),
        },
        DeployMode::Copy => SourceLocation::Git {
            url: git.to_owned(),
        },
    };
    Ok(ResolveRequest {
        name: SourceName::trusted(unit.name.clone()),
        location,
        revision,
    })
}

fn resolve_source(
    store: &dyn SourceStore,
    request: &ResolveRequest,
    source: &ParsedSource,
    lock_entry: Option<&LockedSource>,
    frozen: bool,
    mirror_refreshed: &mut bool,
) -> Result<ResolvedSource> {
    if source.deploy_mode() == DeployMode::Link {
        return store
            .resolve(request, ResolvePolicy::CachedOnly)
            .map_err(Into::into);
    }
    let resolved = if lock_entry.is_some() {
        match store.resolve(request, ResolvePolicy::CachedOnly) {
            Ok(resolved) => return Ok(resolved),
            Err(error) if frozen => return Err(error.into()),
            Err(_) => store.resolve(request, ResolvePolicy::Refresh)?,
        }
    } else if source.mode() == SourceMode::Url || !*mirror_refreshed {
        store.resolve(request, ResolvePolicy::Refresh)?
    } else {
        return store
            .resolve(request, ResolvePolicy::CachedOnly)
            .map_err(Into::into);
    };
    *mirror_refreshed = true;
    Ok(resolved)
}

fn selected_source_digest(
    store: &dyn SourceStore,
    resolved: &ResolvedSource,
    source: &ParsedSource,
) -> Result<String> {
    let inventory = store.inventory(&resolved.snapshot, None)?;
    let root = (source.mode() != SourceMode::Url)
        .then_some(source.root.as_deref())
        .flatten();
    let selection = OfferSelection::compile(
        if source.mode() == SourceMode::Url {
            &[]
        } else {
            source.includes()
        },
        if source.mode() == SourceMode::Url {
            &[]
        } else {
            source.excludes()
        },
        root,
    )?;
    let candidates: Vec<&str> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    let leaves = selection
        .select(&candidates)
        .into_iter()
        .map(|path| SourcePath::new(&path))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match root {
        Some(root) => {
            let root = SourcePath::new(&root.to_string_lossy().replace('\\', "/"))?;
            digest_snapshot(
                &RootedSnapshotStore { store, root },
                &resolved.snapshot,
                &leaves,
            )
            .map_err(Into::into)
        }
        None => digest_snapshot(store, &resolved.snapshot, &leaves).map_err(Into::into),
    }
}

struct RootedSnapshotStore<'a> {
    store: &'a dyn SourceStore,
    root: SourcePath,
}

impl RootedSnapshotStore<'_> {
    fn rooted(
        &self,
        path: &SourcePath,
    ) -> std::result::Result<SourcePath, crate::source::KernelError> {
        SourcePath::new(&format!("{}/{}", self.root.as_str(), path.as_str()))
    }
}

impl SourceStore for RootedSnapshotStore<'_> {
    fn resolve(
        &self,
        request: &ResolveRequest,
        policy: ResolvePolicy,
    ) -> SourceResult<ResolvedSource> {
        self.store.resolve(request, policy)
    }

    fn inventory(
        &self,
        snapshot: &SnapshotId,
        root: Option<&SourcePath>,
    ) -> SourceResult<SourceInventory> {
        let root = match root {
            Some(root) => self.rooted(root)?,
            None => self.root.clone(),
        };
        self.store.inventory(snapshot, Some(&root))
    }

    fn read(&self, snapshot: &SnapshotId, path: &SourcePath) -> SourceResult<SourceEntry> {
        let rooted = self.rooted(path)?;
        let mut entry = self.store.read(snapshot, &rooted)?;
        entry.meta.path = path.clone();
        Ok(entry)
    }

    fn list_directory(
        &self,
        snapshot: &SnapshotId,
        path: Option<&SourcePath>,
    ) -> SourceResult<Vec<SourceDirectoryEntry>> {
        let path = match path {
            Some(path) => self.rooted(path)?,
            None => self.root.clone(),
        };
        self.store.list_directory(snapshot, Some(&path))
    }
}

/// Default rayon pool size when `--jobs` is unset. The cap's floor of 50 is
/// measured, not derived from cores: fetch is network-wait-bound and parked
/// threads cost memory, not CPU, so many small fetches stall on the pool
/// ceiling long before the box is busy (`benches/fetch_sweep.sh`).
fn default_thread_count(units: usize, cores: usize) -> usize {
    units.min((2 * cores).max(50))
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
    store: &dyn SourceStore,
    force: bool,
    frozen: bool,
    jobs: Option<usize>,
) -> Result<RoutedSources> {
    let units = resolution_units(config, parsed);
    let groups = resolution_groups(parsed, remotes, &units)?;

    let cores = std::thread::available_parallelism().map_or(8, std::num::NonZero::get);
    let threads = jobs
        .unwrap_or_else(|| default_thread_count(units.len(), cores))
        .max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| crate::error::Error::Source(e.to_string()))?;

    pool.install(|| -> Result<RoutedSources> {
        let resolved: Vec<Option<Resolved>> = groups
            .into_par_iter()
            .map(|units| {
                let mut mirror_refreshed = false;
                units
                    .into_iter()
                    .map(|unit| {
                        resolve_unit(
                            config,
                            parsed,
                            remotes,
                            instances,
                            effective_lock,
                            store,
                            force,
                            frozen,
                            unit,
                            &mut mirror_refreshed,
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();

        let mut routed = Vec::new();
        let mut resolved_commits = BTreeMap::new();
        let mut resolved_sources = BTreeMap::new();
        for entry in resolved.into_iter().flatten() {
            resolved_commits.insert(
                (entry.name.clone(), entry.encoded_ref),
                entry.commit.clone(),
            );
            resolved_sources.insert((entry.name.clone(), entry.commit), entry.source);
            routed.push((entry.name, entry.locked));
        }
        Ok(RoutedSources {
            locks: routed,
            commits: resolved_commits,
            resolved: resolved_sources,
        })
    })
}

#[cfg(feature = "bench")]
pub fn resolve_sources_for_bench(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    remotes: &BTreeMap<String, String>,
    effective_lock: Option<&Lock>,
    store: &dyn SourceStore,
    force: bool,
    jobs: Option<usize>,
) -> Result<RoutedSources> {
    resolve_sources(
        config,
        parsed,
        remotes,
        &BTreeMap::new(),
        effective_lock,
        store,
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
    fn default_thread_count_uses_one_thread_per_unit_up_to_floor() {
        assert_eq!(default_thread_count(50, 8), 50);
    }

    #[test]
    fn default_thread_count_caps_at_floor_when_twice_cores_is_below_it() {
        assert_eq!(default_thread_count(60, 8), 50);
    }

    #[test]
    fn default_thread_count_caps_at_twice_cores_when_above_floor() {
        assert_eq!(default_thread_count(200, 32), 64);
    }

    #[test]
    fn default_thread_count_single_core_single_unit() {
        assert_eq!(default_thread_count(1, 1), 1);
    }

    #[test]
    fn default_thread_count_single_core_uses_floor_not_cores() {
        assert_eq!(default_thread_count(60, 1), 50);
    }

    #[test]
    fn default_thread_count_zero_units_returns_zero() {
        assert_eq!(default_thread_count(0, 8), 0);
    }
}
