//! Include/exclude selection: path-level gating plus the sole dotfile opt-in gate.

use std::path::Path;

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::Result;

/// Decides artifact and path membership from include/exclude patterns. Hidden
/// artifact names are admitted only by a literal leading-dot include pattern;
/// this is the one place that rule lives.
#[derive(Clone)]
pub struct Selection {
    artifact_include: Option<GlobSet>,
    artifact_exclude: GlobSet,
    path_include: Option<GlobSet>,
    path_exclude: GlobSet,
    dotfile_include: Option<GlobSet>,
}

impl Selection {
    /// # Errors
    ///
    /// Returns an error if any include/exclude pattern is not a valid glob.
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        let (art_inc, path_inc) = Self::partition_patterns(include);
        let (art_exc, path_exc) = Self::partition_patterns(exclude);
        let dotfile: Vec<String> = include
            .iter()
            .filter(|p| p.starts_with('.'))
            .cloned()
            .collect();

        Ok(Self {
            artifact_include: Self::build_globset_opt(&art_inc)?,
            artifact_exclude: Self::build_globset(&art_exc)?,
            path_include: Self::build_globset_opt(&path_inc)?,
            path_exclude: Self::build_globset(&path_exc)?,
            dotfile_include: Self::build_globset_opt(&dotfile)?,
        })
    }

    fn partition_patterns(patterns: &[String]) -> (Vec<String>, Vec<String>) {
        let mut artifact = Vec::new();
        let mut path = Vec::new();

        for p in patterns {
            if Self::is_path_level(p) {
                path.extend(Self::normalize_path_pattern(p));
            } else {
                artifact.push(p.clone());
            }
        }

        (artifact, path)
    }

    /// Artifact-level patterns name an artifact directly: no `/`, no `**`, no leading `/`.
    /// Bare-name globs (`code-*`, `*.bak`) stay artifact-level. Anything with a path
    /// separator or `**` is path-level.
    fn is_path_level(pattern: &str) -> bool {
        pattern.starts_with('/') || pattern.contains('/') || pattern.contains("**")
    }

    /// Anchored (leading `/`): strip the slash, match from the artifact root.
    /// Unanchored: emit BOTH the bare pattern and a `**/`-prefixed variant, because
    /// globset's `**/` requires at least one leading segment yet the bare form must
    /// still match files at the artifact root.
    fn normalize_path_pattern(pattern: &str) -> Vec<String> {
        if let Some(anchored) = pattern.strip_prefix('/') {
            vec![anchored.to_string()]
        } else if pattern.starts_with("**/") {
            vec![pattern.to_string()]
        } else {
            vec![pattern.to_string(), format!("**/{pattern}")]
        }
    }

    fn build_globset(patterns: &[String]) -> Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for p in patterns {
            builder.add(Glob::new(p)?);
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

    fn allows_artifact(&self, name: &str) -> bool {
        if let Some(inc) = &self.artifact_include
            && !inc.is_match(name)
        {
            return false;
        }
        !self.artifact_exclude.is_match(name)
    }

    /// Whether `name` is a selected artifact. Hidden names (leading `.`) require a
    /// literal leading-dot include pattern to match; globset has no dotglob, so `*`
    /// and `code-*` never opt them in. Exclude still wins.
    #[must_use]
    pub fn selects_artifact(&self, name: &str) -> bool {
        if name.starts_with('.') {
            let opted_in = self
                .dotfile_include
                .as_ref()
                .is_some_and(|inc| inc.is_match(name));
            return opted_in && !self.artifact_exclude.is_match(name);
        }
        self.allows_artifact(name)
    }

    fn normalize_path(path: &Path) -> String {
        let s = path.to_string_lossy();
        #[cfg(windows)]
        {
            s.replace('\\', "/")
        }
        #[cfg(not(windows))]
        {
            s.into_owned()
        }
    }

    /// Files must match include (if any) and not match exclude. Directories are never
    /// pruned on include (so traversal reaches nested matches) — only on exclude.
    #[must_use]
    pub fn selects_path(&self, path: &Path, is_dir: bool) -> bool {
        let path_str = Self::normalize_path(path);

        if is_dir {
            return !self.path_exclude.is_match(&path_str);
        }

        if let Some(inc) = &self.path_include
            && !inc.is_match(&path_str)
        {
            return false;
        }
        !self.path_exclude.is_match(&path_str)
    }
}

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
mod tests {
    use std::path::Path;

    use super::Selection;

    fn selection(include: &[&str], exclude: &[&str]) -> Selection {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        Selection::new(&inc, &exc).expect("patterns build into a selection")
    }

    fn file(sel: &Selection, rel: &str) -> bool {
        sel.selects_path(Path::new(rel), false)
    }

    fn dir(sel: &Selection, rel: &str) -> bool {
        sel.selects_path(Path::new(rel), true)
    }

    // ---- is_path_level classification ----

    #[test]
    fn classifies_bare_name_as_artifact_level() {
        assert!(!Selection::is_path_level("editor"));
    }

    #[test]
    fn classifies_bare_name_glob_as_artifact_level() {
        assert!(!Selection::is_path_level("code-*"));
    }

    #[test]
    fn classifies_extension_glob_as_artifact_level() {
        assert!(!Selection::is_path_level("*.bak"));
    }

    #[test]
    fn classifies_double_star_pattern_as_path_level() {
        assert!(Selection::is_path_level("**/test/**"));
    }

    #[test]
    fn classifies_leading_slash_as_path_level() {
        assert!(Selection::is_path_level("/editor"));
    }

    #[test]
    fn classifies_slash_pattern_as_path_level() {
        assert!(Selection::is_path_level("editor/*.json"));
    }

    // ---- normalize_path_pattern ----

    #[test]
    fn unanchored_path_pattern_emits_bare_and_double_star_variant() {
        assert_eq!(
            Selection::normalize_path_pattern("editor/x.json"),
            vec!["editor/x.json".to_string(), "**/editor/x.json".to_string()],
        );
    }

    #[test]
    fn anchored_path_pattern_strips_leading_slash_only() {
        assert_eq!(
            Selection::normalize_path_pattern("/editor"),
            vec!["editor".to_string()],
        );
    }

    #[test]
    fn already_double_star_prefixed_pattern_is_left_alone() {
        assert_eq!(
            Selection::normalize_path_pattern("**/*.bak"),
            vec!["**/*.bak".to_string()],
        );
    }

    // ---- directory exclusion on the path side ----

    #[test]
    fn directory_matching_unanchored_path_exclude_is_pruned_at_root() {
        let sel = selection(&[], &["cache/tmp"]);
        assert!(
            !dir(&sel, "cache/tmp"),
            "root-level cache/tmp dir is pruned"
        );
        assert!(
            !dir(&sel, "nested/cache/tmp"),
            "nested cache/tmp dir is pruned"
        );
    }

    #[test]
    fn unmatched_directory_is_traversable() {
        let sel = selection(&[], &["/build"]);
        assert!(dir(&sel, "src"));
    }

    #[test]
    fn non_bak_files_pass_a_bak_exclude() {
        let sel = selection(&[], &["**/*.bak"]);
        assert!(file(&sel, "foo.json"));
        assert!(file(&sel, "sub/foo.json"));
    }
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
