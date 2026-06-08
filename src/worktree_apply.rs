//! Stateless, idempotent, atomic placement engine for `phora worktree apply`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{Include, IncludeMode};
use crate::error::{Error, Result};
use crate::projection::{copy_file, copy_tree};
use crate::worktree::{IndexKind, index_kind, is_submodule};

/// Why a single include was not placed (skipped) by [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The include path is a committed (tracked) regular file; placing would shadow it.
    TrackedRefused,
    /// The symlink at the destination already points at the canonical source.
    AlreadyCorrect,
    /// A real (non-symlink) file or dir occupies the destination.
    Conflict,
    /// The source under the primary does not exist.
    MissingSource,
    /// `copy` mode was requested for a gitlink (submodule) entry.
    SubmoduleCopyUnsupported,
}

/// Per-include outcome of an [`apply`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryOutcome {
    /// The destination was materialized (symlink created/re-pointed, or copy made).
    Placed,
    /// The destination was left as-is for the given reason.
    Skipped(SkipReason),
    /// Placement of this entry failed; the run continued with the remaining entries.
    Failed(String),
}

/// One include's path paired with the decision the engine reached for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryReport {
    pub path: PathBuf,
    pub outcome: EntryOutcome,
}

/// The result of applying every configured include in one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub entries: Vec<EntryReport>,
    /// `true` when the run was a no-op because it targeted the primary worktree.
    pub primary_noop: bool,
}

impl ApplyReport {
    /// `true` iff any entry was [`EntryOutcome::Failed`].
    #[must_use]
    pub fn had_failures(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e.outcome, EntryOutcome::Failed(_)))
    }
}

/// Materializes each configured include at `<worktree_root>/<path>` from
/// `<primary_root>/<path>`, statelessly and idempotently. In the primary
/// worktree this is a no-op. A per-include placement failure is recorded as
/// [`EntryOutcome::Failed`] and the run continues.
///
/// # Errors
///
/// Per-include failures are recorded as [`EntryOutcome::Failed`]; this function
/// does not currently return an error.
pub fn apply(
    worktree_root: &Path,
    primary_root: &Path,
    repo: &gix::Repository,
    includes: &[Include],
) -> Result<ApplyReport> {
    if is_primary(worktree_root, primary_root) {
        return Ok(ApplyReport {
            entries: Vec::new(),
            primary_noop: true,
        });
    }

    let mut entries = Vec::with_capacity(includes.len());
    for include in includes {
        let outcome = match apply_one(worktree_root, primary_root, repo, include) {
            Ok(outcome) => outcome,
            Err(e) => EntryOutcome::Failed(e.to_string()),
        };
        entries.push(EntryReport {
            path: include.path.clone(),
            outcome,
        });
    }
    Ok(ApplyReport {
        entries,
        primary_noop: false,
    })
}

fn is_primary(worktree_root: &Path, primary_root: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(worktree_root) == canon(primary_root)
}

fn apply_one(
    worktree_root: &Path,
    primary_root: &Path,
    repo: &gix::Repository,
    include: &Include,
) -> Result<EntryOutcome> {
    let path = &include.path;
    if index_kind(repo, path)? == IndexKind::Tracked {
        return Ok(EntryOutcome::Skipped(SkipReason::TrackedRefused));
    }

    let src = primary_root.join(path);
    let dst = worktree_root.join(path);

    if is_submodule(repo, path)? {
        return apply_submodule(&src, &dst, include.mode);
    }

    if std::fs::symlink_metadata(&src).is_err() {
        return Ok(EntryOutcome::Skipped(SkipReason::MissingSource));
    }

    match include.mode {
        IncludeMode::Symlink | IncludeMode::SubmoduleWalk => place_leaf_symlink(&dst, &src),
        IncludeMode::Copy => place_leaf_copy(&dst, &src),
    }
}

fn apply_submodule(src: &Path, dst: &Path, mode: IncludeMode) -> Result<EntryOutcome> {
    if mode == IncludeMode::Copy {
        return Ok(EntryOutcome::Skipped(SkipReason::SubmoduleCopyUnsupported));
    }
    if std::fs::symlink_metadata(src).is_err() {
        return Ok(EntryOutcome::Skipped(SkipReason::MissingSource));
    }
    match mode {
        IncludeMode::SubmoduleWalk => apply_submodule_walk(src, dst),
        IncludeMode::Symlink | IncludeMode::Copy => place_leaf_symlink(dst, src),
    }
}

fn apply_submodule_walk(src: &Path, dst: &Path) -> Result<EntryOutcome> {
    create_dir_all(dst)?;
    let mut all_correct = true;
    for entry in read_dir(src)? {
        let entry = entry
            .map_err(|e| Error::Projection(format!("read entry in {}: {e}", src.display())))?;
        if entry.file_name() == ".git" {
            continue;
        }
        let leaf_src = entry.path();
        let leaf_dst = dst.join(entry.file_name());
        match symlink_state(&leaf_dst, &leaf_src) {
            SymlinkState::AlreadyCorrect => {}
            SymlinkState::Conflict => return Ok(EntryOutcome::Skipped(SkipReason::Conflict)),
            SymlinkState::Place => {
                place_symlink_atomic(&leaf_dst, &leaf_src)?;
                all_correct = false;
            }
        }
    }
    if all_correct {
        Ok(EntryOutcome::Skipped(SkipReason::AlreadyCorrect))
    } else {
        Ok(EntryOutcome::Placed)
    }
}

fn place_leaf_symlink(dst: &Path, src: &Path) -> Result<EntryOutcome> {
    match symlink_state(dst, src) {
        SymlinkState::AlreadyCorrect => Ok(EntryOutcome::Skipped(SkipReason::AlreadyCorrect)),
        SymlinkState::Conflict => Ok(EntryOutcome::Skipped(SkipReason::Conflict)),
        SymlinkState::Place => {
            create_parent(dst)?;
            place_symlink_atomic(dst, src)?;
            Ok(EntryOutcome::Placed)
        }
    }
}

fn place_leaf_copy(dst: &Path, src: &Path) -> Result<EntryOutcome> {
    if let Ok(meta) = std::fs::symlink_metadata(dst)
        && !meta.file_type().is_symlink()
    {
        return Ok(EntryOutcome::Skipped(SkipReason::Conflict));
    }
    create_parent(dst)?;
    let temp = temp_sibling(dst)?;
    let src_is_dir = std::fs::symlink_metadata(src)
        .map_err(|e| Error::Projection(format!("stat {}: {e}", src.display())))?
        .file_type()
        .is_dir();
    let copied = if src_is_dir {
        copy_tree(src, &temp, true)
    } else {
        copy_file(src, &temp)
    };
    if let Err(e) = copied {
        if src_is_dir {
            let _ = std::fs::remove_dir_all(&temp);
        } else {
            let _ = std::fs::remove_file(&temp);
        }
        return Err(e);
    }
    std::fs::rename(&temp, dst).map_err(|e| {
        Error::Projection(format!(
            "rename {} -> {}: {e}",
            temp.display(),
            dst.display()
        ))
    })?;
    Ok(EntryOutcome::Placed)
}

enum SymlinkState {
    AlreadyCorrect,
    Conflict,
    Place,
}

fn symlink_state(dst: &Path, target: &Path) -> SymlinkState {
    match std::fs::symlink_metadata(dst) {
        Err(_) => SymlinkState::Place,
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(dst) {
            Ok(current) if current == target => SymlinkState::AlreadyCorrect,
            _ => SymlinkState::Place,
        },
        Ok(_) => SymlinkState::Conflict,
    }
}

/// Atomically places an absolute symlink at `dst` pointing to `target`
/// (temp symlink in `dst`'s parent + rename), so any existing symlink at `dst`
/// is replaced without `EEXIST`.
///
/// # Errors
///
/// Returns [`Error::Projection`] when the temp symlink cannot be created or the
/// rename into `dst` fails.
pub fn place_symlink_atomic(dst: &Path, target: &Path) -> Result<()> {
    let temp = temp_sibling(dst)?;
    symlink(target, &temp).map_err(|e| {
        Error::Projection(format!(
            "symlink {} -> {}: {e}",
            temp.display(),
            target.display()
        ))
    })?;
    std::fs::rename(&temp, dst).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Error::Projection(format!(
            "rename {} -> {}: {e}",
            temp.display(),
            dst.display()
        ))
    })
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    if std::fs::metadata(target)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

fn temp_sibling(dst: &Path) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = dst
        .parent()
        .ok_or_else(|| Error::Projection(format!("no parent for {}", dst.display())))?;
    let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(parent.join(format!(".phora-tmp-{pid}-{nonce}")))
}

fn create_parent(dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        create_dir_all(parent)?;
    }
    Ok(())
}

fn create_dir_all(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Projection(format!("create dir {}: {e}", dir.display())))
}

fn read_dir(dir: &Path) -> Result<std::fs::ReadDir> {
    std::fs::read_dir(dir)
        .map_err(|e| Error::Projection(format!("read dir {}: {e}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use crate::config::{Include, IncludeMode};

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
        reason = "canonicalizing an existing temp path cannot fail in tests"
    )]
    fn canonical(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap()
    }

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

    /// `(primary tempdir, linked-parent tempdir, linked worktree root)`.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn primary_and_linked() -> (TempDir, TempDir, PathBuf) {
        let primary = init_primary();
        let linked_parent = TempDir::new().unwrap();
        let linked_path = linked_parent.path().join("linked");
        run_git(
            primary.path(),
            &[
                "worktree",
                "add",
                linked_path.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );
        (primary, linked_parent, canonical(&linked_path))
    }

    #[expect(
        clippy::unwrap_used,
        reason = "the linked worktree is a real repo gix can discover in tests"
    )]
    fn open(path: &Path) -> gix::Repository {
        gix::discover(path).unwrap()
    }

    fn include(path: &str, mode: IncludeMode) -> Include {
        Include {
            path: PathBuf::from(path),
            mode,
        }
    }

    fn outcome_for<'a>(report: &'a ApplyReport, path: &str) -> &'a EntryOutcome {
        &report
            .entries
            .iter()
            .find(|e| e.path == Path::new(path))
            .unwrap_or_else(|| panic!("no entry report for include `{path}`"))
            .outcome
    }

    /// Stages a 160000 gitlink at `rel` in the LINKED WORKTREE index (uncommitted,
    /// so git never checks it out on disk) and creates the real source dir under
    /// the primary with one leaf. The asserts pin both invariants.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn add_gitlink_with_worktree(
        primary: &Path,
        worktree: &Path,
        rel: &str,
        leaf: &str,
        body: &[u8],
    ) {
        let sha = String::from_utf8(run_git(primary, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        run_git(
            worktree,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},{rel}"),
            ],
        );
        let staged = run_git(worktree, &["ls-files", "-s", rel]);
        let staged = String::from_utf8_lossy(&staged.stdout);
        assert!(
            staged.starts_with("160000 "),
            "fixture invalid: `{rel}` must be a commit-mode (160000) gitlink in the WORKTREE index, got: {staged}"
        );
        assert!(
            std::fs::symlink_metadata(worktree.join(rel)).is_err(),
            "fixture invalid: staging the gitlink uncommitted must NOT create `{rel}` on disk in the worktree"
        );
        let sub_dir = primary.join(rel);
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(sub_dir.join(leaf), body).unwrap();
    }

    #[test]
    fn symlink_entries_are_absolute_links_to_primary_and_second_apply_is_noop() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join(".envrc"), b"export FOO=bar\n")
            .expect("create source file in primary");

        let repo = open(&worktree);
        let includes = [include(".envrc", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("first apply of a symlink include succeeds");

        let dst = worktree.join(".envrc");
        let link_target = std::fs::read_link(&dst).expect("dst must be a symlink after apply");
        assert!(
            link_target.is_absolute(),
            "the symlink must be ABSOLUTE, got {}",
            link_target.display()
        );
        assert_eq!(
            link_target,
            primary_root.join(".envrc"),
            "the symlink must point at the canonical primary source path"
        );
        assert_eq!(
            outcome_for(&report, ".envrc"),
            &EntryOutcome::Placed,
            "the first apply must report the include as Placed"
        );

        let report2 =
            apply(&worktree, &primary_root, &repo, &includes).expect("second apply is idempotent");
        assert_eq!(
            std::fs::read_link(&dst).expect("dst still a symlink"),
            primary_root.join(".envrc"),
            "a correct symlink must be left untouched on the second apply"
        );
        assert_eq!(
            outcome_for(&report2, ".envrc"),
            &EntryOutcome::Skipped(SkipReason::AlreadyCorrect),
            "the second apply must be a no-op (AlreadyCorrect), not re-place the link"
        );
    }

    #[test]
    fn copy_entries_are_independent_of_primary() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        let src = primary_root.join("local.env");
        std::fs::write(&src, b"SECRET=one\n").expect("create copy source");

        let repo = open(&worktree);
        let includes = [include("local.env", IncludeMode::Copy)];

        let report =
            apply(&worktree, &primary_root, &repo, &includes).expect("copy apply succeeds");
        assert_eq!(outcome_for(&report, "local.env"), &EntryOutcome::Placed);

        let dst = worktree.join("local.env");
        assert!(
            !std::fs::symlink_metadata(&dst)
                .expect("dst exists after copy")
                .file_type()
                .is_symlink(),
            "copy mode must produce a real file, not a symlink"
        );

        std::fs::write(&src, b"SECRET=mutated\n").expect("mutate the primary copy afterwards");
        let copied = std::fs::read(&dst).expect("read the worktree copy");
        assert_eq!(
            copied, b"SECRET=one\n",
            "the worktree copy must be INDEPENDENT of the primary: mutating the primary must not change it"
        );
    }

    #[test]
    fn nested_leaf_links_preserve_parent_and_sibling() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::create_dir_all(primary_root.join(".cmw.local")).expect("create primary parent");
        std::fs::write(
            primary_root.join(".cmw.local").join("config.yaml"),
            b"key: value\n",
        )
        .expect("create nested leaf source");

        let parent = worktree.join(".cmw.local");
        std::fs::create_dir_all(&parent).expect("pre-create the worktree parent dir");
        std::fs::write(parent.join("sibling.txt"), b"local-only\n")
            .expect("pre-create a real local sibling file");

        let repo = open(&worktree);
        let includes = [include(".cmw.local/config.yaml", IncludeMode::Symlink)];

        let report =
            apply(&worktree, &primary_root, &repo, &includes).expect("nested-leaf apply succeeds");
        assert_eq!(
            outcome_for(&report, ".cmw.local/config.yaml"),
            &EntryOutcome::Placed
        );

        let leaf = parent.join("config.yaml");
        assert_eq!(
            std::fs::read_link(&leaf).expect("leaf must be a symlink"),
            primary_root.join(".cmw.local").join("config.yaml"),
            "only the leaf must be linked, pointing at the primary's nested leaf"
        );
        assert!(
            !std::fs::symlink_metadata(&parent)
                .expect("parent exists")
                .file_type()
                .is_symlink(),
            "the parent dir must be a REAL directory, never a symlink to the primary"
        );
        assert_eq!(
            std::fs::read(parent.join("sibling.txt")).expect("sibling still present"),
            b"local-only\n",
            "the pre-existing local-only sibling must be left untouched"
        );
    }

    #[test]
    fn submodule_default_mode_single_dir_symlink() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        add_gitlink_with_worktree(
            &primary_root,
            &worktree,
            "vendor",
            "lib.rs",
            b"// vendored\n",
        );

        let repo = open(&worktree);
        let includes = [include("vendor", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("submodule default-mode apply succeeds");
        assert_eq!(outcome_for(&report, "vendor"), &EntryOutcome::Placed);

        let dst = worktree.join("vendor");
        assert_eq!(
            std::fs::read_link(&dst).expect("vendor must be a single symlink"),
            primary_root.join("vendor"),
            "default-mode submodule must be ONE absolute dir-symlink to the primary submodule worktree"
        );
    }

    #[test]
    fn submodule_walk_places_per_leaf_excludes_dot_git_and_is_idempotent() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        add_gitlink_with_worktree(
            &primary_root,
            &worktree,
            "vendor",
            "lib.rs",
            b"// vendored\n",
        );
        let vendor_src = primary_root.join("vendor");
        std::fs::write(vendor_src.join("mod.rs"), b"// second leaf\n")
            .expect("create a second submodule leaf");
        std::fs::write(vendor_src.join(".git"), b"gitdir: ../.git/modules/vendor\n")
            .expect("create a submodule .git pointer file in the primary submodule worktree");

        let dst = worktree.join("vendor");
        std::fs::create_dir_all(&dst).expect("pre-create the worktree submodule dir");
        std::fs::write(dst.join("local-only.txt"), b"keep me\n")
            .expect("pre-place a local-only file in dst");

        let repo = open(&worktree);
        let includes = [include("vendor", IncludeMode::SubmoduleWalk)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("submodule-walk apply succeeds");
        assert_eq!(outcome_for(&report, "vendor"), &EntryOutcome::Placed);

        assert!(
            !std::fs::symlink_metadata(&dst)
                .expect("dst exists")
                .file_type()
                .is_symlink(),
            "submodule-walk dst must be a REAL dir, not a single symlink"
        );
        assert_eq!(
            std::fs::read_link(dst.join("lib.rs")).expect("submodule leaf must be symlinked"),
            vendor_src.join("lib.rs"),
            "each submodule leaf must be individually symlinked to the primary submodule leaf"
        );
        assert_eq!(
            std::fs::read_link(dst.join("mod.rs"))
                .expect("second submodule leaf must be symlinked"),
            vendor_src.join("mod.rs"),
            "every real submodule leaf must be individually symlinked"
        );
        assert!(
            std::fs::symlink_metadata(dst.join(".git")).is_err(),
            "the submodule-walk must NOT create a link for the `.git` pointer (it is excluded)"
        );
        assert_eq!(
            std::fs::read(dst.join("local-only.txt")).expect("local-only file present"),
            b"keep me\n",
            "submodule-walk must PRESERVE local-only files already in dst"
        );

        let report2 = apply(&worktree, &primary_root, &repo, &includes)
            .expect("second submodule-walk apply is idempotent");
        assert_eq!(
            outcome_for(&report2, "vendor"),
            &EntryOutcome::Skipped(SkipReason::AlreadyCorrect),
            "a second submodule-walk apply must be a no-op (AlreadyCorrect), not re-place leaves"
        );
        assert_eq!(
            std::fs::read_link(dst.join("lib.rs")).expect("leaf still a symlink after re-apply"),
            vendor_src.join("lib.rs"),
            "idempotent re-apply must leave each correct leaf symlink unchanged"
        );
        assert!(
            std::fs::symlink_metadata(dst.join(".git")).is_err(),
            "the excluded `.git` pointer must still be absent after a second apply"
        );
    }

    #[test]
    fn copy_mode_on_submodule_warns_and_skips() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        add_gitlink_with_worktree(
            &primary_root,
            &worktree,
            "vendor",
            "lib.rs",
            b"// vendored\n",
        );

        let repo = open(&worktree);
        let includes = [include("vendor", IncludeMode::Copy)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("copy-on-submodule apply succeeds (skip, not error)");
        assert_eq!(
            outcome_for(&report, "vendor"),
            &EntryOutcome::Skipped(SkipReason::SubmoduleCopyUnsupported),
            "copy mode on a gitlink must be skipped as unsupported, not silently copied"
        );
        assert!(
            std::fs::symlink_metadata(worktree.join("vendor")).is_err(),
            "no copy and no (possibly dangling) symlink may be made for an unsupported copy-on-submodule include"
        );
    }

    #[test]
    fn tracked_path_is_refused() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());

        let repo = open(&worktree);
        let includes = [include("README.md", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("tracked-path guard apply succeeds (skip, not error)");
        assert_eq!(
            outcome_for(&report, "README.md"),
            &EntryOutcome::Skipped(SkipReason::TrackedRefused),
            "a committed regular file must be refused by the tracked-path guard"
        );
        assert!(
            !std::fs::symlink_metadata(worktree.join("README.md"))
                .expect("README.md exists in the linked worktree checkout")
                .file_type()
                .is_symlink(),
            "the guard must NOT replace the committed file with a symlink"
        );
        assert_eq!(
            std::fs::read(worktree.join("README.md")).expect("read committed file"),
            b"hello\n",
            "the committed content must be left intact"
        );
    }

    #[test]
    fn stale_symlink_is_repointed() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join(".envrc"), b"export FOO=bar\n").expect("create source");

        let dst = worktree.join(".envrc");
        std::os::unix::fs::symlink(primary_root.join("WRONG-TARGET"), &dst)
            .expect("pre-create a stale symlink pointing at the wrong target");

        let repo = open(&worktree);
        let includes = [include(".envrc", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("stale-symlink apply succeeds");
        assert_eq!(outcome_for(&report, ".envrc"), &EntryOutcome::Placed);
        assert_eq!(
            std::fs::read_link(&dst).expect("dst still a symlink"),
            primary_root.join(".envrc"),
            "a stale symlink with the wrong target must be RE-POINTED at the source"
        );
    }

    #[test]
    fn real_file_at_symlink_path_is_not_clobbered() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join(".envrc"), b"export FOO=bar\n").expect("create source");

        let dst = worktree.join(".envrc");
        std::fs::write(&dst, b"do not clobber\n").expect("pre-create a REAL file at the dst");

        let repo = open(&worktree);
        let includes = [include(".envrc", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("no-clobber apply succeeds (skip, not error)");
        assert_eq!(
            outcome_for(&report, ".envrc"),
            &EntryOutcome::Skipped(SkipReason::Conflict),
            "a real file at a symlink dst must be reported as a conflict/skip"
        );
        assert!(
            !std::fs::symlink_metadata(&dst)
                .expect("dst still exists")
                .file_type()
                .is_symlink(),
            "the real file must NOT be silently replaced by a symlink"
        );
        assert_eq!(
            std::fs::read(&dst).expect("read the preserved file"),
            b"do not clobber\n",
            "the real file's content must be intact"
        );
    }

    #[test]
    fn missing_primary_source_skips_without_dangling_link() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());

        let repo = open(&worktree);
        let includes = [include("absent-in-primary", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("missing-source apply succeeds (skip, not error)");
        assert_eq!(
            outcome_for(&report, "absent-in-primary"),
            &EntryOutcome::Skipped(SkipReason::MissingSource),
            "an include whose source is absent in the primary must be skipped"
        );
        assert!(
            std::fs::symlink_metadata(worktree.join("absent-in-primary")).is_err(),
            "no dangling symlink must be created when the source is missing"
        );
    }

    #[test]
    fn tracked_only_in_worktree_index_is_refused() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join("secret.txt"), b"primary-source\n")
            .expect("create the include source in the primary");

        std::fs::write(worktree.join("secret.txt"), b"worktree-tracked\n")
            .expect("create the file in the worktree so it can be staged there");
        run_git(&worktree, &["update-index", "--add", "secret.txt"]);

        let staged_worktree = run_git(&worktree, &["ls-files", "-s", "secret.txt"]);
        let staged_worktree = String::from_utf8_lossy(&staged_worktree.stdout);
        assert!(
            staged_worktree.starts_with("100644 "),
            "fixture invalid: `secret.txt` must be a regular blob in the WORKTREE index, got: {staged_worktree}"
        );
        let staged_primary = run_git(primary.path(), &["ls-files", "-s", "secret.txt"]);
        assert!(
            staged_primary.stdout.is_empty(),
            "fixture invalid: `secret.txt` must be ABSENT from the PRIMARY index (worktree index is the discriminator)"
        );

        let repo = open(&worktree);
        let includes = [include("secret.txt", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("tracked-in-worktree guard apply succeeds (skip, not error)");
        assert_eq!(
            outcome_for(&report, "secret.txt"),
            &EntryOutcome::Skipped(SkipReason::TrackedRefused),
            "the tracked-path guard must consult the WORKTREE index: a path tracked there must be refused even though it is Absent in the primary index"
        );
        assert!(
            !std::fs::symlink_metadata(worktree.join("secret.txt"))
                .expect("worktree file still exists")
                .file_type()
                .is_symlink(),
            "a path tracked in the worktree index must NOT be replaced with a symlink to the primary"
        );
    }

    #[test]
    fn missing_submodule_source_skips_without_dangling_link() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        let sha = String::from_utf8(run_git(&worktree, &["rev-parse", "HEAD"]).stdout)
            .expect("HEAD sha is utf8")
            .trim()
            .to_owned();
        run_git(
            &worktree,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},gone"),
            ],
        );
        let staged = run_git(&worktree, &["ls-files", "-s", "gone"]);
        let staged = String::from_utf8_lossy(&staged.stdout);
        assert!(
            staged.starts_with("160000 "),
            "fixture invalid: `gone` must be a 160000 gitlink in the WORKTREE index, got: {staged}"
        );
        assert!(
            std::fs::symlink_metadata(primary_root.join("gone")).is_err(),
            "fixture invalid: the submodule source `gone` must NOT exist under the primary"
        );

        let repo = open(&worktree);
        let includes = [
            include("gone", IncludeMode::Symlink),
            include("gone", IncludeMode::SubmoduleWalk),
        ];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("missing-submodule-source apply succeeds (skip, not error)");
        assert_eq!(
            outcome_for(&report, "gone"),
            &EntryOutcome::Skipped(SkipReason::MissingSource),
            "a submodule whose source is absent under the primary must be skipped as MissingSource, not produce a dangling link or hard-error"
        );
        assert!(
            std::fs::symlink_metadata(worktree.join("gone")).is_err(),
            "no dangling symlink (and no walk dir) may be created for a missing submodule source"
        );
    }

    #[test]
    fn copy_mode_directory_with_symlink_content_succeeds() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        let src = primary_root.join("treelink");
        std::fs::create_dir(&src).expect("create dir source for copy");
        std::fs::write(src.join("realfile"), b"payload\n")
            .expect("create a regular file in source");
        std::os::unix::fs::symlink("realfile", src.join("link"))
            .expect("create a symlink inside the source tree");

        let repo = open(&worktree);
        let includes = [include("treelink", IncludeMode::Copy)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("copy mode on a dir source containing a symlink must SUCCEED, not hard-error");
        assert_eq!(
            outcome_for(&report, "treelink"),
            &EntryOutcome::Placed,
            "a copy-mode directory whose tree contains a symlink must be Placed, not refused by an allow_symlinks=false walk"
        );

        let dst = worktree.join("treelink");
        assert!(
            std::fs::symlink_metadata(&dst)
                .expect("dst exists after copy")
                .file_type()
                .is_dir(),
            "copy mode must produce a REAL directory at the destination"
        );
        assert_eq!(
            std::fs::read(dst.join("realfile")).expect("regular file copied"),
            b"payload\n",
            "the regular file content must be copied into the worktree tree"
        );
    }

    /// Every path that exists under `root`, recursively, as a set (no-follow).
    #[expect(
        clippy::unwrap_used,
        reason = "walking an existing temp tree in tests cannot fail"
    )]
    fn path_set(root: &Path) -> std::collections::BTreeSet<PathBuf> {
        let mut out = std::collections::BTreeSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                out.insert(path.clone());
                if std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_dir()
                {
                    stack.push(path);
                }
            }
        }
        out
    }

    #[test]
    fn apply_writes_only_intended_targets_and_no_state_side_writes() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join(".envrc"), b"export FOO=bar\n").expect("create source");

        let repo = open(&worktree);
        let includes = [include(".envrc", IncludeMode::Symlink)];

        let before_worktree = path_set(&worktree);
        let before_primary = path_set(&primary_root);

        apply(&worktree, &primary_root, &repo, &includes).expect("stateless apply succeeds");

        let after_worktree = path_set(&worktree);
        let after_primary = path_set(&primary_root);

        let new_worktree: std::collections::BTreeSet<_> = after_worktree
            .difference(&before_worktree)
            .cloned()
            .collect();
        let expected: std::collections::BTreeSet<_> =
            std::iter::once(worktree.join(".envrc")).collect();
        assert_eq!(
            new_worktree, expected,
            "apply must create EXACTLY the intended include target under the worktree and nothing else (no registry/lock/state side-writes)"
        );

        assert_eq!(
            after_primary, before_primary,
            "apply must never write anything under the primary repo"
        );
    }

    #[test]
    fn place_symlink_atomic_creates_absolute_link() {
        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"x\n").expect("create target file");
        let dst = dir.path().join("link");

        place_symlink_atomic(&dst, &target).expect("placing an atomic symlink succeeds");

        assert_eq!(
            std::fs::read_link(&dst).expect("dst is a symlink"),
            target,
            "place_symlink_atomic must create a symlink at dst pointing exactly at target"
        );
    }

    #[test]
    fn place_symlink_atomic_overwrites_existing_stale_symlink() {
        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"x\n").expect("create target file");
        let dst = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("WRONG"), &dst)
            .expect("pre-create a stale symlink pointing at the wrong target");

        place_symlink_atomic(&dst, &target).expect(
            "placing over an existing symlink must succeed (temp+rename), not error EEXIST",
        );

        assert_eq!(
            std::fs::read_link(&dst).expect("dst is still a symlink"),
            target,
            "place_symlink_atomic must OVERWRITE a stale symlink so it points at the new target"
        );
    }

    #[test]
    fn place_symlink_atomic_links_a_directory_target() {
        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("target-dir");
        std::fs::create_dir(&target).expect("create target dir");
        std::fs::write(target.join("inside.txt"), b"y\n").expect("create file in target dir");
        let dst = dir.path().join("link");

        place_symlink_atomic(&dst, &target).expect("placing a dir symlink succeeds");

        assert!(
            std::fs::symlink_metadata(&dst)
                .expect("dst exists")
                .file_type()
                .is_symlink(),
            "a directory target must be placed as a symlink, not a real dir"
        );
        assert_eq!(
            std::fs::read_link(&dst).expect("dst is a symlink"),
            target,
            "the dir symlink must point exactly at the target dir"
        );
        assert_eq!(
            std::fs::read(dst.join("inside.txt")).expect("resolve through the dir symlink"),
            b"y\n",
            "the dir symlink must resolve to the target directory's contents"
        );
    }

    #[test]
    fn symlink_mode_on_directory_source_creates_dir_symlink() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        let src = primary_root.join("config-dir");
        std::fs::create_dir(&src).expect("create directory source in primary");
        std::fs::write(src.join("settings.toml"), b"a = 1\n").expect("create file in dir source");

        let repo = open(&worktree);
        let includes = [include("config-dir", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("symlink-on-dir-source apply succeeds");
        assert_eq!(outcome_for(&report, "config-dir"), &EntryOutcome::Placed);

        let dst = worktree.join("config-dir");
        assert!(
            std::fs::symlink_metadata(&dst)
                .expect("dst exists")
                .file_type()
                .is_symlink(),
            "a directory source in symlink mode must be placed as a single dir-symlink"
        );
        assert_eq!(
            std::fs::read_link(&dst).expect("dst is a symlink"),
            src,
            "the dir-symlink must point at the canonical primary directory source"
        );
    }

    #[test]
    fn copy_mode_on_directory_source_copies_tree() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        let src = primary_root.join("tree");
        std::fs::create_dir(&src).expect("create dir source");
        std::fs::write(src.join("top.txt"), b"top\n").expect("create top-level file");
        std::fs::create_dir(src.join("nested")).expect("create nested subdir");
        std::fs::write(src.join("nested").join("leaf.txt"), b"leaf\n").expect("create nested file");

        let repo = open(&worktree);
        let includes = [include("tree", IncludeMode::Copy)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("copy-on-dir-source apply succeeds");
        assert_eq!(outcome_for(&report, "tree"), &EntryOutcome::Placed);

        let dst = worktree.join("tree");
        assert!(
            !std::fs::symlink_metadata(&dst)
                .expect("dst exists")
                .file_type()
                .is_symlink(),
            "copy mode on a dir source must produce a REAL directory, not a symlink"
        );
        assert!(
            std::fs::symlink_metadata(&dst)
                .expect("dst exists")
                .file_type()
                .is_dir(),
            "the copied dst must be a real directory"
        );
        assert_eq!(
            std::fs::read(dst.join("top.txt")).expect("top file copied"),
            b"top\n",
            "the top-level leaf must be copied with its content"
        );
        assert_eq!(
            std::fs::read(dst.join("nested").join("leaf.txt")).expect("nested file copied"),
            b"leaf\n",
            "the nested leaf must be copied with its content"
        );

        std::fs::write(src.join("top.txt"), b"mutated\n").expect("mutate the primary after copy");
        assert_eq!(
            std::fs::read(dst.join("top.txt")).expect("read copied top file"),
            b"top\n",
            "the copied tree must be INDEPENDENT of the primary: later primary mutation must not change it"
        );
    }

    #[test]
    fn apply_in_primary_worktree_is_noop() {
        let (primary, _linked_parent, _worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join(".envrc"), b"export FOO=bar\n")
            .expect("create a source that WOULD be placed if apply did not no-op");

        let repo = open(&primary_root);
        let includes = [include(".envrc", IncludeMode::Symlink)];

        let report = apply(&primary_root, &primary_root, &repo, &includes)
            .expect("apply in the primary worktree must succeed as a no-op");

        assert!(
            report.primary_noop,
            "apply where worktree_root == primary_root must flag the run as a primary no-op"
        );
        assert!(
            report.entries.is_empty(),
            "a primary no-op must place nothing: entries must be empty, got {:?}",
            report.entries
        );

        let meta = std::fs::symlink_metadata(primary_root.join(".envrc"))
            .expect("the original source file must still exist in the primary");
        assert!(
            !meta.file_type().is_symlink(),
            "apply must NEVER symlink the primary's own include path onto itself"
        );
        assert!(!report.had_failures(), "a primary no-op is not a failure");
    }

    #[test]
    fn entry_failure_warns_continues_and_flags_partial_failure() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());

        std::fs::create_dir(primary_root.join("blocked"))
            .expect("create the primary parent for the nested first include");
        std::fs::write(primary_root.join("blocked").join("leaf"), b"first\n")
            .expect("create the nested first source");
        std::fs::write(primary_root.join("second.env"), b"SECOND=ok\n")
            .expect("create the second source");

        std::fs::write(worktree.join("blocked"), b"i am a regular file\n")
            .expect("pre-place a REGULAR FILE where the first include's parent dir must be created, so create_dir_all fails deterministically");

        let repo = open(&worktree);
        let includes = [
            include("blocked/leaf", IncludeMode::Symlink),
            include("second.env", IncludeMode::Symlink),
        ];

        let report = apply(&worktree, &primary_root, &repo, &includes).expect(
            "a per-entry placement failure must NOT abort apply: the run must still return Ok",
        );

        assert!(
            matches!(
                outcome_for(&report, "blocked/leaf"),
                EntryOutcome::Failed(_)
            ),
            "the first include must be recorded as Failed (its parent dir cannot be created), got {:?}",
            outcome_for(&report, "blocked/leaf")
        );
        assert_eq!(
            outcome_for(&report, "second.env"),
            &EntryOutcome::Placed,
            "the second include must still be Placed, proving apply continued after the first entry failed"
        );
        assert!(
            report.had_failures(),
            "had_failures() must be true when any entry Failed, so the CLI can exit non-zero"
        );

        assert_eq!(
            std::fs::read_link(worktree.join("second.env"))
                .expect("second include must be a symlink on disk"),
            primary_root.join("second.env"),
            "the successful second include's symlink must actually exist on disk after the partial failure"
        );
    }

    #[test]
    fn missing_primary_source_warns_and_skips() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());

        let repo = open(&worktree);
        let includes = [include("absent-in-primary", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("a missing primary source must be a skip, not an error");

        assert_eq!(
            outcome_for(&report, "absent-in-primary"),
            &EntryOutcome::Skipped(SkipReason::MissingSource),
            "an include whose primary source is absent must be Skipped(MissingSource)"
        );
        assert!(
            std::fs::symlink_metadata(worktree.join("absent-in-primary")).is_err(),
            "a missing source must leave no dangling link in the worktree"
        );
        assert!(
            !report.had_failures(),
            "a missing source is a SKIP, not a failure: had_failures() must be false"
        );
    }

    #[test]
    fn report_without_failures_reports_no_partial_failure() {
        let (primary, _linked_parent, worktree) = primary_and_linked();
        let primary_root = canonical(primary.path());
        std::fs::write(primary_root.join(".envrc"), b"export FOO=bar\n").expect("create source");

        let repo = open(&worktree);
        let includes = [include(".envrc", IncludeMode::Symlink)];

        let report = apply(&worktree, &primary_root, &repo, &includes)
            .expect("an all-success apply must succeed");

        assert_eq!(outcome_for(&report, ".envrc"), &EntryOutcome::Placed);
        assert!(
            !report.primary_noop,
            "a normal (non-primary) apply must not flag primary_noop"
        );
        assert!(
            !report.had_failures(),
            "an all-success apply must NOT report a partial failure (guards the exit-code signal against false positives)"
        );
    }
}
