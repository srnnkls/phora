//! Serial, cycle-guarded transitive pre-pass: before the parallel resolve pool,
//! walk every imported `transitive = true` source's own `phora.toml`, fetching and
//! parsing each dep manifest, and produce a namespaced composition graph. A failure
//! at any depth fails the sync fail-fast, before any lock write.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::transitive::{FetchNode, Instance, TransitiveManifest};
use crate::config::{Binding, Config, DeployMode, ParsedSource, Remote, SourceMode, Target};
use crate::error::{Error, Result};
use crate::kernel::{SourceName, safe_component};
use crate::source::{SourceBackend, is_local_path, mirror_path};

use super::resolved_remotes;

/// Named-diagnostic phrase emitted when two composed dep targets land on one destination.
const COMPOSED_DEST_COLLISION: &str = "composed targets resolve to the same destination";

const TRANSITIVE_LINK_REJECTED: &str = "transitive source cannot use deploy = \"link\"";

/// One dep target composed under a consumer anchor: a synthetic absolute-path
/// target carrying the dep's own layout, bound to namespaced source instances.
pub(super) struct ComposedTarget {
    pub(super) name: String,
    pub(super) target: Target,
}

/// The transitive pre-pass output: composed targets plus the namespaced source
/// instances (and their resolved remotes) those targets bind.
#[derive(Default)]
pub(super) struct ResolvedGraph {
    pub(super) targets: Vec<ComposedTarget>,
    pub(super) sources: BTreeMap<String, ParsedSource>,
    pub(super) remotes: BTreeMap<String, String>,
}

impl ResolvedGraph {
    pub(super) fn inject(
        self,
        config: &mut Config,
        parsed: &mut BTreeMap<String, ParsedSource>,
        remotes: &mut BTreeMap<String, String>,
    ) {
        parsed.extend(self.sources);
        remotes.extend(self.remotes);
        for composed in self.targets {
            config.targets.insert(composed.name, composed.target);
        }
        strip_absorbed_anchors(config);
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
) -> Result<ResolvedGraph> {
    let Some(mirror_root) = backend.mirror_root() else {
        return Ok(ResolvedGraph::default());
    };
    let remotes = resolved_remotes(config, parsed)?;
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
                fetch_manifest(imported, source, remote, backend, mirror_root, 1)?;
            let node = FetchNode::new(remote, &source.refspec().to_string(), &commit);
            walk_recurse(
                node.clone(),
                &manifest,
                backend,
                mirror_root,
                &mut visited,
                1,
            )?;
            let instance = Instance::new("root", imported, anchor_name, node);
            compose_dep(
                &instance,
                anchor,
                imported,
                &manifest,
                &mut counter,
                &mut graph,
            )?;
        }
    }

    Ok(graph)
}

/// Recurses fetch/parse into a dep's own `transitive = true` inner sources, keying
/// the visited-set on [`FetchNode`] so a diamond collapses to one fetch.
fn walk_recurse(
    parent_node: FetchNode,
    manifest: &TransitiveManifest,
    backend: &(dyn SourceBackend + Sync),
    mirror_root: &Path,
    visited: &mut HashSet<FetchNode>,
    depth: usize,
) -> Result<()> {
    if !visited.insert(parent_node) {
        return Ok(());
    }
    for (inner_name, inner) in &manifest.sources {
        let inner_parsed = ParsedSource::parse(inner_name, inner)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        let inner_remote = inner_remote(inner_name, &inner_parsed)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        reject_escaping_remote(inner_name, &inner_parsed, &inner_remote, depth + 1)?;
        if !inner.is_transitive() {
            continue;
        }
        let (inner_commit, inner_manifest) = fetch_manifest(
            inner_name,
            &inner_parsed,
            &inner_remote,
            backend,
            mirror_root,
            depth + 1,
        )?;
        let inner_node = FetchNode::new(
            &inner_remote,
            &inner_parsed.refspec().to_string(),
            &inner_commit,
        );
        walk_recurse(
            inner_node,
            &inner_manifest,
            backend,
            mirror_root,
            visited,
            depth + 1,
        )?;
    }
    Ok(())
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
    counter: &mut usize,
    graph: &mut ResolvedGraph,
) -> Result<()> {
    let anchor_path = anchor.expanded_path();
    let mut composed_dests: BTreeMap<PathBuf, String> = BTreeMap::new();

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
        let remote = inner_remote(inner_name, &parsed).map_err(|e| {
            Error::Config(format!("imported `{imported}`: source `{inner_name}`: {e}"))
        })?;
        *counter += 1;
        let namespaced = namespaced_key(instance, inner_name, *counter);
        graph.remotes.insert(namespaced.clone(), remote);
        graph.sources.insert(namespaced.clone(), parsed);
        source_names.insert(inner_name.clone(), namespaced);
    }

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
        let synthetic = synthetic_target(
            imported,
            dep_target_name,
            dep_target,
            composed_path,
            anchor_path.clone(),
            &source_names,
        )?;
        *counter += 1;
        graph.targets.push(ComposedTarget {
            name: namespaced_key(instance, dep_target_name, *counter),
            target: synthetic,
        });
    }
    Ok(())
}

fn namespaced_key(instance: &Instance, name: &str, counter: usize) -> String {
    format!("{}%{counter}%{name}", instance.stable_key())
}

fn synthetic_target(
    imported: &str,
    dep_target_name: &str,
    dep_target: &Target,
    composed_path: PathBuf,
    anchor_path: PathBuf,
    source_names: &BTreeMap<String, String>,
) -> Result<Target> {
    let mut target = dep_target.clone();
    target.path = composed_path;
    target.imports = None;
    target.hooks = None;
    target.confine = Some(anchor_path);
    if let Some(bindings) = target.sources.as_mut() {
        for (identity, binding) in bindings.iter_mut() {
            let effective = binding.source.clone().unwrap_or_else(|| identity.clone());
            reject_dep_binding(imported, dep_target_name, identity, binding)?;
            let namespaced = source_names.get(&effective).ok_or_else(|| {
                Error::Config(format!(
                    "imported `{imported}`: target `{dep_target_name}` binds undefined source `{effective}`"
                ))
            })?;
            binding.source = Some(namespaced.clone());
        }
    }
    Ok(target)
}

/// Routes a dep-own binding's `map`/`root` through the same path-safety checks
/// `Config::validate` applies to consumer bindings (the DTO path skips validate, so
/// an escaping dep `map` value would otherwise bypass `safe_component`).
fn reject_dep_binding(
    imported: &str,
    dep_target_name: &str,
    identity: &str,
    binding: &Binding,
) -> Result<()> {
    if let Some(root) = &binding.root
        && (root.starts_with("~")
            || root.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            }))
    {
        return Err(Error::Config(format!(
            "imported `{imported}`: target `{dep_target_name}` binding `{identity}`: \
             `root` must stay inside the source"
        )));
    }
    for (key, value) in binding.map.iter().flatten() {
        if safe_component(value).is_err() {
            return Err(Error::Config(format!(
                "imported `{imported}`: target `{dep_target_name}` binding `{identity}`: \
                 `map` dest `{value}` must be a single safe filename"
            )));
        }
        if key.starts_with('/') || key.split('/').any(|c| c == "..") {
            return Err(Error::Config(format!(
                "imported `{imported}`: target `{dep_target_name}` binding `{identity}`: \
                 `map` key `{key}` must stay inside the source root"
            )));
        }
    }
    Ok(())
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
    mirror_root: &Path,
    depth: usize,
) -> Result<(String, TransitiveManifest)> {
    let refspec = source.refspec();
    let source_name = SourceName::trusted(name.to_owned());
    backend
        .fetch(&source_name, remote)
        .map_err(|e| at_depth(name, depth, &e.to_string()))?;
    let commit = backend
        .resolve(&source_name, remote, &refspec)
        .map_err(|e| at_depth(name, depth, &e.to_string()))?;
    let manifest_text = read_manifest(mirror_root, remote, &commit)
        .map_err(|e| at_depth(name, depth, &e.to_string()))?;
    let manifest = TransitiveManifest::parse(&manifest_text)
        .map_err(|e| at_depth(name, depth, &e.to_string()))?;
    Ok((commit, manifest))
}

fn inner_remote(name: &str, source: &ParsedSource) -> Result<String> {
    if source.mode() == SourceMode::Url {
        return source
            .source_url()
            .map(str::to_owned)
            .ok_or_else(|| Error::Config(format!("source `{name}`: missing url")));
    }
    source.resolved_remote(&BTreeMap::new(), crate::config::Protocol::Https)
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

fn read_manifest(mirror_root: &Path, remote: &str, commit: &str) -> Result<String> {
    let mirror = mirror_path(mirror_root, remote);
    let repo = gix::open(&mirror)
        .map_err(|e| Error::Source(format!("open mirror for `{remote}`: {e}")))?;
    let oid = gix::ObjectId::from_hex(commit.as_bytes())
        .map_err(|e| Error::Source(format!("parse commit {commit}: {e}")))?;
    let tree = repo
        .find_commit(oid)
        .map_err(|e| Error::Source(format!("commit {commit}: {e}")))?
        .tree()
        .map_err(|e| Error::Source(format!("tree of {commit}: {e}")))?;
    let entry = tree
        .find_entry("phora.toml")
        .ok_or_else(|| Error::Config(format!("dependency at `{remote}` has no phora.toml")))?;
    let object = entry
        .object()
        .map_err(|e| Error::Source(format!("read phora.toml at {commit}: {e}")))?;
    String::from_utf8(object.data.clone())
        .map_err(|e| Error::Config(format!("phora.toml at `{remote}` is not utf-8: {e}")))
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

    fn git_source(git: &str) -> ParsedSource {
        let raw: Source = toml::from_str(&format!("git = {git:?}\ntransitive = true\n"))
            .expect("git source DTO parses");
        ParsedSource::parse("dep", &raw).expect("git source parses")
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
}
