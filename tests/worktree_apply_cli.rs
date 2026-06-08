//! Acceptance oracle: drive the real `phora worktree apply` binary end-to-end.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git CLI is available");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize existing path")
}

fn phora(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("phora binary runs")
}

fn exit_code(out: &Output) -> i32 {
    out.status.code().expect("process exited with a code")
}

/// What lives at `path`: a symlink (with its target), a regular file, or absent.
enum Placement {
    Symlink(PathBuf),
    RegularFile,
    Missing,
}

fn placement(path: &Path) -> Placement {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Placement::Symlink(std::fs::read_link(path).expect("read symlink target"))
        }
        Ok(_) => Placement::RegularFile,
        Err(_) => Placement::Missing,
    }
}

/// A primary checkout (`<dir>/primary`) plus a linked worktree (`<dir>/linked`).
struct Fixture {
    _root: TempDir,
    primary: PathBuf,
    linked: PathBuf,
}

/// Builds a two-worktree repo whose committed `phora.toml` declares the given
/// `[[worktree.includes]]` body, plus an untracked `.envrc` in the primary that
/// serves as the symlink/copy source.
fn fixture_with_includes(includes_toml: &str) -> Fixture {
    let root = TempDir::new().expect("temp root");
    let primary = root.path().join("primary");
    std::fs::create_dir(&primary).expect("mkdir primary");

    git(&primary, &["init", "-b", "main", "."]);
    git(&primary, &["config", "user.email", "test@example.com"]);
    git(&primary, &["config", "user.name", "Test"]);

    std::fs::write(primary.join("README.md"), b"hello\n").expect("write README");
    std::fs::write(primary.join(".gitignore"), b".envrc\n").expect("write .gitignore");
    std::fs::write(primary.join(".envrc"), b"export SECRET=42\n").expect("write .envrc");

    let phora_toml = format!("version = 1\n\n[worktree]\n{includes_toml}");
    std::fs::write(primary.join("phora.toml"), phora_toml).expect("write phora.toml");

    git(&primary, &["add", "README.md", ".gitignore", "phora.toml"]);
    git(&primary, &["commit", "-m", "initial"]);

    let linked = root.path().join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            linked.to_str().expect("linked path utf8"),
            "-b",
            "feature",
        ],
    );

    Fixture {
        _root: root,
        primary,
        linked,
    }
}

const ENVRC_SYMLINK: &str = "[[worktree.includes]]\npath = \".envrc\"\nmode = \"symlink\"\n";

#[test]
fn apply_creates_absolute_symlink_to_primary() {
    let fx = fixture_with_includes(ENVRC_SYMLINK);

    let out = phora(&fx.linked, &["worktree", "apply"]);

    assert_eq!(
        exit_code(&out),
        0,
        "`phora worktree apply` in a linked worktree must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    match placement(&fx.linked.join(".envrc")) {
        Placement::Symlink(target) => assert_eq!(
            canonical(&target),
            canonical(&fx.primary.join(".envrc")),
            "the linked worktree's .envrc must be a symlink pointing at the primary's real .envrc"
        ),
        Placement::RegularFile => {
            panic!("expected a symlink at <linked>/.envrc, found a regular file")
        }
        Placement::Missing => panic!("expected a symlink at <linked>/.envrc, found nothing"),
    }
}

#[test]
fn second_apply_is_idempotent_exit_zero() {
    let fx = fixture_with_includes(ENVRC_SYMLINK);

    let first = phora(&fx.linked, &["worktree", "apply"]);
    assert_eq!(exit_code(&first), 0, "first apply must exit 0");
    let Placement::Symlink(after_first) = placement(&fx.linked.join(".envrc")) else {
        panic!("first apply must create a symlink at <linked>/.envrc")
    };

    let second = phora(&fx.linked, &["worktree", "apply"]);
    assert_eq!(
        exit_code(&second),
        0,
        "a second apply over an already-correct link must still exit 0; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let Placement::Symlink(after_second) = placement(&fx.linked.join(".envrc")) else {
        panic!("second apply must leave a symlink at <linked>/.envrc")
    };
    assert_eq!(
        after_first, after_second,
        "an idempotent re-apply must leave the symlink target unchanged"
    );
}

#[test]
fn apply_in_primary_is_noop() {
    let fx = fixture_with_includes(ENVRC_SYMLINK);

    let out = phora(&fx.primary, &["worktree", "apply"]);

    assert_eq!(
        exit_code(&out),
        0,
        "apply in the primary worktree must exit 0 (no-op); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    match placement(&fx.primary.join(".envrc")) {
        Placement::RegularFile => {}
        Placement::Symlink(_) => {
            panic!("apply in the primary must NOT replace the real .envrc with a symlink")
        }
        Placement::Missing => panic!("the primary's real .envrc must survive a no-op apply"),
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("primary") || stderr.contains("no-op"),
        "apply in the primary must announce a primary/no-op notice on stderr, got: {stderr}"
    );
}

#[test]
fn apply_with_path_flag_resolves_target_worktree() {
    let fx = fixture_with_includes(ENVRC_SYMLINK);
    let scratch = TempDir::new().expect("scratch cwd outside both worktrees");

    let out = phora(
        scratch.path(),
        &[
            "worktree",
            "apply",
            fx.linked.to_str().expect("linked path utf8"),
        ],
    );

    assert_eq!(
        exit_code(&out),
        0,
        "apply with a positional worktree path from an unrelated cwd must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    match placement(&fx.linked.join(".envrc")) {
        Placement::Symlink(target) => assert_eq!(
            canonical(&target),
            canonical(&fx.primary.join(".envrc")),
            "the positional path must resolve BOTH config and primary relative to the target \
             worktree, not the process CWD; the symlink must land in <linked> and point at the \
             primary's .envrc"
        ),
        Placement::RegularFile => {
            panic!("expected a symlink at <linked>/.envrc, found a regular file")
        }
        Placement::Missing => panic!(
            "no symlink was created at <linked>/.envrc: config was loaded from the process CWD \
             (which has no phora.toml) instead of the target worktree"
        ),
    }
}

#[test]
fn partial_failure_exits_nonzero() {
    let includes = "[[worktree.includes]]\npath = \".envrc\"\nmode = \"symlink\"\n\n\
         [[worktree.includes]]\npath = \"blocked/leaf\"\nmode = \"symlink\"\n";
    let fx = fixture_with_includes(includes);

    std::fs::write(fx.primary.join(".gitignore"), b".envrc\nblocked\n")
        .expect("ignore blocked source too");
    std::fs::create_dir(fx.primary.join("blocked")).expect("mkdir primary blocked");
    std::fs::write(fx.primary.join("blocked").join("leaf"), b"leaf\n").expect("write leaf source");

    std::fs::write(fx.linked.join("blocked"), b"i am a file, not a dir\n")
        .expect("block the parent dir in the linked worktree");

    let out = phora(&fx.linked, &["worktree", "apply"]);

    assert_eq!(
        exit_code(&out),
        1,
        "a partial failure (one include cannot be placed) must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let Placement::Symlink(target) = placement(&fx.linked.join(".envrc")) else {
        panic!("the valid .envrc include must still be symlinked even when another fails")
    };
    assert_eq!(
        canonical(&target),
        canonical(&fx.primary.join(".envrc")),
        "warn-and-continue: the valid include must still be placed despite the sibling failure"
    );
}
