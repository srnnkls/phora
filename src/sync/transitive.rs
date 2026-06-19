//! Serial, cycle-guarded transitive pre-pass: before the parallel resolve pool,
//! walk every `transitive = true` source's own `phora.toml`, fetching and parsing
//! each dep manifest. A failure at any depth fails the sync fail-fast, before any
//! lock write. This phase only resolves the graph; admission/composition is later.

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::transitive::TransitiveManifest;
use crate::config::{Config, ParsedSource, Remote, SourceMode};
use crate::error::{Error, Result};
use crate::kernel::SourceName;
use crate::source::{NormalizedUrl, SourceBackend, is_local_path, mirror_path};

use super::resolved_remotes;

/// Walks the transitive graph rooted at `config`'s `transitive = true` sources.
/// A failure naming a source below the top level carries `at depth N`.
pub(super) fn resolve_transitive_graph(
    config: &Config,
    parsed: &std::collections::BTreeMap<String, ParsedSource>,
    backend: &(dyn SourceBackend + Sync),
) -> Result<()> {
    let Some(mirror_root) = backend.mirror_root() else {
        return Ok(());
    };
    let mut visited: BTreeSet<(String, String)> = BTreeSet::new();
    let remotes = resolved_remotes(config, parsed)?;
    for (name, source) in parsed {
        if !source.is_transitive() {
            continue;
        }
        let remote = remotes
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| Error::Config(format!("no resolved remote for source `{name}`")))?;
        reject_escaping_remote(name, source, remote, 1)?;
        walk(name, source, remote, backend, mirror_root, &mut visited, 1)?;
    }
    Ok(())
}

/// One transitive node: fetch the dep, read its `phora.toml`, recurse into the
/// dep's own `transitive = true` sources. `(normalized-url, ref)` bounds cycles.
fn walk(
    name: &str,
    source: &ParsedSource,
    remote: &str,
    backend: &(dyn SourceBackend + Sync),
    mirror_root: &Path,
    visited: &mut BTreeSet<(String, String)>,
    depth: usize,
) -> Result<()> {
    let refspec = source.refspec();
    let key = (
        NormalizedUrl::parse(remote).as_str().to_owned(),
        refspec.to_string(),
    );
    if !visited.insert(key) {
        return Ok(());
    }

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

    for (inner_name, inner) in &manifest.sources {
        let inner_parsed = ParsedSource::parse(inner_name, inner)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        let inner_remote = inner_remote(inner_name, &inner_parsed)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        reject_escaping_remote(inner_name, &inner_parsed, &inner_remote, depth + 1)
            .map_err(|e| at_depth(inner_name, depth + 1, &e.to_string()))?;
        if !inner.is_transitive() {
            continue;
        }
        walk(
            inner_name,
            &inner_parsed,
            &inner_remote,
            backend,
            mirror_root,
            visited,
            depth + 1,
        )?;
    }
    Ok(())
}

fn inner_remote(name: &str, source: &ParsedSource) -> Result<String> {
    if source.mode() == SourceMode::Url {
        return source
            .source_url()
            .map(str::to_owned)
            .ok_or_else(|| Error::Config(format!("source `{name}`: missing url")));
    }
    source.resolved_remote(
        &std::collections::BTreeMap::new(),
        crate::config::Protocol::Https,
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
