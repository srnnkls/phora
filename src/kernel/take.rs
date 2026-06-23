//! Take resolution: projects a binding's `take` directive over a sealed offer
//! set into the deployed-leaf set.

use std::collections::{BTreeMap, BTreeSet};

use unicode_normalization::UnicodeNormalization;

use crate::diagnostic::SelectionDiagnostic;
use crate::error::Result;
use crate::kernel::safe_relpath;
use crate::kernel::selection::compile_take_glob;

/// A take entry is a glob iff it ends in `/` or carries any of `* ? [ ]`;
/// brace expansion (`{a,b}`) is not a glob marker.
#[must_use]
pub fn is_take_glob(s: &str) -> bool {
    s.ends_with('/') || s.contains(['*', '?', '[', ']'])
}

/// A classified take directive. `config`'s `TakeEntry` maps onto this; the
/// kernel never depends on `config`.
#[derive(Debug, Clone)]
pub enum Take<'a> {
    /// A literal leaf; it must be present in the offer set.
    Literal(&'a str),
    /// A gitignore glob; it expands over the OFFER set only.
    Glob(&'a str),
    /// A destructive rename: literal `src` is consumed out, emitted at `dest`.
    Rename { src: &'a str, dest: &'a str },
}

/// A kept published-leaf mapping: `source` (an offered leaf) deploys at `dest`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedTake {
    pub source: String,
    pub dest: String,
}

/// Non-fatal resolution outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeWarning {
    /// A take glob matched zero offered leaves.
    NoMatchGlob(String),
}

/// The result of projecting a take directive over an offer: `kept` is sorted by
/// `dest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeResolution {
    pub kept: Vec<ResolvedTake>,
    pub warnings: Vec<TakeWarning>,
}

/// Projects `take` over the sorted, root-relative `offer` leaf set.
///
/// `None` = take everything (identity over the whole offer). `Some(&[])` =
/// take nothing.
///
/// # Errors
///
/// Returns an error when a literal/rename-src is not offered, when a leaf is
/// both literal and rename-src, when two sources resolve to the same dest, or
/// when a rename dest fails `safe_relpath`.
pub fn resolve_take(offer: &[String], take: Option<&[Take]>) -> Result<TakeResolution> {
    let Some(directives) = take else {
        return Ok(identity(offer));
    };

    let offered: BTreeSet<&str> = offer.iter().map(String::as_str).collect();
    let literals = collect_literals(directives, &offered)?;
    let renames = collect_renames(directives, &offered, &literals)?;

    let consumed: BTreeSet<&str> = literals
        .iter()
        .copied()
        .chain(renames.iter().map(|(src, _)| *src))
        .collect();

    let mut kept = Vec::new();
    for &leaf in &literals {
        kept.push(mapping(leaf, leaf));
    }
    for &(src, dest) in &renames {
        kept.push(mapping(src, dest));
    }

    let warnings = expand_globs(directives, offer, &consumed, &mut kept)?;

    reject_duplicate_dest(&kept)?;
    kept.sort_by(|a, b| a.dest.cmp(&b.dest));
    Ok(TakeResolution { kept, warnings })
}

fn identity(offer: &[String]) -> TakeResolution {
    let mut kept: Vec<ResolvedTake> = offer.iter().map(|leaf| mapping(leaf, leaf)).collect();
    kept.sort_by(|a, b| a.dest.cmp(&b.dest));
    TakeResolution {
        kept,
        warnings: Vec::new(),
    }
}

fn mapping(source: &str, dest: &str) -> ResolvedTake {
    ResolvedTake {
        source: source.to_string(),
        dest: dest.to_string(),
    }
}

fn collect_literals<'a>(
    directives: &[Take<'a>],
    offered: &BTreeSet<&str>,
) -> Result<BTreeSet<&'a str>> {
    let mut literals = BTreeSet::new();
    for directive in directives {
        if let Take::Literal(leaf) = directive {
            require_offered(leaf, offered)?;
            literals.insert(*leaf);
        }
    }
    Ok(literals)
}

fn collect_renames<'a>(
    directives: &[Take<'a>],
    offered: &BTreeSet<&str>,
    literals: &BTreeSet<&str>,
) -> Result<Vec<(&'a str, &'a str)>> {
    let mut renames = Vec::new();
    let mut dest_of: BTreeMap<&str, &str> = BTreeMap::new();
    for directive in directives {
        let Take::Rename { src, dest } = directive else {
            continue;
        };
        require_offered(src, offered)?;
        if literals.contains(src) {
            return Err(literal_and_rename_diagnostic(src));
        }
        safe_relpath(dest).map_err(|_| unsafe_dest_diagnostic(dest))?;
        match dest_of.insert(src, dest) {
            Some(prior) if prior != *dest => {
                return Err(rename_fan_out_diagnostic(src, prior, dest));
            }
            Some(_) => {}
            None => renames.push((*src, *dest)),
        }
    }
    Ok(renames)
}

fn expand_globs(
    directives: &[Take],
    offer: &[String],
    consumed: &BTreeSet<&str>,
    kept: &mut Vec<ResolvedTake>,
) -> Result<Vec<TakeWarning>> {
    let mut warnings = Vec::new();
    let mut matched: BTreeSet<&str> = BTreeSet::new();
    for directive in directives {
        let Take::Glob(pattern) = directive else {
            continue;
        };
        let globset = compile_take_glob(pattern)?;
        let hits: Vec<&str> = offer
            .iter()
            .map(String::as_str)
            .filter(|leaf| globset.is_match(leaf))
            .collect();
        if hits.is_empty() {
            warnings.push(TakeWarning::NoMatchGlob((*pattern).to_string()));
            continue;
        }
        for leaf in hits {
            if !consumed.contains(leaf) && matched.insert(leaf) {
                kept.push(mapping(leaf, leaf));
            }
        }
    }
    warnings.sort_by(|a, b| {
        let TakeWarning::NoMatchGlob(pa) = a;
        let TakeWarning::NoMatchGlob(pb) = b;
        pa.cmp(pb)
    });
    Ok(warnings)
}

fn reject_duplicate_dest(kept: &[ResolvedTake]) -> Result<()> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for entry in kept {
        if let Some(first) = seen.insert(fold_dest(&entry.dest), &entry.dest) {
            return Err(duplicate_dest_diagnostic(first, &entry.dest));
        }
    }
    Ok(())
}

// NFC + simple lowercase, NOT full Unicode case-folding: macOS (APFS) and
// Windows (NTFS) collide names by simple-fold only, so `straße` and `strasse`
// remain distinct files there — full folding would over-reject them.
pub(crate) fn fold_dest(dest: &str) -> String {
    dest.nfc().collect::<String>().to_lowercase()
}

fn require_offered(entry: &str, offered: &BTreeSet<&str>) -> Result<()> {
    if offered.contains(entry) {
        Ok(())
    } else {
        Err(non_offered_diagnostic(entry, offered))
    }
}

fn non_offered_diagnostic(entry: &str, offered: &BTreeSet<&str>) -> crate::error::Error {
    SelectionDiagnostic {
        entry: entry.to_string(),
        matched_against: "the offer set".to_string(),
        why: "not present in the offer; `take` may not widen the offer".to_string(),
        did_you_mean: crate::diagnostic::did_you_mean(entry, offered.iter().copied()),
        remedy: "name a leaf the source offers, or add it to the source's include".to_string(),
        debug_hint: None,
    }
    .sync()
}

fn literal_and_rename_diagnostic(leaf: &str) -> crate::error::Error {
    SelectionDiagnostic {
        entry: leaf.to_string(),
        matched_against: "the offer set".to_string(),
        why: "named both as a literal `take` and as a rename source".to_string(),
        did_you_mean: None,
        remedy: "keep the leaf either literally or as a rename source, not both".to_string(),
        debug_hint: None,
    }
    .sync()
}

fn rename_fan_out_diagnostic(src: &str, first: &str, second: &str) -> crate::error::Error {
    let (a, b) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    SelectionDiagnostic {
        entry: src.to_string(),
        matched_against: "the offer set".to_string(),
        why: "renamed to two different destinations".to_string(),
        did_you_mean: None,
        remedy: format!("keep one rename of `{src}`: either `{a}` or `{b}`, not both"),
        debug_hint: None,
    }
    .sync()
}

fn unsafe_dest_diagnostic(dest: &str) -> crate::error::Error {
    SelectionDiagnostic {
        entry: dest.to_string(),
        matched_against: "the deploy root".to_string(),
        why: "rename destination is not a portable relative path".to_string(),
        did_you_mean: None,
        remedy: "use a forward-slashed relative path inside the deploy root".to_string(),
        debug_hint: None,
    }
    .sync()
}

fn duplicate_dest_diagnostic(first: &str, second: &str) -> crate::error::Error {
    let entry = if first == second {
        first.to_string()
    } else if first <= second {
        format!("{first} / {second}")
    } else {
        format!("{second} / {first}")
    };
    SelectionDiagnostic {
        entry,
        matched_against: "the binding's kept destinations".to_string(),
        why: "two distinct sources resolve to the same destination".to_string(),
        did_you_mean: None,
        remedy: "rename one source so each kept leaf lands at a distinct path".to_string(),
        debug_hint: None,
    }
    .sync()
}

#[cfg(test)]
mod take_resolution_tests {
    use super::{Take, TakeWarning, is_take_glob, resolve_take};
    use crate::diagnostic::{DID_YOU_MEAN, MATCHED_AGAINST, REMEDY, SELECTION};

    fn offer(leaves: &[&str]) -> Vec<String> {
        leaves.iter().map(|s| (*s).to_string()).collect()
    }

    fn resolve(offer: &[String], take: &[Take]) -> super::TakeResolution {
        resolve_take(offer, Some(take)).expect("take resolves")
    }

    fn pairs(res: &super::TakeResolution) -> Vec<(String, String)> {
        res.kept
            .iter()
            .map(|r| (r.source.clone(), r.dest.clone()))
            .collect()
    }

    fn assert_kept(res: &super::TakeResolution, expected: &[(&str, &str)]) {
        let got: Vec<(String, String)> = pairs(res);
        let want: Vec<(String, String)> = expected
            .iter()
            .map(|(s, d)| ((*s).to_string(), (*d).to_string()))
            .collect();
        assert_eq!(
            got, want,
            "exact kept mapping (sorted by dest) mismatch: extras, duplicates, \
             or omissions all fail here"
        );
    }

    fn dests(res: &super::TakeResolution) -> Vec<String> {
        res.kept.iter().map(|r| r.dest.clone()).collect()
    }

    fn assert_warnings(res: &super::TakeResolution, expected: &[&str]) {
        let want: Vec<TakeWarning> = expected
            .iter()
            .map(|p| TakeWarning::NoMatchGlob((*p).to_string()))
            .collect();
        assert_eq!(
            res.warnings, want,
            "exact warnings set (sorted by pattern) mismatch"
        );
    }

    fn rendered_error(offer: &[String], take: &[Take]) -> String {
        resolve_take(offer, Some(take))
            .expect_err("this take directive must be rejected")
            .to_string()
    }

    fn assert_named_diagnostic(rendered: &str, entry: &str) {
        for phrase in [SELECTION, MATCHED_AGAINST, REMEDY] {
            assert!(
                rendered.contains(phrase),
                "the rejection must render the named phrase `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains(entry),
            "the rejection must name the offending entry `{entry}`; got:\n{rendered}"
        );
    }

    // ---- 1. set composition is order-independent ----

    #[test]
    fn resolving_the_same_entries_in_any_permutation_yields_the_identical_kept_set() {
        let offer = offer(&["a.md", "b.md", "c.md", "ignored.md"]);
        let forward = resolve(
            &offer,
            &[
                Take::Literal("a.md"),
                Take::Literal("b.md"),
                Take::Literal("c.md"),
            ],
        );
        let reversed = resolve(
            &offer,
            &[
                Take::Literal("c.md"),
                Take::Literal("b.md"),
                Take::Literal("a.md"),
            ],
        );
        let shuffled = resolve(
            &offer,
            &[
                Take::Literal("b.md"),
                Take::Literal("a.md"),
                Take::Literal("c.md"),
            ],
        );
        assert_kept(
            &forward,
            &[("a.md", "a.md"), ("b.md", "b.md"), ("c.md", "c.md")],
        );
        assert_eq!(
            pairs(&forward),
            pairs(&reversed),
            "reversal must not change the kept set"
        );
        assert_eq!(
            pairs(&forward),
            pairs(&shuffled),
            "shuffling must not change the kept set"
        );
    }

    #[test]
    fn a_mixed_glob_literal_rename_set_resolves_identically_under_reversal() {
        let offer = offer(&["a/x.md", "a/y.md", "keep.md"]);
        let forward = resolve(
            &offer,
            &[
                Take::Glob("a/**"),
                Take::Literal("keep.md"),
                Take::Rename {
                    src: "a/x.md",
                    dest: "renamed/x.md",
                },
            ],
        );
        let reversed = resolve(
            &offer,
            &[
                Take::Rename {
                    src: "a/x.md",
                    dest: "renamed/x.md",
                },
                Take::Literal("keep.md"),
                Take::Glob("a/**"),
            ],
        );
        assert_kept(
            &forward,
            &[
                ("a/y.md", "a/y.md"),
                ("keep.md", "keep.md"),
                ("a/x.md", "renamed/x.md"),
            ],
        );
        assert_eq!(
            pairs(&forward),
            pairs(&reversed),
            "permuting a mixed glob/literal/rename set must not change the kept set"
        );
    }

    // ---- 2. globs expand over the OFFER, never the tree (no-widen / sealed offer) ----

    #[test]
    fn a_take_glob_matches_only_offered_leaves_and_never_introduces_an_unoffered_one() {
        let offer = offer(&["skills/a/SKILL.md", "skills/b/SKILL.md"]);
        let res = resolve(&offer, &[Take::Glob("skills/**")]);
        assert_kept(
            &res,
            &[
                ("skills/a/SKILL.md", "skills/a/SKILL.md"),
                ("skills/b/SKILL.md", "skills/b/SKILL.md"),
            ],
        );
        assert!(
            res.kept.iter().all(|r| offer.contains(&r.source)),
            "every kept source must already be in the offer; a glob may not widen it"
        );
    }

    // ---- 3. explicit consumes its leaf out of overlapping globs ----

    #[test]
    fn explicit_literal_consumes_its_leaf_out_of_an_overlapping_glob() {
        let offer = offer(&["skills/a/SKILL.md", "skills/b/SKILL.md"]);
        let res = resolve(
            &offer,
            &[Take::Glob("skills/**"), Take::Literal("skills/a/SKILL.md")],
        );
        assert_kept(
            &res,
            &[
                ("skills/a/SKILL.md", "skills/a/SKILL.md"),
                ("skills/b/SKILL.md", "skills/b/SKILL.md"),
            ],
        );
    }

    #[test]
    fn rename_src_is_consumed_out_of_an_overlapping_glob_and_not_re_emitted_at_identity() {
        let offer = offer(&["a/x.md", "a/y.md"]);
        let res = resolve(
            &offer,
            &[
                Take::Glob("a/**"),
                Take::Rename {
                    src: "a/x.md",
                    dest: "renamed/x.md",
                },
            ],
        );
        assert_kept(&res, &[("a/y.md", "a/y.md"), ("a/x.md", "renamed/x.md")]);
    }

    // ---- 4. rename is destructive ----

    #[test]
    fn rename_emits_the_leaf_only_at_its_destination() {
        let offer = offer(&["x", "untouched"]);
        let res = resolve(
            &offer,
            &[Take::Rename {
                src: "x",
                dest: "a",
            }],
        );
        assert_kept(&res, &[("x", "a")]);
        assert!(
            !dests(&res).iter().any(|d| d == "x"),
            "a destructive rename must NOT also emit the leaf at its original path `x`"
        );
    }

    // ---- 5. duplicate literal is idempotent ----

    #[test]
    fn two_identical_literal_entries_keep_one_pair_without_error() {
        let offer = offer(&["dup.md", "other.md"]);
        let res = resolve(&offer, &[Take::Literal("dup.md"), Take::Literal("dup.md")]);
        assert_kept(&res, &[("dup.md", "dup.md")]);
        assert!(
            res.warnings.is_empty(),
            "an idempotent duplicate literal must not warn"
        );
    }

    // ---- 5b. same src renamed twice: identical dest is idempotent, divergent is a hard error ----

    #[test]
    fn two_identical_rename_entries_keep_one_pair_without_error() {
        let offer = offer(&["a.md", "other.md"]);
        let res = resolve(
            &offer,
            &[
                Take::Rename {
                    src: "a.md",
                    dest: "b.md",
                },
                Take::Rename {
                    src: "a.md",
                    dest: "b.md",
                },
            ],
        );
        assert_kept(&res, &[("a.md", "b.md")]);
        assert!(
            res.warnings.is_empty(),
            "an idempotent identical rename must not warn"
        );
    }

    #[test]
    fn one_src_renamed_to_two_different_dests_is_a_hard_error_naming_both() {
        let offer = offer(&["a.md", "other.md"]);
        let rendered = rendered_error(
            &offer,
            &[
                Take::Rename {
                    src: "a.md",
                    dest: "b.md",
                },
                Take::Rename {
                    src: "a.md",
                    dest: "c.md",
                },
            ],
        );
        assert_named_diagnostic(&rendered, "a.md");
        assert!(
            rendered.contains("b.md") && rendered.contains("c.md"),
            "the rejection must name both divergent destinations; got:\n{rendered}"
        );
    }

    // ---- 6. non-offered literal / rename-src is a hard error (the seal) ----

    #[test]
    fn a_literal_not_in_the_offer_is_a_hard_error_naming_the_entry() {
        let offer = offer(&["present.md"]);
        let rendered = rendered_error(&offer, &[Take::Literal("absent.md")]);
        assert_named_diagnostic(&rendered, "absent.md");
    }

    #[test]
    fn a_rename_src_not_in_the_offer_is_a_hard_error_naming_the_src() {
        let offer = offer(&["present.md"]);
        let rendered = rendered_error(
            &offer,
            &[Take::Rename {
                src: "absent.md",
                dest: "x.md",
            }],
        );
        assert_named_diagnostic(&rendered, "absent.md");
    }

    #[test]
    fn a_non_offered_literal_close_to_an_offered_leaf_suggests_it() {
        let offer = offer(&["editor/init.lua", "editor/keymaps.lua"]);
        let rendered = rendered_error(&offer, &[Take::Literal("editor/inti.lua")]);
        assert!(
            rendered.contains(DID_YOU_MEAN) && rendered.contains("editor/init.lua"),
            "a typo'd non-offered literal must suggest the closest offered leaf via the \
             `did you mean` line; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("editor/keymaps.lua"),
            "a far-off offered leaf must NOT be suggested (bounded edit distance); got:\n{rendered}"
        );
    }

    // ---- 7. a no-match glob warns, it does not error ----

    #[test]
    fn a_glob_matching_zero_offered_leaves_warns_and_resolution_still_succeeds() {
        let offer = offer(&["kept.md"]);
        let res = resolve(
            &offer,
            &[Take::Literal("kept.md"), Take::Glob("nomatch/**")],
        );
        assert_kept(&res, &[("kept.md", "kept.md")]);
        assert_warnings(&res, &["nomatch/**"]);
    }

    #[test]
    fn no_match_warnings_are_sorted_by_pattern_independent_of_directive_order() {
        let offer = offer(&["kept.md"]);
        let forward = resolve(
            &offer,
            &[
                Take::Literal("kept.md"),
                Take::Glob("zeta/**"),
                Take::Glob("alpha/**"),
            ],
        );
        assert_warnings(&forward, &["alpha/**", "zeta/**"]);
        let reversed = resolve(
            &offer,
            &[
                Take::Literal("kept.md"),
                Take::Glob("alpha/**"),
                Take::Glob("zeta/**"),
            ],
        );
        assert_eq!(
            forward.warnings, reversed.warnings,
            "permuting no-match globs must not reorder the warnings"
        );
    }

    #[test]
    fn a_glob_whose_hits_are_all_already_consumed_keeps_nothing_and_does_not_warn() {
        let offer = offer(&["a.md"]);
        let res = resolve(
            &offer,
            &[
                Take::Rename {
                    src: "a.md",
                    dest: "renamed.md",
                },
                Take::Glob("*.md"),
            ],
        );
        assert_kept(&res, &[("a.md", "renamed.md")]);
        assert_warnings(&res, &[]);
    }

    // ---- 8. literal + rename of the SAME source leaf is a hard error ----

    #[test]
    fn a_leaf_used_as_both_literal_and_rename_src_is_a_hard_error() {
        let offer = offer(&["a.md"]);
        let rendered = rendered_error(
            &offer,
            &[
                Take::Literal("a.md"),
                Take::Rename {
                    src: "a.md",
                    dest: "b.md",
                },
            ],
        );
        assert_named_diagnostic(&rendered, "a.md");
    }

    // ---- 9. single-binding duplicate DESTINATION is a hard error (case-insensitive + NFC) ----

    #[test]
    fn two_distinct_sources_resolving_to_the_same_dest_is_a_hard_error() {
        let offer = offer(&["a", "x"]);
        let rendered = rendered_error(
            &offer,
            &[
                Take::Rename {
                    src: "a",
                    dest: "x",
                },
                Take::Literal("x"),
            ],
        );
        assert_named_diagnostic(&rendered, "x");
    }

    #[test]
    fn case_insensitively_colliding_dests_are_a_hard_error() {
        let offer = offer(&["a", "b"]);
        let rendered = rendered_error(
            &offer,
            &[
                Take::Rename {
                    src: "a",
                    dest: "C",
                },
                Take::Rename {
                    src: "b",
                    dest: "c",
                },
            ],
        );
        assert_named_diagnostic(&rendered, "C");
    }

    #[test]
    fn nfc_equivalent_unicode_dests_collide_as_a_hard_error() {
        let offer = offer(&["a", "b"]);
        let precomposed = "café";
        let decomposed = "cafe\u{0301}";
        assert_ne!(
            precomposed, decomposed,
            "the two dests differ byte-for-byte before NFC"
        );
        let rendered = rendered_error(
            &offer,
            &[
                Take::Rename {
                    src: "a",
                    dest: precomposed,
                },
                Take::Rename {
                    src: "b",
                    dest: decomposed,
                },
            ],
        );
        for phrase in [SELECTION, MATCHED_AGAINST, REMEDY] {
            assert!(
                rendered.contains(phrase),
                "NFC-equivalent dest collision must render the named phrase `{phrase}`; got:\n{rendered}"
            );
        }
        assert!(
            rendered.contains("café"),
            "the collision diagnostic must name the offending dest `café`; got:\n{rendered}"
        );
    }

    // ---- 10. rename dest portability (safe_relpath portion of D22) ----

    #[test]
    fn a_rename_dest_escaping_the_root_is_a_hard_error() {
        let offer = offer(&["a"]);
        let rendered = rendered_error(
            &offer,
            &[Take::Rename {
                src: "a",
                dest: "../escape",
            }],
        );
        assert_named_diagnostic(&rendered, "../escape");
    }

    #[test]
    fn an_absolute_rename_dest_is_a_hard_error() {
        let offer = offer(&["a"]);
        let rendered = rendered_error(
            &offer,
            &[Take::Rename {
                src: "a",
                dest: "/abs/x",
            }],
        );
        assert_named_diagnostic(&rendered, "/abs/x");
    }

    #[test]
    fn a_backslash_rename_dest_is_a_hard_error() {
        let offer = offer(&["a"]);
        let rendered = rendered_error(
            &offer,
            &[Take::Rename {
                src: "a",
                dest: "a\\b",
            }],
        );
        assert_named_diagnostic(&rendered, "a\\b");
    }

    // ---- 11. take = [] keeps nothing ----

    #[test]
    fn an_empty_take_keeps_nothing_with_no_warnings() {
        let offer = offer(&["a", "b", "c"]);
        let res = resolve_take(&offer, Some(&[])).expect("empty take resolves");
        assert!(
            res.kept.is_empty(),
            "`take = []` must keep nothing; got: {:?}",
            res.kept
        );
        assert!(res.warnings.is_empty(), "`take = []` must not warn");
    }

    // ---- 12. omitted take keeps everything at identity ----

    #[test]
    fn an_omitted_take_keeps_every_offered_leaf_at_its_identity_dest() {
        let offer = offer(&["a", "nested/b", "c"]);
        let res = resolve_take(&offer, None).expect("None take resolves");
        assert_kept(&res, &[("a", "a"), ("c", "c"), ("nested/b", "nested/b")]);
        assert!(res.warnings.is_empty(), "identity projection must not warn");
    }

    // ---- 13. D21 classification (consolidated) ----

    #[test]
    fn trailing_slash_classifies_as_a_glob() {
        assert!(
            is_take_glob("build/"),
            "a trailing slash makes a take entry a glob"
        );
    }

    #[test]
    fn star_question_and_bracket_classify_as_globs() {
        assert!(is_take_glob("skills/**"));
        assert!(is_take_glob("*.bak"));
        assert!(is_take_glob("file?.md"));
        assert!(is_take_glob("file[0-9].md"));
    }

    #[test]
    fn a_plain_nested_path_classifies_as_a_literal() {
        assert!(
            !is_take_glob("skills/gestalt/skill.md"),
            "a plain nested path with no glob metacharacters is a literal"
        );
    }

    #[test]
    fn brace_expansion_classifies_as_a_literal_not_a_glob() {
        assert!(
            !is_take_glob("{a,b}"),
            "brace expansion is NOT a glob marker in the take classifier"
        );
    }

    // ---- 14. dotfile matching (no opt-in, consistent with OfferSelection) ----

    #[test]
    fn a_star_glob_matches_offered_dotfiles_with_no_opt_in() {
        let offer = offer(&[".zshrc", ".config/nvim/init.lua", "plain.txt"]);
        let res = resolve(&offer, &[Take::Glob("*")]);
        assert_kept(
            &res,
            &[
                (".config/nvim/init.lua", ".config/nvim/init.lua"),
                (".zshrc", ".zshrc"),
                ("plain.txt", "plain.txt"),
            ],
        );
    }

    #[test]
    fn a_double_star_glob_matches_nested_offered_dotfiles() {
        let offer = offer(&[".config/nvim/init.lua", "plain.txt"]);
        let res = resolve(&offer, &[Take::Glob("**")]);
        assert_kept(
            &res,
            &[
                (".config/nvim/init.lua", ".config/nvim/init.lua"),
                ("plain.txt", "plain.txt"),
            ],
        );
    }

    // ---- 15. path-identity: full relative path is the unit, basenames may collide ----

    #[test]
    fn leaves_sharing_a_basename_are_distinct_and_both_kept_by_a_glob() {
        let offer = offer(&["a/SKILL.md", "b/SKILL.md"]);
        let res = resolve(&offer, &[Take::Glob("**")]);
        assert_kept(
            &res,
            &[("a/SKILL.md", "a/SKILL.md"), ("b/SKILL.md", "b/SKILL.md")],
        );
    }

    #[test]
    fn a_literal_keeps_the_full_relative_path_not_the_basename() {
        let offer = offer(&["a/SKILL.md", "b/SKILL.md"]);
        let res = resolve(&offer, &[Take::Literal("a/SKILL.md")]);
        assert_kept(&res, &[("a/SKILL.md", "a/SKILL.md")]);
    }
}
