use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn src_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(rel)
}

fn read_src(rel: &str) -> String {
    fs::read_to_string(src_path(rel)).unwrap_or_default()
}

fn is_char_literal(chars: &[char], i: usize) -> bool {
    match chars.get(i + 1) {
        Some('\\') => true,
        Some(&c) if c != '\'' => chars.get(i + 2) == Some(&'\''),
        _ => false,
    }
}

fn blank(c: char) -> char {
    if c == '\n' { '\n' } else { ' ' }
}

fn strip_comments(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '/' if chars.get(i + 1) == Some(&'/') => {
                i += 2;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
                out.push(' ');
            }
            '"' => {
                out.push('"');
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(' ');
                        i += 1;
                        if i < chars.len() {
                            out.push(blank(chars[i]));
                            i += 1;
                        }
                    } else {
                        out.push(blank(chars[i]));
                        i += 1;
                    }
                }
                if i < chars.len() {
                    out.push('"');
                    i += 1;
                }
            }
            '\'' if is_char_literal(&chars, i) => {
                out.push('\'');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' {
                        out.push(' ');
                        i += 1;
                    }
                    if i < chars.len() && chars[i] != '\'' {
                        out.push(' ');
                        i += 1;
                    }
                }
                if i < chars.len() {
                    out.push('\'');
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn declares_file_module(src: &str, name: &str) -> bool {
    strip_comments(src).split(';').any(|stmt| {
        let t = stmt.trim();
        if t.contains('{') {
            return false;
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        let Some(pos) = toks.iter().position(|&x| x == "mod") else {
            return false;
        };
        toks.get(pos + 1) == Some(&name)
            && toks.len() == pos + 2
            && toks[..pos].iter().all(|v| v.starts_with("pub"))
    })
}

fn unaliased_projection_leaves(stripped: &str) -> BTreeSet<String> {
    let mut leaves = BTreeSet::new();
    for stmt in stripped.split(';') {
        let t = stmt.trim();
        if !t.starts_with("pub use") || !t.contains("crate::projection") {
            continue;
        }
        let flat = t.replace(['{', '}'], " ");
        for token in flat.split(',') {
            let tok = token.trim();
            if tok.is_empty() || tok.contains(" as ") {
                continue;
            }
            let leaf = tok.rsplit("::").next().unwrap_or(tok);
            let leaf = leaf.split_whitespace().last().unwrap_or(leaf);
            if !leaf.is_empty() && leaf.chars().all(|c| c.is_alphanumeric() || c == '_') {
                leaves.insert(leaf.to_string());
            }
        }
    }
    leaves
}

fn keyword_names_type(src: &str, keyword: &str, name: &str) -> bool {
    let bytes = src.as_bytes();
    src.match_indices(keyword).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let rest = src[i + keyword.len()..].trim_start();
        before_ok
            && rest.strip_prefix(name).is_some_and(|after| {
                after
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
            })
    })
}

fn defines_materialization(src: &str) -> bool {
    let s = strip_comments(src);
    keyword_names_type(&s, "struct", "Materialization")
        || keyword_names_type(&s, "enum", "Materialization")
}

fn owns_materialization(src: &str) -> bool {
    let s = strip_comments(src);
    keyword_names_type(&s, "pub struct", "Materialization")
        || keyword_names_type(&s, "pub enum", "Materialization")
}

fn test_fn_names(src: &str) -> BTreeSet<String> {
    let s = strip_comments(src);
    let bytes = s.as_bytes();
    let mut names = BTreeSet::new();
    let mut from = 0;
    while let Some(rel) = s[from..].find("#[test]") {
        let start = from + rel + "#[test]".len();
        let mut i = start;
        loop {
            while i < s.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if s[i..].starts_with("#[") {
                match s[i..].find(']') {
                    Some(off) => i += off + 1,
                    None => break,
                }
            } else {
                break;
            }
        }
        if let Some(rest) = s[i..].strip_prefix("fn ") {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.insert(name);
            }
        }
        from = start;
    }
    names
}

fn assert_retained(rel: &str, pinned: &[&str]) {
    let present = test_fn_names(&read_src(rel));
    let missing: Vec<&str> = pinned
        .iter()
        .copied()
        .filter(|name| !present.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{rel} must retain every moved `#[test] fn` verbatim (move-only relocation of the \
         kernel unit tests); absent from the moved file: {missing:?}"
    );
}

const OFFER_UNIT_TESTS: &[&str] = &[
    "admits_published_separates_a_config_narrow_from_a_source_drop",
    "include_selects_a_matching_leaf",
    "exclude_wins_over_include_for_the_same_leaf",
    "leaf_outside_every_include_is_not_selected",
    "double_star_spans_multiple_path_segments",
    "leading_slash_anchors_to_the_offer_root_only",
    "unanchored_pattern_matches_at_any_depth",
    "trailing_slash_pattern_matches_directory_contents",
    "trailing_slash_exclude_prunes_a_whole_subtree",
    "star_include_matches_a_dotfile_with_no_opt_in",
    "double_star_include_matches_nested_dotfiles_and_dotdirs",
    "implicit_full_offer_selects_everything_except_vcs_metadata",
    "implicit_full_offer_honors_user_exclude_and_still_prunes_vcs",
    "explicit_include_does_not_prune_a_dot_git_named_match",
    "root_re_anchors_matching_and_publishes_root_relative_names",
    "rooted_implicit_full_publishes_root_relative_and_keeps_vcs_prune",
    "leaves_sharing_a_basename_are_distinct_and_both_selectable",
    "published_leaf_keeps_its_full_relative_path",
    "dot_github_include_does_not_disable_the_vcs_prune",
    "explicit_double_star_include_still_prunes_dot_git",
    "explicit_dot_git_component_include_selects_dot_git_contents",
    "implicit_full_offer_prunes_nested_dot_git",
    "bare_name_matches_a_directory_and_its_contents_at_any_depth",
    "slash_pattern_stays_root_anchored",
    "standalone_star_matches_at_any_depth_including_dotfiles",
    "empty_root_behaves_as_no_root",
    "slash_only_root_behaves_as_no_root",
    "traversal_shaped_published_leaf_is_skipped",
];

const TAKE_UNIT_TESTS: &[&str] = &[
    "each_take_rejection_ends_with_the_attribution_debug_command",
    "resolving_the_same_entries_in_any_permutation_yields_the_identical_kept_set",
    "a_mixed_glob_literal_rename_set_resolves_identically_under_reversal",
    "a_take_glob_matches_only_offered_leaves_and_never_introduces_an_unoffered_one",
    "explicit_literal_consumes_its_leaf_out_of_an_overlapping_glob",
    "rename_src_is_consumed_out_of_an_overlapping_glob_and_not_re_emitted_at_identity",
    "rename_emits_the_leaf_only_at_its_destination",
    "two_identical_literal_entries_keep_one_pair_without_error",
    "two_identical_rename_entries_keep_one_pair_without_error",
    "one_src_renamed_to_two_different_dests_is_a_hard_error_naming_both",
    "a_literal_not_in_the_offer_is_a_hard_error_naming_the_entry",
    "a_rename_src_not_in_the_offer_is_a_hard_error_naming_the_src",
    "a_non_offered_literal_close_to_an_offered_leaf_suggests_it",
    "a_glob_matching_zero_offered_leaves_warns_and_resolution_still_succeeds",
    "no_match_warnings_are_sorted_by_pattern_independent_of_directive_order",
    "a_glob_whose_hits_are_all_already_consumed_keeps_nothing_and_does_not_warn",
    "a_leaf_used_as_both_literal_and_rename_src_is_a_hard_error",
    "two_distinct_sources_resolving_to_the_same_dest_is_a_hard_error",
    "case_insensitively_colliding_dests_are_a_hard_error",
    "nfc_equivalent_unicode_dests_collide_as_a_hard_error",
    "a_rename_dest_escaping_the_root_is_a_hard_error",
    "an_absolute_rename_dest_is_a_hard_error",
    "a_backslash_rename_dest_is_a_hard_error",
    "an_empty_take_keeps_nothing_with_no_warnings",
    "an_omitted_take_keeps_every_offered_leaf_at_its_identity_dest",
    "trailing_slash_classifies_as_a_glob",
    "star_question_and_bracket_classify_as_globs",
    "a_plain_nested_path_classifies_as_a_literal",
    "brace_expansion_classifies_as_a_literal_not_a_glob",
    "a_star_glob_matches_offered_dotfiles_with_no_opt_in",
    "a_double_star_glob_matches_nested_offered_dotfiles",
    "leaves_sharing_a_basename_are_distinct_and_both_kept_by_a_glob",
    "a_literal_keeps_the_full_relative_path_not_the_basename",
    "cyrillic_case_colliding_dests_are_a_hard_error",
    "latin_accented_case_colliding_dests_are_a_hard_error",
    "fold_dest_keeps_eszett_and_ss_distinct_documented_limitation",
    "fold_dest_keeps_final_sigma_distinct_but_folds_capital_sigma",
];

const COLLAPSE_UNIT_TESTS: &[&str] = &[
    "default_link_collapses_a_wholly_taken_directory_to_one_dir_symlink",
    "default_copy_collapses_a_wholly_taken_directory_to_one_subtree",
    "the_same_kept_set_collapses_identically_regardless_of_how_the_dir_was_offered",
    "a_single_leaf_directory_collapses_under_link",
    "a_single_leaf_directory_collapses_under_copy",
    "a_top_level_leaf_outside_any_directory_stays_a_per_leaf_artifact",
    "a_top_level_leaf_coexists_with_a_collapsed_sibling_dir",
    "link_within_dir_exclude_blocks_collapse_falls_back_per_leaf_and_warns",
    "link_fallback_never_leaks_the_excluded_leaf_into_the_plan",
    "copy_within_dir_exclude_does_not_block_collapse_and_emits_no_warning",
    "link_per_leaf_rename_in_a_dir_blocks_collapse_without_a_lost_collapse_warning",
    "copy_per_leaf_rename_in_a_dir_blocks_collapse_without_a_warning",
    "renaming_the_only_leaf_out_of_a_dir_keeps_it_per_leaf_not_collapsed",
    "two_sibling_dirs_one_collapses_and_one_falls_back_under_link",
    "force_per_leaf_emits_every_kept_leaf_even_for_a_wholly_taken_dir_under_link",
    "force_per_leaf_emits_every_kept_leaf_even_for_a_wholly_taken_dir_under_copy",
    "force_collapse_collapses_a_wholly_taken_dir_with_no_warning_under_link",
    "force_collapse_link_blocked_by_within_dir_exclude_is_a_hard_error_naming_the_dir",
    "force_collapse_blocked_by_a_per_leaf_rename_is_a_hard_error_naming_the_dir",
    "force_collapse_copy_with_a_within_dir_exclude_collapses_and_does_not_error",
    "permuting_kept_and_full_tree_yields_an_identical_plan",
    "a_wholly_taken_nested_tree_collapses_at_the_topmost_directory_under_link",
    "a_wholly_taken_nested_tree_collapses_at_the_topmost_directory_under_copy",
    "link_a_deep_within_dir_exclude_blocks_only_its_subtree_a_clean_sibling_still_collapses",
    "copy_a_deep_within_dir_exclude_does_not_block_the_topmost_collapse",
    "force_collapse_link_with_a_deep_within_dir_exclude_is_a_hard_error",
    "a_rename_dest_landing_inside_a_collapsible_dir_blocks_that_dirs_collapse",
    "force_collapse_with_a_rename_dest_landing_inside_a_collapsible_dir_is_a_hard_error",
    "force_collapse_blocked_dir_is_deterministic_under_kept_permutation_for_renames",
    "force_collapse_blocked_dir_is_deterministic_under_kept_permutation_for_within_dir_exclude",
    "a_link_blocked_ancestor_with_a_clean_collapsed_descendant_does_not_warn",
    "force_collapse_with_a_clean_collapsed_descendant_under_a_link_blocked_ancestor_is_ok",
];

#[test]
fn projection_mod_exists_and_is_registered_in_lib_rs() {
    assert!(
        src_path("projection/mod.rs").is_file(),
        "src/projection/mod.rs must exist (the projection module root created by T006)"
    );
    assert!(
        declares_file_module(&read_src("lib.rs"), "projection"),
        "src/lib.rs must register the projection module with an item-level `pub mod projection;` \
         declaration (a bare mention in a comment or doc line does not register it)"
    );
}

#[test]
fn projection_mod_declares_the_four_file_backed_submodules() {
    let mod_rs = read_src("projection/mod.rs");
    for (file, module) in [
        ("offer.rs", "offer"),
        ("take.rs", "take"),
        ("collapse.rs", "collapse"),
        ("model.rs", "model"),
    ] {
        let rel = format!("projection/{file}");
        assert!(
            src_path(&rel).is_file(),
            "src/{rel} must exist after the git mv of kernel/{{selection->offer, take, collapse}} \
             and the Materialization move into projection::model"
        );
        assert!(
            declares_file_module(&mod_rs, module),
            "src/projection/mod.rs must declare `mod {module};` so src/{rel} is a live file-backed \
             module, not an orphaned placeholder file"
        );
    }
}

#[test]
fn projection_build_and_diagnostic_modules_exist_and_are_declared() {
    let mod_rs = read_src("projection/mod.rs");
    for (file, module) in [("build.rs", "build"), ("diagnostic.rs", "diagnostic")] {
        let rel = format!("projection/{file}");
        assert!(
            src_path(&rel).is_file(),
            "src/{rel} must exist after the T009 move of the projection cluster out of \
             sync/plan.rs (specs and output types → model.rs, verbs and validation → build.rs, \
             ProjectionWarning/ProjectionError → diagnostic.rs)"
        );
        assert!(
            declares_file_module(&mod_rs, module),
            "src/projection/mod.rs must declare `mod {module};` so src/{rel} is a live \
             file-backed module, not an orphaned placeholder file"
        );
    }
}

#[test]
fn moved_kernel_source_files_are_gone() {
    for file in ["selection.rs", "take.rs", "collapse.rs"] {
        let rel = format!("kernel/{file}");
        assert!(
            !src_path(&rel).exists(),
            "src/{rel} must no longer exist — its implementation moved into src/projection/ \
             (a lingering copy means the move was duplicated, not relocated)"
        );
    }
}

#[test]
fn kernel_facade_is_absent_and_moved_symbols_are_public_from_projection_owners() {
    use phora::projection::collapse::{
        CollapseChoice, CollapseMode, CollapsePlan, CollapseWarning, plan_collapse,
    };
    use phora::projection::model::Materialization;
    use phora::projection::offer::{OfferSelection, compile_take_glob};
    use phora::projection::take::{
        ResolvedTake, Take, TakeResolution, TakeWarning, is_take_glob, resolve_take,
    };

    fn assert_public_type<T>() {}
    fn assert_public_item<T>(_: T) {}

    assert!(
        !src_path("kernel/mod.rs").exists(),
        "src/kernel/mod.rs must be absent after the T030 compatibility facade expires"
    );
    assert!(
        !declares_file_module(&read_src("lib.rs"), "kernel"),
        "src/lib.rs must not register the removed file-backed kernel module"
    );

    assert_public_type::<OfferSelection>();
    assert_public_type::<ResolvedTake>();
    assert_public_type::<Take<'static>>();
    assert_public_type::<TakeResolution>();
    assert_public_type::<TakeWarning>();
    assert_public_type::<CollapseChoice>();
    assert_public_type::<CollapseMode>();
    assert_public_type::<CollapsePlan>();
    assert_public_type::<CollapseWarning>();
    assert_public_type::<Materialization>();
    assert_public_item(compile_take_glob);
    assert_public_item(is_take_glob);
    assert_public_item(resolve_take);
    assert_public_item(plan_collapse);
}

#[test]
fn materialization_is_owned_by_projection_model_exclusively() {
    fn assert_public_type<T>() {}

    let model = read_src("projection/model.rs");
    assert!(
        owns_materialization(&model),
        "src/projection/model.rs must OWN the Materialization type as a real \
         `pub struct`/`pub enum Materialization` item (moved out of the former kernel/collapse.rs), \
         not merely re-export or reference it"
    );
    assert!(
        declares_file_module(&read_src("projection/mod.rs"), "model"),
        "src/projection/mod.rs must publicly expose the file-backed model module so the final \
         projection::model::Materialization path remains available"
    );
    assert_public_type::<phora::projection::model::Materialization>();
    for file in ["offer.rs", "take.rs", "collapse.rs", "mod.rs"] {
        let rel = format!("projection/{file}");
        assert!(
            !defines_materialization(&read_src(&rel)),
            "src/{rel} must NOT define Materialization — ownership is exclusive to \
             projection/model.rs; a leftover struct/enum definition here means the move was \
             duplicated, not relocated"
        );
    }
}

#[test]
fn moved_offer_module_retains_every_unit_test() {
    assert_retained("projection/offer.rs", OFFER_UNIT_TESTS);
}

#[test]
fn moved_take_module_retains_every_unit_test() {
    assert_retained("projection/take.rs", TAKE_UNIT_TESTS);
}

#[test]
fn moved_collapse_module_retains_every_unit_test() {
    assert_retained("projection/collapse.rs", COLLAPSE_UNIT_TESTS);
}

#[test]
fn helper_aliased_reexport_leaf_does_not_preserve_source_name() {
    let aliased =
        unaliased_projection_leaves("pub use crate::projection::offer::OfferSelection as _;");
    assert!(
        !aliased.contains("OfferSelection"),
        "an aliased leaf (`as _`) rebinds the exported name and must not count as preserving \
         kernel::OfferSelection"
    );
    let plain = unaliased_projection_leaves("pub use crate::projection::offer::OfferSelection;");
    assert!(
        plain.contains("OfferSelection"),
        "a plain unaliased leaf must count as preserving kernel::OfferSelection"
    );
    let grouped = unaliased_projection_leaves(
        "pub use crate::projection::collapse::{CollapseMode, Materialization};",
    );
    assert!(grouped.contains("CollapseMode") && grouped.contains("Materialization"));
}

#[test]
fn helper_de_attributed_or_commented_test_stub_is_not_counted() {
    assert!(
        !test_fn_names("fn admits_published_x() {}").contains("admits_published_x"),
        "a fn with no #[test] attribute must not count as a retained unit test"
    );
    assert!(
        !test_fn_names("// #[test]\nfn admits_published_x() {}").contains("admits_published_x"),
        "a commented-out #[test] must not count as a retained unit test"
    );
    assert!(
        test_fn_names("#[test]\n#[ignore]\nfn admits_published_x() {}")
            .contains("admits_published_x"),
        "a genuine #[test] fn (even behind further attributes) must be counted"
    );
}

#[test]
fn helper_module_and_type_matchers_reject_comments_reexports_and_impls() {
    assert!(!declares_file_module(
        "// pub mod projection;",
        "projection"
    ));
    assert!(declares_file_module("pub mod projection;", "projection"));
    assert!(declares_file_module("pub(crate) mod offer;", "offer"));
    assert!(
        !declares_file_module("mod offer { fn x() {} }", "offer"),
        "an inline module body is not a file-backed module declaration"
    );
    assert!(owns_materialization(
        "#[derive(Debug)]\npub enum Materialization { A }"
    ));
    assert!(
        !owns_materialization("pub use crate::projection::model::Materialization;"),
        "a re-export must not satisfy type ownership"
    );
    assert!(
        !defines_materialization("impl Materialization { fn key() {} }"),
        "an impl block must not count as a type definition"
    );
    assert!(defines_materialization("enum Materialization { A }"));
}

#[test]
fn helper_comment_or_string_cannot_fake_or_trip_materialization_ownership() {
    assert!(
        !owns_materialization("// pub struct Materialization { key: String }\n"),
        "a line comment naming `pub struct Materialization` must not satisfy ownership"
    );
    assert!(
        !owns_materialization("/* pub enum Materialization { A } */"),
        "a block comment naming `pub enum Materialization` must not satisfy ownership"
    );
    assert!(
        !owns_materialization("const DOC: &str = \"pub struct Materialization { a: u8 }\";"),
        "a string literal containing `pub struct Materialization` must not satisfy ownership"
    );
    assert!(
        !defines_materialization("// enum Materialization { A }\n"),
        "a comment naming a Materialization definition must not falsely trip exclusivity"
    );
    assert!(
        !defines_materialization("let s = \"struct Materialization { a: u8 }\";"),
        "a string literal naming a Materialization definition must not falsely trip exclusivity"
    );
    assert!(
        owns_materialization("#[derive(Debug)]\npub struct Materialization { key: String }"),
        "a genuine `pub struct Materialization` item must still satisfy ownership"
    );
}

fn extract_comment_text(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                out.push(chars[i]);
                i += 1;
            }
            out.push('\n');
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                out.push(chars[i]);
                i += 1;
            }
            i = (i + 2).min(chars.len());
            out.push('\n');
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

const COMPAT_VOCAB: &[&str] = &[
    "compat",
    "facade",
    "phase",
    "temporary",
    "transitional",
    "shim",
    "migration",
];

const COMPAT_PURPOSE: &[&str] = &["re-export", "reexport", "compat", "callers", "kernel"];

const COMPAT_NEGATORS: &[&str] = &["not", "never", "no", "isn", "aren"];

const COMPAT_NEG_WINDOW: usize = 3;

fn line_has_positive_vocab(line: &str) -> bool {
    let toks: Vec<String> = line
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    toks.iter().enumerate().any(|(i, tok)| {
        COMPAT_VOCAB.contains(&tok.as_str())
            && !toks[i.saturating_sub(COMPAT_NEG_WINDOW)..i]
                .iter()
                .any(|w| COMPAT_NEGATORS.contains(&w.as_str()))
    })
}

fn comment_states_phase_scoped_facade(comments: &str) -> bool {
    comments.lines().any(|line| {
        let lower = line.to_lowercase();
        line_has_positive_vocab(line) && COMPAT_PURPOSE.iter().any(|p| lower.contains(p))
    })
}

fn publicly_exposes_kernel_facade(src: &str) -> bool {
    strip_comments(src).split(';').any(|item| {
        let words: Vec<&str> = item
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .filter(|word| !word.is_empty())
            .collect();
        let Some(public) = words.iter().position(|word| *word == "pub") else {
            return false;
        };
        let tail = &words[public + 1..];
        tail.windows(2).any(|pair| pair == ["mod", "kernel"])
            || tail
                .iter()
                .position(|word| *word == "use")
                .is_some_and(|use_pos| tail[use_pos + 1..].contains(&"kernel"))
    })
}

#[test]
fn phase_scoped_kernel_compatibility_shim_has_expired() {
    let lib_rs = read_src("lib.rs");
    assert!(
        !src_path("kernel/mod.rs").exists(),
        "the phase-scoped kernel compatibility shim must expire by removing src/kernel/mod.rs"
    );
    assert!(
        !publicly_exposes_kernel_facade(&lib_rs),
        "src/lib.rs must not recreate the expired kernel compatibility surface through either a \
         module declaration or a public-use alias"
    );
}

#[test]
fn helper_extract_comment_text_and_reexport_context_are_substance_based() {
    assert_eq!(
        extract_comment_text("let x = 1; // facade shim\npub use a;").trim(),
        "facade shim",
        "line-comment content after `//` must be extracted"
    );
    assert_eq!(
        extract_comment_text("/* phase-scoped compat */\npub use a;").trim(),
        "phase-scoped compat",
        "block-comment content must be extracted"
    );
    assert!(
        extract_comment_text("const S: &str = \"facade compat phase\";")
            .trim()
            .is_empty(),
        "compat vocabulary inside a string literal must NOT count as a comment"
    );
}

#[test]
fn helper_compat_constraint_requires_positive_attribution_and_purpose() {
    assert!(
        comment_states_phase_scoped_facade(
            "Phase-scoped compat facade: keeps kernel:: callers green until the kernel dissolves (T030)"
        ),
        "a legitimate phase-scoped compat facade note (non-negated vocab + a compat/callers/kernel \
         purpose reference) must satisfy"
    );
    assert!(
        !comment_states_phase_scoped_facade("NOT a temporary shim — this is permanent kernel API"),
        "a negation marker governing the vocab word (NOT a temporary shim) must not satisfy, even \
         though the line mentions kernel"
    );
    assert!(
        !comment_states_phase_scoped_facade("migration of unrelated code happens elsewhere"),
        "a bare vocab word (migration) with no re-export/compat/callers/kernel purpose reference \
         must not satisfy — an unrelated note must not game the oracle"
    );
}

#[test]
fn helper_string_literal_cannot_fake_test_retention() {
    let faked = "const SNIPPET: &str = \"#[test]\nfn admits_published_x() {}\";";
    assert!(
        !test_fn_names(faked).contains("admits_published_x"),
        "a `#[test] fn` buried inside a string literal must not count as a retained unit test"
    );
    let real = "#[test]\nfn admits_published_x() {}";
    assert!(
        test_fn_names(real).contains("admits_published_x"),
        "a genuine `#[test] fn` item must still be counted as retained"
    );
}
