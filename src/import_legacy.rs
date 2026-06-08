//! One-shot migration of legacy `.worktreeinclude` manifests into phora
//! `[worktree].includes` config.

use std::path::{Component, Path};

use crate::config::{Include, IncludeMode};

fn mode_name(mode: IncludeMode) -> &'static str {
    match mode {
        IncludeMode::Symlink => "symlink",
        IncludeMode::Copy => "copy",
        IncludeMode::SubmoduleWalk => "submodule-walk",
    }
}

/// A legacy manifest line that could not map to an explicit phora include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLine {
    pub line: String,
    pub reason: String,
}

/// Outcome of converting a legacy `.worktreeinclude` manifest: the includes that
/// mapped to explicit literal paths, plus the lines that could not.
#[derive(Debug, Clone, Default)]
pub struct LegacyImport {
    pub includes: Vec<Include>,
    pub skipped: Vec<SkippedLine>,
}

/// Converts a legacy `.worktreeinclude` manifest body into phora includes.
///
/// Lines that cannot become a safe relative literal include (globs, negations,
/// unsafe paths, or a `submodule-walk` without `symlink`) land in
/// [`LegacyImport::skipped`] rather than being imported.
#[must_use]
pub fn convert_legacy(contents: &str) -> LegacyImport {
    let mut import = LegacyImport::default();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match convert_line(line) {
            Ok(include) => import.includes.push(include),
            Err(reason) => import.skipped.push(SkippedLine {
                line: line.to_owned(),
                reason,
            }),
        }
    }
    import
}

fn convert_line(line: &str) -> Result<Include, String> {
    let mut fields = line.split_whitespace();
    let path = fields
        .next()
        .map(unquote)
        .ok_or_else(|| "empty line".to_owned())?;
    let attrs: Vec<&str> = fields.collect();

    if path.starts_with('!') {
        return Err(format!("`{path}` is an unsupported negation (`!`)"));
    }
    if is_glob(path) {
        return Err(format!("`{path}` is a glob, not a literal path"));
    }
    if !is_safe_relative(path) {
        return Err(format!(
            "`{path}` is not a safe relative path (absolute, `..`, `.`, or empty)"
        ));
    }

    let mode = resolve_mode(&attrs)?;
    Ok(Include {
        path: path.into(),
        mode,
    })
}

fn resolve_mode(attrs: &[&str]) -> Result<IncludeMode, String> {
    let symlink = attrs.contains(&"symlink");
    let walk = attrs.contains(&"submodule-walk");

    if walk && !symlink {
        return Err(
            "`submodule-walk` is valid only together with `symlink`, not with `copy` or alone"
                .to_owned(),
        );
    }
    if walk {
        return Ok(IncludeMode::SubmoduleWalk);
    }
    if symlink {
        return Ok(IncludeMode::Symlink);
    }
    Ok(IncludeMode::Copy)
}

fn unquote(field: &str) -> &str {
    field
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(field)
}

fn is_glob(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// Renders includes as a phora `[worktree].includes` TOML document.
#[must_use]
pub fn render_worktree_toml(includes: &[Include]) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("version = 1\n");
    for include in includes {
        let path = toml_edit::value(include.path.to_string_lossy().as_ref());
        let _ = write!(
            out,
            "\n[[worktree.includes]]\npath ={path}\nmode = \"{}\"\n",
            mode_name(include.mode),
        );
    }
    out
}

/// Mirrors the relative-path rule [`Config::parse`](crate::config::Config::parse)
/// enforces on worktree include paths: non-empty, relative, no `.`/`..` segment.
fn is_safe_relative(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = Path::new(path);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    !path
        .split(['/', '\\'])
        .any(|segment| segment == "." || segment == "..")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{Config, IncludeMode};

    fn paths(import: &LegacyImport) -> Vec<PathBuf> {
        import.includes.iter().map(|i| i.path.clone()).collect()
    }

    fn mode_name(mode: IncludeMode) -> &'static str {
        match mode {
            IncludeMode::Symlink => "symlink",
            IncludeMode::Copy => "copy",
            IncludeMode::SubmoduleWalk => "submodule-walk",
        }
    }

    fn render_worktree_toml(import: &LegacyImport) -> String {
        use std::fmt::Write as _;
        let mut out = String::from("version = 1\n");
        for include in &import.includes {
            let _ = write!(
                out,
                "\n[[worktree.includes]]\npath = \"{}\"\nmode = \"{}\"\n",
                include.path.display(),
                mode_name(include.mode),
            );
        }
        out
    }

    #[test]
    fn symlink_attr_maps_to_symlink_mode() {
        let import = convert_legacy("\".codex\" symlink\n");

        assert_eq!(
            paths(&import),
            vec![PathBuf::from(".codex")],
            "a literal symlink line must produce exactly one include for that path"
        );
        assert_eq!(
            import.includes[0].mode,
            IncludeMode::Symlink,
            "the `symlink` attribute must map to IncludeMode::Symlink"
        );
        assert!(
            import.skipped.is_empty(),
            "a literal symlink line must not be skipped, got {:?}",
            import.skipped
        );
    }

    #[test]
    fn default_and_copy_attr_map_to_copy_mode() {
        let import = convert_legacy("\"mise.local.toml\"\n\"fnox.local.toml\" copy\n");

        assert_eq!(
            paths(&import),
            vec![
                PathBuf::from("mise.local.toml"),
                PathBuf::from("fnox.local.toml"),
            ],
            "both literal lines must be imported in order"
        );
        assert_eq!(
            import.includes[0].mode,
            IncludeMode::Copy,
            "a line with no mode attribute must use the legacy copy default, emitted explicitly \
             (phora's own default is Symlink, so an omitted mode would silently flip semantics)"
        );
        assert_eq!(
            import.includes[1].mode,
            IncludeMode::Copy,
            "an explicit `copy` attribute must map to IncludeMode::Copy"
        );
    }

    #[test]
    fn submodule_walk_maps_to_submodule_walk_mode() {
        let import = convert_legacy("\"resources/effect\" symlink submodule-walk\n");

        assert_eq!(
            paths(&import),
            vec![PathBuf::from("resources/effect")],
            "the literal submodule-walk line must produce one include"
        );
        assert_eq!(
            import.includes[0].mode,
            IncludeMode::SubmoduleWalk,
            "`symlink submodule-walk` on a literal path must map to IncludeMode::SubmoduleWalk"
        );
        assert_eq!(
            import.includes.len(),
            1,
            "a valid `symlink submodule-walk` line must produce exactly one include"
        );
        assert!(
            import.skipped.is_empty(),
            "a valid `symlink submodule-walk` line must not be skipped, got {:?}",
            import.skipped
        );
    }

    #[test]
    fn glob_line_is_skipped_and_reported() {
        let import = convert_legacy("\"*.bak\" copy\n");

        assert!(
            import.includes.is_empty(),
            "a glob line cannot become an explicit literal include and must not be imported, \
             got {:?}",
            paths(&import)
        );
        assert_eq!(
            import.skipped.len(),
            1,
            "the glob line must be reported as skipped, not silently dropped"
        );
        let skipped = &import.skipped[0];
        assert!(
            skipped.line.contains("*.bak"),
            "the skipped report must echo the offending line, got {:?}",
            skipped.line
        );
        let reason = skipped.reason.to_lowercase();
        assert!(
            reason.contains("glob") || reason.contains("literal"),
            "the skip reason must explain the line is a glob / non-literal path, got {:?}",
            skipped.reason
        );
    }

    #[test]
    fn negation_line_is_skipped_and_reported() {
        let import = convert_legacy("!keep.me\n");

        assert!(
            import.includes.is_empty(),
            "a negation line cannot map to an explicit include and must not be imported, got {:?}",
            paths(&import)
        );
        assert_eq!(
            import.skipped.len(),
            1,
            "the negation line must be reported as skipped"
        );
        assert!(
            import.skipped[0].line.contains("!keep.me"),
            "the skipped report must echo the offending negation line, got {:?}",
            import.skipped[0].line
        );
        let reason = import.skipped[0].reason.to_lowercase();
        assert!(
            !reason.is_empty(),
            "the negation skip must carry a non-empty reason"
        );
        assert!(
            reason.contains("negat") || reason.contains('!') || reason.contains("unsupported"),
            "the skip reason must explain the line is an unsupported negation (`!`), got {:?}",
            import.skipped[0].reason
        );
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let import = convert_legacy("# a comment\n\n   \n# another\n");

        assert!(
            import.includes.is_empty(),
            "comments and blank lines must produce no includes, got {:?}",
            paths(&import)
        );
        assert!(
            import.skipped.is_empty(),
            "comments and blank lines must be ignored silently, not reported as skipped, got {:?}",
            import.skipped
        );
    }

    #[test]
    fn generated_includes_roundtrip_through_config() {
        let manifest = "\
\".codex\" symlink
\"mise.local.toml\"
\"fnox.local.toml\" copy
\"resources/effect\" symlink submodule-walk
\"*.bak\" copy
!keep.me
# trailing comment
";
        let import = convert_legacy(manifest);

        let toml = render_worktree_toml(&import);
        let cfg = Config::parse(&toml).unwrap_or_else(|e| {
            panic!("generated [worktree] TOML must be valid phora config: {e}\n{toml}")
        });
        let parsed = cfg
            .worktree
            .expect("the rendered config must carry a [worktree] section")
            .includes;

        let got: Vec<(PathBuf, IncludeMode)> =
            parsed.iter().map(|i| (i.path.clone(), i.mode)).collect();
        assert_eq!(
            got,
            vec![
                (PathBuf::from(".codex"), IncludeMode::Symlink),
                (PathBuf::from("mise.local.toml"), IncludeMode::Copy),
                (PathBuf::from("fnox.local.toml"), IncludeMode::Copy),
                (
                    PathBuf::from("resources/effect"),
                    IncludeMode::SubmoduleWalk
                ),
            ],
            "the converted includes must round-trip through Config::parse with the same paths \
             and modes; the glob and negation lines must be absent"
        );
    }

    #[test]
    fn symlink_beats_copy_regardless_of_order() {
        let copy_first = convert_legacy("\"foo\" copy symlink\n");
        assert_eq!(
            paths(&copy_first),
            vec![PathBuf::from("foo")],
            "`foo copy symlink` must import the literal path once"
        );
        assert_eq!(
            copy_first.includes[0].mode,
            IncludeMode::Symlink,
            "when both `copy` and `symlink` are present, symlink must win (copy then symlink)"
        );
        assert!(
            copy_first.skipped.is_empty(),
            "a line carrying both attrs is valid, not skipped, got {:?}",
            copy_first.skipped
        );

        let symlink_first = convert_legacy("\"foo\" symlink copy\n");
        assert_eq!(
            symlink_first.includes[0].mode,
            IncludeMode::Symlink,
            "symlink must win regardless of attribute order (symlink then copy)"
        );
        assert!(symlink_first.skipped.is_empty());
    }

    #[test]
    fn question_and_bracket_globs_are_skipped() {
        for line in ["\"file?.txt\"\n", "\"f[0-9].txt\"\n"] {
            let import = convert_legacy(line);
            assert!(
                import.includes.is_empty(),
                "the glob line {line:?} must not be imported, got {:?}",
                paths(&import)
            );
            assert_eq!(
                import.skipped.len(),
                1,
                "the glob line {line:?} must be reported as skipped, not silently dropped"
            );
            let reason = import.skipped[0].reason.to_lowercase();
            assert!(
                reason.contains("glob") || reason.contains("literal"),
                "the skip reason for {line:?} must explain it is a glob / non-literal path, \
                 got {:?}",
                import.skipped[0].reason
            );
        }
    }

    #[test]
    fn submodule_walk_without_symlink_is_skipped() {
        let import = convert_legacy("\"foo\" submodule-walk\n");
        assert!(
            import.includes.is_empty(),
            "`submodule-walk` without `symlink` is invalid and must not be imported, got {:?}",
            paths(&import)
        );
        assert_eq!(
            import.skipped.len(),
            1,
            "`submodule-walk` without `symlink` must be reported as skipped, not aborted or \
             imported"
        );
        assert!(
            import.skipped[0].line.contains("foo"),
            "the skipped report must echo the offending line, got {:?}",
            import.skipped[0].line
        );
        let reason = import.skipped[0].reason.to_lowercase();
        assert!(
            reason.contains("submodule-walk")
                || reason.contains("submodule")
                || reason.contains("symlink"),
            "the skip reason must mention submodule-walk/symlink, got {:?}",
            import.skipped[0].reason
        );
    }

    #[test]
    fn copy_submodule_walk_is_skipped() {
        let import = convert_legacy("\"foo\" copy submodule-walk\n");
        assert!(
            import.includes.is_empty(),
            "`copy submodule-walk` is contradictory and must not be imported, got {:?}",
            paths(&import)
        );
        assert_eq!(
            import.skipped.len(),
            1,
            "the contradictory `copy submodule-walk` line must be reported as skipped"
        );
        let reason = import.skipped[0].reason.to_lowercase();
        assert!(
            reason.contains("submodule-walk")
                || reason.contains("submodule")
                || reason.contains("symlink"),
            "the skip reason must mention submodule-walk/symlink, got {:?}",
            import.skipped[0].reason
        );
    }

    #[test]
    fn unsafe_paths_are_skipped_and_reported() {
        for line in ["\"/etc/hosts\"\n", "\"../secret\"\n", "\".\"\n"] {
            let import = convert_legacy(line);
            assert!(
                import.includes.is_empty(),
                "the unsafe path {line:?} must never be imported (Config::parse would reject it), \
                 got {:?}",
                paths(&import)
            );
            assert_eq!(
                import.skipped.len(),
                1,
                "the unsafe path {line:?} must be reported as skipped"
            );
            assert!(
                !import.skipped[0].reason.is_empty(),
                "the unsafe path {line:?} skip must carry a non-empty reason"
            );
        }
    }

    #[test]
    fn unsafe_paths_skipped_so_render_reparses() {
        let import = convert_legacy("\"/etc/hosts\"\n\"../secret\"\n\".\"\n\"keep.me\" copy\n");
        let toml = render_worktree_toml(&import);
        Config::parse(&toml).unwrap_or_else(|e| {
            panic!("dropping unsafe legacy paths must guarantee the rendered [worktree] re-parses: {e}\n{toml}")
        });
    }

    #[test]
    fn interior_dot_paths_are_skipped() {
        let import = convert_legacy("\"a/./b\" symlink\n\"foo/.\" copy\n");

        assert!(
            import.includes.is_empty(),
            "interior `.` segments (`a/./b`, `foo/.`) survive Path::components() normalization \
             but Config::parse rejects them by string-splitting on `/`; they must be skipped, \
             not imported, or the emitted config would fail to re-parse, got {:?}",
            paths(&import)
        );
        assert_eq!(
            import.skipped.len(),
            2,
            "both interior-`.` lines must be reported as skipped, not silently dropped or imported"
        );
        for skipped in &import.skipped {
            assert!(
                !skipped.reason.is_empty(),
                "each skipped interior-`.` line must carry a non-empty reason, got {skipped:?}"
            );
        }
    }

    #[test]
    fn rendered_toml_escapes_special_chars_and_reparses() {
        let includes = vec![
            Include {
                path: PathBuf::from("fo\"o"),
                mode: IncludeMode::Copy,
            },
            Include {
                path: PathBuf::from("ba\\r"),
                mode: IncludeMode::Symlink,
            },
        ];

        let toml = super::render_worktree_toml(&includes);
        let cfg = Config::parse(&toml).unwrap_or_else(|e| {
            panic!(
                "a safe path containing `\"` or `\\` must be escaped so the rendered \
                 [worktree] TOML re-parses: {e}\n{toml}"
            )
        });
        let parsed = cfg
            .worktree
            .expect("the rendered config must carry a [worktree] section")
            .includes;

        let got: Vec<PathBuf> = parsed.iter().map(|i| i.path.clone()).collect();
        assert_eq!(
            got,
            vec![PathBuf::from("fo\"o"), PathBuf::from("ba\\r")],
            "the special chars must be escaped, not corrupted: the re-parsed paths must equal \
             the inputs `fo\"o` and `ba\\r`"
        );
    }

    #[test]
    fn unknown_attr_is_ignored_path_still_imported() {
        let import = convert_legacy("\"foo\" frobnicate\n");
        assert_eq!(
            paths(&import),
            vec![PathBuf::from("foo")],
            "an unknown trailing attribute must be ignored, leaving the literal path imported"
        );
        assert_eq!(
            import.includes[0].mode,
            IncludeMode::Copy,
            "with no recognized mode attr, the legacy copy default must apply"
        );
        assert!(
            import.skipped.is_empty(),
            "an unknown attribute must not cause the line to be skipped, got {:?}",
            import.skipped
        );
    }

    #[test]
    fn tab_and_surrounding_whitespace_are_handled() {
        let tabbed = convert_legacy("\"foo\"\tsymlink\n");
        assert_eq!(
            paths(&tabbed),
            vec![PathBuf::from("foo")],
            "a TAB between path and attribute must parse as path `foo`"
        );
        assert_eq!(
            tabbed.includes[0].mode,
            IncludeMode::Symlink,
            "a TAB-separated `symlink` attribute must map to IncludeMode::Symlink"
        );
        assert!(tabbed.skipped.is_empty());

        let spaced = convert_legacy("   \"foo\" symlink   \n");
        assert_eq!(
            paths(&spaced),
            vec![PathBuf::from("foo")],
            "leading/trailing whitespace must be tolerated"
        );
        assert_eq!(spaced.includes[0].mode, IncludeMode::Symlink);
        assert!(spaced.skipped.is_empty());
    }
}
