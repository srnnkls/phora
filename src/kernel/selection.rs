//! Offer selection: gitignore-style include/exclude compiled into a leaf-set selector.

use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::Result;

/// Compiles an offer's include/exclude patterns (gitignore syntax) into a leaf-set
/// selector. A candidate leaf is selected iff it matches the include set (or the
/// offer is implicit-full) and matches no exclude. Dotfiles match plain `*`/`**`
/// with no opt-in. VCS metadata under any `.git/` is pruned unless an include names
/// the `.git` path component exactly.
pub struct OfferSelection {
    include: Option<GlobSet>,
    exclude: GlobSet,
    root: Option<String>,
    vcs_named: bool,
}

impl OfferSelection {
    /// Compiles offer patterns relative to an optional `root`. With a root set,
    /// only candidates under it are considered, matching is anchored at the root,
    /// and published leaves are root-relative.
    ///
    /// # Errors
    ///
    /// Returns an error if any include/exclude pattern is not a valid glob.
    pub fn compile(include: &[String], exclude: &[String], root: Option<&Path>) -> Result<Self> {
        let root = root
            .map(|r| r.to_string_lossy().trim_end_matches('/').to_string())
            .filter(|r| !r.is_empty());
        Ok(Self {
            include: Self::build_globset_opt(include)?,
            exclude: Self::build_globset(exclude)?,
            root,
            vcs_named: include
                .iter()
                .any(|p| p.split(['/', '\\']).any(|seg| seg == ".git")),
        })
    }

    #[must_use]
    pub fn admits_published(&self, published: &str) -> bool {
        self.selects_leaf(published) || self.selects_leaf(&format!("{published}/"))
    }

    /// Returns the root-relative published paths of every selected candidate leaf.
    #[must_use]
    pub fn select(&self, candidates: &[&str]) -> Vec<String> {
        let mut selected: Vec<String> = candidates
            .iter()
            .filter_map(|c| self.local_path(c))
            .filter(|local| self.selects_leaf(local))
            .collect();
        selected.sort_unstable();
        selected
    }

    fn local_path(&self, candidate: &str) -> Option<String> {
        let local = match &self.root {
            None => candidate.to_string(),
            Some(root) => candidate
                .strip_prefix(root)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(str::to_string)?,
        };
        (!Self::is_traversal_shaped(&local)).then_some(local)
    }

    fn is_traversal_shaped(local: &str) -> bool {
        local.is_empty()
            || local.starts_with('/')
            || local.contains('\\')
            || local.split('/').any(|seg| seg == "..")
    }

    fn selects_leaf(&self, local: &str) -> bool {
        if self.is_pruned_vcs_metadata(local) || self.exclude.is_match(local) {
            return false;
        }
        match &self.include {
            Some(inc) => inc.is_match(local),
            None => true,
        }
    }

    fn is_pruned_vcs_metadata(&self, local: &str) -> bool {
        !self.vcs_named && local.split('/').any(|seg| seg == ".git")
    }

    fn build_globset(patterns: &[String]) -> Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            for translated in Self::translate(pattern) {
                builder.add(
                    GlobBuilder::new(&translated)
                        .literal_separator(true)
                        .build()?,
                );
            }
        }
        Ok(builder.build()?)
    }

    fn build_globset_opt(patterns: &[String]) -> Result<Option<GlobSet>> {
        if patterns.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Self::build_globset(patterns)?))
        }
    }

    fn translate(pattern: &str) -> Vec<String> {
        let directory = pattern.ends_with('/');
        let trimmed = pattern.trim_end_matches('/');

        if let Some(anchored) = trimmed.strip_prefix('/') {
            return vec![Self::as_dir(anchored, directory)];
        }
        if trimmed.contains('/') {
            return vec![Self::as_dir(trimmed, directory)];
        }
        let base = Self::as_dir(trimmed, directory);
        if trimmed == "**" {
            return vec![base];
        }
        if directory {
            return vec![format!("**/{base}"), base];
        }
        vec![
            format!("**/{base}"),
            base.clone(),
            format!("**/{base}/**"),
            format!("{base}/**"),
        ]
    }

    fn as_dir(pattern: &str, directory: bool) -> String {
        if directory {
            format!("{pattern}/**")
        } else {
            pattern.to_string()
        }
    }
}

/// Compiles one gitignore pattern through the same translation `OfferSelection`
/// applies, so a take glob matches offered leaves the way the offer compiled them.
///
/// # Errors
///
/// Returns an error if the pattern does not translate to a valid glob.
pub fn compile_take_glob(pattern: &str) -> Result<GlobSet> {
    OfferSelection::build_globset(std::slice::from_ref(&pattern.to_string()))
}

#[cfg(test)]
mod offer_compiler_tests {
    use std::path::Path;

    use super::OfferSelection;

    fn compile(include: &[&str], exclude: &[&str]) -> OfferSelection {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        OfferSelection::compile(&inc, &exc, None).expect("offer patterns compile")
    }

    fn compile_rooted(include: &[&str], exclude: &[&str], root: &str) -> OfferSelection {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        OfferSelection::compile(&inc, &exc, Some(Path::new(root))).expect("rooted offer compiles")
    }

    fn selected(sel: &OfferSelection, candidates: &[&str]) -> Vec<String> {
        sel.select(candidates)
    }

    #[test]
    fn admits_published_separates_a_config_narrow_from_a_source_drop() {
        let full = compile(&[], &[]);
        assert!(
            full.admits_published("editor"),
            "a full offer still admits `editor`, so a recorded `editor` the source dropped is a \
             genuine source narrowing (D9), not a config narrowing"
        );

        let narrowed = compile(&["docs/**"], &[]);
        assert!(
            !narrowed.admits_published("editor"),
            "an offer narrowed to `docs/**` no longer admits `editor`: the config narrowed past it \
             on purpose, so the record is a pure orphan for prune, not a seal violation"
        );
        assert!(
            narrowed.admits_published("docs/readme.md"),
            "the narrowed offer still admits the `docs/**` path it kept"
        );
    }

    fn assert_selects(sel: &OfferSelection, candidates: &[&str], expected: &[&str]) {
        let mut got = selected(sel, candidates);
        got.sort();
        let mut want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
        want.sort();
        assert_eq!(
            got, want,
            "exact published-leaf set mismatch for candidates {candidates:?}: \
             extras, duplicates, or omissions all fail here"
        );
    }

    // ---- 1. set composition: include − exclude, exclude-wins ----

    #[test]
    fn include_selects_a_matching_leaf() {
        let sel = compile(&["skills/**"], &[]);
        assert_selects(
            &sel,
            &["skills/gestalt/SKILL.md", "editor/init.lua"],
            &["skills/gestalt/SKILL.md"],
        );
    }

    #[test]
    fn exclude_wins_over_include_for_the_same_leaf() {
        let sel = compile(&["skills/**"], &["skills/private/**"]);
        assert_selects(
            &sel,
            &["skills/gestalt/SKILL.md", "skills/private/secret.md"],
            &["skills/gestalt/SKILL.md"],
        );
    }

    #[test]
    fn leaf_outside_every_include_is_not_selected() {
        let sel = compile(&["skills/**"], &[]);
        assert_selects(
            &sel,
            &["skills/gestalt/SKILL.md", "editor/init.lua"],
            &["skills/gestalt/SKILL.md"],
        );
    }

    // ---- 2. gitignore syntax ----

    #[test]
    fn double_star_spans_multiple_path_segments() {
        let sel = compile(&["skills/**"], &[]);
        assert_selects(
            &sel,
            &["skills/a/b/c/deep.md", "editor/init.lua"],
            &["skills/a/b/c/deep.md"],
        );
    }

    #[test]
    fn leading_slash_anchors_to_the_offer_root_only() {
        let sel = compile(&["/config.toml"], &[]);
        assert_selects(
            &sel,
            &["config.toml", "nested/config.toml"],
            &["config.toml"],
        );
    }

    #[test]
    fn unanchored_pattern_matches_at_any_depth() {
        let sel = compile(&["*.toml"], &[]);
        assert_selects(
            &sel,
            &["config.toml", "nested/deep/config.toml", "notes.md"],
            &["config.toml", "nested/deep/config.toml"],
        );
    }

    #[test]
    fn trailing_slash_pattern_matches_directory_contents() {
        let sel = compile(&["build/"], &[]);
        assert_selects(
            &sel,
            &["build/out.js", "build/sub/more.js", "src/main.rs"],
            &["build/out.js", "build/sub/more.js"],
        );
    }

    #[test]
    fn trailing_slash_exclude_prunes_a_whole_subtree() {
        let sel = compile(&["**"], &["cache/"]);
        assert_selects(
            &sel,
            &["app.rs", "cache/x.tmp", "cache/deep/y.tmp"],
            &["app.rs"],
        );
    }

    // ---- 3. dotfiles MATCH with no opt-in (D4 / RQ-3) ----

    #[test]
    fn star_include_matches_a_dotfile_with_no_opt_in() {
        let sel = compile(&["*"], &[]);
        assert_selects(
            &sel,
            &[".zshrc", "nested/buried.txt"],
            &[".zshrc", "nested/buried.txt"],
        );
    }

    #[test]
    fn double_star_include_matches_nested_dotfiles_and_dotdirs() {
        let sel = compile(&["**"], &[]);
        assert_selects(
            &sel,
            &[".config/nvim/init.lua", "plain.txt", ".git/config"],
            &[".config/nvim/init.lua", "plain.txt"],
        );
    }

    // ---- 5. implicit-full offer (empty include) = everything except VCS metadata (M5) ----

    #[test]
    fn implicit_full_offer_selects_everything_except_vcs_metadata() {
        let sel = compile(&[], &[]);
        assert_selects(
            &sel,
            &[
                "README.md",
                ".zshrc",
                ".git/config",
                ".git/hooks/pre-commit",
            ],
            &["README.md", ".zshrc"],
        );
    }

    #[test]
    fn implicit_full_offer_honors_user_exclude_and_still_prunes_vcs() {
        let sel = compile(&[], &["cache/"]);
        assert_selects(
            &sel,
            &["README.md", "cache/x.tmp", ".git/config"],
            &["README.md"],
        );
    }

    #[test]
    fn explicit_include_does_not_prune_a_dot_git_named_match() {
        let sel = compile(&[".git/**"], &[]);
        assert_selects(&sel, &[".git/config", "README.md"], &[".git/config"]);
    }

    // ---- 6. root re-anchors AND publishes a root-relative namespace (D20) ----

    #[test]
    fn root_re_anchors_matching_and_publishes_root_relative_names() {
        let sel = compile_rooted(&["*.lua"], &[], "editor");
        assert_selects(
            &sel,
            &["editor/init.lua", "editor/README.md", "other/init.lua"],
            &["init.lua"],
        );
    }

    #[test]
    fn rooted_implicit_full_publishes_root_relative_and_keeps_vcs_prune() {
        let sel = compile_rooted(&[], &[], "editor");
        assert_selects(
            &sel,
            &["editor/init.lua", "editor/.git/config", "outside.txt"],
            &["init.lua"],
        );
    }

    // ---- 7. path-identity: full relative path is the unit, basenames may collide (D18) ----

    #[test]
    fn leaves_sharing_a_basename_are_distinct_and_both_selectable() {
        let sel = compile(&["**"], &[]);
        assert_selects(
            &sel,
            &["a/SKILL.md", "b/SKILL.md", ".git/config"],
            &["a/SKILL.md", "b/SKILL.md"],
        );
    }

    #[test]
    fn published_leaf_keeps_its_full_relative_path() {
        let sel = compile(&["skills/**"], &[]);
        assert_selects(
            &sel,
            &["skills/gestalt/SKILL.md", "editor/init.lua"],
            &["skills/gestalt/SKILL.md"],
        );
    }

    // ---- fix 1: VCS prune uses a component-wise `.git` check, not a substring ----

    #[test]
    fn dot_github_include_does_not_disable_the_vcs_prune() {
        let sel = compile(&[".github/**", "**"], &[]);
        assert_selects(
            &sel,
            &[".github/workflows/ci.yml", ".git/config", "README.md"],
            &[".github/workflows/ci.yml", "README.md"],
        );
    }

    #[test]
    fn explicit_double_star_include_still_prunes_dot_git() {
        let sel = compile(&["**"], &[]);
        assert_selects(&sel, &[".git/config", "README.md"], &["README.md"]);
    }

    #[test]
    fn explicit_dot_git_component_include_selects_dot_git_contents() {
        let sel = compile(&[".git/**"], &[]);
        assert_selects(&sel, &[".git/config", "README.md"], &[".git/config"]);
    }

    #[test]
    fn implicit_full_offer_prunes_nested_dot_git() {
        let sel = compile(&[], &[]);
        assert_selects(
            &sel,
            &["sub/.git/config", "sub/keep.txt", "README.md"],
            &["sub/keep.txt", "README.md"],
        );
    }

    // ---- fix 2: bare no-slash name also matches a directory's contents (D4) ----

    #[test]
    fn bare_name_matches_a_directory_and_its_contents_at_any_depth() {
        let sel = compile(&["build"], &[]);
        assert_selects(
            &sel,
            &["build", "build/out.js", "x/build/deep.js", "src/main.rs"],
            &["build", "build/out.js", "x/build/deep.js"],
        );
    }

    #[test]
    fn slash_pattern_stays_root_anchored() {
        let sel = compile(&["src/*.rs"], &[]);
        assert_selects(
            &sel,
            &["src/main.rs", "nested/src/lib.rs"],
            &["src/main.rs"],
        );
    }

    // ---- fix 3: a standalone `*` matches at any depth ----

    #[test]
    fn standalone_star_matches_at_any_depth_including_dotfiles() {
        let sel = compile(&["*"], &[]);
        assert_selects(
            &sel,
            &["root.txt", "nested/file.txt", ".zshrc"],
            &["root.txt", "nested/file.txt", ".zshrc"],
        );
    }

    // ---- fix 4: empty/slash-only root behaves as no root ----

    #[test]
    fn empty_root_behaves_as_no_root() {
        let inc = vec!["*.lua".to_string()];
        let sel =
            OfferSelection::compile(&inc, &[], Some(Path::new(""))).expect("empty root compiles");
        assert_selects(
            &sel,
            &["init.lua", "nested/extra.lua", "notes.md"],
            &["init.lua", "nested/extra.lua"],
        );
    }

    #[test]
    fn slash_only_root_behaves_as_no_root() {
        let inc = vec!["*.lua".to_string()];
        let sel = OfferSelection::compile(&inc, &[], Some(Path::new("/")))
            .expect("slash-only root compiles");
        assert_selects(
            &sel,
            &["init.lua", "nested/extra.lua", "notes.md"],
            &["init.lua", "nested/extra.lua"],
        );
    }

    // ---- fix 5: guard published path against traversal after root-strip ----

    #[test]
    fn traversal_shaped_published_leaf_is_skipped() {
        let sel = compile_rooted(&[], &[], "editor");
        assert_selects(
            &sel,
            &["editor/init.lua", "editor/../outside.txt"],
            &["init.lua"],
        );
    }
}
