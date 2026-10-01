//! Serial, cycle-guarded transitive pre-pass: before the parallel resolve pool,
//! walk every bound `transitive = true` source's own `phora.toml`, fetching and
//! parsing each dep manifest, and produce a namespaced composition graph. A failure
//! at any depth fails the sync fail-fast, before any lock write.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::transitive::{FetchNode, Instance, Member, TransitiveManifest};
use crate::config::{
    Binding, BuildTool, Config, DeployMode, HookAdmissionDiagnostic, HookCommand, Host,
    OfferBinding, ParsedSource, Protocol, Refspec, Remote, Source, SourceMode, TakeEntry, Target,
    ToolGrant, admit_transitive_hooks, hook_preimage,
};
use crate::error::{Error, Result};
use crate::projection::offer::OfferSelection;
use crate::source::{
    Commit, ResolvePolicy, ResolveRequest, RevisionSpec, SourceLocation, SourceName, SourceStore,
};

use super::resolve::ImportResolution;
use super::resolved_remotes;

/// Named-diagnostic phrase emitted when two composed dep targets land on one destination.
const COMPOSED_DEST_COLLISION: &str = "composed targets resolve to the same destination";

const TRANSITIVE_LINK_REJECTED: &str = "transitive source cannot use deploy = \"link\"";

/// The fetch-node commit of a package read live from its working tree.
const WORKTREE_COMMIT: &str = "link";

/// Where a package's manifest and self source (`path = "."`) are read from.
#[derive(Clone, Copy)]
enum PackageSnapshot<'a> {
    /// The package's committed snapshot in its git mirror at `remote`.
    Mirror(&'a str),
    /// The consumer's linked working tree at `root`, uncommitted files included.
    Worktree(&'a str),
}

impl<'a> PackageSnapshot<'a> {
    fn remote(self) -> &'a str {
        match self {
            Self::Mirror(remote) | Self::Worktree(remote) => remote,
        }
    }
}

/// Fail-closed bound: an acyclic ever-deeper `transitive = true` import chain would otherwise stack-overflow (`DoS`) on untrusted manifests.
const MAX_TRANSITIVE_DEPTH: usize = 64;

/// One dep target composed under a consumer anchor: a synthetic absolute-path
/// target carrying the dep's own layout, bound to namespaced source instances.
#[derive(Debug, Clone)]
pub(crate) struct ComposedTarget {
    pub(crate) name: String,
    pub(crate) target: Target,
    /// Consumer config target whose `offer_bindings` roots this composition.
    pub(crate) anchor: String,
    /// Consumer binding identity rooting this composition.
    pub(crate) import: String,
    pub(crate) member: Member,
}

/// An interpreted transitive `on_change` hook pinned to its dep's resolved commit, awaiting
/// the consumer trust decision in [`sync`](super::sync). Stripped from the deployed target.
#[derive(Debug, Clone)]
pub(super) struct TransitiveHookCandidate {
    pub(super) dep_instance: String,
    pub(super) hook_id: String,
    pub(super) command: HookCommand,
    pub(super) preimage: String,
    pub(super) target_path: PathBuf,
    /// Consumer-facing root import name this subtree was reached through.
    pub(super) source: String,
    /// The dep's resolved commit; recorded so `phora trust` can diff it against the last trusted commit.
    pub(super) commit: String,
}

impl From<&TransitiveHookCandidate> for crate::lock::CandidateHookRecord {
    fn from(c: &TransitiveHookCandidate) -> Self {
        Self {
            dep_instance: c.dep_instance.clone(),
            hook_id: c.hook_id.clone(),
            preimage: c.preimage.clone(),
            command: c.command.display(),
            source: c.source.clone(),
            commit: c.commit.clone(),
        }
    }
}

/// The transitive pre-pass output: composed targets plus the namespaced source
/// instances (and their resolved remotes) those targets bind.
#[derive(Debug, Default)]
pub(crate) struct ResolvedGraph {
    pub(crate) targets: Vec<ComposedTarget>,
    pub(super) sources: BTreeMap<String, ParsedSource>,
    pub(super) remotes: BTreeMap<String, String>,
    /// Namespaced source name → owning `Instance.stable_key()`; the lock stamps this so
    /// a transitive node is keyed by its instance, not a bare name that never lines up.
    pub(super) instances: BTreeMap<String, String>,
    pub(super) import_refs: Vec<ImportResolution>,
    pub(super) hook_candidates: Vec<TransitiveHookCandidate>,
    pub(super) hook_diagnostics: Vec<HookAdmissionDiagnostic>,
    /// The pinned dependency repos composed builds read, keyed by namespaced name.
    pub(super) build_inputs: BTreeMap<String, ParsedSource>,
}

impl ResolvedGraph {
    /// Dep-repo-relative files the named composed target binds, read offline at each binding's own locked commit; `Err` when the target or a commit is unknown.
    pub(crate) fn composed_files(
        &self,
        composed_target_name: &str,
        store: &dyn SourceStore,
        lock: &crate::lock::Lock,
    ) -> Result<Vec<String>> {
        let target = self
            .targets
            .iter()
            .find(|t| t.name == composed_target_name)
            .ok_or_else(|| {
                Error::Config(format!(
                    "no composed target `{composed_target_name}` in the offline graph"
                ))
            })?;
        let mut out = Vec::new();
        for binding in target.target.resolve_sources(&self.sources) {
            let namespaced = binding.source;
            let remote = self.remotes.get(namespaced).ok_or_else(|| {
                Error::Config(format!(
                    "no resolved remote for composed source `{namespaced}`"
                ))
            })?;
            let source = self.sources.get(namespaced).ok_or_else(|| {
                Error::Config(format!("no parsed composed source `{namespaced}`"))
            })?;
            let disc = crate::lock::ref_discriminator(&binding.effective_ref, &source.refspec());
            let entry = lock
                .find_entry(namespaced, disc.as_deref())
                .ok_or_else(|| {
                    Error::Lock(format!(
                        "composed source `{namespaced}` is not pinned in the lock"
                    ))
                })?;
            let name = SourceName::trusted(namespaced.to_owned());
            let revision = RevisionSpec::Commit(entry.commit.parse::<Commit>()?);
            let location = match source.mode() {
                SourceMode::Git | SourceMode::Host => SourceLocation::Git {
                    url: remote.clone(),
                },
                SourceMode::Url => SourceLocation::Url {
                    url: remote.clone(),
                },
                SourceMode::Build => SourceLocation::Build {
                    output: None,
                    follow_symlinks: false,
                },
            };
            let resolved = store.resolve(
                &ResolveRequest {
                    name,
                    location,
                    revision,
                },
                ResolvePolicy::CachedOnly,
            )?;
            let leaves: Vec<String> = store
                .inventory(&resolved.snapshot, None)?
                .entries
                .into_iter()
                .map(|entry| entry.path.to_string())
                .collect();
            let offer = source.offer();
            let selection =
                OfferSelection::compile(offer.includes(), offer.excludes(), offer.root())?;
            let refs: Vec<&str> = leaves.iter().map(String::as_str).collect();
            for published in selection.select(&refs) {
                out.push(dep_relative_path(binding.root, &published));
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    pub(super) fn inject(
        self,
        config: &mut Config,
        parsed: &mut BTreeMap<String, ParsedSource>,
        remotes: &mut BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        parsed.extend(self.sources);
        remotes.extend(self.remotes);
        for composed in self.targets {
            config.targets.insert(composed.name, composed.target);
        }
        strip_absorbed_anchors(config);
        self.instances
    }
}

fn dep_relative_path(root: Option<&Path>, leaf: &str) -> String {
    match root {
        Some(r) => format!("{}/{leaf}", r.display()),
        None => leaf.to_owned(),
    }
}

/// Once composition absorbs its `offer_bindings`, a bindingless anchor would deploy as a live empty target.
fn strip_absorbed_anchors(config: &mut Config) {
    config.targets.retain(|_, target| {
        if target.offer_bindings.is_none() {
            return true;
        }
        target.offer_bindings = None;
        target.sources.as_ref().is_some_and(|s| !s.is_empty())
    });
}

/// Walks the transitive graph rooted at the consumer's bound `transitive = true`
/// sources, producing the namespaced composition graph. A failure naming a source
/// below the top level carries `at depth N`.
pub(super) fn resolve_transitive_graph(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    backend: &dyn SourceStore,
    frozen: bool,
    effective_lock: Option<&crate::lock::Lock>,
) -> Result<ResolvedGraph> {
    debug_assert!(
        config.targets.values().all(|target| target
            .declared_sources()
            .all(|name| !config.sources.get(name).is_some_and(Source::is_transitive))),
        "transitive bindings must be lowered into offer bindings before composition"
    );
    let has_offer_bindings = config
        .targets
        .values()
        .any(|t| t.offer_bindings.iter().flatten().next().is_some());
    if !has_offer_bindings {
        return Ok(ResolvedGraph::default());
    }
    let remotes = resolved_remotes(config, parsed)?;
    let frozen_gate = FrozenGate {
        frozen,
        lock: effective_lock,
    };
    let mut visited: HashSet<FetchNode> = HashSet::new();
    let mut graph = ResolvedGraph::default();

    for (anchor_name, anchor) in &config.targets {
        for import in anchor.offer_bindings.iter().flatten() {
            let imported = import.source();
            let (manifest, snapshot, instance) = acquire_root_import(
                &RootImport {
                    config,
                    parsed,
                    remotes: &remotes,
                    backend,
                    frozen,
                    effective_lock,
                },
                import,
                anchor_name,
                &mut graph,
            )?;
            let layout = OfferLayout::new(&manifest, import.offer())
                .map_err(|e| Error::Config(format!("source `{imported}`: {e}")))?;
            let instance = instance.placed_at(&layout.root);
            visited.insert(instance.fetch_node().clone());
            compose_dep(
                &instance,
                &anchor.expanded_path(),
                imported,
                &layout,
                snapshot,
                &MountOverride::of(import),
                &mut WalkCtx {
                    backend,
                    visited: &mut visited,
                    ancestors: Vec::new(),
                    graph: &mut graph,
                    frozen: &frozen_gate,
                    consumer_hosts: &config.hosts,
                    consumer_tools: &config.tools,
                    default_protocol: config.protocol,
                    root_source: imported,
                    root_identity: &import.identity,
                    root_offer: import.offer(),
                    root_anchor: anchor_name,
                },
                1,
            )?;
        }
    }

    Ok(graph)
}

struct RootImport<'a> {
    config: &'a Config,
    parsed: &'a BTreeMap<String, ParsedSource>,
    remotes: &'a BTreeMap<String, String>,
    backend: &'a dyn SourceStore,
    frozen: bool,
    effective_lock: Option<&'a crate::lock::Lock>,
}

/// Pins a consumer-bound transitive source and reads its manifest at that pin.
fn acquire_root_import<'a>(
    root: &RootImport<'a>,
    import: &OfferBinding,
    anchor_name: &str,
    graph: &mut ResolvedGraph,
) -> Result<(TransitiveManifest, PackageSnapshot<'a>, Instance)> {
    let imported = import.source();
    let base = root
        .parsed
        .get(imported)
        .ok_or_else(|| Error::Config(format!("no resolved source for `{imported}`")))?;
    let source = import.resolve(base)?;
    let remote = root
        .remotes
        .get(imported)
        .map(String::as_str)
        .ok_or_else(|| Error::Config(format!("no resolved remote for source `{imported}`")))?;
    crate::source::transitive::validate_dependency_remote(imported, &source, remote, 1)?;
    let (snapshot, commit, manifest) = if source.deploy_mode() == DeployMode::Link {
        let manifest = read_worktree_manifest(imported, remote)?;
        (
            PackageSnapshot::Worktree(remote),
            WORKTREE_COMMIT.to_owned(),
            manifest,
        )
    } else {
        let pin = locked_import_commit(root.config, imported, base, &source, root.effective_lock);
        let (commit, manifest) =
            acquire_import_manifest(imported, &source, remote, root.backend, root.frozen, pin)?;
        (PackageSnapshot::Mirror(remote), commit, manifest)
    };
    graph.import_refs.push(ImportResolution {
        source: imported.to_owned(),
        refspec: source.refspec(),
        commit: matches!(snapshot, PackageSnapshot::Mirror(_)).then(|| commit.clone()),
    });
    let instance = import_instance(
        "root",
        &import.identity,
        anchor_name,
        remote,
        &source.refspec(),
        &commit,
    );
    Ok((manifest, snapshot, instance))
}

fn locked_import_commit<'l>(
    config: &Config,
    imported: &str,
    base: &ParsedSource,
    source: &ParsedSource,
    effective_lock: Option<&'l crate::lock::Lock>,
) -> Option<&'l str> {
    let discriminator = crate::lock::ref_discriminator(&source.refspec(), &base.refspec());
    effective_lock
        .and_then(|lock| lock.find_entry(imported, discriminator.as_deref()))
        .filter(|entry| {
            crate::lock::entry_matches(
                base,
                &source.refspec(),
                entry,
                &config.hosts,
                super::effective_protocol(base, config),
            )
        })
        .map(|entry| entry.commit.as_str())
}

/// A linked package is read live; only a local working tree can be linked.
fn read_worktree_manifest(imported: &str, root: &str) -> Result<TransitiveManifest> {
    if !crate::source::is_local_path(root) {
        return Err(Error::Config(format!(
            "source `{imported}`: deploy = \"link\" requires a local filesystem path, \
             not a remote URL `{root}`"
        )));
    }
    crate::source::transitive::read_worktree_manifest(root).map_err(|source| {
        Error::TransitiveSource {
            name: imported.to_owned(),
            depth: 1,
            source,
        }
    })
}

/// A manifest and the files its offers publish share one pin.
fn acquire_import_manifest(
    imported: &str,
    source: &ParsedSource,
    remote: &str,
    backend: &dyn SourceStore,
    frozen: bool,
    pin: Option<&str>,
) -> Result<(String, TransitiveManifest)> {
    if frozen && pin.is_none() {
        return Err(frozen_transitive_miss(imported, 1));
    }
    crate::source::transitive::acquire_dependency_manifest(
        backend,
        &SourceName::trusted(imported.to_owned()),
        source,
        remote,
        pin,
        if frozen {
            ResolvePolicy::CachedOnly
        } else {
            ResolvePolicy::Refresh
        },
    )
    .map_err(|source| Error::TransitiveSource {
        name: imported.to_owned(),
        depth: 1,
        source,
    })
}

/// [`resolve_transitive_graph`] under the frozen gate: lock-pinned commits, mirror-only reads, no fetch.
pub(crate) fn resolve_transitive_graph_offline(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    backend: &dyn SourceStore,
    lock: &crate::lock::Lock,
) -> Result<ResolvedGraph> {
    resolve_transitive_graph(config, parsed, backend, true, Some(lock))
}

struct WalkCtx<'a> {
    backend: &'a dyn SourceStore,
    /// `visited`: fetch-closure dedup gating `descend_for_validation` (LOCK-001); per-Instance nested composition intentionally ignores it. `ancestors`: current-path cycle guard.
    visited: &'a mut HashSet<FetchNode>,
    ancestors: Vec<FetchNode>,
    graph: &'a mut ResolvedGraph,
    frozen: &'a FrozenGate<'a>,
    consumer_hosts: &'a BTreeMap<String, Host>,
    consumer_tools: &'a BTreeMap<String, ToolGrant>,
    default_protocol: Option<Protocol>,
    /// Consumer source rooting this subtree; stamped on every hook candidate it yields.
    root_source: &'a str,
    /// Consumer binding identity rooting this subtree; labels its composed targets.
    root_identity: &'a str,
    root_anchor: &'a str,
    root_offer: &'a str,
}

/// Checked at the top of every `fetch_manifest` so no depth can fetch an unpinned/drifted node under `--frozen`.
struct FrozenGate<'a> {
    frozen: bool,
    lock: Option<&'a crate::lock::Lock>,
}

/// One offer of a manifest as composition works over it.
struct OfferLayout<'m> {
    manifest: &'m TransitiveManifest,
    root: PathBuf,
    sources: &'m BTreeMap<String, Source>,
    files: Option<Source>,
    targets: BTreeMap<String, Target>,
    hooks: Option<&'m toml::Value>,
}

impl<'m> OfferLayout<'m> {
    fn new(manifest: &'m TransitiveManifest, name: &str) -> Result<Self> {
        let offer = manifest.offer(name)?;
        Ok(Self {
            manifest,
            root: offer.root,
            sources: &manifest.sources,
            files: offer.files,
            targets: offer.targets,
            hooks: manifest.hooks(),
        })
    }

    /// Sources the offer's targets bind or compose; nothing else is namespaced or fetched.
    fn bound_sources(&self) -> HashSet<&str> {
        self.targets
            .values()
            .flat_map(|target| {
                target.declared_sources().chain(
                    target
                        .offer_bindings
                        .iter()
                        .flatten()
                        .map(OfferBinding::source),
                )
            })
            .collect()
    }
}

/// Composes an offer's targets under `anchor_path`: each becomes a synthetic target at
/// `anchor_path / target.path`, keeping its own layout, bound to source instances
/// namespaced by the dep [`Instance`]. Two targets sharing a destination is a hard error.
#[expect(
    clippy::too_many_arguments,
    reason = "composition threads instance, anchor, layout, snapshot, override, ctx, and depth together"
)]
fn compose_dep(
    instance: &Instance,
    anchor_path: &Path,
    imported: &str,
    layout: &OfferLayout<'_>,
    package: PackageSnapshot<'_>,
    mount: &MountOverride<'_>,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<()> {
    reject_depth_overflow(imported, depth)?;
    let source_names = namespace_dep_sources(instance, imported, layout, package, ctx, depth)?;
    let target_paths: Vec<PathBuf> = layout
        .targets
        .values()
        .map(|target| normalized(&target.path))
        .collect();
    if let Some(files) = &layout.files {
        let namespaced = namespace_own_files(instance, imported, files, package, ctx)?;
        let take = mount
            .take
            .map(|entries| route_take(imported, entries, Path::new("."), &target_paths))
            .transpose()?;
        ctx.graph.targets.push(ComposedTarget {
            name: instance.key(&Member::Files),
            target: own_files_target(ctx.root_identity, namespaced, anchor_path, take, mount),
            anchor: ctx.root_anchor.to_owned(),
            import: ctx.root_identity.to_owned(),
            member: Member::Files,
        });
    }
    let mut composed_dests: BTreeMap<PathBuf, String> = BTreeMap::new();

    for (dep_target_name, dep_target) in &layout.targets {
        let composed_path = normalized(&anchor_path.join(&dep_target.path));
        if let Some(other) = composed_dests.insert(composed_path.clone(), dep_target_name.clone()) {
            return Err(Error::Config(format!(
                "{COMPOSED_DEST_COLLISION}: dep targets `{other}` and `{dep_target_name}` of \
                 source `{imported}` both compose to {}",
                composed_path.display()
            )));
        }
        compose_nested_offers(
            instance,
            imported,
            dep_target_name,
            dep_target,
            &composed_path,
            layout,
            ctx,
            depth,
        )?;
        let take = mount
            .take
            .map(|entries| route_take(imported, entries, &dep_target.path, &target_paths))
            .transpose()?;
        let synthetic = synthetic_target(
            imported,
            dep_target_name,
            dep_target,
            composed_path.clone(),
            anchor_path.to_path_buf(),
            &source_names,
            take.as_deref(),
            mount.collapse,
        )?;
        let member = Member::Named(dep_target_name.clone());
        let composed_name = instance.key(&member);
        admit_hook_candidates(
            instance,
            layout,
            dep_target_name,
            &composed_name,
            &composed_path,
            ctx,
        );
        ctx.graph.targets.push(ComposedTarget {
            name: composed_name,
            target: synthetic,
            anchor: ctx.root_anchor.to_owned(),
            import: ctx.root_identity.to_owned(),
            member,
        });
    }
    Ok(())
}

fn own_files_target(
    identity: &str,
    namespaced: String,
    anchor_path: &Path,
    take: Option<Vec<TakeEntry>>,
    mount: &MountOverride<'_>,
) -> Target {
    let binding = Binding {
        source: Some(namespaced),
        take,
        collapse: mount.collapse.or(Some(false)),
        ..Binding::default()
    };
    Target {
        path: anchor_path.to_path_buf(),
        sources: Some(BTreeMap::from([(identity.to_owned(), binding)])),
        layout: None,
        hooks: None,
        offer_bindings: None,
        confine: Some(anchor_path.to_path_buf()),
    }
}

fn namespace_dep_build(
    instance: &Instance,
    imported: &str,
    name: &str,
    source: &Source,
    manifest: &TransitiveManifest,
    package: PackageSnapshot<'_>,
    ctx: &mut WalkCtx<'_>,
) -> Result<String> {
    let fail = |e: Error| Error::Config(format!("source `{imported}`: source `{name}`: {e}"));
    let repo = instance.key(&Member::Repo);
    let spec = source
        .build
        .as_ref()
        .map(|build| build.reading_repo_as(&repo));
    if let Some(BuildTool::Tool { spec: tool, .. }) = spec.as_ref().map(|spec| &spec.tool)
        && !ctx.consumer_tools.contains_key(&tool.identity)
    {
        let defined = crate::config::link::file_link(
            ctx.consumer_hosts,
            package.remote(),
            instance.fetch_node().commit(),
            "phora.toml",
            manifest.build_line(name),
        );
        return Err(Error::Config(format!(
            "target `{}`: offer `{}` of `{}` builds with tool `{tool}`; {}\n  defined: {defined}",
            ctx.root_anchor,
            ctx.root_offer,
            ctx.root_identity,
            crate::config::tools::grant_hint(&tool.identity)
        )));
    }
    if !ctx.graph.build_inputs.contains_key(&repo) {
        let snapshot = match package {
            PackageSnapshot::Mirror(remote) => format!(
                "git = {remote:?}\nrev = {:?}\ntransitive = true\n",
                instance.fetch_node().commit()
            ),
            PackageSnapshot::Worktree(root) => {
                format!("path = {root:?}\ndeploy = \"link\"\ntransitive = true\n")
            }
        };
        let snapshot: Source =
            toml::from_str(&snapshot).map_err(|e| fail(Error::Config(e.to_string())))?;
        ctx.graph
            .build_inputs
            .insert(repo.clone(), ParsedSource::parse(&repo, &snapshot)?);
    }
    let mut parsed = ParsedSource::parse(name, source).map_err(fail)?;
    if let Some(spec) = spec {
        parsed.remote = Remote::Build(spec);
    }
    let namespaced = instance.key(&Member::Named(name.to_owned()));
    ctx.graph
        .remotes
        .insert(namespaced.clone(), crate::source::BUILD_MIRROR.to_owned());
    ctx.graph.sources.insert(namespaced.clone(), parsed);
    ctx.graph
        .instances
        .insert(namespaced.clone(), instance.stable_key());
    Ok(namespaced)
}

fn namespace_own_files(
    instance: &Instance,
    imported: &str,
    files: &Source,
    package: PackageSnapshot<'_>,
    ctx: &mut WalkCtx<'_>,
) -> Result<String> {
    let mut parsed = ParsedSource::parse(imported, files)?;
    match package {
        PackageSnapshot::Mirror(remote) => {
            parsed.remote = Remote::Git(remote.to_owned());
            parsed.rev = Some(instance.fetch_node().commit().to_owned());
        }
        PackageSnapshot::Worktree(root) => parsed.link_worktree(root),
    }
    let namespaced = instance.key(&Member::Files);
    ctx.graph
        .remotes
        .insert(namespaced.clone(), package.remote().to_owned());
    ctx.graph.sources.insert(namespaced.clone(), parsed);
    ctx.graph
        .instances
        .insert(namespaced.clone(), instance.stable_key());
    Ok(namespaced)
}

/// A source already composed by a nested import is graphed (so the frozen gate can pin it) but
/// omitted from the returned bind map: it only re-exports its children, binding no target itself.
fn namespace_dep_sources(
    instance: &Instance,
    imported: &str,
    layout: &OfferLayout<'_>,
    package: PackageSnapshot<'_>,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<BTreeMap<String, String>> {
    let needed = layout.bound_sources();
    let nested: Vec<&OfferBinding> = layout
        .targets
        .values()
        .flat_map(|t| t.offer_bindings.iter().flatten())
        .collect();
    let mut source_names: BTreeMap<String, String> = BTreeMap::new();
    for (inner_name, inner) in layout.sources {
        if !needed.contains(inner_name.as_str()) {
            continue;
        }
        if inner.build.is_some() {
            let namespaced = namespace_dep_build(
                instance,
                imported,
                inner_name,
                inner,
                layout.manifest,
                package,
                ctx,
            )?;
            source_names.insert(inner_name.clone(), namespaced);
            continue;
        }
        let parsed = ParsedSource::parse(inner_name, inner).map_err(|e| {
            Error::Config(format!("source `{imported}`: source `{inner_name}`: {e}"))
        })?;
        if parsed.deploy_mode() == DeployMode::Link {
            return Err(Error::Config(format!(
                "source `{imported}`: source `{inner_name}`: {TRANSITIVE_LINK_REJECTED}"
            )));
        }
        let remote = inner_remote(
            inner_name,
            &parsed,
            ctx.consumer_hosts,
            ctx.default_protocol,
        )?;
        crate::source::transitive::validate_dependency_remote(
            inner_name,
            &parsed,
            &remote,
            depth + 1,
        )?;
        let composed_by_nested_import = nested.iter().any(|i| i.source() == inner_name);
        if inner.is_transitive() && !composed_by_nested_import && !ctx.frozen.frozen {
            descend_for_validation(inner_name, &parsed, &remote, ctx, depth + 1)?;
        }
        let namespaced = instance.key(&Member::Named(inner_name.clone()));
        for import in nested.iter().filter(|i| i.source() == inner_name) {
            ctx.graph.import_refs.push(ImportResolution {
                source: namespaced.clone(),
                refspec: import.resolve(&parsed)?.refspec(),
                commit: None,
            });
        }
        ctx.graph.remotes.insert(namespaced.clone(), remote);
        ctx.graph.sources.insert(namespaced.clone(), parsed);
        ctx.graph
            .instances
            .insert(namespaced.clone(), instance.stable_key());
        if !composed_by_nested_import {
            source_names.insert(inner_name.clone(), namespaced);
        }
    }
    Ok(source_names)
}

#[expect(
    clippy::too_many_arguments,
    reason = "nested composition threads parent instance, anchor path, layout, ctx, and depth together"
)]
fn compose_nested_offers(
    parent_instance: &Instance,
    imported: &str,
    dep_target_name: &str,
    dep_target: &Target,
    composed_path: &Path,
    layout: &OfferLayout<'_>,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<()> {
    for import in dep_target.offer_bindings.iter().flatten() {
        let inner_name = import.source();
        let inner = layout.sources.get(inner_name).ok_or_else(|| {
            Error::Config(format!(
                "source `{imported}`: target `{dep_target_name}` binds undefined source `{inner_name}`"
            ))
        })?;
        let inner_parsed = import
            .resolve(&ParsedSource::parse(inner_name, inner)?)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        let inner_remote = inner_remote(
            inner_name,
            &inner_parsed,
            ctx.consumer_hosts,
            ctx.default_protocol,
        )
        .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        crate::source::transitive::validate_dependency_remote(
            inner_name,
            &inner_parsed,
            &inner_remote,
            depth + 1,
        )?;
        let (inner_commit, inner_manifest) = fetch_manifest(
            inner_name,
            &inner_parsed,
            &inner_remote,
            ctx.backend,
            depth + 1,
            ctx.frozen,
        )?;
        let inner_layout = OfferLayout::new(&inner_manifest, import.offer())
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        let inner_instance = import_instance(
            &parent_instance.stable_key(),
            &import.identity,
            dep_target_name,
            &inner_remote,
            &inner_parsed.refspec(),
            &inner_commit,
        )
        .placed_at(&inner_layout.root);
        let inner_node = inner_instance.fetch_node().clone();
        ctx.visited.insert(inner_node.clone());
        if ctx.ancestors.contains(&inner_node) {
            continue;
        }
        ctx.ancestors.push(inner_node);
        let composed = compose_dep(
            &inner_instance,
            composed_path,
            inner_name,
            &inner_layout,
            PackageSnapshot::Mirror(&inner_remote),
            &MountOverride::of(import),
            ctx,
            depth + 1,
        );
        ctx.ancestors.pop();
        composed?;
    }
    Ok(())
}

fn descend_for_validation(
    name: &str,
    parsed: &ParsedSource,
    remote: &str,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<()> {
    reject_depth_overflow(name, depth)?;
    let (commit, manifest) = fetch_manifest(name, parsed, remote, ctx.backend, depth, ctx.frozen)?;
    let node = FetchNode::new(remote, &parsed.refspec().to_string(), &commit);
    if !ctx.visited.insert(node) {
        return Ok(());
    }
    for (inner_name, inner) in &manifest.sources {
        let inner_parsed = ParsedSource::parse(inner_name, inner)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        let inner_remote = inner_remote(
            inner_name,
            &inner_parsed,
            ctx.consumer_hosts,
            ctx.default_protocol,
        )
        .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        crate::source::transitive::validate_dependency_remote(
            inner_name,
            &inner_parsed,
            &inner_remote,
            depth + 1,
        )?;
        if !inner.is_transitive() {
            continue;
        }
        descend_for_validation(inner_name, &inner_parsed, &inner_remote, ctx, depth + 1)?;
    }
    Ok(())
}

impl FrozenGate<'_> {
    fn require_pinned<'l>(
        &'l self,
        name: &str,
        remote: &str,
        refspec: &Refspec,
        depth: usize,
    ) -> Result<Option<&'l str>> {
        if !self.frozen {
            return Ok(None);
        }
        let remote_id = crate::source::NormalizedUrl::parse(remote);
        let resolved_ref = refspec.to_string();
        let nested_suffix = format!("%{name}");
        let entry = self.lock.and_then(|lock| {
            lock.sources.iter().find(|s| {
                let identity_ok = crate::source::NormalizedUrl::parse(&s.git) == remote_id
                    && s.resolved == resolved_ref;
                let scope_ok = if depth > 1 {
                    s.instance.is_some() && s.name.ends_with(&nested_suffix)
                } else {
                    s.instance.is_none() && s.name == name
                };
                identity_ok && scope_ok
            })
        });
        match entry {
            Some(locked) => Ok(Some(locked.commit.as_str())),
            None => Err(frozen_transitive_miss(name, depth)),
        }
    }
}

fn frozen_transitive_miss(name: &str, depth: usize) -> Error {
    Error::Lock(format!(
        "transitive source `{name}` at depth {depth} is not pinned in the lock; \
         --frozen refuses to fetch its manifest"
    ))
}

fn import_instance(
    parent: &str,
    identity: &str,
    anchor: &str,
    remote: &str,
    refspec: &Refspec,
    commit: &str,
) -> Instance {
    let node = FetchNode::new(remote, &crate::lock::encode_ref(refspec), commit);
    Instance::new(parent, identity, anchor, node)
}

/// Interprets the dep target's stripped `on_change` hooks into commit-pinned candidates the
/// trust decision in [`sync`](super::sync) consumes, recording any parse-failure diagnostic.
fn admit_hook_candidates(
    instance: &Instance,
    layout: &OfferLayout<'_>,
    dep_target_name: &str,
    composed_name: &str,
    composed_path: &Path,
    ctx: &mut WalkCtx<'_>,
) {
    let Some(opaque) = layout.hooks else {
        return;
    };
    let (candidates, diagnostics) =
        admit_transitive_hooks(opaque, dep_target_name, composed_name, instance);
    ctx.graph.hook_diagnostics.extend(diagnostics);
    let commit = instance.fetch_node().commit();
    for candidate in candidates {
        ctx.graph.hook_candidates.push(TransitiveHookCandidate {
            preimage: hook_preimage(&candidate.command, "on_change", commit),
            dep_instance: candidate.dep_instance,
            hook_id: candidate.hook_id,
            command: candidate.command,
            target_path: composed_path.to_path_buf(),
            source: ctx.root_source.to_owned(),
            commit: commit.to_owned(),
        });
    }
}

fn normalized(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

/// A consumer binding's `take`/`collapse` over one composed offer.
struct MountOverride<'a> {
    take: Option<&'a [TakeEntry]>,
    collapse: Option<bool>,
}

impl<'a> MountOverride<'a> {
    fn of(import: &'a OfferBinding) -> Self {
        Self {
            take: import.binding.take.as_deref(),
            collapse: import.binding.collapse,
        }
    }
}

/// Routes a `take` written against the offer's output to one composed target: entries
/// under the target's path lose that prefix, `**/` globs reach every target, and the
/// own-files target (at `.`) gets entries under no other target's path.
fn route_take(
    imported: &str,
    entries: &[TakeEntry],
    target_path: &Path,
    other_paths: &[PathBuf],
) -> Result<Vec<TakeEntry>> {
    let target_path = normalized(target_path);
    let local = |path: &str| -> Option<String> {
        if path.starts_with("**/") {
            return Some(path.to_owned());
        }
        let path_buf = normalized(Path::new(path));
        let subtree = path
            .strip_suffix("/**")
            .or_else(|| path.strip_suffix('/'))
            .map(|dir| normalized(Path::new(dir)));
        if let Some(dir) = &subtree
            && !target_path.as_os_str().is_empty()
            && target_path.starts_with(dir)
            && &target_path != dir
        {
            return Some("**".to_owned());
        }
        if target_path.as_os_str().is_empty() {
            let claimed = other_paths
                .iter()
                .any(|other| !other.as_os_str().is_empty() && path_buf.starts_with(other));
            return (!claimed).then(|| path.to_owned());
        }
        let rest = path_buf.strip_prefix(&target_path).ok()?;
        let mut rest = rest.to_string_lossy().into_owned();
        if path.ends_with('/') && !rest.is_empty() {
            rest.push('/');
        }
        Some(if rest.is_empty() {
            "**".to_owned()
        } else {
            rest
        })
    };
    let mut routed = Vec::new();
    for entry in entries {
        match entry {
            TakeEntry::Leaf(leaf) => routed.extend(local(leaf).map(TakeEntry::Leaf)),
            TakeEntry::Rename { src, dest } => {
                let Some(src_local) = local(src) else {
                    continue;
                };
                let dest_local = local(dest).ok_or_else(|| {
                    Error::Config(format!(
                        "source `{imported}`: take rename `{src}` -> `{dest}` leaves the composed target at `{}`",
                        target_path.display()
                    ))
                })?;
                routed.push(TakeEntry::Rename {
                    src: src_local,
                    dest: dest_local,
                });
            }
        }
    }
    Ok(routed)
}

#[expect(
    clippy::too_many_arguments,
    reason = "a synthetic target combines the dep target, its placement, namespacing, and the consumer override"
)]
fn synthetic_target(
    imported: &str,
    dep_target_name: &str,
    dep_target: &Target,
    composed_path: PathBuf,
    anchor_path: PathBuf,
    source_names: &BTreeMap<String, String>,
    take: Option<&[TakeEntry]>,
    collapse: Option<bool>,
) -> Result<Target> {
    let mut target = dep_target.clone();
    target.path = composed_path;
    target.offer_bindings = None;
    target.hooks = None;
    target.confine = Some(anchor_path);
    if let Some(bindings) = target.sources.as_mut() {
        for (identity, binding) in bindings.iter_mut() {
            if binding.history {
                return Err(Error::Config(format!(
                    "source `{imported}`: target `{dep_target_name}` binding `{identity}` cannot set history"
                )));
            }
            let effective = binding.source.clone().unwrap_or_else(|| identity.clone());
            let namespaced = source_names.get(&effective).ok_or_else(|| {
                Error::Config(format!(
                    "source `{imported}`: target `{dep_target_name}` binds undefined source `{effective}`"
                ))
            })?;
            binding.source = Some(namespaced.clone());
            if let Some(take) = take {
                binding.composed_take = Some(take.to_vec());
            }
            binding.collapse = collapse.or(binding.collapse).or(Some(false));
        }
    }
    Ok(target)
}

fn fetch_manifest(
    name: &str,
    source: &ParsedSource,
    remote: &str,
    backend: &dyn SourceStore,
    depth: usize,
    frozen: &FrozenGate<'_>,
) -> Result<(String, TransitiveManifest)> {
    let refspec = source.refspec();
    let pinned = frozen.require_pinned(name, remote, &refspec, depth)?;
    let source_name = SourceName::trusted(name.to_owned());
    crate::source::transitive::acquire_dependency_manifest(
        backend,
        &source_name,
        source,
        remote,
        pinned,
        if frozen.frozen {
            ResolvePolicy::CachedOnly
        } else {
            ResolvePolicy::Refresh
        },
    )
    .map_err(|source| Error::TransitiveSource {
        name: name.to_owned(),
        depth,
        source,
    })
}

/// Trust surface: a dep's [`Remote::Host`] source resolves against the CONSUMER's host
/// registry, never the dep's own `[hosts]` — so a dep cannot redirect a clone.
fn inner_remote(
    name: &str,
    source: &ParsedSource,
    hosts: &BTreeMap<String, Host>,
    default_protocol: Option<Protocol>,
) -> Result<String> {
    if source.mode() == SourceMode::Url {
        return source
            .source_url()
            .map(str::to_owned)
            .ok_or_else(|| Error::Config(format!("source `{name}`: missing url")));
    }
    source.resolved_remote(
        hosts,
        source
            .protocol()
            .or(default_protocol)
            .unwrap_or(Protocol::Https),
    )
}

#[cfg(test)]
fn reject_escaping_remote(
    name: &str,
    source: &ParsedSource,
    remote: &str,
    depth: usize,
) -> Result<()> {
    crate::source::transitive::validate_dependency_remote(name, source, remote, depth)?;
    Ok(())
}

fn reject_depth_overflow(name: &str, depth: usize) -> Result<()> {
    if depth > MAX_TRANSITIVE_DEPTH {
        return Err(at_depth(
            name,
            depth,
            &format!("transitive source chain exceeds the maximum depth of {MAX_TRANSITIVE_DEPTH}"),
        ));
    }
    Ok(())
}

fn at_depth(name: &str, depth: usize, detail: &str) -> Error {
    Error::Config(format!(
        "transitive source `{name}` at depth {depth}: {detail}"
    ))
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use super::*;
    use crate::config::Source;
    use crate::config::{Host, Protocol, transitive::TransitiveManifest};
    use crate::lock::LockedSource;
    use crate::source::mirror_path;

    fn git_source(git: &str) -> ParsedSource {
        let raw: Source = toml::from_str(&format!("git = {git:?}\ntransitive = true\n"))
            .expect("git source DTO parses");
        ParsedSource::parse("dep", &raw).expect("git source parses")
    }

    fn error_chain_has<T>(error: &(dyn StdError + 'static)) -> bool
    where
        T: StdError + 'static,
    {
        let mut current = Some(error);
        while let Some(item) = current {
            if item.is::<T>() {
                return true;
            }
            current = item.source();
        }
        false
    }

    fn error_chain_has_source_variant<F>(error: &(dyn StdError + 'static), predicate: F) -> bool
    where
        F: Fn(&crate::source::SourceError) -> bool,
    {
        let mut current = Some(error);
        while let Some(item) = current {
            if item
                .downcast_ref::<crate::source::SourceError>()
                .is_some_and(&predicate)
            {
                return true;
            }
            current = item.source();
        }
        false
    }

    fn host_source(host: &str, repo: &str, protocol: Option<&str>) -> ParsedSource {
        let proto_line = protocol
            .map(|p| format!("protocol = {p:?}\n"))
            .unwrap_or_default();
        let raw: Source =
            toml::from_str(&format!("host = {host:?}\nrepo = {repo:?}\n{proto_line}"))
                .expect("host source DTO parses");
        ParsedSource::parse("inner", &raw).expect("host source parses")
    }

    fn corp_hosts() -> BTreeMap<String, Host> {
        let host: Host =
            toml::from_str("remote = { https = \"https://git.corp.example/{path}.git\", ssh = \"git@git.corp.example:{path}.git\" }")
                .expect("corp host DTO parses");
        BTreeMap::from([("corp".to_owned(), host)])
    }

    // Intended signature: inner_remote(name, source, hosts: &BTreeMap<String, Host>, default_protocol: Option<Protocol>).
    #[test]
    fn inner_host_source_resolves_against_consumer_custom_host() {
        let source = host_source("corp", "team/dots", None);
        let remote = inner_remote("inner", &source, &corp_hosts(), Some(Protocol::Https)).expect(
            "a dep host source naming a consumer-custom host must resolve via consumer hosts",
        );
        assert_eq!(
            remote, "https://git.corp.example/team/dots.git",
            "inner_remote must resolve the inner Host source against the CONSUMER's host map, \
             not an empty map"
        );
    }

    #[test]
    fn inner_host_source_falls_back_to_consumer_default_protocol() {
        let source = host_source("github", "owner/repo", None);
        let remote = inner_remote("inner", &source, &BTreeMap::new(), Some(Protocol::Ssh))
            .expect("an inner host source with no own protocol resolves via the consumer default");
        assert_eq!(
            remote, "git@github.com:owner/repo.git",
            "with consumer default protocol = ssh and no source-level protocol, inner_remote must \
             pick the host's SSH template, not the hardcoded HTTPS"
        );
    }

    #[test]
    fn transitive_hosts_override_cannot_redirect_a_clone() {
        let manifest = TransitiveManifest::parse(
            "version = 1\n\n\
             [hosts.github]\n\
             remote = \"https://evil.example/{path}.git\"\n\n\
             [sources.x]\n\
             host = \"github\"\n\
             repo = \"owner/repo\"\n",
        )
        .expect("a dep manifest declaring [hosts.github] still parses");

        let inner = ParsedSource::parse(
            "x",
            manifest.sources.get("x").expect("dep source `x` present"),
        )
        .expect("inner host source parses");

        let remote = inner_remote("x", &inner, &BTreeMap::new(), Some(Protocol::Https))
            .expect("the inner github source resolves against the consumer/builtin registry");

        assert_eq!(
            remote, "https://github.com/owner/repo.git",
            "TRUST SURFACE: a transitive dep's [hosts.github] override must be dropped — the inner \
             source must resolve to the builtin/consumer github.com, never the dep's redirect"
        );
        assert!(
            !remote.contains("evil.example"),
            "the resolved remote must not contain the dep-declared attacker host, got: {remote}"
        );
    }

    #[test]
    fn dep_top_level_protocol_does_not_influence_inner_resolution() {
        let manifest = TransitiveManifest::parse(
            "version = 1\n\
             protocol = \"ssh\"\n\n\
             [vars]\n\
             editor = \"nvim\"\n\n\
             [defaults]\n\
             auto_target = false\n\n\
             [sources.x]\n\
             host = \"github\"\n\
             repo = \"owner/repo\"\n",
        )
        .expect("a dep manifest with top-level vars/protocol/defaults is tolerated");

        let inner = ParsedSource::parse(
            "x",
            manifest.sources.get("x").expect("dep source `x` present"),
        )
        .expect("inner host source parses");

        let remote = inner_remote("x", &inner, &BTreeMap::new(), Some(Protocol::Https))
            .expect("inner source resolves under the consumer's default protocol");

        assert_eq!(
            remote, "https://github.com/owner/repo.git",
            "the dep's top-level protocol = \"ssh\" must be dropped, never merged: inner_remote \
             must resolve under the CONSUMER default (https), yielding the https template"
        );
    }

    #[test]
    fn relative_git_remote_is_rejected_at_top_level() {
        for remote in ["../escape", "./escape", "escape/sub"] {
            let source = git_source(remote);
            let err = reject_escaping_remote("dep", &source, remote, 1)
                .expect_err("a relative git remote must be rejected");
            assert!(
                err.to_string().contains("transitive remote not allowed"),
                "relative git remote `{remote}` must emit the named diagnostic, got: {err}"
            );
        }
    }

    #[test]
    fn depth_cap_fails_closed_past_max() {
        reject_depth_overflow("dep", MAX_TRANSITIVE_DEPTH).expect("at the limit must be allowed");
        let err = reject_depth_overflow("dep", MAX_TRANSITIVE_DEPTH + 1)
            .expect_err("past the limit must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains(&MAX_TRANSITIVE_DEPTH.to_string()) && msg.contains("depth"),
            "depth-cap diagnostic must name the limit, got: {msg}"
        );
    }

    #[test]
    fn absolute_and_url_git_remotes_are_allowed_at_top_level() {
        for remote in [
            "/abs/local/repo",
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
        ] {
            let source = git_source(remote);
            reject_escaping_remote("dep", &source, remote, 1).unwrap_or_else(|e| {
                panic!("non-relative git remote `{remote}` must be allowed: {e}")
            });
        }
    }

    fn locked_node(name: &str, git: &str, commit: &str, instance: Option<&str>) -> LockedSource {
        LockedSource {
            name: name.to_owned(),
            git: git.to_owned(),
            resolved: "main".to_owned(),
            commit: commit.to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: "blake3:cfg".to_owned(),
            r#ref: None,
            instance: instance.map(str::to_owned),
            build: None,
        }
    }

    fn lock_of(sources: Vec<LockedSource>) -> crate::lock::Lock {
        crate::lock::Lock {
            version: crate::lock::LOCK_SCHEMA_VERSION,
            sources,
            trusted_hooks: Vec::new(),
            candidate_hooks: Vec::new(),
        }
    }

    #[test]
    fn require_pinned_is_inactive_without_frozen() {
        let gate = FrozenGate {
            frozen: false,
            lock: None,
        };
        let pinned = gate
            .require_pinned(
                "dep",
                "https://x/r.git",
                &Refspec::Branch("main".to_owned()),
                1,
            )
            .expect("a non-frozen gate never errors");
        assert!(
            pinned.is_none(),
            "without --frozen the gate must be inactive, yielding no drift commit"
        );
    }

    #[test]
    fn require_pinned_errors_naming_unpinned_nested_node_with_depth() {
        let lock = lock_of(vec![locked_node(
            "dep",
            "https://dep/anchor.git",
            "c0",
            None,
        )]);
        let gate = FrozenGate {
            frozen: true,
            lock: Some(&lock),
        };
        let err = gate
            .require_pinned(
                "inner",
                "https://dep/inner.git",
                &Refspec::Branch("main".to_owned()),
                2,
            )
            .expect_err("an unpinned nested node must hard-error under --frozen");
        let msg = err.to_string();
        assert!(
            msg.contains("inner") && msg.contains("depth 2") && msg.contains("--frozen"),
            "the frozen miss must name the nested source, its depth, and --frozen, got: {msg}"
        );
    }

    // TDEP-HOOK-GATE-001

    fn dep_target_with_hooks() -> Target {
        let toml = "version = 1\n\n\
                    [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
                    [targets.editor]\npath = \"nvim\"\n\n\
                    [targets.editor.hooks]\non_change = \"./install.sh\"\n";
        crate::config::Config::parse(toml)
            .expect("dep config parses")
            .targets
            .remove("editor")
            .expect("dep target `editor` present")
    }

    #[test]
    fn composed_target_strips_hooks_so_dispatch_runs_none() {
        let dep_target = dep_target_with_hooks();
        assert!(
            dep_target.hooks.is_some(),
            "premise: the dep's own target declares an on_change hook"
        );

        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep_target,
            PathBuf::from("/home/me/deploy/nvim"),
            PathBuf::from("/home/me/deploy"),
            &BTreeMap::new(),
            None,
            None,
        )
        .expect("a composed dep target with no bindings synthesizes");

        assert!(
            synthetic.hooks.is_none(),
            "strip-by-default: a composed transitive target must carry NO hooks, so dispatch_hooks \
             (which only iterates config.targets[*].hooks) runs zero transitive hooks"
        );
        assert_eq!(
            synthetic.path,
            PathBuf::from("/home/me/deploy/nvim"),
            "premise: files still deploy — the composed target keeps its destination path"
        );
    }

    #[test]
    fn composed_hooks_are_stripped_yet_the_gate_surfaces_them_as_candidates() {
        use crate::config::admit_transitive_hooks;
        use crate::config::transitive::TransitiveManifest;

        let manifest = TransitiveManifest::parse(
            "version = 1\n\n\
             [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
             [targets.editor]\npath = \"nvim\"\n\n\
             [targets.editor.hooks]\non_change = \"./install.sh\"\n",
        )
        .expect("dep manifest parses");
        let opaque = manifest.hooks().expect("opaque per-target hooks retained");

        let node = FetchNode::new("https://github.com/dep/nvim.git", "main", "blake3:dead");
        let instance = Instance::new("root", "dep", "anchor", node);
        let (candidates, diagnostics) =
            admit_transitive_hooks(opaque, "editor", "ns%1%editor", &instance);

        assert_eq!(
            candidates.len(),
            1,
            "the gate must surface the dep's per-target hook as a candidate even though composition \
             stripped it from the deployed target — GATE owns candidates, dispatch never runs them"
        );
        assert!(
            diagnostics.is_empty(),
            "a well-formed dep hook must surface no parse diagnostic, got: {diagnostics:?}"
        );
    }

    // ── isolation: read_manifest reads only phora.toml ─────────────

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn git(cwd: &Path, args: &[&str]) {
        let _serial = crate::sync::state::locking::guard_git_fork();
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // ── mount take/collapse is CONSUMER-owned, keyed by the imported dep name (SMR-030/D13) ──

    use crate::config::TakeEntry;

    /// A plain dep target — it owns NO mount tables; the consumer keys those.
    fn dep_target() -> Target {
        let toml = "version = 1\n\n\
                    [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
                    [targets.editor]\npath = \"nvim\"\nsources = [\"nvim\"]\n";
        crate::config::Config::parse(toml)
            .expect("a plain dep config parses")
            .targets
            .remove("editor")
            .expect("dep target `editor` present")
    }

    /// A dep target whose own binding ALREADY carries a `take` — used to pin that a
    /// consumer mount override beats the dep's binding-local choice.
    fn dep_target_with_binding_local_take() -> Target {
        let toml = "version = 1\n\n\
                    [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
                    [targets.editor]\npath = \"nvim\"\n\n\
                    [targets.editor.sources]\nnvim = { take = [\"dep-local.lua\"] }\n";
        crate::config::Config::parse(toml)
            .expect("a dep config with a binding-local take parses")
            .targets
            .remove("editor")
            .expect("dep target `editor` present")
    }

    /// The CONSUMER anchor target binding transitive `dep` with `binding` refinements, lowered
    /// into its import exactly as a sync lowers it.
    fn consumer_anchor(binding: &str) -> Target {
        let toml = format!(
            "version = 1\n\n\
             [sources.dep]\ngit = \"https://github.com/me/dep.git\"\ntransitive = true\n\n\
             [targets.claude]\npath = \"~/.claude\"\nsources.dep = {{ {binding} }}\n"
        );
        crate::config::merge_configs(
            crate::config::Config::parse(&toml).expect("a consumer anchor parses"),
            None,
        )
        .targets
        .remove("claude")
        .expect("consumer target `claude` present")
    }

    /// The consumer's override for its one import, exactly as `compose_dep` receives it.
    fn mount_for(anchor: &Target) -> MountOverride<'_> {
        MountOverride::of(
            &anchor
                .offer_bindings
                .as_deref()
                .expect("the binding lowered")[0],
        )
    }

    fn namespaced_for(internal: &str, namespaced: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(internal.to_owned(), namespaced.to_owned())])
    }

    fn binding_of<'a>(target: &'a Target, identity: &str) -> &'a crate::config::Binding {
        target
            .sources
            .as_ref()
            .expect("synthetic target keeps its bindings")
            .get(identity)
            .expect("binding present under its identity")
    }

    #[test]
    fn mount_take_table_subsets_a_mounted_deps_offer() {
        let dep = dep_target();
        let anchor = consumer_anchor("take = [\"lua/init.lua\"]");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            mount_for(&anchor).take,
            mount_for(&anchor).collapse,
        )
        .expect("a dep mounted under a consumer anchor with a mount take synthesizes");

        let binding = binding_of(&synthetic, "nvim");
        let take = binding
            .composed_take
            .as_deref()
            .expect("the consumer's mount take for bound `dep` folds into the binding's `take`");
        assert!(
            matches!(take, [TakeEntry::Leaf(s)] if s == "lua/init.lua"),
            "the CONSUMER mount take `[\"lua/init.lua\"]` (keyed by bound `dep`) must fold \
             verbatim into the namespaced binding so the resolver stays mount-agnostic; got: {take:?}"
        );
    }

    #[test]
    fn mount_take_table_resolves_against_instance_keyed_published_names() {
        let dep = dep_target();
        let anchor = consumer_anchor("take = [{ \"a/X.md\" = \"a/x.md\" }]");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            mount_for(&anchor).take,
            mount_for(&anchor).collapse,
        )
        .expect("a dep with a consumer rename mount take synthesizes");

        let binding = binding_of(&synthetic, "nvim");
        assert_eq!(
            binding.source.as_deref(),
            Some("dep%1%nvim"),
            "the binding still points at the Instance-keyed published source; got: {:?}",
            binding.source
        );
        let take = binding
            .composed_take
            .as_deref()
            .expect("the consumer mount take folds into the binding");
        assert!(
            matches!(take, [TakeEntry::Rename { src, dest }] if src == "a/X.md" && dest == "a/x.md"),
            "the consumer rename mount entry resolves against the Instance-keyed published name \
             verbatim; got: {take:?}"
        );
    }

    #[test]
    fn two_anchors_mounting_same_dep_are_independent() {
        let dep = dep_target();
        let anchor_a = consumer_anchor("take = [\"only-a.md\"]");
        let anchor_b = consumer_anchor("take = [\"only-b.md\"]");
        let first = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/a/nvim"),
            PathBuf::from("/home/me/a"),
            &namespaced_for("nvim", "dep%1%nvim"),
            mount_for(&anchor_a).take,
            mount_for(&anchor_a).collapse,
        )
        .expect("first anchor synthesizes");
        let second = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/b/nvim"),
            PathBuf::from("/home/me/b"),
            &namespaced_for("nvim", "dep%2%nvim"),
            mount_for(&anchor_b).take,
            mount_for(&anchor_b).collapse,
        )
        .expect("second anchor synthesizes");

        assert_eq!(
            binding_of(&first, "nvim").source.as_deref(),
            Some("dep%1%nvim"),
            "anchor one binds the first Instance-keyed source"
        );
        assert_eq!(
            binding_of(&second, "nvim").source.as_deref(),
            Some("dep%2%nvim"),
            "anchor two binds a DISTINCT Instance-keyed source; the same dep mounted twice is \
             two independent instances"
        );
        let take_a = binding_of(&first, "nvim")
            .composed_take
            .as_deref()
            .expect("anchor one take");
        let take_b = binding_of(&second, "nvim")
            .composed_take
            .as_deref()
            .expect("anchor two take");
        assert!(
            matches!(take_a, [TakeEntry::Leaf(s)] if s == "only-a.md"),
            "anchor one applies its OWN consumer mount take; got: {take_a:?}"
        );
        assert!(
            matches!(take_b, [TakeEntry::Leaf(s)] if s == "only-b.md"),
            "anchor two applies its OWN consumer mount take, independent of anchor one; got: {take_b:?}"
        );
        assert_eq!(
            first.path,
            PathBuf::from("/home/me/a/nvim"),
            "anchor one deploys under its own anchor path"
        );
        assert_eq!(
            second.path,
            PathBuf::from("/home/me/b/nvim"),
            "anchor two deploys under its own anchor path, independent of anchor one"
        );
    }

    #[test]
    fn mount_collapse_table_maps_anchor_bool_to_collapse_choice() {
        let dep = dep_target();
        let anchor = consumer_anchor("collapse = true");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            mount_for(&anchor).take,
            mount_for(&anchor).collapse,
        )
        .expect("a dep mounted under a consumer collapse override synthesizes");

        assert_eq!(
            binding_of(&synthetic, "nvim").collapse,
            Some(true),
            "the consumer binding's `collapse` overrides the composed default; got: {:?}",
            binding_of(&synthetic, "nvim").collapse
        );
    }

    #[test]
    fn composed_bindings_default_to_per_leaf() {
        let dep = dep_target();
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            None,
            None,
        )
        .expect("a dep with no collapse anywhere synthesizes");
        assert_eq!(binding_of(&synthetic, "nvim").collapse, Some(false));
    }

    fn leaves(entries: &[TakeEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                TakeEntry::Leaf(leaf) => leaf.clone(),
                TakeEntry::Rename { src, dest } => format!("{src}->{dest}"),
            })
            .collect()
    }

    #[test]
    fn take_routes_by_target_path() {
        let entries = [
            TakeEntry::Leaf("skills/gestalt/**".to_owned()),
            TakeEntry::Leaf("skills/loqui/reference/loqui/languages/**".to_owned()),
            TakeEntry::Leaf("**/README.md".to_owned()),
        ];
        let others = [PathBuf::from("skills/loqui/reference/loqui")];
        let own = route_take("dep", &entries, Path::new("."), &others).expect("own files route");
        assert_eq!(leaves(&own), vec!["skills/gestalt/**", "**/README.md"]);
        let loqui = route_take(
            "dep",
            &entries,
            Path::new("skills/loqui/reference/loqui"),
            &others,
        )
        .expect("loqui routes");
        assert_eq!(leaves(&loqui), vec!["languages/**", "**/README.md"]);
    }

    #[test]
    fn take_of_an_enclosing_subtree_reaches_the_targets_below_it() {
        let others = [PathBuf::from("skills/loqui/reference/loqui")];
        for entry in ["skills/**", "skills/"] {
            let loqui = route_take(
                "dep",
                &[TakeEntry::Leaf(entry.to_owned())],
                Path::new("skills/loqui/reference/loqui"),
                &others,
            )
            .expect("routes");
            assert_eq!(leaves(&loqui), vec!["**"], "{entry}");
        }
    }

    #[test]
    fn take_reaching_no_entry_projects_nothing_and_a_whole_target_takes_all() {
        let others = [PathBuf::from("rules/fas/moira")];
        let none = route_take(
            "dep",
            &[TakeEntry::Leaf("skills/**".to_owned())],
            Path::new("rules/fas/moira"),
            &others,
        )
        .expect("routes");
        assert!(none.is_empty(), "no entry reaches moira: {none:?}");
        let all = route_take(
            "dep",
            &[TakeEntry::Leaf("rules/fas/moira/".to_owned())],
            Path::new("rules/fas/moira"),
            &others,
        )
        .expect("routes");
        assert_eq!(leaves(&all), vec!["**"]);
    }

    #[test]
    fn take_rename_leaving_its_target_is_rejected() {
        let others = [PathBuf::from("rules/fas/moira")];
        let err = route_take(
            "dep",
            &[TakeEntry::Rename {
                src: "rules/fas/moira/a.cue".to_owned(),
                dest: "elsewhere/a.cue".to_owned(),
            }],
            Path::new("rules/fas/moira"),
            &others,
        )
        .expect_err("the rename escapes moira");
        assert!(err.to_string().contains("leaves"), "got: {err}");
        let inside = route_take(
            "dep",
            &[TakeEntry::Rename {
                src: "rules/fas/moira/a.cue".to_owned(),
                dest: "rules/fas/moira/b.cue".to_owned(),
            }],
            Path::new("rules/fas/moira"),
            &others,
        )
        .expect("a rename inside moira routes");
        assert_eq!(leaves(&inside), vec!["a.cue->b.cue"]);
    }

    #[test]
    fn a_consumer_take_composes_over_a_binding_local_take() {
        let dep = dep_target_with_binding_local_take();
        let anchor = consumer_anchor("take = [\"consumer-wins.lua\"]");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            mount_for(&anchor).take,
            mount_for(&anchor).collapse,
        )
        .expect("a dep whose binding has its own take, under a consumer take, synthesizes");

        let binding = binding_of(&synthetic, "nvim");
        assert!(
            matches!(binding.take.as_deref(), Some([TakeEntry::Leaf(s)]) if s == "dep-local.lua"),
            "the dep's own take still shapes its output; got: {:?}",
            binding.take
        );
        assert!(
            matches!(binding.composed_take.as_deref(), Some([TakeEntry::Leaf(s)]) if s == "consumer-wins.lua"),
            "the consumer take applies to that output; got: {:?}",
            binding.composed_take
        );
    }

    #[test]
    fn absent_consumer_mount_take_leaves_a_binding_local_take_intact() {
        let dep = dep_target_with_binding_local_take();
        let anchor = consumer_anchor("");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            mount_for(&anchor).take,
            mount_for(&anchor).collapse,
        )
        .expect("a dep with a binding-local take and no consumer override synthesizes");

        let take = binding_of(&synthetic, "nvim")
            .take
            .as_deref()
            .expect("the dep's binding-local take survives");
        assert!(
            matches!(take, [TakeEntry::Leaf(s)] if s == "dep-local.lua"),
            "with NO consumer mount override, the dep's binding-local `take` must survive \
             unclobbered; got: {take:?}"
        );
    }

    use std::sync::atomic::{AtomicUsize, Ordering as FetchOrdering};

    struct CountingFetchBackend {
        inner: crate::source::GitBackend,
        fetches: AtomicUsize,
    }

    impl CountingFetchBackend {
        fn over(git_dir: PathBuf) -> Self {
            Self {
                inner: crate::source::GitBackend::new(git_dir),
                fetches: AtomicUsize::new(0),
            }
        }
        fn fetch_count(&self) -> usize {
            self.fetches.load(FetchOrdering::SeqCst)
        }
    }

    impl SourceStore for CountingFetchBackend {
        fn resolve(
            &self,
            request: &ResolveRequest,
            policy: ResolvePolicy,
        ) -> std::result::Result<crate::source::ResolvedSource, crate::source::SourceError>
        {
            if policy == ResolvePolicy::Refresh {
                self.fetches.fetch_add(1, FetchOrdering::SeqCst);
            }
            SourceStore::resolve(&self.inner, request, policy)
        }

        fn inventory(
            &self,
            snapshot: &crate::source::SnapshotId,
            root: Option<&crate::source::SourcePath>,
        ) -> std::result::Result<crate::source::SourceInventory, crate::source::SourceError>
        {
            self.inner.inventory(snapshot, root)
        }

        fn read(
            &self,
            snapshot: &crate::source::SnapshotId,
            path: &crate::source::SourcePath,
        ) -> std::result::Result<crate::source::SourceEntry, crate::source::SourceError> {
            self.inner.read(snapshot, path)
        }

        fn list_directory(
            &self,
            snapshot: &crate::source::SnapshotId,
            path: Option<&crate::source::SourcePath>,
        ) -> std::result::Result<Vec<crate::source::SourceDirectoryEntry>, crate::source::SourceError>
        {
            self.inner.list_directory(snapshot, path)
        }
    }

    struct SourceFailureBackend;

    impl SourceStore for SourceFailureBackend {
        fn resolve(
            &self,
            _request: &ResolveRequest,
            _policy: ResolvePolicy,
        ) -> std::result::Result<crate::source::ResolvedSource, crate::source::SourceError>
        {
            Err(crate::source::SourceError::Source(
                "backend sentinel".to_owned(),
            ))
        }

        fn inventory(
            &self,
            _snapshot: &crate::source::SnapshotId,
            _root: Option<&crate::source::SourcePath>,
        ) -> std::result::Result<crate::source::SourceInventory, crate::source::SourceError>
        {
            Err(crate::source::SourceError::Source(
                "backend sentinel".to_owned(),
            ))
        }

        fn read(
            &self,
            _snapshot: &crate::source::SnapshotId,
            _path: &crate::source::SourcePath,
        ) -> std::result::Result<crate::source::SourceEntry, crate::source::SourceError> {
            Err(crate::source::SourceError::Source(
                "backend sentinel".to_owned(),
            ))
        }

        fn list_directory(
            &self,
            _snapshot: &crate::source::SnapshotId,
            _path: Option<&crate::source::SourcePath>,
        ) -> std::result::Result<Vec<crate::source::SourceDirectoryEntry>, crate::source::SourceError>
        {
            Err(crate::source::SourceError::Source(
                "backend sentinel".to_owned(),
            ))
        }
    }

    #[test]
    fn sync_contextualizes_source_manifest_error_once() {
        let remote = "https://example.test/dep.git";
        let error = fetch_manifest(
            "dep",
            &git_source(remote),
            remote,
            &SourceFailureBackend,
            2,
            &FrozenGate {
                frozen: false,
                lock: None,
            },
        )
        .expect_err("the source backend failure must cross the sync boundary");
        let diagnostic = error.to_string();
        assert_eq!(
            diagnostic,
            "config error: transitive source `dep` at depth 2: source error: backend sentinel",
            "sync must add its depth/name context exactly once while preserving the existing CLI diagnostic"
        );
        assert_eq!(diagnostic.matches("backend sentinel").count(), 1);
        assert_eq!(
            diagnostic
                .matches("transitive source `dep` at depth 2")
                .count(),
            1
        );
        assert!(
            error_chain_has::<crate::source::SourceError>(&error),
            "sync contextualization must retain the concrete SourceError in the aggregate chain: {diagnostic}"
        );
        assert!(
            error_chain_has_source_variant(&error, |source| matches!(
                source,
                crate::source::SourceError::Source(message) if message == "backend sentinel"
            )),
            "sync must preserve the backend's original SourceError variant: {diagnostic}"
        );
    }

    enum SyncManifestRead {
        Bytes(Vec<u8>),
        Absent,
        BackendFailure,
    }

    struct SyncManifestBackend(SyncManifestRead);

    impl SourceStore for SyncManifestBackend {
        fn resolve(
            &self,
            request: &ResolveRequest,
            _policy: ResolvePolicy,
        ) -> std::result::Result<crate::source::ResolvedSource, crate::source::SourceError>
        {
            let crate::source::SourceLocation::Git { url } = &request.location else {
                unreachable!("transitive manifest fixtures use Git sources")
            };
            let normalized = crate::source::NormalizedUrl::parse(url);
            let commit: crate::source::Commit = "a".repeat(40).parse().expect("fixture commit");
            Ok(crate::source::ResolvedSource {
                name: request.name.clone(),
                snapshot: crate::source::SnapshotId::Git {
                    mirror: crate::source::MirrorKey::from_url(&normalized),
                    commit: commit.clone(),
                },
                revision: crate::source::ResolvedRevision::Commit(commit),
                authored_at: crate::source::SourceTimestamp::from_unix_seconds(0),
                normalized_location: crate::source::SourceIdentity::Git(normalized),
            })
        }

        fn inventory(
            &self,
            _snapshot: &crate::source::SnapshotId,
            _root: Option<&crate::source::SourcePath>,
        ) -> std::result::Result<crate::source::SourceInventory, crate::source::SourceError>
        {
            Ok(crate::source::SourceInventory::default())
        }

        fn read(
            &self,
            snapshot: &crate::source::SnapshotId,
            path: &crate::source::SourcePath,
        ) -> std::result::Result<crate::source::SourceEntry, crate::source::SourceError> {
            match &self.0 {
                SyncManifestRead::Bytes(bytes) => Ok(crate::source::SourceEntry {
                    meta: crate::source::SourceEntryMeta {
                        path: path.clone(),
                        kind: crate::source::SourceEntryKind::File,
                    },
                    bytes: bytes.clone(),
                }),
                SyncManifestRead::Absent => Err(crate::source::SourceError::FileAbsent {
                    source_name: "dep".to_owned(),
                    commit: snapshot.commit().to_string(),
                    path: PathBuf::from(path.as_str()),
                }),
                SyncManifestRead::BackendFailure => Err(crate::source::SourceError::Source(
                    "backend sentinel".to_owned(),
                )),
            }
        }

        fn list_directory(
            &self,
            _snapshot: &crate::source::SnapshotId,
            _path: Option<&crate::source::SourcePath>,
        ) -> std::result::Result<Vec<crate::source::SourceDirectoryEntry>, crate::source::SourceError>
        {
            Ok(Vec::new())
        }
    }

    fn sync_manifest_error(read: SyncManifestRead) -> Error {
        let remote = "https://example.test/dep.git";
        fetch_manifest(
            "dep",
            &git_source(remote),
            remote,
            &SyncManifestBackend(read),
            2,
            &FrozenGate {
                frozen: false,
                lock: None,
            },
        )
        .expect_err("the malformed source manifest must cross the sync boundary")
    }

    enum ExpectedStructuredCause {
        Missing,
        Utf8,
        Parse,
        Backend,
    }

    #[test]
    fn sync_preserves_source_manifest_failure_diagnostics() {
        for (read, expected, cause) in [
            (
                SyncManifestRead::Absent,
                "dependency at `https://example.test/dep.git` has no phora.toml",
                ExpectedStructuredCause::Missing,
            ),
            (
                SyncManifestRead::Bytes(vec![0xff]),
                "phora.toml at `https://example.test/dep.git` is not utf-8",
                ExpectedStructuredCause::Utf8,
            ),
            (
                SyncManifestRead::Bytes(b"version = [\n".to_vec()),
                "unclosed array",
                ExpectedStructuredCause::Parse,
            ),
            (
                SyncManifestRead::BackendFailure,
                "backend sentinel",
                ExpectedStructuredCause::Backend,
            ),
        ] {
            let error = sync_manifest_error(read);
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains(expected),
                "sync lost the source-owned diagnostic `{expected}`: {diagnostic}"
            );
            assert_eq!(
                diagnostic
                    .matches("transitive source `dep` at depth 2")
                    .count(),
                1,
                "sync must contextualize the SourceError exactly once: {diagnostic}"
            );
            assert_eq!(
                diagnostic.matches(expected).count(),
                1,
                "the source diagnostic must not be duplicated or stringified repeatedly: {diagnostic}"
            );
            assert!(
                error_chain_has::<crate::source::SourceError>(&error),
                "sync must retain the concrete SourceError in the aggregate chain: {diagnostic}"
            );
            let cause_preserved = match cause {
                ExpectedStructuredCause::Missing => {
                    error_chain_has_source_variant(&error, |source| {
                        matches!(
                            source,
                            crate::source::SourceError::FileAbsent { path, .. }
                                if path == Path::new("phora.toml")
                        )
                    })
                }
                ExpectedStructuredCause::Utf8 => {
                    error_chain_has::<std::string::FromUtf8Error>(&error)
                }
                ExpectedStructuredCause::Parse => error_chain_has::<toml::de::Error>(&error),
                ExpectedStructuredCause::Backend => {
                    error_chain_has_source_variant(&error, |source| {
                        matches!(
                            source,
                            crate::source::SourceError::Source(message)
                                if message == "backend sentinel"
                        )
                    })
                }
            };
            assert!(
                cause_preserved,
                "sync must preserve the structured source cause for `{expected}`: {diagnostic}"
            );
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn dep_mirror_with_editor_target() -> (tempfile::TempDir, String, String) {
        let src = tempfile::TempDir::new().unwrap();
        let src_path = src.path();
        crate::sync::state::locking::assert_git_sandboxed(src_path);
        git(src_path, &["init", "-b", "main", "."]);
        git(src_path, &["config", "user.email", "t@example.com"]);
        git(src_path, &["config", "user.name", "T"]);
        std::fs::write(
            src_path.join("phora.toml"),
            b"version = 1\n\n\
              [sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
              [targets.editor]\npath = \"nvim\"\nsources = [\"nvim\"]\n",
        )
        .unwrap();
        git(src_path, &["add", "-A"]);
        git(src_path, &["commit", "-m", "dep with an editor target"]);

        let mirror_root = tempfile::TempDir::new().unwrap();
        let url = src_path.to_string_lossy().into_owned();
        let mirror = mirror_path(mirror_root.path(), &url);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        {
            let _serial = crate::sync::state::locking::guard_git_fork();
            git(
                mirror_root.path(),
                &["clone", "--mirror", &url, mirror.to_str().unwrap()],
            );
        }
        let commit = {
            let _serial = crate::sync::state::locking::guard_git_fork();
            let out = std::process::Command::new("git")
                .args(["-C", mirror.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        drop(src);
        (mirror_root, url, commit)
    }

    fn consumer_importing(url: &str) -> Config {
        let toml = format!(
            "version = 1\n\n\
             [sources.dep]\ngit = {url:?}\nbranch = \"main\"\ntransitive = true\n\n\
             [targets.claude]\npath = \"/home/me/.claude\"\nsources = [\"dep\"]\n"
        );
        crate::config::merge_configs(
            Config::parse(&toml).expect("a consumer binding a transitive dep parses"),
            None,
        )
    }

    fn parsed_of(config: &Config) -> BTreeMap<String, ParsedSource> {
        config
            .sources
            .iter()
            .map(|(n, s)| {
                (
                    n.clone(),
                    ParsedSource::parse(n, s).expect("consumer source parses"),
                )
            })
            .collect()
    }

    #[test]
    fn offline_resolve_composes_pinned_dep_without_any_fetch() {
        let (mirror_root, url, commit) = dep_mirror_with_editor_target();
        let backend = CountingFetchBackend::over(mirror_root.path().to_path_buf());
        let config = consumer_importing(&url);
        let parsed = parsed_of(&config);
        let mut entry = locked_node("dep", &url, &commit, None);
        entry.config_digest = parsed["dep"].config_digest();
        let lock = lock_of(vec![entry]);

        let graph = resolve_transitive_graph_offline(&config, &parsed, &backend, &lock).expect(
            "the offline lock-pinned resolve composes the dep from the mirror without fetching",
        );

        assert_eq!(
            backend.fetch_count(),
            0,
            "NETWORK CONTRACT: trust inspection reads the lock-pinned commit from the mirror only; \
             resolve_transitive_graph_offline must never call backend.fetch"
        );
        assert!(
            graph.targets.iter().any(|t| t.name.contains("editor")),
            "the offline resolve must compose the dep's `editor` target; got: {:?}",
            graph
                .targets
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn offline_resolve_surfaces_a_commit_absent_from_the_mirror() {
        let (mirror_root, url, _head) = dep_mirror_with_editor_target();
        let backend = CountingFetchBackend::over(mirror_root.path().to_path_buf());
        let config = consumer_importing(&url);
        let parsed = parsed_of(&config);
        let absent = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let mut entry = locked_node("dep", &url, absent, None);
        entry.config_digest = parsed["dep"].config_digest();
        let lock = lock_of(vec![entry]);

        let err = resolve_transitive_graph_offline(&config, &parsed, &backend, &lock).expect_err(
            "a lock-pinned commit absent from the mirror must surface an error the CLI maps to \
             'run phora sync first', never a silent empty graph",
        );

        assert_eq!(
            backend.fetch_count(),
            0,
            "even on the degraded path the offline resolve must not fetch to paper over the \
             missing commit"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(absent),
            "the absent-commit failure must name the missing lock-pinned commit `{absent}` — the \
             offline mirror read for that SHA is what fails — not surface a generic error that \
             could be any unrelated config/parse failure; got: {msg}"
        );
    }
}
