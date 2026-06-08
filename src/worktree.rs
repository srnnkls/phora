//! Primary (main, non-linked) worktree resolution.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Resolves the repository's primary (main, non-linked) worktree root.
///
/// Given any directory inside a repository — including a linked worktree —
/// returns the absolute, canonical root of the primary worktree that linked
/// worktrees back-reference.
///
/// # Errors
///
/// Returns [`Error::Worktree`] when `start` is not inside any git repository,
/// when the primary repository is bare and thus has no working tree, or when
/// the primary work dir cannot be canonicalized (e.g. it was removed or a
/// permission error occurred).
pub fn primary_worktree(start: &Path) -> Result<PathBuf> {
    let repo = gix::discover(start).map_err(|e| {
        Error::Worktree(format!("discover repository from {}: {e}", start.display()))
    })?;
    let main = repo
        .main_repo()
        .map_err(|e| Error::Worktree(format!("open primary repository: {e}")))?;
    let work_dir = main
        .workdir()
        .ok_or_else(|| Error::Worktree("primary repository is bare: no working tree".to_owned()))?;
    std::fs::canonicalize(work_dir).map_err(|e| {
        Error::Worktree(format!(
            "canonicalize primary work dir {}: {e}",
            work_dir.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    #[expect(
        clippy::unwrap_used,
        reason = "test canonicalization of an existing path cannot fail"
    )]
    fn canonical(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap()
    }

    /// `git init` + identity + one commit, returning the temp dir holding it.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn init_primary() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        run_git(path, &["init", "-b", "main", "."]);
        run_git(path, &["config", "user.email", "test@example.com"]);
        run_git(path, &["config", "user.name", "Test"]);
        std::fs::write(path.join("README.md"), b"hello\n").unwrap();
        run_git(path, &["add", "README.md"]);
        run_git(path, &["commit", "-m", "initial"]);
        dir
    }

    #[test]
    fn resolves_main_root_from_linked_worktree() {
        let primary = init_primary();
        let primary_root = canonical(primary.path());

        let linked_parent = TempDir::new().unwrap();
        let linked_path = linked_parent.path().join("linked");
        run_git(
            primary.path(),
            &[
                "worktree",
                "add",
                linked_path.to_str().expect("linked path is utf8"),
                "-b",
                "feature",
            ],
        );
        let linked_root = canonical(&linked_path);

        assert_ne!(
            primary_root, linked_root,
            "fixture invalid: linked worktree must live outside the primary checkout"
        );

        let resolved =
            primary_worktree(&linked_path).expect("linked worktree resolves to a primary root");

        assert_eq!(
            resolved, primary_root,
            "from a linked worktree, primary_worktree must return the MAIN checkout root"
        );
        assert_ne!(
            resolved, linked_root,
            "primary_worktree must NOT return the linked worktree directory"
        );
    }

    #[test]
    fn resolves_self_from_primary_worktree() {
        let primary = init_primary();
        let primary_root = canonical(primary.path());

        let resolved = primary_worktree(primary.path())
            .expect("primary worktree resolves to its own repo root");

        assert_eq!(
            resolved, primary_root,
            "from the primary worktree, detection must resolve to that same primary root"
        );
    }

    #[test]
    fn resolves_main_root_from_nested_subdir_of_linked_worktree() {
        let primary = init_primary();
        let primary_root = canonical(primary.path());

        let linked_parent = TempDir::new().unwrap();
        let linked_path = linked_parent.path().join("linked");
        run_git(
            primary.path(),
            &[
                "worktree",
                "add",
                linked_path.to_str().expect("linked path is utf8"),
                "-b",
                "feature",
            ],
        );
        let nested = canonical(&linked_path).join("nested");
        std::fs::create_dir_all(&nested).expect("create nested subdir in linked worktree");

        let resolved = primary_worktree(&nested)
            .expect("a subdir of a linked worktree resolves to the primary root");

        assert_eq!(
            resolved, primary_root,
            "detection must walk up from a nested start dir and still find the MAIN root"
        );
    }

    #[test]
    fn errors_on_bare_repo_without_working_tree() {
        let bare = TempDir::new().unwrap();
        run_git(bare.path(), &["init", "--bare", "."]);

        let result = primary_worktree(bare.path());

        let err = result.expect_err("a bare repo has no primary worktree to back-reference");
        match err {
            Error::Worktree(msg) => {
                let msg = msg.to_lowercase();
                assert!(
                    !msg.contains("not implemented"),
                    "error must be a real bare-repo diagnostic, not the unimplemented stub, got: {msg}"
                );
                assert!(
                    msg.contains("bare")
                        || msg.contains("working tree")
                        || msg.contains("work dir"),
                    "bare-repo error must explain there is no working tree, got: {msg}"
                );
            }
            other => panic!("expected Error::Worktree, got {other:?}"),
        }
    }

    #[test]
    fn errors_when_not_inside_any_repository() {
        let outside = TempDir::new().unwrap();

        let result = primary_worktree(outside.path());

        let err = result.expect_err("a path outside any git repo has no primary worktree");
        match err {
            Error::Worktree(msg) => {
                let msg = msg.to_lowercase();
                assert!(
                    !msg.contains("not implemented"),
                    "error must be a real not-a-repo diagnostic, not the unimplemented stub, got: {msg}"
                );
                assert!(
                    msg.contains("repository") || msg.contains("repo") || msg.contains("discover"),
                    "not-a-repo error must explain the path is not inside a git repository, got: {msg}"
                );
            }
            other => panic!("expected Error::Worktree, got {other:?}"),
        }
    }
}
