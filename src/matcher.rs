//! Include/exclude pattern classification and matching.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::Result;

pub struct PathMatcher {
    artifact_include: Option<GlobSet>,
    artifact_exclude: GlobSet,
    path_include: Option<GlobSet>,
    path_exclude: GlobSet,
}

impl PathMatcher {
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        let (art_inc, path_inc) = Self::partition_patterns(include);
        let (art_exc, path_exc) = Self::partition_patterns(exclude);

        Ok(Self {
            artifact_include: Self::build_globset_opt(&art_inc)?,
            artifact_exclude: Self::build_globset(&art_exc)?,
            path_include: Self::build_globset_opt(&path_inc)?,
            path_exclude: Self::build_globset(&path_exc)?,
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

    #[must_use]
    pub fn allows_artifact(&self, name: &str) -> bool {
        if let Some(inc) = &self.artifact_include
            && !inc.is_match(name)
        {
            return false;
        }
        !self.artifact_exclude.is_match(name)
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
    pub fn allows_path(&self, path: &Path, is_dir: bool) -> bool {
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
    use super::*;

    #[test]
    fn bare_name_glob_is_artifact_level() {
        assert!(!PathMatcher::is_path_level("code-*"));
        assert!(!PathMatcher::is_path_level("editor"));
        assert!(PathMatcher::is_path_level("**/test/**"));
        assert!(PathMatcher::is_path_level("/editor"));
        assert!(PathMatcher::is_path_level("editor/x.json"));
    }

    #[test]
    fn unanchored_pattern_emits_bare_and_prefixed() {
        assert_eq!(
            PathMatcher::normalize_path_pattern("x/y"),
            vec!["x/y".to_string(), "**/x/y".to_string()]
        );
    }
}
