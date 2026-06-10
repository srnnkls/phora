//! Include/exclude selection: path-level gating plus the sole dotfile opt-in gate.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

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
