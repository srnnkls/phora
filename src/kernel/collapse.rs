//! Hybrid directory collapse: a wholly-taken offered directory collapses to one
//! dir artifact (symlink under link / subtree copy under copy) instead of N
//! per-leaf artifacts. Pure kernel — no `config` dependency.

use std::collections::BTreeSet;

use crate::diagnostic::SelectionDiagnostic;
use crate::error::Result;
use crate::kernel::take::ResolvedTake;

/// How a collapsed directory materializes downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapseMode {
    /// One directory symlink: every physical child must be taken at identity.
    Link,
    /// One subtree copy: excluded children are simply pruned from the copy.
    Copy,
}

/// The binding's `collapse` override: omitted, `collapse = false`, `collapse = true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapseChoice {
    /// Collapse where it is sound; fall back per-leaf under link when blocked.
    Default,
    /// Never collapse: every kept leaf stays a per-leaf artifact.
    ForcePerLeaf,
    /// Demand collapse: a dir that would collapse but is blocked is a hard error.
    ForceCollapse,
}

/// One planned deployment unit: a collapsed directory or a single kept leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Materialization {
    /// A whole directory deployed as one artifact, rooted at `dir`.
    CollapsedDir { dir: String },
    /// A single kept leaf deployed on its own.
    Leaf(ResolvedTake),
}

impl Materialization {
    /// The published artifact key: the collapsed dir, or the leaf's destination.
    #[must_use]
    pub fn published_key(&self) -> &str {
        match self {
            Materialization::CollapsedDir { dir } => dir,
            Materialization::Leaf(take) => &take.dest,
        }
    }
}

/// Non-fatal collapse outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollapseWarning {
    /// A dir that would collapse under copy fell back to per-leaf under link
    /// because a physical child was excluded.
    LostCollapseToExclude { dir: String },
}

/// The materialization plan: `items` sorted by path key, `warnings` sorted by dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapsePlan {
    pub items: Vec<Materialization>,
    pub warnings: Vec<CollapseWarning>,
}

/// Projects the kept leaf set onto collapsed directories plus per-leaf artifacts.
///
/// Collapse is maximal: a wholly-taken nested tree collapses at its topmost clean
/// directory.
///
/// # Errors
///
/// Under [`CollapseChoice::ForceCollapse`], a directory that would collapse but
/// is blocked (a within-dir exclude under link, or a per-leaf rename touching
/// it) is a hard error naming the directory.
pub fn plan_collapse(
    kept: &[ResolvedTake],
    full_tree: &[String],
    mode: CollapseMode,
    choice: CollapseChoice,
) -> Result<CollapsePlan> {
    if choice == CollapseChoice::ForcePerLeaf {
        return Ok(per_leaf_plan(kept));
    }

    let identity: BTreeSet<&str> = kept
        .iter()
        .filter(|r| r.source == r.dest)
        .map(|r| r.source.as_str())
        .collect();
    let phys: BTreeSet<&str> = full_tree.iter().map(String::as_str).collect();

    // tradeoff: O(kept^2) per binding — collapsibility re-scans `kept` per candidate dir,
    // `*_blocked_dir` per leaf. Upgrade path: a dir->leaves prefix index built over `kept`.
    let mut collapsible: BTreeSet<&str> = BTreeSet::new();
    let mut copy_collapsible: BTreeSet<&str> = BTreeSet::new();
    for dir in candidate_dirs(kept) {
        if is_copy_collapsible(dir, kept, &identity) {
            copy_collapsible.insert(dir);
            if mode == CollapseMode::Copy || is_link_clean(dir, &phys, &identity) {
                collapsible.insert(dir);
            }
        }
    }

    let mut items: Vec<Materialization> = Vec::new();
    let mut emitted_dirs: BTreeSet<String> = BTreeSet::new();
    let mut lost: BTreeSet<String> = BTreeSet::new();
    let mut force_blocked: BTreeSet<String> = BTreeSet::new();

    for leaf in kept {
        if let Some(dir) = topmost(&leaf.source, &collapsible) {
            if emitted_dirs.insert(dir.to_string()) {
                items.push(Materialization::CollapsedDir {
                    dir: dir.to_string(),
                });
            }
            continue;
        }
        if let Some(blocked) = blocked_dir(&leaf.source, &copy_collapsible, &collapsible) {
            lost.insert(blocked.to_string());
            force_blocked.insert(blocked.to_string());
        } else if let Some(rename_blocked) = rename_blocked_dir(leaf, kept) {
            force_blocked.insert(rename_blocked.to_string());
        }
        items.push(Materialization::Leaf(leaf.clone()));
    }

    if choice == CollapseChoice::ForceCollapse
        && let Some(dir) = force_blocked.iter().next()
    {
        return Err(force_collapse_blocked_diagnostic(dir));
    }

    items.sort_by(|a, b| path_key(a).cmp(path_key(b)));
    let warnings = lost
        .into_iter()
        .map(|dir| CollapseWarning::LostCollapseToExclude { dir })
        .collect();
    Ok(CollapsePlan { items, warnings })
}

fn per_leaf_plan(kept: &[ResolvedTake]) -> CollapsePlan {
    let mut items: Vec<Materialization> = kept
        .iter()
        .map(|r| Materialization::Leaf(r.clone()))
        .collect();
    items.sort_by(|a, b| path_key(a).cmp(path_key(b)));
    CollapsePlan {
        items,
        warnings: Vec::new(),
    }
}

fn path_key(m: &Materialization) -> &str {
    match m {
        Materialization::CollapsedDir { dir } => dir,
        Materialization::Leaf(r) => &r.dest,
    }
}

fn candidate_dirs(kept: &[ResolvedTake]) -> BTreeSet<&str> {
    let mut dirs = BTreeSet::new();
    for leaf in kept {
        let mut rest = leaf.source.as_str();
        while let Some(cut) = rest.rfind('/') {
            dirs.insert(&leaf.source[..cut]);
            rest = &leaf.source[..cut];
        }
    }
    dirs
}

fn under(path: &str, dir: &str) -> bool {
    path.len() > dir.len() + 1 && path.as_bytes()[dir.len()] == b'/' && path.starts_with(dir)
}

fn is_copy_collapsible(dir: &str, kept: &[ResolvedTake], identity: &BTreeSet<&str>) -> bool {
    let mut any = false;
    for leaf in kept {
        if under(&leaf.source, dir) || under(&leaf.dest, dir) {
            if !identity.contains(leaf.source.as_str()) {
                return false;
            }
            any = true;
        }
    }
    any
}

fn is_link_clean(dir: &str, phys: &BTreeSet<&str>, identity: &BTreeSet<&str>) -> bool {
    phys.iter()
        .filter(|leaf| under(leaf, dir))
        .all(|leaf| identity.contains(leaf))
}

fn topmost<'a>(source: &str, collapsible: &BTreeSet<&'a str>) -> Option<&'a str> {
    collapsible
        .iter()
        .filter(|dir| under(source, dir))
        .min_by_key(|dir| dir.len())
        .copied()
}

/// A collapsed descendant excludes its blocked ancestor from the warning: a clean
/// sibling subtree that did collapse must not widen the loss.
fn blocked_dir<'a>(
    source: &str,
    copy_collapsible: &BTreeSet<&'a str>,
    collapsible: &BTreeSet<&str>,
) -> Option<&'a str> {
    copy_collapsible
        .iter()
        .filter(|dir| {
            under(source, dir)
                && !collapsible.contains(*dir)
                && !collapsible.iter().any(|kept| under(kept, dir))
        })
        .min_by_key(|dir| dir.len())
        .copied()
}

fn rename_blocked_dir<'a>(leaf: &'a ResolvedTake, kept: &[ResolvedTake]) -> Option<&'a str> {
    let by_source = ancestor_holding_rename(&leaf.source, kept);
    if leaf.source == leaf.dest {
        return by_source;
    }
    let by_dest = ancestor_holding_rename(&leaf.dest, kept);
    match (by_source, by_dest) {
        (Some(s), Some(d)) => Some(if s.len() <= d.len() { s } else { d }),
        (some, None) | (None, some) => some,
    }
}

fn ancestor_holding_rename<'a>(path: &'a str, kept: &[ResolvedTake]) -> Option<&'a str> {
    let mut rest = path;
    let mut candidate = None;
    while let Some(cut) = rest.rfind('/') {
        let dir = &path[..cut];
        if kept.iter().any(|leaf| {
            leaf.source != leaf.dest && (under(&leaf.source, dir) || under(&leaf.dest, dir))
        }) {
            candidate = Some(dir);
        }
        rest = dir;
    }
    candidate
}

fn force_collapse_blocked_diagnostic(dir: &str) -> crate::error::Error {
    SelectionDiagnostic {
        entry: dir.to_string(),
        matched_against: "the source tree under the collapsed directory".to_string(),
        why: "`collapse = true` was demanded but the directory cannot collapse to one artifact"
            .to_string(),
        did_you_mean: None,
        remedy: "drop the within-dir exclude or rename, deploy in copy mode, or omit `collapse`"
            .to_string(),
        debug_hint: Some("phora preview --files".to_string()),
        details: Vec::new(),
    }
    .sync()
}

#[cfg(test)]
mod collapse_tests {
    use crate::diagnostic::{MATCHED_AGAINST, REMEDY, SELECTION, TO_DEBUG};
    use crate::kernel::collapse::{
        CollapseChoice, CollapseMode, CollapsePlan, CollapseWarning, Materialization, plan_collapse,
    };
    use crate::kernel::take::ResolvedTake;

    fn kept(pairs: &[(&str, &str)]) -> Vec<ResolvedTake> {
        pairs
            .iter()
            .map(|(s, d)| ResolvedTake {
                source: (*s).to_string(),
                dest: (*d).to_string(),
            })
            .collect()
    }

    fn tree(leaves: &[&str]) -> Vec<String> {
        leaves.iter().map(|s| (*s).to_string()).collect()
    }

    fn plan(
        kept: &[ResolvedTake],
        full_tree: &[String],
        mode: CollapseMode,
        choice: CollapseChoice,
    ) -> CollapsePlan {
        plan_collapse(kept, full_tree, mode, choice).expect("collapse plans")
    }

    fn dir(name: &str) -> Materialization {
        Materialization::CollapsedDir {
            dir: name.to_string(),
        }
    }

    fn leaf(source: &str, dest: &str) -> Materialization {
        Materialization::Leaf(ResolvedTake {
            source: source.to_string(),
            dest: dest.to_string(),
        })
    }

    fn lost(name: &str) -> CollapseWarning {
        CollapseWarning::LostCollapseToExclude {
            dir: name.to_string(),
        }
    }

    fn assert_items(plan: &CollapsePlan, expected: &[Materialization]) {
        assert_eq!(
            plan.items, expected,
            "exact materialization vec (stable-sorted) mismatch: extras, omissions, \
             or duplicates all fail here"
        );
    }

    fn assert_warnings(plan: &CollapsePlan, expected: &[CollapseWarning]) {
        assert_eq!(
            plan.warnings, expected,
            "exact collapse-warning vec (sorted by dir) mismatch"
        );
    }

    fn rendered_error(
        kept: &[ResolvedTake],
        full_tree: &[String],
        mode: CollapseMode,
        choice: CollapseChoice,
    ) -> String {
        plan_collapse(kept, full_tree, mode, choice)
            .expect_err("this collapse request must hard-error")
            .to_string()
    }

    fn assert_named_diagnostic(rendered: &str, entry: &str) {
        for phrase in [SELECTION, MATCHED_AGAINST, REMEDY, TO_DEBUG] {
            assert!(
                rendered.contains(phrase),
                "the rejection must render the named phrase `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains(entry),
            "the rejection must name the offending dir `{entry}`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("to debug: phora preview --files"),
            "a collapse rejection must point at the preview command so the shape is inspectable; \
             got:\n{rendered}"
        );
    }

    // ---- 1. wholly-taken dir collapses (Default), both modes ----

    #[test]
    fn default_link_collapses_a_wholly_taken_directory_to_one_dir_symlink() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn default_copy_collapses_a_wholly_taken_directory_to_one_subtree() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::Default);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    // ---- 2. collapse is insensitive to HOW the dir was offered (whole-dir vs glob) ----

    #[test]
    fn the_same_kept_set_collapses_identically_regardless_of_how_the_dir_was_offered() {
        let whole_dir = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let via_glob = kept(&[("d/b.md", "d/b.md"), ("d/a.md", "d/a.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let from_whole = plan(&whole_dir, &t, CollapseMode::Link, CollapseChoice::Default);
        let from_glob = plan(&via_glob, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&from_whole, &[dir("d")]);
        assert_eq!(
            from_whole.items, from_glob.items,
            "the same kept set must collapse identically; collapse cannot depend on how the \
             dir was offered"
        );
    }

    // ---- 3. single-leaf dir collapses (Default, both modes) ----

    #[test]
    fn a_single_leaf_directory_collapses_under_link() {
        let k = kept(&[("d/only.md", "d/only.md")]);
        let t = tree(&["d/only.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn a_single_leaf_directory_collapses_under_copy() {
        let k = kept(&[("d/only.md", "d/only.md")]);
        let t = tree(&["d/only.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::Default);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    // ---- 4. a root-level leaf (under no collapsible dir) stays a Leaf ----

    #[test]
    fn a_top_level_leaf_outside_any_directory_stays_a_per_leaf_artifact() {
        let k = kept(&[("readme.md", "readme.md")]);
        let t = tree(&["readme.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[leaf("readme.md", "readme.md")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn a_top_level_leaf_coexists_with_a_collapsed_sibling_dir() {
        let k = kept(&[("readme.md", "readme.md"), ("d/a.md", "d/a.md")]);
        let t = tree(&["readme.md", "d/a.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[dir("d"), leaf("readme.md", "readme.md")]);
        assert_warnings(&p, &[]);
    }

    // ---- 5. D16: within-dir exclude under LINK blocks collapse, warns, falls back ----

    #[test]
    fn link_within_dir_exclude_blocks_collapse_falls_back_per_leaf_and_warns() {
        let k = kept(&[("d/a.md", "d/a.md")]);
        let t = tree(&["d/a.md", "d/secret.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[leaf("d/a.md", "d/a.md")]);
        assert_warnings(&p, &[lost("d")]);
    }

    #[test]
    fn link_fallback_never_leaks_the_excluded_leaf_into_the_plan() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let t = tree(&["d/a.md", "d/b.md", "d/secret.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[leaf("d/a.md", "d/a.md"), leaf("d/b.md", "d/b.md")]);
        assert!(
            !p.items
                .iter()
                .any(|m| matches!(m, Materialization::Leaf(r) if r.source == "d/secret.md"))
                && !p
                    .items
                    .iter()
                    .any(|m| matches!(m, Materialization::CollapsedDir { dir } if dir == "d")),
            "the excluded leaf must never appear and the dir must NOT collapse; got: {:?}",
            p.items
        );
        assert_warnings(&p, &[lost("d")]);
    }

    // ---- 6. D5/D16: within-dir exclude under COPY does NOT block — collapses, no warning ----

    #[test]
    fn copy_within_dir_exclude_does_not_block_collapse_and_emits_no_warning() {
        let k = kept(&[("d/a.md", "d/a.md")]);
        let t = tree(&["d/a.md", "d/secret.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::Default);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    // ---- 7. a per-leaf rename touching the dir blocks collapse (per-leaf, no D16 warning) ----

    #[test]
    fn link_per_leaf_rename_in_a_dir_blocks_collapse_without_a_lost_collapse_warning() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "renamed/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(
            &p,
            &[leaf("d/a.md", "d/a.md"), leaf("d/b.md", "renamed/b.md")],
        );
        assert_warnings(&p, &[]);
    }

    #[test]
    fn copy_per_leaf_rename_in_a_dir_blocks_collapse_without_a_warning() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "renamed/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::Default);
        assert_items(
            &p,
            &[leaf("d/a.md", "d/a.md"), leaf("d/b.md", "renamed/b.md")],
        );
        assert_warnings(&p, &[]);
    }

    // ---- 8. whole-dir rename (the only leaf renamed out of the dir) is per-leaf, not collapsed ----

    #[test]
    fn renaming_the_only_leaf_out_of_a_dir_keeps_it_per_leaf_not_collapsed() {
        let k = kept(&[("d/only.md", "elsewhere/only.md")]);
        let t = tree(&["d/only.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[leaf("d/only.md", "elsewhere/only.md")]);
        assert_warnings(&p, &[]);
    }

    // ---- 9. mixed: two sibling dirs, one collapses, one falls back (LINK) ----

    #[test]
    fn two_sibling_dirs_one_collapses_and_one_falls_back_under_link() {
        let k = kept(&[
            ("keep/a.md", "keep/a.md"),
            ("keep/b.md", "keep/b.md"),
            ("partial/x.md", "partial/x.md"),
        ]);
        let t = tree(&[
            "keep/a.md",
            "keep/b.md",
            "partial/x.md",
            "partial/dropped.md",
        ]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[dir("keep"), leaf("partial/x.md", "partial/x.md")]);
        assert_warnings(&p, &[lost("partial")]);
    }

    // ---- 10. ForcePerLeaf (collapse = false): never collapse, no warning ----

    #[test]
    fn force_per_leaf_emits_every_kept_leaf_even_for_a_wholly_taken_dir_under_link() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::ForcePerLeaf);
        assert_items(&p, &[leaf("d/a.md", "d/a.md"), leaf("d/b.md", "d/b.md")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn force_per_leaf_emits_every_kept_leaf_even_for_a_wholly_taken_dir_under_copy() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::ForcePerLeaf);
        assert_items(&p, &[leaf("d/a.md", "d/a.md"), leaf("d/b.md", "d/b.md")]);
        assert_warnings(&p, &[]);
    }

    // ---- 11. ForceCollapse (collapse = true): collapses when possible ----

    #[test]
    fn force_collapse_collapses_a_wholly_taken_dir_with_no_warning_under_link() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "d/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::ForceCollapse);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    // ---- 12. ForceCollapse blocked by a within-dir exclude under LINK is a HARD ERROR ----

    #[test]
    fn force_collapse_link_blocked_by_within_dir_exclude_is_a_hard_error_naming_the_dir() {
        let k = kept(&[("d/a.md", "d/a.md")]);
        let t = tree(&["d/a.md", "d/secret.md"]);
        let rendered = rendered_error(&k, &t, CollapseMode::Link, CollapseChoice::ForceCollapse);
        assert_named_diagnostic(&rendered, "d");
    }

    // ---- 13. ForceCollapse blocked by a per-leaf rename is a HARD ERROR ----

    #[test]
    fn force_collapse_blocked_by_a_per_leaf_rename_is_a_hard_error_naming_the_dir() {
        let k = kept(&[("d/a.md", "d/a.md"), ("d/b.md", "renamed/b.md")]);
        let t = tree(&["d/a.md", "d/b.md"]);
        let rendered = rendered_error(&k, &t, CollapseMode::Link, CollapseChoice::ForceCollapse);
        assert_named_diagnostic(&rendered, "d");
    }

    // ---- 14. ForceCollapse + COPY + within-dir exclude COLLAPSES (not an error) ----

    #[test]
    fn force_collapse_copy_with_a_within_dir_exclude_collapses_and_does_not_error() {
        let k = kept(&[("d/a.md", "d/a.md")]);
        let t = tree(&["d/a.md", "d/secret.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::ForceCollapse);
        assert_items(&p, &[dir("d")]);
        assert_warnings(&p, &[]);
    }

    // ---- 15. order-independence: permuting kept / full_tree yields an identical plan ----

    #[test]
    fn permuting_kept_and_full_tree_yields_an_identical_plan() {
        let forward_kept = kept(&[
            ("keep/a.md", "keep/a.md"),
            ("keep/b.md", "keep/b.md"),
            ("partial/x.md", "partial/x.md"),
            ("top.md", "top.md"),
        ]);
        let forward_tree = tree(&[
            "keep/a.md",
            "keep/b.md",
            "partial/x.md",
            "partial/dropped.md",
            "top.md",
        ]);
        let reversed_kept = kept(&[
            ("top.md", "top.md"),
            ("partial/x.md", "partial/x.md"),
            ("keep/b.md", "keep/b.md"),
            ("keep/a.md", "keep/a.md"),
        ]);
        let reversed_tree = tree(&[
            "top.md",
            "partial/dropped.md",
            "partial/x.md",
            "keep/b.md",
            "keep/a.md",
        ]);
        let forward = plan(
            &forward_kept,
            &forward_tree,
            CollapseMode::Link,
            CollapseChoice::Default,
        );
        let reversed = plan(
            &reversed_kept,
            &reversed_tree,
            CollapseMode::Link,
            CollapseChoice::Default,
        );
        assert_items(
            &forward,
            &[
                dir("keep"),
                leaf("partial/x.md", "partial/x.md"),
                leaf("top.md", "top.md"),
            ],
        );
        assert_eq!(
            forward.items, reversed.items,
            "permuting kept / full_tree must not change the plan items"
        );
        assert_eq!(
            forward.warnings, reversed.warnings,
            "permuting kept / full_tree must not change the plan warnings"
        );
        assert_warnings(&forward, &[lost("partial")]);
    }

    // ---- 16. maximal/nested collapse: a wholly-taken nested tree collapses at the topmost dir ----

    #[test]
    fn a_wholly_taken_nested_tree_collapses_at_the_topmost_directory_under_link() {
        let k = kept(&[
            ("a/b/x.md", "a/b/x.md"),
            ("a/b/y.md", "a/b/y.md"),
            ("a/c/z.md", "a/c/z.md"),
        ]);
        let t = tree(&["a/b/x.md", "a/b/y.md", "a/c/z.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[dir("a")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn a_wholly_taken_nested_tree_collapses_at_the_topmost_directory_under_copy() {
        let k = kept(&[
            ("a/b/x.md", "a/b/x.md"),
            ("a/b/y.md", "a/b/y.md"),
            ("a/c/z.md", "a/c/z.md"),
        ]);
        let t = tree(&["a/b/x.md", "a/b/y.md", "a/c/z.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::Default);
        assert_items(&p, &[dir("a")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn link_a_deep_within_dir_exclude_blocks_only_its_subtree_a_clean_sibling_still_collapses() {
        let k = kept(&[("a/b/x.md", "a/b/x.md"), ("a/c/z.md", "a/c/z.md")]);
        let t = tree(&["a/b/x.md", "a/b/secret", "a/c/z.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[leaf("a/b/x.md", "a/b/x.md"), dir("a/c")]);
        assert_warnings(&p, &[lost("a/b")]);
        assert!(
            !p.items.iter().any(|m| matches!(
                m,
                Materialization::Leaf(r) if r.source == "a/b/secret"
            )),
            "the excluded leaf must never appear in the plan; got: {:?}",
            p.items
        );
    }

    #[test]
    fn copy_a_deep_within_dir_exclude_does_not_block_the_topmost_collapse() {
        let k = kept(&[("a/b/x.md", "a/b/x.md"), ("a/c/z.md", "a/c/z.md")]);
        let t = tree(&["a/b/x.md", "a/b/secret", "a/c/z.md"]);
        let p = plan(&k, &t, CollapseMode::Copy, CollapseChoice::Default);
        assert_items(&p, &[dir("a")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn force_collapse_link_with_a_deep_within_dir_exclude_is_a_hard_error() {
        let k = kept(&[("a/b/x.md", "a/b/x.md"), ("a/c/z.md", "a/c/z.md")]);
        let t = tree(&["a/b/x.md", "a/b/secret", "a/c/z.md"]);
        let rendered = rendered_error(&k, &t, CollapseMode::Link, CollapseChoice::ForceCollapse);
        assert_named_diagnostic(&rendered, "a/b");
    }

    // ---- 17. a rename whose DEST lands under a collapsible dir blocks that collapse ----

    #[test]
    fn a_rename_dest_landing_inside_a_collapsible_dir_blocks_that_dirs_collapse() {
        let k = kept(&[("d/a.md", "d/a.md"), ("x/c.md", "d/c.md")]);
        let t = tree(&["d/a.md", "x/c.md"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[leaf("d/a.md", "d/a.md"), leaf("x/c.md", "d/c.md")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn force_collapse_with_a_rename_dest_landing_inside_a_collapsible_dir_is_a_hard_error() {
        let k = kept(&[("d/a.md", "d/a.md"), ("x/c.md", "d/c.md")]);
        let t = tree(&["d/a.md", "x/c.md"]);
        let rendered = rendered_error(&k, &t, CollapseMode::Link, CollapseChoice::ForceCollapse);
        assert_named_diagnostic(&rendered, "d");
    }

    // ---- 18. ForceCollapse diagnostic is deterministic under kept permutation ----

    #[test]
    fn force_collapse_blocked_dir_is_deterministic_under_kept_permutation_for_renames() {
        let t = tree(&["a/x", "b/y"]);
        let forward = kept(&[("a/x", "RENAMED/ax"), ("b/y", "RENAMED/by")]);
        let reversed = kept(&[("b/y", "RENAMED/by"), ("a/x", "RENAMED/ax")]);
        let from_forward = rendered_error(
            &forward,
            &t,
            CollapseMode::Link,
            CollapseChoice::ForceCollapse,
        );
        let from_reversed = rendered_error(
            &reversed,
            &t,
            CollapseMode::Link,
            CollapseChoice::ForceCollapse,
        );
        assert_named_diagnostic(&from_forward, "a");
        assert_eq!(
            from_forward, from_reversed,
            "the ForceCollapse rejection must name the same dir regardless of kept order"
        );
    }

    #[test]
    fn force_collapse_blocked_dir_is_deterministic_under_kept_permutation_for_within_dir_exclude() {
        let t = tree(&["p/x.md", "p/secret", "q/y.md", "q/secret"]);
        let forward = kept(&[("p/x.md", "p/x.md"), ("q/y.md", "q/y.md")]);
        let reversed = kept(&[("q/y.md", "q/y.md"), ("p/x.md", "p/x.md")]);
        let from_forward = rendered_error(
            &forward,
            &t,
            CollapseMode::Link,
            CollapseChoice::ForceCollapse,
        );
        let from_reversed = rendered_error(
            &reversed,
            &t,
            CollapseMode::Link,
            CollapseChoice::ForceCollapse,
        );
        assert_named_diagnostic(&from_forward, "p");
        assert_eq!(
            from_forward, from_reversed,
            "the ForceCollapse within-dir-exclude rejection must name the same dir regardless \
             of kept order"
        );
    }

    // ---- 19. no per-leaf D16 fallback ⇒ no loss (clean collapsed descendant) ----

    #[test]
    fn a_link_blocked_ancestor_with_a_clean_collapsed_descendant_does_not_warn() {
        let k = kept(&[("a/b/x", "a/b/x")]);
        let t = tree(&["a/b/x", "a/secret"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::Default);
        assert_items(&p, &[dir("a/b")]);
        assert_warnings(&p, &[]);
    }

    #[test]
    fn force_collapse_with_a_clean_collapsed_descendant_under_a_link_blocked_ancestor_is_ok() {
        let k = kept(&[("a/b/x", "a/b/x")]);
        let t = tree(&["a/b/x", "a/secret"]);
        let p = plan(&k, &t, CollapseMode::Link, CollapseChoice::ForceCollapse);
        assert_items(&p, &[dir("a/b")]);
        assert_warnings(&p, &[]);
    }
}
