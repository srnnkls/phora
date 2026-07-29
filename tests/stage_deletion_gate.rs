use std::fs;
use std::path::PathBuf;

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(rel: &str) -> String {
    fs::read_to_string(src_dir().join(rel)).unwrap_or_default()
}

fn blank(c: char) -> char {
    if c == '\n' { '\n' } else { ' ' }
}

fn is_char_literal(chars: &[char], i: usize) -> bool {
    match chars.get(i + 1) {
        Some('\\') => true,
        Some(&c) if c != '\'' => chars.get(i + 2) == Some(&'\''),
        _ => false,
    }
}

fn is_raw_string_open(chars: &[char], i: usize) -> bool {
    let prev_ok = i == 0 || (!chars[i - 1].is_alphanumeric() && chars[i - 1] != '_');
    if !prev_ok {
        return false;
    }
    let mut j = i + 1;
    while chars.get(j) == Some(&'#') {
        j += 1;
    }
    chars.get(j) == Some(&'"')
}

fn strip(src: &str) -> String {
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
            'r' if is_raw_string_open(&chars, i) => {
                let mut hashes = 0;
                let mut j = i + 1;
                while chars.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                out.push('"');
                j += 1;
                while j < chars.len() {
                    if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
                        break;
                    }
                    out.push(blank(chars[j]));
                    j += 1;
                }
                out.push('"');
                i = (j + 1 + hashes).min(chars.len());
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
                    out.push(' ');
                    i += 1;
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

fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn balanced_body(after: &str) -> Option<String> {
    let open = after.find('{')?;
    let end = matching_brace(after.as_bytes(), open)?;
    Some(after[open..=end].to_string())
}

fn strip_cfg_test(stripped: &str) -> String {
    let mut out = stripped.to_owned();
    while let Some(start) = out.find("#[cfg(test)]") {
        let after = start + "#[cfg(test)]".len();
        let brace = out[after..].find('{').map(|i| after + i);
        let semi = out[after..].find(';').map(|i| after + i);
        let end = match (brace, semi) {
            (Some(b), s) if s.is_none_or(|s| b < s) => matching_brace(out.as_bytes(), b),
            (_, Some(s)) => Some(s),
            _ => None,
        };
        match end {
            Some(end) => out.replace_range(start..=end, " "),
            None => out.truncate(start),
        }
    }
    out
}

fn scan(src: &str) -> String {
    strip_cfg_test(&strip(src))
}

fn references_token(body: &str, token: &str) -> bool {
    let bytes = body.as_bytes();
    body.match_indices(token).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after = i + token.len();
        let after_ok =
            after >= bytes.len() || (!bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_');
        before_ok && after_ok
    })
}

fn word_at(bytes: &[u8], at: usize, word: &[u8]) -> bool {
    bytes.get(at..at + word.len()) == Some(word)
        && (at == 0 || (!bytes[at - 1].is_ascii_alphanumeric() && bytes[at - 1] != b'_'))
        && bytes
            .get(at + word.len())
            .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
}

fn top_level_public_use_references(stripped: &str, token: &str) -> bool {
    let bytes = stripped.as_bytes();
    let mut brace_depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'p' if brace_depth == 0 && word_at(bytes, i, b"pub") => {
                let mut use_start = i + b"pub".len();
                while bytes.get(use_start).is_some_and(u8::is_ascii_whitespace) {
                    use_start += 1;
                }
                if word_at(bytes, use_start, b"use") {
                    let mut end = use_start + b"use".len();
                    let mut use_tree_depth = 0usize;
                    while end < bytes.len() {
                        match bytes[end] {
                            b'{' => use_tree_depth += 1,
                            b'}' => use_tree_depth = use_tree_depth.saturating_sub(1),
                            b';' if use_tree_depth == 0 => {
                                if references_token(&stripped[i..=end], token) {
                                    return true;
                                }
                                break;
                            }
                            _ => {}
                        }
                        end += 1;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn top_level_field_type<'a>(struct_body: &'a str, field: &str) -> Option<&'a str> {
    let bytes = struct_body.as_bytes();
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut angle_depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'<' if brace_depth == 1 && bracket_depth == 0 && paren_depth == 0 => angle_depth += 1,
            b'>' if brace_depth == 1 && bracket_depth == 0 && paren_depth == 0 => {
                angle_depth = angle_depth.saturating_sub(1);
            }
            _ if brace_depth == 1
                && bracket_depth == 0
                && paren_depth == 0
                && angle_depth == 0
                && word_at(bytes, i, field.as_bytes()) =>
            {
                let mut colon = i + field.len();
                while bytes.get(colon).is_some_and(u8::is_ascii_whitespace) {
                    colon += 1;
                }
                if bytes.get(colon) != Some(&b':') || bytes.get(colon + 1) == Some(&b':') {
                    i += field.len();
                    continue;
                }

                let type_start = colon + 1;
                let mut end = type_start;
                let mut type_brace_depth = 0usize;
                let mut type_bracket_depth = 0usize;
                let mut type_paren_depth = 0usize;
                let mut angle_depth = 0usize;
                while end < bytes.len() {
                    match bytes[end] {
                        b'{' => type_brace_depth += 1,
                        b'}' if type_brace_depth > 0 => type_brace_depth -= 1,
                        b'}' if type_bracket_depth == 0
                            && type_paren_depth == 0
                            && angle_depth == 0 =>
                        {
                            break;
                        }
                        b'[' => type_bracket_depth += 1,
                        b']' => type_bracket_depth = type_bracket_depth.saturating_sub(1),
                        b'(' => type_paren_depth += 1,
                        b')' => type_paren_depth = type_paren_depth.saturating_sub(1),
                        b'<' => angle_depth += 1,
                        b'>' => angle_depth = angle_depth.saturating_sub(1),
                        b',' if type_brace_depth == 0
                            && type_bracket_depth == 0
                            && type_paren_depth == 0
                            && angle_depth == 0 =>
                        {
                            break;
                        }
                        _ => {}
                    }
                    end += 1;
                }
                return Some(struct_body[type_start..end].trim());
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn keyword_names(stripped: &str, keyword: &str, name: &str) -> bool {
    let bytes = stripped.as_bytes();
    stripped.match_indices(keyword).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let rest = stripped[i + keyword.len()..].trim_start();
        before_ok
            && rest.strip_prefix(name).is_some_and(|after| {
                after
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
            })
    })
}

fn defines_fn(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "fn", name)
}

fn defines_type(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "struct", name) || keyword_names(stripped, "enum", name)
}

fn trait_method_names(stripped: &str, trait_name: &str) -> Option<Vec<String>> {
    let start = stripped.find(&format!("pub trait {trait_name}"))?;
    let body = balanced_body(&stripped[start..])?;
    let bytes = body.as_bytes();
    let mut names = Vec::new();
    for (i, _) in body.match_indices("fn") {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        if !before_ok {
            continue;
        }
        let name: String = body[i + 2..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            names.push(name);
        }
    }
    Some(names)
}

fn calls_method(stripped: &str, method: &str) -> bool {
    stripped.match_indices(method).any(|(i, _)| {
        let after = i + method.len();
        let boundary_after = stripped
            .as_bytes()
            .get(after)
            .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_');
        if !boundary_after {
            return false;
        }
        let called = stripped[after..].trim_start().starts_with('(');
        if !called {
            return false;
        }
        let before = stripped[..i].trim_end().as_bytes();
        let dotted = before.last() == Some(&b'.');
        let pathed = before.len() >= 2 && &before[before.len() - 2..] == b"::";
        dotted || pathed
    })
}

fn collect_rs_files(dir: &PathBuf, prefix: &str, out: &mut Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            collect_rs_files(&path, &rel, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push((rel, fs::read_to_string(&path).unwrap_or_default()));
        }
    }
}

fn prod_src_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    collect_rs_files(&src_dir(), "", &mut out);
    out.retain(|(rel, _)| !(rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs")));
    out
}

fn source_files() -> Vec<(String, String)> {
    prod_src_files()
        .into_iter()
        .filter(|(rel, _)| rel.starts_with("source/"))
        .collect()
}

const OLD_EXPORT_TYPES: &[&str] = &["ExportRequest", "ExportLeaf", "ExportResult"];

const FORBIDDEN_SOURCE_IMPORTS: &[&str] = &["crate::sync", "crate::store", "crate::projection"];

const PORTED_STAGING_TESTS: &[&str] = &[
    "export_aborts_a_runaway_template_via_fuel_instead_of_hanging",
    "export_excludes_bak_files_by_path_matcher",
    "export_keeps_eszett_and_ss_deployed_names_distinct_documented_limitation",
    "export_materializes_files_with_exact_content",
    "export_materializes_symlink_when_allowed",
    "export_materializes_symlink_with_in_root_dotdot_target",
    "export_preserves_executable_bit_by_default",
    "export_prunes_a_nested_dot_git_dest_at_write",
    "export_prunes_a_top_level_dot_git_dest_at_write",
    "export_rejects_deployed_names_colliding_only_by_ascii_case",
    "export_rejects_deployed_names_colliding_only_by_cyrillic_case",
    "export_rejects_deployed_names_colliding_only_by_latin_accented_case",
    "export_rejects_directory_colliding_with_rendered_deployed_name",
    "export_rejects_symlink_colliding_with_rendered_deployed_name",
    "export_rejects_symlink_target_escaping_root_via_dotdot",
    "export_rejects_symlink_when_policy_disallows",
    "export_rejects_symlink_with_absolute_target",
    "export_result_lists_exported_files",
    "export_sets_mtime_to_commit_time",
    "export_vars_digest_changes_when_a_var_value_changes",
    "export_vars_digest_hashes_full_vars_not_only_consumed_keys",
    "export_vars_digest_is_none_when_no_template_rendered",
    "export_vars_digest_is_some_when_a_template_rendered",
    "export_writes_a_dot_git_dest_when_policy_opts_in",
    "export_writes_a_dot_gitignore_basename_dest_without_opt_in",
    "mapped_export_errors_when_key_is_a_directory",
    "mapped_export_errors_when_key_is_missing",
    "mapped_export_errors_when_two_keys_share_a_dest",
    "mapped_export_flattens_nested_key_to_dest",
    "mapped_export_preserves_executable_bit",
    "mapped_export_renames_top_level_blob",
    "mapped_export_stages_all_entries_of_a_multi_key_map",
];

/// No cfg(test) strip here: `#[test]` fns are exactly what this scans.
fn test_fn_names(src: &str) -> Vec<String> {
    let s = strip(src);
    let bytes = s.as_bytes();
    let mut names = Vec::new();
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
                names.push(name);
            }
        }
        from = start;
    }
    names
}

fn source_backend_declares_export_artifact() -> bool {
    trait_method_names(&scan(&read_src("source/mod.rs")), "SourceBackend")
        .is_some_and(|methods| methods.contains(&"export_artifact".to_owned()))
}

// ─── end-state: the old export port is gone ──────────────────────────────────

#[test]
fn source_backend_is_removed_after_the_staging_rewire() {
    let survivors: Vec<String> = source_files()
        .into_iter()
        .filter(|(_, content)| references_token(&scan(content), "SourceBackend"))
        .map(|(rel, _)| format!("src/{rel}"))
        .collect();
    assert!(
        survivors.is_empty(),
        "T030 removes SourceBackend after staging rewires onto sync::stage and source reads \
         through SourceStore; live source files still naming the legacy capability: \
         {survivors:?}"
    );
}

#[test]
fn old_export_request_types_are_deleted_from_source() {
    let scanned: Vec<(String, String)> = source_files()
        .into_iter()
        .map(|(rel, content)| (rel, scan(&content)))
        .collect();
    let mut survivors: Vec<String> = Vec::new();
    for ty in OLD_EXPORT_TYPES {
        for (rel, s) in &scanned {
            if defines_type(s, ty) {
                survivors.push(format!("{ty} in src/{rel}"));
            }
        }
    }
    assert!(
        survivors.is_empty(),
        "T016 deletes the old export request/result value types — ExportRequest/ExportLeaf/\
         ExportResult carried the staging plan across the port that no longer exists. The \
         relocated staging speaks StageRequest/StagedArtifact/StagedFile (sync/stage.rs). \
         Still defined under src/source/: {survivors:?}"
    );
}

#[test]
fn source_git_backend_has_no_export_artifact_impl() {
    assert!(
        !defines_fn(&scan(&read_src("source/git.rs")), "export_artifact"),
        "T016 deletes the GitBackend export_artifact impl — this is the tripwire's inverse: \
         the old staging path stayed alive through T015 (delegating to the relocated \
         machinery) and dies here after the differential proved old == new"
    );
}

#[test]
fn source_no_longer_imports_sync_store_or_projection() {
    let mut offenders: Vec<String> = Vec::new();
    for (rel, content) in source_files() {
        let scanned = scan(&content);
        for forbidden in FORBIDDEN_SOURCE_IMPORTS {
            if scanned.contains(forbidden) {
                offenders.push(format!("`{forbidden}` in src/{rel}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "INV-2 staging clause goes LIVE at T016: no production file under src/source/ may reach \
         into sync (staging/target-path), store (manifest/registry), or projection. The \
         transitional `pub(crate) use crate::sync::stage::{{ExportWalk, Renderer}}` re-export, \
         the `crate::store::ManifestFile` import, and the seam-trait impls all die with the old \
         path — source's digest helpers (hash_framed_entry, vars_digest) STAY source-owned and \
         are imported FROM source by stage.rs (sync -> source is the legal direction). \
         Offending imports: {offenders:?}"
    );
}

// ─── end-state: both consumers drive the relocated staging ───────────────────

#[test]
fn rebuild_one_drives_the_relocated_stage_artifact() {
    let scanned = scan(&read_src("sync/rebuild.rs"));
    assert!(
        references_token(&scanned, "stage_artifact") || references_token(&scanned, "StageRequest"),
        "T016 rewires rebuild_one (the SECOND staging consumer — it builds a plan from \
         RegistryRecords with NO projection values) onto the relocated stage_artifact before \
         the old path dies; rebuild parity is pinned by sync/tests.rs (rebuild_reconstructs_*, \
         rebuild_reports_modified_*) and tests/compat/cli/rebuild_registry.golden"
    );
    for old in ["export_artifact", "ExportRequest", "ExportLeaf"] {
        assert!(
            !references_token(&scanned, old),
            "src/sync/rebuild.rs must not reference the deleted export port `{old}` after the \
             rewire — it stages through stage_artifact and converts StagedFile -> ManifestFile \
             for the ArtifactRecord"
        );
    }
}

#[test]
fn deploy_one_drives_the_relocated_stage_artifact() {
    let scanned = scan(&read_src("sync/target.rs"));
    assert!(
        references_token(&scanned, "stage_artifact") || references_token(&scanned, "StageRequest"),
        "T016 rewires deploy_one onto stage_artifact through the StageBridge that T014 already \
         threaded; the T003 goldens (via the rewired compat_staging driver) pin staged \
         bytes/modes/mtimes/digests unchanged"
    );
    for old in ["export_artifact", "ExportRequest", "ExportLeaf"] {
        assert!(
            !references_token(&scanned, old),
            "src/sync/target.rs must not reference the deleted export port `{old}` after the \
             rewire"
        );
    }
}

#[test]
fn no_production_code_outside_source_calls_export_artifact() {
    let callers: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with("source/"))
        .filter(|(_, content)| calls_method(&scan(content), "export_artifact"))
        .map(|(rel, _)| format!("src/{rel}"))
        .collect();
    assert!(
        callers.is_empty(),
        "T016 leaves ZERO production callers of export_artifact outside src/source/ (the \
         source-internal delegations in import.rs/router.rs die with the port). The migration \
         gate derives the removed set from the trait surface and scans callers — deletion must \
         satisfy it as-is. Remaining callers: {callers:?}"
    );
}

// ─── the deletion must not over-reach ────────────────────────────────────────

#[test]
fn manifest_file_remains_registry_state_owned_after_store_facade_deletion() {
    let state_file = scan(&read_src("sync/state/file.rs"));
    let lib_rs = strip(&read_src("lib.rs"));
    let definition_sites: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(_, content)| defines_type(&scan(content), "ManifestFile"))
        .map(|(rel, _)| rel)
        .collect();

    assert!(
        defines_type(&state_file, "ManifestFile"),
        "guard: ManifestFile remains a REGISTRY/state value type and must be defined in \
         src/sync/state/file.rs"
    );
    assert_eq!(
        definition_sites,
        ["sync/state/file.rs"],
        "ManifestFile must have exactly one production definition in its registry/state owner; \
         it must not become source- or staging-owned: {definition_sites:?}"
    );
    assert!(
        !src_dir().join("store.rs").exists(),
        "T030 deletes the obsolete src/store.rs compatibility facade"
    );
    assert!(
        !keyword_names(&lib_rs, "mod", "store"),
        "T030 removes the obsolete store module declaration from src/lib.rs"
    );

    let registry_record = state_file
        .find("pub struct ArtifactRecord")
        .and_then(|start| balanced_body(&state_file[start..]))
        .unwrap_or_default();
    let files_type = top_level_field_type(&registry_record, "files").unwrap_or_default();
    assert!(
        references_token(files_type, "ManifestFile"),
        "ManifestFile remains the registry/state value stored specifically by \
         ArtifactRecord.files; found type: {files_type:?}"
    );
}

#[test]
fn ported_staging_behavior_tests_move_with_their_machinery() {
    if source_backend_declares_export_artifact() {
        let names = test_fn_names(&read_src("source/mod.rs"));
        let missing: Vec<&&str> = PORTED_STAGING_TESTS
            .iter()
            .filter(|name| !names.contains(&(**name).to_owned()))
            .collect();
        assert!(
            missing.is_empty(),
            "pre-deletion state: every audited port-candidate must still exist as a `#[test]` \
             fn in src/source/mod.rs — a missing name means the audited port list drifted from \
             the tree and must be re-audited: {missing:?}"
        );
        return;
    }
    let names = test_fn_names(&read_src("sync/stage.rs"));
    let missing: Vec<&&str> = PORTED_STAGING_TESTS
        .iter()
        .filter(|name| !names.contains(&(**name).to_owned()))
        .collect();
    assert!(
        missing.is_empty(),
        "once the old export path is deleted, the {} audited staging-BEHAVIOR tests must exist \
         as `#[test]` fns in src/sync/stage.rs UNDER THE SAME NAMES (bodies adapted to drive \
         stage_artifact) — tests move WITH their machinery, and same-name porting is what stops \
         a net coverage deletion or junk-test padding; the source-tree survivor floor in \
         tests/source_layout.rs is this pin's twin. Missing from stage.rs: {missing:?}",
        PORTED_STAGING_TESTS.len()
    );
}

// ─── helper self-tests ───────────────────────────────────────────────────────

#[test]
fn helper_test_fn_names_reads_cfg_test_modules() {
    let names = test_fn_names(
        "#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn inside_cfg() {}\n\n    \
         #[test]\n    #[cfg(unix)]\n    fn attributed() {}\n}\n// #[test]\n// fn commented_out() {}\n",
    );
    assert!(
        names.contains(&"inside_cfg".to_owned()) && names.contains(&"attributed".to_owned()),
        "test fns inside a #[cfg(test)] module (with or without extra attributes) must be \
         counted — the ported-names pin scans unit tests, so the cfg strip the import scans \
         use would blind it: {names:?}"
    );
    assert!(
        !names.contains(&"commented_out".to_owned()),
        "a commented-out test must not count: {names:?}"
    );
}

#[test]
fn helper_trait_method_names_parses_declarations_over_defaults() {
    let stripped = strip(
        "pub trait SourceBackend {\n    fn fetch(&self);\n\
         fn export_artifact(&self, r: &ExportRequest) -> Result<ExportResult> { body }\n}",
    );
    let names = trait_method_names(&stripped, "SourceBackend").expect("trait body parses");
    assert!(
        names.contains(&"fetch".to_owned()) && names.contains(&"export_artifact".to_owned()),
        "both a declaration and a default-bodied method are named: {names:?}"
    );
}

#[test]
fn helper_calls_method_ignores_definitions_and_longer_neighbours() {
    assert!(calls_method(
        "backend.export_artifact(req)",
        "export_artifact"
    ));
    assert!(calls_method(
        "SourceBackend::export_artifact(&b, req)",
        "export_artifact"
    ));
    assert!(
        !calls_method("fn export_artifact(&self) {}", "export_artifact"),
        "a definition is not a call"
    );
    assert!(
        !calls_method("self.export_artifact_entry(x)", "export_artifact"),
        "a longer-named neighbour must not satisfy the scan"
    );
    assert!(
        !calls_method(&scan("// backend.export_artifact(req)"), "export_artifact"),
        "a call inside a comment must not count once scanned"
    );
}

#[test]
fn helper_forbidden_import_scan_ignores_comments_and_strings() {
    let scanned = scan(
        "// use crate::sync::stage::stage_artifact;\n\
         let s = \"crate::store::ManifestFile\";\n\
         use crate::config::Refspec;\n",
    );
    for forbidden in FORBIDDEN_SOURCE_IMPORTS {
        assert!(
            !scanned.contains(forbidden),
            "`{forbidden}` appearing only in a comment or string must not read as an import"
        );
    }
    let real = scan("pub(crate) use crate::sync::stage::Renderer;\n");
    assert!(
        real.contains("crate::sync"),
        "a real re-export of the sync staging module is caught: {real}"
    );
}

#[test]
fn helper_top_level_public_use_handles_groups_but_rejects_private_modules() {
    let grouped =
        scan("pub use crate::sync::state::{\n    ArtifactRecord,\n    ManifestFile,\n};\n");
    assert!(
        top_level_public_use_references(&grouped, "ManifestFile"),
        "a grouped multiline top-level public re-export must count"
    );

    let private_only = scan(
        "mod private {\n    pub use crate::sync::state::ManifestFile;\n}\n\
         pub use crate::sync::state::ArtifactRecord;\n",
    );
    assert!(
        !top_level_public_use_references(&private_only, "ManifestFile"),
        "a public use nested in a private module must not count as a facade re-export"
    );
}

#[test]
fn helper_top_level_field_type_binds_the_named_field() {
    let valid = balanced_body(&scan(
        "pub struct ArtifactRecord {\n    marker: String,\n    pub files: \
             Vec<ManifestFile>,\n}",
    ))
    .expect("valid ArtifactRecord body parses");
    assert!(
        top_level_field_type(&valid, "files")
            .is_some_and(|ty| references_token(ty, "ManifestFile")),
        "the files field's generic ManifestFile type must be extracted"
    );

    let decoy = balanced_body(&scan(
        "pub struct ArtifactRecord {\n    pub files: Vec<ScannedFile>,\n    \
             marker: Option<ManifestFile>,\n}",
    ))
    .expect("decoy ArtifactRecord body parses");
    assert!(
        top_level_field_type(&decoy, "files")
            .is_some_and(|ty| !references_token(ty, "ManifestFile")),
        "ManifestFile on an unrelated field must not satisfy the files-field contract"
    );
}
