//! Destination confinement for composed transitive targets (TDEP-CONFINE-001).

use std::path::{Component, Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::config::Paths;
use crate::error::{Error, Result};
use crate::kernel::safe_component;
use crate::paths::{cache_root_for, state_root_for};

/// Named diagnostics owned by confinement; tests assert these exact phrases.
const ANCHOR_SYMLINK: &str = "anchor ancestor is a symlink";
const PROTECTED_PATH: &str = "protected path";

/// Paths a transitive dep must never write, even when they do not yet exist.
pub(super) struct ProtectedPathSet {
    cwd: PathBuf,
    members: Vec<PathBuf>,
}

impl ProtectedPathSet {
    pub(super) fn resolve(paths: &Paths, cwd: &Path) -> Result<Self> {
        let mut members = vec![
            cwd.join("phora.toml"),
            cwd.join("phora.local.toml"),
            cwd.join("phora.lock"),
            cwd.join("phora.local.lock"),
            cwd.join(".git"),
        ];
        members.push(state_root_for(paths.state.as_deref(), cwd)?.join("projects"));
        members.push(cache_root_for(paths.cache.as_deref(), cwd)?);
        Ok(Self {
            cwd: cwd.to_path_buf(),
            members: members
                .iter()
                .map(PathBuf::as_path)
                .map(normalize_lexical)
                .collect(),
        })
    }

    fn protects(&self, path: &Path) -> bool {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let candidate = canonical_lexical(&absolute);
        self.members
            .iter()
            .any(|member| starts_with_components(&candidate, &canonical_lexical(member)))
    }
}

/// Returns the path deploy must write verbatim, or rejects any escape of `anchor`.
pub(super) fn confine_destination(
    anchor: &Path,
    dst: &Path,
    protected: &ProtectedPathSet,
) -> Result<PathBuf> {
    let anchor_norm = normalize_lexical(anchor);
    let dst_norm = normalize_lexical(dst);

    if protected.protects(&dst_norm) {
        return Err(Error::Config(format!(
            "confinement: {PROTECTED_PATH} {} may not be written by a transitive dependency",
            dst.display()
        )));
    }

    let relative = strip_prefix_folded(&dst_norm, &anchor_norm).ok_or_else(|| {
        Error::Config(format!(
            "confinement: destination {} escapes its anchor {}",
            dst.display(),
            anchor.display()
        ))
    })?;
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                safe_component(&name).map_err(|_| {
                    Error::Config(format!(
                        "confinement: destination component `{name}` of {} is not a safe filename",
                        dst.display()
                    ))
                })?;
            }
            _ => {
                return Err(Error::Config(format!(
                    "confinement: destination {} escapes its anchor {}",
                    dst.display(),
                    anchor.display()
                )));
            }
        }
    }

    reject_symlink_ancestor(&anchor_norm, &dst_norm)?;
    Ok(dst_norm)
}

fn reject_symlink_ancestor(anchor: &Path, dst: &Path) -> Result<()> {
    let Some(tail) = strip_prefix_folded(dst, anchor) else {
        return Ok(());
    };
    let mut current = anchor.to_path_buf();
    for component in tail.components() {
        current.push(component);
        reject_if_symlink(&current)?;
    }
    Ok(())
}

fn reject_if_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(Error::Config(format!(
            "confinement: {ANCHOR_SYMLINK}: {} is a symlink and a transitive write \
             must not follow it",
            path.display()
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Residual risk: a cross-process TOCTOU race and hardlink-to-directory canonicalization stay unguarded.
pub(super) fn reject_symlink_ancestor_at_write(anchor: &Path, dst: &Path) -> Result<()> {
    reject_symlink_ancestor(&normalize_lexical(anchor), &normalize_lexical(dst))
}

/// Collapses `.`/`..` without touching the filesystem, so confinement holds for paths that do not yet exist.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Same file via different representations (e.g. `/var` -> `/private/var`) must
/// compare equal; non-canonicalizable or relative paths fall back to lexical.
fn canonical_lexical(path: &Path) -> PathBuf {
    let lexical = normalize_lexical(path);
    if !lexical.is_absolute() {
        return lexical;
    }
    let mut existing = lexical.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(existing) {
            let mut out = canon;
            out.extend(tail.iter().rev());
            return out;
        }
        match existing.parent() {
            Some(parent) if parent != existing => {
                if let Some(name) = existing.file_name() {
                    tail.push(name);
                }
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

fn starts_with_components(path: &Path, prefix: &Path) -> bool {
    strip_prefix_folded(path, prefix).is_some()
}

/// NFC-normalizes, then ASCII-folds only; non-ASCII case-variants are not folded.
fn fold_key(component: &std::ffi::OsStr) -> String {
    component
        .to_string_lossy()
        .nfc()
        .collect::<String>()
        .to_lowercase()
}

pub(crate) fn fold_path(path: &Path) -> PathBuf {
    path.components().map(|c| fold_key(c.as_os_str())).collect()
}

fn strip_prefix_folded(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let mut tail = path.components();
    for pc in prefix.components() {
        match tail.next() {
            Some(tc) if fold_key(tc.as_os_str()) == fold_key(pc.as_os_str()) => {}
            _ => return None,
        }
    }
    Some(tail.as_path().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected(cwd: &Path) -> ProtectedPathSet {
        ProtectedPathSet::resolve(&Paths::default(), cwd).expect("resolve protected set")
    }

    #[test]
    fn confines_a_plain_descendant() {
        let anchor = Path::new("/home/u/.config");
        let dst = anchor.join("nvim/init.lua");
        let set = protected(Path::new("/home/u/proj"));
        let confined = confine_destination(anchor, &dst, &set).expect("descendant confines");
        assert_eq!(confined, dst, "joined_dst must equal confined_dst");
    }

    #[test]
    fn rejects_parent_dir_escape() {
        let anchor = Path::new("/home/u/.config");
        let dst = Path::new("/home/u/.config/../.ssh/authorized_keys");
        let set = protected(Path::new("/home/u/proj"));
        confine_destination(anchor, dst, &set).expect_err("a `..` escape must be rejected");
    }

    #[test]
    fn rejects_prefix_sibling_not_descendant() {
        let anchor = Path::new("/home/u/.config");
        let dst = Path::new("/home/u/.config-unsafe/x");
        let set = protected(Path::new("/home/u/proj"));
        confine_destination(anchor, dst, &set)
            .expect_err("a prefix sibling must fail a component-wise anchor check");
    }

    fn err_message(e: &Error) -> String {
        match e {
            Error::Config(m) => m.clone(),
            other => panic!("expected Error::Config from confinement, got {other:?}"),
        }
    }

    #[test]
    fn returned_dst_is_normalized_canonical_no_raw_dot_segments() {
        let anchor = Path::new("/home/u/.config");
        let composed = Path::new("/home/u/.config/./nvim/./lua/init.lua");
        let canonical = Path::new("/home/u/.config/nvim/lua/init.lua");
        let set = protected(Path::new("/home/u/proj"));

        let confined =
            confine_destination(anchor, composed, &set).expect("a `.`-laden descendant confines");
        assert_eq!(
            confined, canonical,
            "confinement must return the normalized canonical dst (deploy's single source of \
             truth); a raw `.`/`..` segment reaching fs::rename is a TOCTOU hole"
        );
        assert!(
            !confined
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir)),
            "no `.`/`..` component may survive into the dst deploy writes verbatim; got {}",
            confined.display()
        );
    }

    #[test]
    fn staging_base_is_derived_from_confined_dst_not_raw_target() {
        let anchor = Path::new("/home/u/.config");
        let raw = Path::new("/home/u/.config/nvim/./init.lua");
        let set = protected(Path::new("/home/u/proj"));

        let confined = confine_destination(anchor, raw, &set).expect("descendant confines");
        let staging_base = super::super::target_parent(&confined).join(".phora-stage");

        assert_eq!(
            staging_base,
            Path::new("/home/u/.config/nvim/.phora-stage"),
            "staging_base must hang off the confined canonical parent so the rename lands under \
             the anchor; deriving it from the raw target_path would reintroduce the `.` segment"
        );
        assert!(
            starts_with_components(&staging_base, &normalize_lexical(anchor)),
            "the confined-derived staging_base must itself be component-wise under the anchor; \
             got {}",
            staging_base.display()
        );
    }

    #[test]
    fn prefix_sibling_rejection_names_the_escape_and_anchor() {
        let anchor = Path::new("/home/u/.config");
        let dst = Path::new("/home/u/.config-unsafe/payload");
        let set = protected(Path::new("/home/u/proj"));

        let err = confine_destination(anchor, dst, &set)
            .expect_err("`.config-unsafe` must not pass a `.config` anchor");
        let msg = err_message(&err);
        assert!(
            msg.contains("escapes its anchor"),
            "a prefix-sibling escape must emit the named `escapes its anchor` diagnostic (proving \
             a component-wise strip_prefix, not string starts_with); got: {msg}"
        );
        assert!(
            msg.contains(".config-unsafe"),
            "the diagnostic must name the offending destination `.config-unsafe`; got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonicalized_out_of_anchor_ancestor_is_rejected() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().expect("tmp");
        let root = tmp.path();
        let anchor = root.join("anchor");
        std::fs::create_dir_all(&anchor).expect("anchor");
        let escape = root.join("escape");
        std::fs::create_dir_all(&escape).expect("escape");

        symlink(&escape, anchor.join("real")).expect("plant escaping ancestor");
        let dst = anchor.join("real/payload");
        let set = protected(root);

        let err = confine_destination(&anchor, &dst, &set).expect_err(
            "an ancestor whose canonical target escapes the anchor must be rejected; lexical \
             normalization alone cannot see through it",
        );
        let msg = err_message(&err);
        assert!(
            msg.contains(ANCHOR_SYMLINK) || msg.contains("escapes its anchor"),
            "an out-of-anchor real ancestor must be rejected with a confinement diagnostic; \
             got: {msg}"
        );
        assert!(
            !escape.join("payload").exists(),
            "no write may reach the canonical out-of-anchor target {}",
            escape.join("payload").display()
        );
    }

    #[test]
    fn protected_member_caught_under_case_fold_identity() {
        let cwd = Path::new("/home/u/proj");
        let anchor = cwd; // target anchored at the project root
        let set = protected(cwd);
        let cased = cwd.join(".GIT/config");

        let err = confine_destination(anchor, &cased, &set).expect_err(
            "`.GIT` is the same path as the protected `.git` under case-fold identity and must \
             be rejected as a protected path",
        );
        let msg = err_message(&err);
        assert!(
            msg.contains(PROTECTED_PATH),
            "a case-variant of a protected member must emit `{PROTECTED_PATH}`; got: {msg}"
        );
    }

    #[test]
    fn anchor_match_is_nfc_normalized() {
        let anchor = Path::new("/home/u/caf\u{00e9}");
        let dst = Path::new("/home/u/cafe\u{0301}/init.lua");
        let set = protected(Path::new("/home/u/proj"));

        let confined = confine_destination(anchor, dst, &set).expect(
            "a dst differing from the anchor only by Unicode normalization form must confine to \
             the same anchor (NFC identity), not be treated as a prefix-sibling escape",
        );
        assert_eq!(
            confined
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            Some("init.lua".to_owned()),
            "the NFC-equal dst must keep its leaf file; got {}",
            confined.display()
        );
        let parent = super::super::target_parent(&confined);
        assert!(
            starts_with_components(&parent, &normalize_lexical(anchor)),
            "the confined parent must sit under the anchor by the impl's NFC/casefold component \
             identity, not be split off to a prefix-sibling; got parent {}",
            parent.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn nfc_variant_anchor_does_not_skip_symlink_no_follow_guard() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().expect("tmp");
        let root = tmp.path();
        let anchor_nfc = root.join("caf\u{00e9}");
        std::fs::create_dir_all(&anchor_nfc).expect("anchor");
        let escape = root.join("escape");
        std::fs::create_dir_all(&escape).expect("escape");
        symlink(&escape, anchor_nfc.join("real")).expect("plant anchor-side symlink ancestor");

        let dst = root.join("cafe\u{0301}").join("real/payload");
        let set = protected(root);

        let err = confine_destination(&anchor_nfc, &dst, &set).expect_err(
            "an anchor-side symlink ancestor reached via an NFC/NFD-variant anchor must still be \
             rejected; the no-follow guard must fold like the escape check, not raw-strip",
        );
        let msg = err_message(&err);
        assert!(
            msg.contains(ANCHOR_SYMLINK),
            "an NFC-variant anchor must NOT bypass the symlink no-follow guard; expected \
             `{ANCHOR_SYMLINK}`, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn protected_member_caught_across_symlinked_representation() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().expect("tmp");
        let root = tmp.path();
        let realroot = root.join("realroot");
        std::fs::create_dir_all(realroot.join("proj/.git")).expect("real proj/.git");
        let linkroot = root.join("linkroot");
        symlink(&realroot, &linkroot).expect("plant linkroot -> realroot");

        // Protected set is built under the `linkroot` representation.
        let set = protected(&linkroot.join("proj"));
        let anchor = linkroot.join("proj");
        // Candidate dst denotes the SAME protected `.git` via the `realroot` representation.
        let dst = realroot.join("proj/.git/hooks/x");

        let err = confine_destination(&anchor, &dst, &set).expect_err(
            "the candidate reaches the protected `.git` through the alternate (realroot) \
             representation; a purely-lexical compare misses it, canonicalization must catch it",
        );
        let msg = err_message(&err);
        assert!(
            msg.contains(PROTECTED_PATH),
            "a protected member reached via an alternate filesystem representation must emit \
             `{PROTECTED_PATH}`; got: {msg}"
        );
    }

    #[test]
    fn dep_cannot_overwrite_the_consumer_phora_manifest_via_take_rename() {
        let cwd = Path::new("/home/u/proj");
        let anchor = cwd; // a dep target composed at the consumer root
        let set = protected(cwd);
        // The dest a `take` rename `{ "anything" = "phora.toml" }` resolves to
        // after layout composition: the consumer's own manifest.
        let dst = cwd.join("phora.toml");

        let err = confine_destination(anchor, &dst, &set).expect_err(
            "a transitive dep must never overwrite the consumer's phora.toml, even via a `take` \
             rename whose dest resolves onto the manifest",
        );
        let msg = err_message(&err);
        assert!(
            msg.contains(PROTECTED_PATH),
            "the rejection must carry the `{PROTECTED_PATH}` diagnostic; got: {msg}"
        );
        assert!(
            msg.contains("phora.toml"),
            "the rejection must name the protected `phora.toml`; got: {msg}"
        );
    }

    #[test]
    fn prune_confinement_rejects_forged_out_of_anchor_record_path() {
        let anchor = Path::new("/home/u/.config");
        let forged = Path::new("/home/u/.config/../.ssh/authorized_keys");
        let set = protected(Path::new("/home/u/proj"));

        let err = confine_destination(anchor, forged, &set).expect_err(
            "prune confines each deletion: a forged record path escaping the anchor must be \
             REJECTED, never handed to remove_orphan_path",
        );
        let msg = err_message(&err);
        assert!(
            msg.contains("escapes its anchor"),
            "the prune-side confinement rejection must carry the `escapes its anchor` diagnostic \
             so prune logs a refusal instead of deleting; got: {msg}"
        );
    }
}
