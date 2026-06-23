//! Serial, cycle-guarded transitive pre-pass: before the parallel resolve pool,
//! walk every imported `transitive = true` source's own `phora.toml`, fetching and
//! parsing each dep manifest, and produce a namespaced composition graph. A failure
//! at any depth fails the sync fail-fast, before any lock write.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::transitive::{FetchNode, Instance, TransitiveManifest};
use crate::config::{
    Config, DeployMode, HookCommand, Host, ParsedSource, Protocol, Refspec, Remote, SourceMode,
    TakeEntry, Target, admit_transitive_hooks, hook_preimage,
};
use crate::error::{Error, Result};
use crate::kernel::SourceName;
use crate::source::{SourceBackend, is_local_path};

use super::resolved_remotes;

/// Named-diagnostic phrase emitted when two composed dep targets land on one destination.
const COMPOSED_DEST_COLLISION: &str = "composed targets resolve to the same destination";

const TRANSITIVE_LINK_REJECTED: &str = "transitive source cannot use deploy = \"link\"";

/// Fail-closed bound: an acyclic ever-deeper `transitive = true` import chain would otherwise stack-overflow (`DoS`) on untrusted manifests.
const MAX_TRANSITIVE_DEPTH: usize = 64;

/// One dep target composed under a consumer anchor: a synthetic absolute-path
/// target carrying the dep's own layout, bound to namespaced source instances.
pub(super) struct ComposedTarget {
    pub(super) name: String,
    pub(super) target: Target,
}

/// An interpreted transitive `on_change` hook pinned to its dep's resolved commit, awaiting
/// the consumer trust decision in [`sync`](super::sync). Stripped from the deployed target.
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
            command: c.command.run.clone(),
            source: c.source.clone(),
            commit: c.commit.clone(),
        }
    }
}

/// The transitive pre-pass output: composed targets plus the namespaced source
/// instances (and their resolved remotes) those targets bind.
#[derive(Default)]
pub(super) struct ResolvedGraph {
    pub(super) targets: Vec<ComposedTarget>,
    pub(super) sources: BTreeMap<String, ParsedSource>,
    pub(super) remotes: BTreeMap<String, String>,
    /// Namespaced source name → owning `Instance.stable_key()`; the lock stamps this so
    /// a transitive node is keyed by its instance, not a bare name that never lines up.
    pub(super) instances: BTreeMap<String, String>,
    pub(super) hook_candidates: Vec<TransitiveHookCandidate>,
    pub(super) hook_diagnostics: Vec<String>,
}

impl ResolvedGraph {
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

/// Once composition absorbs its `imports`, a bindingless anchor would deploy as a live empty target.
fn strip_absorbed_anchors(config: &mut Config) {
    config.targets.retain(|_, target| {
        if target.imports.is_none() {
            return true;
        }
        target.imports = None;
        target.sources.as_ref().is_some_and(|s| !s.is_empty())
    });
}

/// Walks the transitive graph rooted at the consumer's imported `transitive = true`
/// sources, producing the namespaced composition graph. A failure naming a source
/// below the top level carries `at depth N`.
pub(super) fn resolve_transitive_graph(
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    backend: &(dyn SourceBackend + Sync),
    frozen: bool,
    effective_lock: Option<&crate::lock::Lock>,
) -> Result<ResolvedGraph> {
    let has_imports = config
        .targets
        .values()
        .any(|t| t.imports.iter().flatten().next().is_some());
    if !has_imports {
        return Ok(ResolvedGraph::default());
    }
    let remotes = resolved_remotes(config, parsed)?;
    let frozen_gate = FrozenGate {
        frozen,
        lock: effective_lock,
    };
    let mut visited: HashSet<FetchNode> = HashSet::new();
    let mut graph = ResolvedGraph::default();
    let mut counter: usize = 0;

    for (anchor_name, anchor) in &config.targets {
        for imported in anchor.imports.iter().flatten() {
            let source = parsed.get(imported).ok_or_else(|| {
                Error::Config(format!("no resolved source for imported `{imported}`"))
            })?;
            let remote = remotes.get(imported).map(String::as_str).ok_or_else(|| {
                Error::Config(format!("no resolved remote for source `{imported}`"))
            })?;
            reject_escaping_remote(imported, source, remote, 1)?;
            let (commit, manifest) =
                fetch_manifest(imported, source, remote, backend, 1, &frozen_gate)?;
            let node = FetchNode::new(remote, &source.refspec().to_string(), &commit);
            visited.insert(node.clone());
            let instance = Instance::new("root", imported, anchor_name, node);
            compose_dep(
                &instance,
                anchor,
                imported,
                &manifest,
                &mut WalkCtx {
                    backend,
                    visited: &mut visited,
                    ancestors: Vec::new(),
                    counter: &mut counter,
                    graph: &mut graph,
                    frozen: &frozen_gate,
                    consumer_hosts: &config.hosts,
                    default_protocol: config.protocol,
                    root_source: imported,
                },
                1,
            )?;
        }
    }

    Ok(graph)
}

struct WalkCtx<'a> {
    backend: &'a (dyn SourceBackend + Sync),
    /// `visited`: fetch-closure dedup gating `descend_for_validation` (LOCK-001); per-Instance nested composition intentionally ignores it. `ancestors`: current-path cycle guard.
    visited: &'a mut HashSet<FetchNode>,
    ancestors: Vec<FetchNode>,
    counter: &'a mut usize,
    graph: &'a mut ResolvedGraph,
    frozen: &'a FrozenGate<'a>,
    consumer_hosts: &'a BTreeMap<String, Host>,
    default_protocol: Option<Protocol>,
    /// Consumer-facing import name rooting this subtree; stamped on every hook candidate it yields.
    root_source: &'a str,
}

/// Checked at the top of every `fetch_manifest` so no depth can fetch an unpinned/drifted node under `--frozen`.
struct FrozenGate<'a> {
    frozen: bool,
    lock: Option<&'a crate::lock::Lock>,
}

/// Composes a dep's own targets under `anchor`: each becomes a synthetic target at
/// `anchor.expanded_path / dep_target.path`, keeping the dep's own per-target layout,
/// bound to source instances namespaced by the dep [`Instance`]. Two composed targets
/// sharing a destination is a hard error.
fn compose_dep(
    instance: &Instance,
    anchor: &Target,
    imported: &str,
    manifest: &TransitiveManifest,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<()> {
    reject_depth_overflow(imported, depth)?;
    let anchor_path = anchor.expanded_path();
    let mut composed_dests: BTreeMap<PathBuf, String> = BTreeMap::new();

    let imported_inner: HashSet<&str> = manifest
        .targets
        .values()
        .flat_map(|t| t.imports.iter().flatten())
        .map(String::as_str)
        .collect();

    let source_names =
        namespace_dep_sources(instance, imported, manifest, &imported_inner, ctx, depth)?;

    let mount = MountOverride {
        take: anchor
            .take
            .as_ref()
            .and_then(|t| t.get(imported))
            .map(Vec::as_slice),
        collapse: anchor
            .collapse
            .as_ref()
            .and_then(|c| c.get(imported))
            .copied(),
    };

    for (dep_target_name, dep_target) in &manifest.targets {
        reject_dep_target_path(imported, dep_target_name, &dep_target.path)?;
        let composed_path = anchor_path.join(&dep_target.path);
        if let Some(other) = composed_dests.insert(composed_path.clone(), dep_target_name.clone()) {
            return Err(Error::Config(format!(
                "{COMPOSED_DEST_COLLISION}: dep targets `{other}` and `{dep_target_name}` of \
                 imported `{imported}` both compose to {}",
                composed_path.display()
            )));
        }
        compose_nested_imports(
            instance,
            imported,
            dep_target_name,
            dep_target,
            &composed_path,
            manifest,
            ctx,
            depth,
        )?;
        let synthetic = synthetic_target(
            imported,
            dep_target_name,
            dep_target,
            composed_path.clone(),
            anchor_path.clone(),
            &source_names,
            &mount,
        )?;
        *ctx.counter += 1;
        let composed_name = namespaced_key(instance, dep_target_name, *ctx.counter);
        admit_hook_candidates(
            instance,
            manifest,
            dep_target_name,
            &composed_name,
            &composed_path,
            ctx,
        );
        ctx.graph.targets.push(ComposedTarget {
            name: composed_name,
            target: synthetic,
        });
    }
    Ok(())
}

/// A source already composed by a nested import is graphed (so the frozen gate can pin it) but
/// omitted from the returned bind map: it only re-exports its children, binding no target itself.
fn namespace_dep_sources(
    instance: &Instance,
    imported: &str,
    manifest: &TransitiveManifest,
    imported_inner: &HashSet<&str>,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<BTreeMap<String, String>> {
    let mut source_names: BTreeMap<String, String> = BTreeMap::new();
    for (inner_name, inner) in &manifest.sources {
        let parsed = ParsedSource::parse(inner_name, inner).map_err(|e| {
            Error::Config(format!("imported `{imported}`: source `{inner_name}`: {e}"))
        })?;
        if parsed.deploy_mode() == DeployMode::Link {
            return Err(Error::Config(format!(
                "imported `{imported}`: source `{inner_name}`: {TRANSITIVE_LINK_REJECTED}"
            )));
        }
        let remote = inner_remote(
            inner_name,
            &parsed,
            ctx.consumer_hosts,
            ctx.default_protocol,
        )
        .map_err(|e| Error::Config(format!("imported `{imported}`: source `{inner_name}`: {e}")))?;
        reject_escaping_remote(inner_name, &parsed, &remote, depth + 1)?;
        let composed_by_nested_import =
            inner.is_transitive() && imported_inner.contains(inner_name.as_str());
        if inner.is_transitive() && !composed_by_nested_import && !ctx.frozen.frozen {
            descend_for_validation(inner_name, &parsed, &remote, ctx, depth + 1)?;
        }
        *ctx.counter += 1;
        let namespaced = namespaced_key(instance, inner_name, *ctx.counter);
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
    reason = "nested composition threads parent instance, anchor path, manifest, ctx, and depth together"
)]
fn compose_nested_imports(
    parent_instance: &Instance,
    imported: &str,
    dep_target_name: &str,
    dep_target: &Target,
    composed_path: &Path,
    manifest: &TransitiveManifest,
    ctx: &mut WalkCtx<'_>,
    depth: usize,
) -> Result<()> {
    for inner_name in dep_target.imports.iter().flatten() {
        let inner = manifest.sources.get(inner_name).ok_or_else(|| {
            Error::Config(format!(
                "imported `{imported}`: target `{dep_target_name}` imports undefined source `{inner_name}`"
            ))
        })?;
        let inner_parsed = ParsedSource::parse(inner_name, inner)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        let inner_remote = inner_remote(
            inner_name,
            &inner_parsed,
            ctx.consumer_hosts,
            ctx.default_protocol,
        )
        .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        reject_escaping_remote(inner_name, &inner_parsed, &inner_remote, depth + 1)?;
        let (inner_commit, inner_manifest) = fetch_manifest(
            inner_name,
            &inner_parsed,
            &inner_remote,
            ctx.backend,
            depth + 1,
            ctx.frozen,
        )?;
        let inner_node = FetchNode::new(
            &inner_remote,
            &inner_parsed.refspec().to_string(),
            &inner_commit,
        );
        ctx.visited.insert(inner_node.clone());
        if ctx.ancestors.contains(&inner_node) {
            continue;
        }
        let inner_instance = Instance::new(
            &parent_instance.stable_key(),
            inner_name,
            dep_target_name,
            inner_node.clone(),
        );
        let nested_anchor = Target {
            path: composed_path.to_path_buf(),
            sources: None,
            layout: None,
            hooks: None,
            imports: None,
            take: None,
            collapse: None,
            confine: None,
        };
        ctx.ancestors.push(inner_node);
        let composed = compose_dep(
            &inner_instance,
            &nested_anchor,
            inner_name,
            &inner_manifest,
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
        reject_escaping_remote(inner_name, &inner_parsed, &inner_remote, depth + 1)?;
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

fn namespaced_key(instance: &Instance, name: &str, counter: usize) -> String {
    format!("{}%{counter}%{name}", instance.stable_key())
}

/// Interprets the dep target's stripped `on_change` hooks into commit-pinned candidates the
/// trust decision in [`sync`](super::sync) consumes, recording any parse-failure diagnostic.
fn admit_hook_candidates(
    instance: &Instance,
    manifest: &TransitiveManifest,
    dep_target_name: &str,
    composed_name: &str,
    composed_path: &Path,
    ctx: &mut WalkCtx<'_>,
) {
    let Some(opaque) = manifest.hooks() else {
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

/// Consumer-owned (D13/C3): the importing anchor's mount take/collapse for one imported dep,
/// looked up by the imported name — never the dep's own `take`/`collapse` tables.
struct MountOverride<'a> {
    take: Option<&'a [TakeEntry]>,
    collapse: Option<bool>,
}

fn synthetic_target(
    imported: &str,
    dep_target_name: &str,
    dep_target: &Target,
    composed_path: PathBuf,
    anchor_path: PathBuf,
    source_names: &BTreeMap<String, String>,
    mount: &MountOverride<'_>,
) -> Result<Target> {
    let mut target = dep_target.clone();
    target.path = composed_path;
    target.imports = None;
    target.hooks = None;
    target.confine = Some(anchor_path);
    target.take = None;
    target.collapse = None;
    if let Some(bindings) = target.sources.as_mut() {
        for (identity, binding) in bindings.iter_mut() {
            let effective = binding.source.clone().unwrap_or_else(|| identity.clone());
            let namespaced = source_names.get(&effective).ok_or_else(|| {
                Error::Config(format!(
                    "imported `{imported}`: target `{dep_target_name}` binds undefined source `{effective}`"
                ))
            })?;
            binding.source = Some(namespaced.clone());
            if let Some(take) = mount.take {
                binding.take = Some(take.to_vec());
            }
            if let Some(collapse) = mount.collapse {
                binding.collapse = Some(collapse);
            }
        }
    }
    Ok(target)
}

/// A dep target path must be a relative subpath: absolute, `~/`, or `..` escapes the anchor.
fn reject_dep_target_path(imported: &str, dep_target_name: &str, path: &Path) -> Result<()> {
    let escapes = path.is_absolute()
        || path.starts_with("~")
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if escapes {
        return Err(Error::Config(format!(
            "imported `{imported}`: target `{dep_target_name}` path `{}` must be a relative \
             subpath of the anchor",
            path.display()
        )));
    }
    Ok(())
}

fn fetch_manifest(
    name: &str,
    source: &ParsedSource,
    remote: &str,
    backend: &(dyn SourceBackend + Sync),
    depth: usize,
    frozen: &FrozenGate<'_>,
) -> Result<(String, TransitiveManifest)> {
    let refspec = source.refspec();
    let pinned = frozen.require_pinned(name, remote, &refspec, depth)?;
    let source_name = SourceName::trusted(name.to_owned());
    let commit = if let Some(locked_commit) = pinned {
        locked_commit.to_owned()
    } else {
        backend
            .fetch(&source_name, remote)
            .map_err(|e| at_depth(name, depth, &e.to_string()))?;
        backend
            .resolve(&source_name, remote, &refspec)
            .map_err(|e| at_depth(name, depth, &e.to_string()))?
    };
    let manifest_text = read_manifest(backend, &source_name, remote, &commit)
        .map_err(|e| at_depth(name, depth, &e.to_string()))?;
    let manifest = TransitiveManifest::parse(&manifest_text)
        .map_err(|e| at_depth(name, depth, &e.to_string()))?;
    Ok((commit, manifest))
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

/// A transitive source may not reach a local `path` or `file://` remote resolved on
/// the consumer host unless it resolves inside the already-materialized dep tree
/// (nothing is materialized at this phase, so any such remote is rejected). A
/// literal `git = <local repo>` is the consumer's explicit choice and is allowed.
fn reject_escaping_remote(
    name: &str,
    source: &ParsedSource,
    remote: &str,
    depth: usize,
) -> Result<()> {
    let escapes = matches!(source.remote, Remote::Path(_))
        || remote.starts_with("file://")
        || is_relative_fs_remote(remote)
        || (depth > 1 && is_local_path(remote));
    if escapes {
        return Err(Error::Config(format!(
            "source `{name}`: transitive remote not allowed — `{remote}` is a local path \
             or file:// remote and does not resolve inside the materialized dependency tree"
        )));
    }
    Ok(())
}

/// True for a relative filesystem path; false for URL/scp remotes and absolute paths.
fn is_relative_fs_remote(remote: &str) -> bool {
    if remote.contains("://") {
        return false;
    }
    if let Some(colon) = remote.find(':') {
        let first_slash = remote.find('/');
        if first_slash.is_none_or(|slash| colon < slash) {
            return false;
        }
    }
    !Path::new(remote).is_absolute()
}

fn read_manifest(
    backend: &(dyn SourceBackend + Sync),
    source: &SourceName,
    remote: &str,
    commit: &str,
) -> Result<String> {
    let bytes = backend
        .read_file_at(source, remote, commit, Path::new("phora.toml"))
        .map_err(|e| match e {
            crate::source::SourceError::FileAbsent { .. } => {
                Error::Config(format!("dependency at `{remote}` has no phora.toml"))
            }
            other => Error::Source(other.to_string()),
        })?;
    String::from_utf8(bytes)
        .map_err(|e| Error::Config(format!("phora.toml at `{remote}` is not utf-8: {e}")))
}

fn reject_depth_overflow(name: &str, depth: usize) -> Result<()> {
    if depth > MAX_TRANSITIVE_DEPTH {
        return Err(at_depth(
            name,
            depth,
            &format!("transitive import chain exceeds the maximum depth of {MAX_TRANSITIVE_DEPTH}"),
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
            &MountOverride {
                take: None,
                collapse: None,
            },
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
        let _serial = crate::store::guard_git_fork();
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

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn read_manifest_ignores_a_dep_shipped_phora_lock() {
        let src = tempfile::TempDir::new().unwrap();
        let src_path = src.path();
        crate::store::assert_git_sandboxed(src_path);
        git(src_path, &["init", "-b", "main", "."]);
        git(src_path, &["config", "user.email", "t@example.com"]);
        git(src_path, &["config", "user.name", "T"]);

        std::fs::write(
            src_path.join("phora.toml"),
            b"version = 1\n\n[sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n",
        )
        .unwrap();
        // A malicious dep ships a self-trusting lock alongside its manifest (mise GHSA-436v-8fw5-4mj8).
        std::fs::write(
            src_path.join("phora.lock"),
            b"version = 2\n\n[[trusted_hooks]]\ndep_instance = \"selftrust\"\nhook_id = \"editor#on_change\"\npreimage = \"blake3:evil\"\napproved_at = \"2026-06-20T00:00:00Z\"\n",
        )
        .unwrap();
        git(src_path, &["add", "-A"]);
        git(src_path, &["commit", "-m", "dep with self-trusting lock"]);

        let mirror_root = tempfile::TempDir::new().unwrap();
        let url = src_path.to_string_lossy().into_owned();
        let mirror = mirror_path(mirror_root.path(), &url);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        {
            let _serial = crate::store::guard_git_fork();
            git(
                mirror_root.path(),
                &["clone", "--mirror", &url, mirror.to_str().unwrap()],
            );
        }
        let commit = {
            let _serial = crate::store::guard_git_fork();
            let out = std::process::Command::new("git")
                .args(["-C", mirror.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };

        let backend = crate::source::GitBackend::new(mirror_root.path().to_path_buf());
        let text = read_manifest(&backend, &SourceName::trusted("dep"), &url, &commit)
            .expect("read_manifest reads the dep's phora.toml from its git tree");

        assert!(
            text.contains("[sources.nvim]"),
            "read_manifest must return the dep's phora.toml content, got: {text}"
        );
        assert!(
            !text.contains("trusted_hooks") && !text.contains("blake3:evil"),
            "ISOLATION: read_manifest must NEVER fold a dep-shipped phora.lock into the manifest \
             text; a self-trusting dep lock must be entirely ignored, got: {text}"
        );
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn read_file_at_reads_only_phora_toml_never_a_dep_shipped_lock() {
        use crate::source::GitBackend;

        let src = tempfile::TempDir::new().unwrap();
        let src_path = src.path();
        crate::store::assert_git_sandboxed(src_path);
        git(src_path, &["init", "-b", "main", "."]);
        git(src_path, &["config", "user.email", "t@example.com"]);
        git(src_path, &["config", "user.name", "T"]);

        std::fs::write(
            src_path.join("phora.toml"),
            b"version = 1\n\n[sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n",
        )
        .unwrap();
        std::fs::write(
            src_path.join("phora.lock"),
            b"version = 2\n\n[[trusted_hooks]]\ndep_instance = \"selftrust\"\nhook_id = \"editor#on_change\"\npreimage = \"blake3:evil\"\napproved_at = \"2026-06-20T00:00:00Z\"\n",
        )
        .unwrap();
        git(src_path, &["add", "-A"]);
        git(src_path, &["commit", "-m", "dep with self-trusting lock"]);

        let git_dir = tempfile::TempDir::new().unwrap();
        let url = src_path.to_string_lossy().into_owned();
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        backend
            .fetch(&crate::kernel::SourceName::trusted("dep"), &url)
            .expect("fetch builds the bare mirror the offline read targets");
        let commit = backend
            .resolve(
                &crate::kernel::SourceName::trusted("dep"),
                &url,
                &crate::config::Refspec::Branch("main".to_owned()),
            )
            .expect("resolve main to a commit");

        let manifest_bytes = backend
            .read_file_at(
                &crate::kernel::SourceName::trusted("dep"),
                &url,
                &commit,
                Path::new("phora.toml"),
            )
            .expect("read_file_at returns the dep's phora.toml from the fetched mirror");
        let text = String::from_utf8(manifest_bytes).expect("phora.toml is utf-8");

        assert!(
            text.contains("[sources.nvim]"),
            "read_file_at(phora.toml) must return the dep manifest content, got: {text}"
        );
        assert!(
            !text.contains("trusted_hooks") && !text.contains("blake3:evil"),
            "ISOLATION: read_manifest refactored onto read_file_at(phora.toml) must NEVER reach a \
             dep-shipped phora.lock; the self-trusting lock must be unreachable, got: {text}"
        );
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn read_manifest_says_no_phora_toml_only_when_genuinely_absent() {
        use crate::source::GitBackend;

        let src = tempfile::TempDir::new().unwrap();
        let src_path = src.path();
        crate::store::assert_git_sandboxed(src_path);
        git(src_path, &["init", "-b", "main", "."]);
        git(src_path, &["config", "user.email", "t@example.com"]);
        git(src_path, &["config", "user.name", "T"]);
        std::fs::write(src_path.join("README.md"), b"hi\n").unwrap();
        git(src_path, &["add", "-A"]);
        git(src_path, &["commit", "-m", "no manifest"]);

        let git_dir = tempfile::TempDir::new().unwrap();
        let url = src_path.to_string_lossy().into_owned();
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let name = SourceName::trusted("dep");
        backend.fetch(&name, &url).unwrap();
        let commit = backend
            .resolve(
                &name,
                &url,
                &crate::config::Refspec::Branch("main".to_owned()),
            )
            .unwrap();

        let absent = read_manifest(&backend, &name, &url, &commit)
            .expect_err("a dep without phora.toml must error");
        assert!(
            absent.to_string().contains("no phora.toml"),
            "a genuinely-absent phora.toml must keep the 'no phora.toml' diagnostic, got: {absent}"
        );
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn read_manifest_propagates_underlying_failure_instead_of_no_phora_toml() {
        use crate::source::GitBackend;

        let git_dir = tempfile::TempDir::new().unwrap();
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let name = SourceName::trusted("dep");

        let err = read_manifest(
            &backend,
            &name,
            "https://github.com/never/fetched.git",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        )
        .expect_err("reading from a mirror that was never fetched must error");

        assert!(
            !err.to_string().contains("no phora.toml"),
            "a mirror-open/git failure must NOT be masked as 'no phora.toml'; the underlying error \
             must surface, got: {err}"
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

    /// The CONSUMER anchor target carrying `imports = ["dep"]` and the mount tables, keyed by the
    /// imported dep name `dep` (the ground-truth key shape from the config mount-take tests).
    fn consumer_anchor(mount_body: &str) -> Target {
        let toml = format!(
            "version = 1\n\n\
             [sources.dep]\ngit = \"https://github.com/me/dep.git\"\ntransitive = true\n\n\
             [targets.claude]\npath = \"~/.claude\"\nimports = [\"dep\"]\n{mount_body}"
        );
        crate::config::Config::parse(&toml)
            .expect("a consumer anchor with mount tables parses")
            .targets
            .remove("claude")
            .expect("consumer target `claude` present")
    }

    /// The consumer's mount override for imported `dep`, exactly as `compose_dep` builds it.
    fn mount_for<'a>(anchor: &'a Target, imported: &str) -> MountOverride<'a> {
        MountOverride {
            take: anchor
                .take
                .as_ref()
                .and_then(|t| t.get(imported))
                .map(Vec::as_slice),
            collapse: anchor
                .collapse
                .as_ref()
                .and_then(|c| c.get(imported))
                .copied(),
        }
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
        let anchor = consumer_anchor("[targets.claude.take]\n\"dep\" = [\"lua/init.lua\"]\n");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            &mount_for(&anchor, "dep"),
        )
        .expect("a dep mounted under a consumer anchor with a mount take synthesizes");

        let binding = binding_of(&synthetic, "nvim");
        let take = binding
            .take
            .as_deref()
            .expect("the consumer's mount take for imported `dep` folds into the binding's `take`");
        assert!(
            matches!(take, [TakeEntry::Leaf(s)] if s == "lua/init.lua"),
            "the CONSUMER mount take `[\"lua/init.lua\"]` (keyed by imported `dep`) must fold \
             verbatim into the namespaced binding so the resolver stays mount-agnostic; got: {take:?}"
        );
    }

    #[test]
    fn mount_take_table_resolves_against_instance_keyed_published_names() {
        let dep = dep_target();
        let anchor =
            consumer_anchor("[targets.claude.take]\n\"dep\" = [{ \"a/X.md\" = \"a/x.md\" }]\n");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            &mount_for(&anchor, "dep"),
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
            .take
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
        let anchor_a = consumer_anchor("[targets.claude.take]\n\"dep\" = [\"only-a.md\"]\n");
        let anchor_b = consumer_anchor("[targets.claude.take]\n\"dep\" = [\"only-b.md\"]\n");
        let first = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/a/nvim"),
            PathBuf::from("/home/me/a"),
            &namespaced_for("nvim", "dep%1%nvim"),
            &mount_for(&anchor_a, "dep"),
        )
        .expect("first anchor synthesizes");
        let second = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/b/nvim"),
            PathBuf::from("/home/me/b"),
            &namespaced_for("nvim", "dep%2%nvim"),
            &mount_for(&anchor_b, "dep"),
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
            .take
            .as_deref()
            .expect("anchor one take");
        let take_b = binding_of(&second, "nvim")
            .take
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
        let anchor = consumer_anchor("[targets.claude.collapse]\n\"dep\" = false\n");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            &mount_for(&anchor, "dep"),
        )
        .expect("a dep mounted under a consumer collapse override synthesizes");

        assert_eq!(
            binding_of(&synthetic, "nvim").collapse,
            Some(false),
            "the CONSUMER mount collapse `false` (keyed by imported `dep`) folds into the \
             binding's `collapse` (Some(false) = ForcePerLeaf); got: {:?}",
            binding_of(&synthetic, "nvim").collapse
        );
    }

    #[test]
    fn consumer_mount_take_overrides_a_binding_local_take() {
        let dep = dep_target_with_binding_local_take();
        let anchor = consumer_anchor("[targets.claude.take]\n\"dep\" = [\"consumer-wins.lua\"]\n");
        let synthetic = synthetic_target(
            "dep",
            "editor",
            &dep,
            PathBuf::from("/home/me/.claude/nvim"),
            PathBuf::from("/home/me/.claude"),
            &namespaced_for("nvim", "dep%1%nvim"),
            &mount_for(&anchor, "dep"),
        )
        .expect(
            "a dep whose binding has its own take, mounted under a consumer override, synthesizes",
        );

        let take = binding_of(&synthetic, "nvim")
            .take
            .as_deref()
            .expect("the binding carries a take");
        assert!(
            matches!(take, [TakeEntry::Leaf(s)] if s == "consumer-wins.lua"),
            "PRECEDENCE: a present consumer mount take must OVERRIDE the dep's binding-local \
             `take = [\"dep-local.lua\"]`, not be ignored or merged; got: {take:?}"
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
            &mount_for(&anchor, "dep"),
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
}
