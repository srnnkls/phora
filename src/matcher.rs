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
    use std::path::Path;

    use super::PathMatcher;

    fn matcher(include: &[&str], exclude: &[&str]) -> PathMatcher {
        let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
        let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
        PathMatcher::new(&inc, &exc).expect("patterns build into a matcher")
    }

    fn file(matcher: &PathMatcher, rel: &str) -> bool {
        matcher.allows_path(Path::new(rel), false)
    }

    fn dir(matcher: &PathMatcher, rel: &str) -> bool {
        matcher.allows_path(Path::new(rel), true)
    }

    // ---- PAM-006: classification (is_path_level) ----
    // Per spec table: artifact-level = no `/`, no `**`, no leading `/` (matches NAME).
    // Path-level = contains `/` or `**` or leading `/` (matches full relative path).

    #[test]
    fn classifies_bare_name_as_artifact_level() {
        assert!(!PathMatcher::is_path_level("editor"));
    }

    #[test]
    fn classifies_bare_name_glob_as_artifact_level() {
        // marker M020: `code-*` (no `/`, no `**`) targets artifact NAMES.
        assert!(!PathMatcher::is_path_level("code-*"));
    }

    #[test]
    fn classifies_extension_glob_as_artifact_level() {
        // `*.bak` has no `/` and no `**`, so per spec it is an artifact-NAME glob,
        // NOT a file-extension matcher. (Use `**/*.bak` for files.)
        assert!(!PathMatcher::is_path_level("*.bak"));
    }

    #[test]
    fn classifies_double_star_pattern_as_path_level() {
        assert!(PathMatcher::is_path_level("**/test/**"));
    }

    #[test]
    fn classifies_leading_slash_as_path_level() {
        assert!(PathMatcher::is_path_level("/editor"));
    }

    #[test]
    fn classifies_slash_pattern_as_path_level() {
        assert!(PathMatcher::is_path_level("editor/*.json"));
    }

    // ---- PAM-007: normalization + anchoring ----

    #[test]
    fn unanchored_path_pattern_emits_bare_and_double_star_variant() {
        assert_eq!(
            PathMatcher::normalize_path_pattern("editor/x.json"),
            vec!["editor/x.json".to_string(), "**/editor/x.json".to_string()],
        );
    }

    #[test]
    fn anchored_path_pattern_strips_leading_slash_only() {
        assert_eq!(
            PathMatcher::normalize_path_pattern("/editor"),
            vec!["editor".to_string()],
        );
    }

    #[test]
    fn already_double_star_prefixed_pattern_is_left_alone() {
        assert_eq!(
            PathMatcher::normalize_path_pattern("**/*.bak"),
            vec!["**/*.bak".to_string()],
        );
    }

    // ---- PAM-008 / PAM-006: allows_artifact ----

    #[test]
    fn empty_include_allows_all_artifacts() {
        let m = matcher(&[], &[]);
        assert!(m.allows_artifact("editor"));
        assert!(m.allows_artifact("anything-at-all"));
    }

    #[test]
    fn artifact_level_exclude_filters_by_name() {
        let m = matcher(&[], &["code-*"]);
        assert!(!m.allows_artifact("code-review"));
        assert!(m.allows_artifact("editor"));
    }

    #[test]
    fn artifact_include_admits_only_listed_names() {
        let m = matcher(&["editor", "lint"], &[]);
        assert!(m.allows_artifact("editor"));
        assert!(m.allows_artifact("lint"));
        assert!(!m.allows_artifact("vim"));
    }

    #[test]
    fn artifact_exclude_overrides_include() {
        let m = matcher(&["editor", "code-review"], &["code-*"]);
        assert!(m.allows_artifact("editor"));
        assert!(!m.allows_artifact("code-review"));
    }

    // ---- PAM-008: allows_path — M011 root-file matching ----

    #[test]
    fn double_star_bak_exclude_matches_root_level_file() {
        // CRITICAL (M011): `**/*.bak` must reject a file at the artifact ROOT,
        // not only nested ones. globset's `**/` alone would miss the root file.
        let m = matcher(&[], &["**/*.bak"]);
        assert!(!file(&m, "foo.bak"), "root-level foo.bak must be excluded");
    }

    #[test]
    fn double_star_bak_exclude_matches_nested_file() {
        let m = matcher(&[], &["**/*.bak"]);
        assert!(!file(&m, "sub/foo.bak"), "nested sub/foo.bak must be excluded");
    }

    #[test]
    fn double_star_bak_exclude_allows_non_bak_files() {
        let m = matcher(&[], &["**/*.bak"]);
        assert!(file(&m, "foo.json"));
        assert!(file(&m, "sub/foo.json"));
    }

    #[test]
    fn unanchored_path_exclude_matches_at_root_and_any_depth() {
        // M011: unanchored `editor/x.json` matches at the artifact root AND nested.
        let m = matcher(&[], &["editor/x.json"]);
        assert!(!file(&m, "editor/x.json"), "root-level editor/x.json excluded");
        assert!(
            !file(&m, "nested/editor/x.json"),
            "nested editor/x.json excluded via `**/` variant"
        );
        assert!(file(&m, "editor/y.json"), "non-matching file allowed");
    }

    // ---- PAM-008: allows_path — anchoring ----

    #[test]
    fn anchored_exclude_matches_root_only() {
        let m = matcher(&[], &["/secret.txt"]);
        assert!(!file(&m, "secret.txt"), "root secret.txt excluded");
        assert!(
            file(&m, "sub/secret.txt"),
            "anchored pattern must NOT reach nested secret.txt"
        );
    }

    // ---- PAM-008: allows_path — include semantics + precedence ----

    #[test]
    fn path_include_admits_only_matching_files() {
        let m = matcher(&["**/*.json"], &[]);
        assert!(file(&m, "config.json"), "root json passes include");
        assert!(file(&m, "sub/config.json"), "nested json passes include");
        assert!(!file(&m, "config.yaml"), "non-json rejected by include");
    }

    #[test]
    fn path_include_never_prunes_directories() {
        // Directories must not be pruned by include so traversal can reach nested
        // matches; only exclude prunes a directory.
        let m = matcher(&["**/*.json"], &[]);
        assert!(dir(&m, "sub"), "include must not prune directories");
    }

    #[test]
    fn directory_matching_anchored_exclude_is_pruned() {
        // Anchored path-level exclude `/build` prunes the root dir but not nested ones.
        let m = matcher(&[], &["/build"]);
        assert!(!dir(&m, "build"), "anchored exclude prunes root build dir");
        assert!(dir(&m, "src"), "unmatched directory is traversable");
        assert!(dir(&m, "sub/build"), "anchored exclude does not reach nested build");
    }

    #[test]
    fn directory_matching_unanchored_path_exclude_is_pruned_at_root() {
        // M011 on the directory side: unanchored `cache/tmp` must prune the dir at
        // the artifact root, not only when nested.
        let m = matcher(&[], &["cache/tmp"]);
        assert!(!dir(&m, "cache/tmp"), "root-level cache/tmp dir is pruned");
        assert!(!dir(&m, "nested/cache/tmp"), "nested cache/tmp dir is pruned");
    }

    #[test]
    fn path_exclude_overrides_path_include() {
        // A file matching BOTH include and exclude is rejected (exclude precedence).
        let m = matcher(&["**/*.json"], &["**/secret.json"]);
        assert!(file(&m, "ok.json"));
        assert!(!file(&m, "secret.json"), "exclude wins over include at root");
        assert!(!file(&m, "sub/secret.json"), "exclude wins over include nested");
    }
}
