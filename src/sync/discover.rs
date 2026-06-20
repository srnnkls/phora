use std::path::Path;

use crate::config::{DeployMode, ParsedSource};
use crate::error::{Error, Result};
use crate::kernel::{ArtifactName, Selection, SourceName, locator_basename};
use crate::source::SourceBackend;
use crate::store::RecordKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkArtifact {
    pub(super) name: ArtifactName,
    pub(super) kind: RecordKind,
    /// Source relpath under the effective root that matched; multi-segment for a nested locator.
    pub(super) locator: String,
}

/// Discover artifacts by scanning the live working tree at `<git>/<root>` (Link
/// mode). Mirrors the ODB `discover_artifacts`: directories, matched loose files,
/// and nested locators (named by basename) become artifacts; `Selection` gates
/// inclusion (the dotfile opt-in lives there); the result is sorted. A selected
/// symlink is refused. A missing path/root is an error.
pub(super) fn discover_working_tree(
    git: &Path,
    root: Option<&Path>,
    selection: &Selection,
) -> Result<Vec<LinkArtifact>> {
    let base = root.map_or_else(|| git.to_path_buf(), |r| git.join(r));
    let entries = std::fs::read_dir(&base)
        .map_err(|e| Error::Sync(format!("scan working tree {}: {e}", base.display())))?;

    let mut artifacts = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Sync(format!("read entry in {}: {e}", base.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry
            .file_type()
            .map_err(|e| Error::Sync(format!("file_type for {name} in {}: {e}", base.display())))?;
        if !selection.selects_top_level_artifact(&name, file_type.is_dir()) {
            continue;
        }
        if file_type.is_symlink() {
            return Err(Error::SymlinkNotAllowed { path: name.into() });
        }
        artifacts.push(LinkArtifact {
            name: ArtifactName::trusted(name.clone()),
            kind: kind_of(file_type.is_dir()),
            locator: name,
        });
    }

    for locator in selection.nested_locators() {
        let leaf = base.join(locator);
        let meta = std::fs::symlink_metadata(&leaf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::SourceCtx(crate::source::SourceError::ArtifactNotFound {
                    artifact: locator.clone(),
                })
            } else {
                Error::Sync(format!(
                    "locate nested selector {locator} in {}: {e}",
                    base.display()
                ))
            }
        })?;
        if meta.file_type().is_symlink() {
            return Err(Error::SymlinkNotAllowed {
                path: locator.into(),
            });
        }
        artifacts.push(LinkArtifact {
            name: ArtifactName::trusted(locator_basename(locator)),
            kind: kind_of(meta.is_dir()),
            locator: locator.clone(),
        });
    }

    artifacts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(artifacts)
}

fn kind_of(is_dir: bool) -> RecordKind {
    if is_dir {
        RecordKind::Dir
    } else {
        RecordKind::File
    }
}

pub(super) fn discover_artifacts_for_source(
    source: &ParsedSource,
    git: &str,
    source_name: &SourceName,
    commit: &str,
    backend: &dyn SourceBackend,
    selection: &Selection,
    root: Option<&Path>,
) -> Result<Vec<ArtifactName>> {
    Ok(
        discover_link_artifacts(source, git, source_name, commit, backend, selection, root)?
            .into_iter()
            .map(|a| a.name)
            .collect(),
    )
}

/// Copy-mode `kind` is a placeholder (`Dir`); the real kind is resolved at export.
pub(super) fn discover_link_artifacts(
    source: &ParsedSource,
    git: &str,
    source_name: &SourceName,
    commit: &str,
    backend: &dyn SourceBackend,
    selection: &Selection,
    root: Option<&Path>,
) -> Result<Vec<LinkArtifact>> {
    match source.deploy_mode() {
        DeployMode::Link => discover_working_tree(Path::new(git), root, selection),
        DeployMode::Copy => Ok(backend
            .discover_artifacts(source_name, git, commit, root, selection)?
            .into_iter()
            .map(|name| LinkArtifact {
                locator: name.as_str().to_owned(),
                name,
                kind: RecordKind::Dir,
            })
            .collect()),
    }
}

#[cfg(test)]
mod fps002_link_discovery {
    use super::{LinkArtifact, discover_working_tree};
    use crate::error::Error;
    use crate::kernel::{ArtifactName, Selection};
    use crate::store::RecordKind;
    use std::path::Path;
    use tempfile::TempDir;

    fn an(name: &str) -> ArtifactName {
        ArtifactName::trusted(name)
    }

    fn names(artifacts: &[LinkArtifact]) -> Vec<ArtifactName> {
        artifacts.iter().map(|a| a.name.clone()).collect()
    }

    fn discover_names(
        git: &Path,
        root: Option<&Path>,
        sel: &Selection,
    ) -> Result<Vec<ArtifactName>, Error> {
        discover_working_tree(git, root, sel).map(|a| names(&a))
    }

    fn selection(include: &[&str], exclude: &[&str]) -> Selection {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        Selection::new(&inc, &exc).expect("patterns build into a selection")
    }

    fn tree_with_loose_files() -> TempDir {
        let dir = TempDir::new().expect("working tree tempdir");
        let p = dir.path();
        std::fs::write(p.join("init.lua"), b"-- init\n").expect("write init.lua");
        std::fs::write(p.join("settings.json"), b"{}\n").expect("write settings.json");
        std::fs::write(p.join("notes.md"), b"# notes\n").expect("write notes.md");
        std::fs::create_dir_all(p.join("a/b")).expect("create a/b");
        std::fs::write(p.join("a/b/c"), b"leaf\n").expect("write a/b/c");
        std::fs::create_dir_all(p.join("editor")).expect("create editor");
        std::fs::write(p.join("editor/init.lua"), b"-- ed\n").expect("write editor file");
        std::fs::create_dir_all(p.join(".zfunc")).expect("create .zfunc");
        std::fs::write(p.join(".zfunc/_fn"), b"# fn\n").expect("write zfunc file");
        dir
    }

    #[test]
    fn link_discover_yields_loose_file_as_artifact() {
        let tree = tree_with_loose_files();
        let sel = selection(&["init.lua"], &[]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert_eq!(
            artifacts,
            vec![an("init.lua")],
            "link-mode discovery must yield a loose file artifact, not drop it"
        );
    }

    #[test]
    fn link_discover_glob_pulls_loose_root_json_files() {
        let tree = tree_with_loose_files();
        let sel = selection(&["*.json"], &[]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert_eq!(
            artifacts,
            vec![an("settings.json")],
            "include=[\"*.json\"] must match the loose root settings.json"
        );
    }

    #[test]
    fn link_discover_nested_path_yields_basename_artifact() {
        let tree = tree_with_loose_files();
        let sel = selection(&["a/b/c"], &[]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert_eq!(
            artifacts,
            vec![an("c")],
            "nested include a/b/c yields ONE artifact named `c` (basename), not `a`"
        );
    }

    #[test]
    fn link_discover_surfaces_included_dot_dir() {
        let tree = tree_with_loose_files();
        let sel = selection(&[".zfunc"], &[]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert!(
            artifacts.iter().any(|a| a.as_str() == ".zfunc"),
            "explicitly-included dot-dir must surface, got {artifacts:?}"
        );
    }

    #[test]
    fn link_discover_excludes_dot_dir_when_not_included() {
        let tree = tree_with_loose_files();
        let sel = selection(&["init.lua"], &[]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert!(
            !artifacts.iter().any(|a| a.as_str() == ".zfunc"),
            "dot-dir stays excluded unless explicitly included, got {artifacts:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn link_discover_refuses_included_symlink() {
        let tree = tree_with_loose_files();
        std::os::unix::fs::symlink("init.lua", tree.path().join("dangle")).expect("create symlink");
        let sel = selection(&["dangle"], &[]);
        let err = discover_working_tree(tree.path(), None, &sel)
            .expect_err("a symlink selected as a file artifact must be refused");
        assert!(
            matches!(
                err,
                Error::SymlinkNotAllowed { .. }
                    | Error::SourceCtx(crate::source::SourceError::SymlinkNotAllowed { .. })
            ),
            "refusal must be the symlink variant, not some unrelated error, got: {err:?}"
        );
        assert!(
            err.to_string().contains("dangle"),
            "rejection must name the offending symlink `dangle`, got: {err}"
        );
    }

    #[test]
    fn link_discover_excludes_nested_locator_by_path() {
        let tree = tree_with_loose_files();
        let sel = selection(&["a/b/c"], &["a/b/c"]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert!(
            !artifacts.iter().any(|a| a.as_str() == "c"),
            "exclude of the nested locator's full path must drop it; exclude wins, got {artifacts:?}"
        );
    }

    #[test]
    fn link_discover_excludes_nested_locator_by_basename() {
        let tree = tree_with_loose_files();
        let sel = selection(&["a/b/c"], &["c"]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert!(
            !artifacts.iter().any(|a| a.as_str() == "c"),
            "exclude of the nested locator's basename must drop it; exclude wins, got {artifacts:?}"
        );
    }

    #[test]
    fn link_discover_missing_nested_locator_is_not_found_not_io() {
        let tree = tree_with_loose_files();
        let sel = selection(&["a/b/missing"], &[]);
        let err = discover_working_tree(tree.path(), None, &sel)
            .expect_err("a nested locator whose path is absent must error");
        assert!(
            matches!(
                err,
                Error::SourceCtx(crate::source::SourceError::ArtifactNotFound { .. })
            ),
            "an absent locator must be a not-found error, not a generic Sync I/O error, got: {err:?}"
        );
        assert!(
            err.to_string().contains("a/b/missing"),
            "the not-found error must name the missing locator, got: {err}"
        );
    }

    #[test]
    fn link_discover_dir_artifacts_unchanged() {
        let tree = tree_with_loose_files();
        let sel = selection(&["editor"], &[]);
        let artifacts = discover_names(tree.path(), None, &sel).expect("discover succeeds");
        assert_eq!(
            artifacts,
            vec![an("editor")],
            "a plain dir-artifact selector still resolves to the dir"
        );
    }

    #[test]
    fn link_discover_tags_dir_and_file_with_their_node_kind() {
        let tree = tree_with_loose_files();
        let sel = selection(&["editor", "init.lua", "a/b/c"], &[]);
        let found = discover_working_tree(tree.path(), None, &sel).expect("discover succeeds");

        let kind_of = |name: &str| {
            found
                .iter()
                .find(|a| a.name.as_str() == name)
                .map(|a| a.kind)
        };
        assert_eq!(
            kind_of("editor"),
            Some(RecordKind::Dir),
            "a directory artifact must be tagged kind=dir"
        );
        assert_eq!(
            kind_of("init.lua"),
            Some(RecordKind::File),
            "a loose-file artifact must be tagged kind=file"
        );
        assert_eq!(
            kind_of("c"),
            Some(RecordKind::File),
            "a nested-locator leaf file must be tagged kind=file"
        );
    }
}
