//! Primary (main, non-linked) worktree resolution.

use std::path::{Path, PathBuf};

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::Mode;

use crate::error::{Error, Result};

/// Classification of a repo-relative path against the worktree index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    Absent,
    Tracked,
    Submodule,
}

/// Classifies a repo-relative path against the repository's worktree index.
///
/// # Errors
///
/// Returns [`Error::Worktree`] when the index cannot be opened or read.
pub fn index_kind(repo: &gix::Repository, rel_path: &Path) -> Result<IndexKind> {
    let index = repo
        .index()
        .map_err(|e| Error::Worktree(format!("open worktree index: {e}")))?;

    let key = forward_slash_key(rel_path);
    let Some(entry) = index.entry_by_path(key.as_bstr()) else {
        return Ok(IndexKind::Absent);
    };

    if entry.mode == Mode::COMMIT {
        Ok(IndexKind::Submodule)
    } else {
        Ok(IndexKind::Tracked)
    }
}

fn forward_slash_key(rel_path: &Path) -> BString {
    let bytes: Vec<u8> = rel_path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|&b| if b == b'\\' { b'/' } else { b })
        .collect();
    let mut key = bytes.as_slice();
    while let Some(rest) = key.strip_prefix(b"./") {
        key = rest;
    }
    BString::from(key)
}

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

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn head_sha(cwd: &Path) -> String {
        let out = run_git(cwd, &["rev-parse", "HEAD"]);
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; gix is assumed to open the temp repo"
    )]
    fn open(path: &Path) -> gix::Repository {
        gix::discover(path).unwrap()
    }

    #[test]
    fn forward_slash_key_normalizes_separators_and_dot_prefix() {
        assert_eq!(
            forward_slash_key(Path::new("./a/b")).as_bstr(),
            BString::from(&b"a/b"[..]).as_bstr(),
            "a single leading `./` must be stripped from the index key"
        );
        assert_eq!(
            forward_slash_key(Path::new("././a")).as_bstr(),
            BString::from(&b"a"[..]).as_bstr(),
            "every leading `./` segment must be stripped, not just the first"
        );
        assert_eq!(
            forward_slash_key(Path::new("a\\b")).as_bstr(),
            BString::from(&b"a/b"[..]).as_bstr(),
            "backslashes must be transformed to forward slashes at the byte level"
        );
    }

    #[test]
    fn committed_regular_file_reads_tracked() {
        let primary = init_primary();
        let repo = open(primary.path());

        let kind = index_kind(&repo, Path::new("README.md"))
            .expect("classifying a committed file must succeed");

        assert_eq!(
            kind,
            IndexKind::Tracked,
            "a committed regular file must classify as Tracked so the guard refuses it"
        );
    }

    #[test]
    fn tracked_nested_path_reads_tracked() {
        let primary = init_primary();
        std::fs::create_dir_all(primary.path().join("nested"))
            .expect("create nested dir for a deep tracked path");
        std::fs::write(primary.path().join("nested").join("file.txt"), b"deep\n")
            .expect("create file at nested path");
        run_git(primary.path(), &["add", "nested/file.txt"]);
        run_git(primary.path(), &["commit", "-m", "add nested file"]);

        let staged = run_git(primary.path(), &["ls-files", "nested/file.txt"]);
        let staged = String::from_utf8_lossy(&staged.stdout);
        assert_eq!(
            staged.trim(),
            "nested/file.txt",
            "fixture invalid: `nested/file.txt` must actually be in the index"
        );

        let repo = open(primary.path());
        let kind = index_kind(&repo, Path::new("nested/file.txt"))
            .expect("classifying a committed nested path must succeed");

        assert_eq!(
            kind,
            IndexKind::Tracked,
            "a committed file at a multi-component repo-relative path must classify as Tracked (forward-slash index key, not OS-separator)"
        );
    }

    #[test]
    fn gitlink_entry_reads_submodule_not_tracked() {
        let primary = init_primary();
        let sha = head_sha(primary.path());
        run_git(
            primary.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},sub"),
            ],
        );

        let staged = run_git(primary.path(), &["ls-files", "-s", "sub"]);
        let staged = String::from_utf8_lossy(&staged.stdout);
        assert!(
            staged.starts_with("160000 "),
            "fixture invalid: `sub` must be a commit-mode (160000) gitlink entry, got: {staged}"
        );

        let repo = open(primary.path());
        let kind =
            index_kind(&repo, Path::new("sub")).expect("classifying a gitlink path must succeed");

        assert_eq!(
            kind,
            IndexKind::Submodule,
            "a gitlink (commit-mode) entry must classify as Submodule, NOT Tracked, so the guard allows it"
        );
    }

    #[test]
    fn untracked_gitignored_present_path_reads_absent() {
        let primary = init_primary();
        std::fs::write(
            primary.path().join(".gitignore"),
            b"mise.local.toml\n.codex/\n",
        )
        .expect("write .gitignore");
        std::fs::write(primary.path().join("mise.local.toml"), b"local = true\n")
            .expect("create untracked local file");
        std::fs::create_dir_all(primary.path().join(".codex")).expect("create untracked local dir");
        std::fs::write(primary.path().join(".codex").join("config"), b"x\n")
            .expect("create file inside untracked local dir");

        let repo = open(primary.path());

        let file_kind = index_kind(&repo, Path::new("mise.local.toml"))
            .expect("classifying an untracked-but-present file must succeed");
        assert_eq!(
            file_kind,
            IndexKind::Absent,
            "an untracked gitignored file present on disk must read as Absent (the motivating local-only case)"
        );

        let dir_kind = index_kind(&repo, Path::new(".codex"))
            .expect("classifying an untracked-but-present dir must succeed");
        assert_eq!(
            dir_kind,
            IndexKind::Absent,
            "an untracked gitignored dir present on disk must read as Absent so the guard allows the include"
        );

        let tracked_kind = index_kind(&repo, Path::new("README.md"))
            .expect("classifying the committed file in this same repo must succeed");
        assert_eq!(
            tracked_kind,
            IndexKind::Tracked,
            "a genuinely tracked file must still read Tracked here, so an impl that short-circuits on disk presence (Absent for everything) is caught"
        );
    }

    #[test]
    fn tracked_path_deleted_from_disk_still_reads_tracked() {
        let primary = init_primary();
        std::fs::remove_file(primary.path().join("README.md"))
            .expect("remove the tracked file from disk only");

        let repo = open(primary.path());
        let kind = index_kind(&repo, Path::new("README.md"))
            .expect("classifying a deleted-on-disk tracked file must succeed");

        assert_eq!(
            kind,
            IndexKind::Tracked,
            "a path still in the index must read Tracked even when removed from the working tree"
        );
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
