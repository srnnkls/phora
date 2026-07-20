use std::fs;
use std::path::PathBuf;

const APPLY: &str = "sync/apply.rs";
const DEPLOY: &str = "deploy.rs";
const INSPECT: &str = "sync/inspect.rs";
const JOURNAL: &str = "sync/journal.rs";
const RECOVERY: &str = "sync/recovery.rs";
const SYNC_MOD: &str = "sync/mod.rs";

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

/// Blanks comments and literal contents while preserving code punctuation and newlines.
fn strip_non_code(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => {
                i += 2;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                let mut depth = 1usize;
                while i < chars.len() && depth > 0 {
                    match (chars[i], chars.get(i + 1)) {
                        ('/', Some('*')) => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            i += 2;
                        }
                        _ => {
                            out.push(blank(chars[i]));
                            i += 1;
                        }
                    }
                }
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
                    out.push(blank(chars[i]));
                    i += 1;
                }
                if i < chars.len() {
                    out.push('\'');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &byte) in bytes.iter().enumerate().skip(open) {
        match byte {
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

/// Presence checks ignore test-only items; absence checks deliberately use
/// `strip_non_code` directly so a duplicate hidden under cfg(test) still fails.
fn strip_cfg_test(stripped: &str) -> String {
    let mut out = stripped.to_owned();
    while let Some(start) = out.find("#[cfg(test)]") {
        let after = start + "#[cfg(test)]".len();
        let brace = out[after..].find('{').map(|i| after + i);
        let semi = out[after..].find(';').map(|i| after + i);
        let end = match (brace, semi) {
            (Some(brace), semi) if semi.is_none_or(|semi| brace < semi) => {
                matching_brace(out.as_bytes(), brace)
            }
            (_, Some(semi)) => Some(semi),
            _ => None,
        };
        match end {
            Some(end) => out.replace_range(start..=end, " "),
            None => out.truncate(start),
        }
    }
    out
}

fn production(src: &str) -> String {
    strip_cfg_test(&strip_non_code(src))
}

struct SourceScans {
    production: Vec<(String, String)>,
    all_code: Vec<(String, String)>,
}

impl SourceScans {
    fn load() -> Self {
        fn visit(dir: &std::path::Path, paths: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).expect("read src directory") {
                let path = entry.expect("read src entry").path();
                if path.is_dir() {
                    visit(&path, paths);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    paths.push(path);
                }
            }
        }

        let root = src_dir();
        let mut paths = Vec::new();
        visit(&root, &mut paths);
        paths.sort();
        let raw: Vec<(String, String)> = paths
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(&root)
                    .expect("Rust source lives under src")
                    .to_str()
                    .expect("repository paths are UTF-8")
                    .to_owned();
                let source = fs::read_to_string(path).expect("read Rust source");
                (relative, source)
            })
            .collect();
        Self::from_raw_owned(&raw)
    }

    fn from_raw(raw: &[(&str, &str)]) -> Self {
        let owned: Vec<(String, String)> = raw
            .iter()
            .map(|(path, source)| ((*path).to_owned(), (*source).to_owned()))
            .collect();
        Self::from_raw_owned(&owned)
    }

    fn from_raw_owned(raw: &[(String, String)]) -> Self {
        Self {
            production: raw
                .iter()
                .map(|(path, source)| (path.clone(), production(source)))
                .collect(),
            all_code: raw
                .iter()
                .map(|(path, source)| (path.clone(), strip_non_code(source)))
                .collect(),
        }
    }
}

fn names_keyword(src: &str, keyword: &str, name: &str) -> bool {
    count_names_keyword(src, keyword, name) > 0
}

fn names_keyword_at(src: &str, i: usize, keyword: &str, name: &str) -> bool {
    let bytes = src.as_bytes();
    let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
    let rest_all = &src[i + keyword.len()..];
    let rest = rest_all.trim_start();
    before_ok
        && rest_all.len() > rest.len()
        && rest.strip_prefix(name).is_some_and(|after| {
            after
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        })
}

fn count_names_keyword(src: &str, keyword: &str, name: &str) -> usize {
    src.match_indices(keyword)
        .filter(|(i, _)| names_keyword_at(src, *i, keyword, name))
        .count()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BraceContext {
    Module,
    NonModule,
}

fn brace_context(src: &str, open: usize) -> BraceContext {
    let header_start = src[..open]
        .rfind(['{', '}', ';'])
        .map_or(0, |boundary| boundary + 1);
    let header = &src[header_start..open];
    if has_keyword(header, "fn") || has_keyword(header, "impl") || has_keyword(header, "trait") {
        BraceContext::NonModule
    } else if has_keyword(header, "mod") {
        BraceContext::Module
    } else {
        BraceContext::NonModule
    }
}

fn count_module_free_fn(src: &str, name: &str) -> usize {
    let mut contexts = Vec::new();
    let mut count = 0;
    for (i, c) in src.char_indices() {
        match c {
            '{' => contexts.push(brace_context(src, i)),
            '}' => {
                contexts.pop();
            }
            'f' if src[i..].starts_with("fn")
                && contexts
                    .iter()
                    .all(|context| *context == BraceContext::Module)
                && names_keyword_at(src, i, "fn", name) =>
            {
                count += 1;
            }
            _ => {}
        }
    }
    count
}

fn has_keyword(src: &str, keyword: &str) -> bool {
    let bytes = src.as_bytes();
    src.match_indices(keyword).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after = i + keyword.len();
        let after_ok =
            after == bytes.len() || (!bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_');
        before_ok && after_ok
    })
}

fn defines_fn(src: &str, name: &str) -> bool {
    count_module_free_fn(src, name) > 0
}

fn defines_type(src: &str, name: &str) -> bool {
    names_keyword(src, "struct", name) || names_keyword(src, "enum", name)
}

fn named_item_bodies<'a>(src: &'a str, keyword: &str, name: &str) -> Vec<&'a str> {
    let needle = format!("{keyword} {name}");
    let bytes = src.as_bytes();
    src.match_indices(&needle)
        .filter_map(|(start, _)| {
            let before_ok = start == 0
                || (!bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_');
            let after = start + needle.len();
            let after_ok = after == bytes.len()
                || (!bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_');
            if !(before_ok && after_ok) {
                return None;
            }
            let open = src[after..].find('{').map(|offset| after + offset)?;
            let semi = src[after..].find(';').map(|offset| after + offset);
            if semi.is_some_and(|semi| semi < open) {
                return None;
            }
            let close = matching_brace(src.as_bytes(), open)?;
            Some(&src[open..=close])
        })
        .collect()
}

fn definition_sites<'a>(
    modules: &'a [(String, String)],
    keyword: &str,
    name: &str,
) -> Vec<(&'a str, usize)> {
    modules
        .iter()
        .filter_map(|(path, src)| {
            let count = if keyword == "fn" {
                count_module_free_fn(src, name)
            } else {
                count_names_keyword(src, keyword, name)
            };
            (count > 0).then_some((path.as_str(), count))
        })
        .collect()
}

fn has_exact_unique_anchor(
    scans: &SourceScans,
    keyword: &str,
    name: &str,
    home: &str,
    expected_home_count: usize,
) -> bool {
    let expected = [(home, expected_home_count)];
    definition_sites(&scans.production, keyword, name) == expected
        && definition_sites(&scans.all_code, keyword, name) == expected
}

fn assert_unique_anchor(
    scans: &SourceScans,
    keyword: &str,
    name: &str,
    home: &str,
    expected_home_count: usize,
) {
    let production_sites = definition_sites(&scans.production, keyword, name);
    let all_code_sites = definition_sites(&scans.all_code, keyword, name);
    assert!(
        has_exact_unique_anchor(scans, keyword, name, home, expected_home_count),
        "`{keyword} {name}` must have exactly one required production ownership site at \
         src/{home} and no duplicate definition anywhere under src/**/*.rs, including \
         cfg(test); production sites/counts: {production_sites:?}; all-code sites/counts: \
         {all_code_sites:?}"
    );
}

fn impl_method_sites<'a>(
    modules: &'a [(String, String)],
    impl_keyword: &str,
    type_name: &str,
    method: &str,
) -> Vec<(&'a str, usize)> {
    modules
        .iter()
        .filter_map(|(path, src)| {
            let count = named_item_bodies(src, impl_keyword, type_name)
                .iter()
                .map(|body| count_names_keyword(body, "fn", method))
                .sum();
            (count > 0).then_some((path.as_str(), count))
        })
        .collect()
}

fn assert_unique_impl_methods(
    scans: &SourceScans,
    impl_keyword: &str,
    type_name: &str,
    home: &str,
    methods: &[&str],
) {
    assert_unique_anchor(scans, impl_keyword, type_name, home, 1);
    for method in methods {
        let production_sites =
            impl_method_sites(&scans.production, impl_keyword, type_name, method);
        let all_code_sites = impl_method_sites(&scans.all_code, impl_keyword, type_name, method);
        assert!(
            production_sites == [(home, 1)] && all_code_sites == [(home, 1)],
            "method `{type_name}::{method}` must occur exactly once inside its assigned \
             production impl at src/{home} and nowhere under cfg(test); production \
             sites/counts: {production_sites:?}; all-code sites/counts: {all_code_sites:?}"
        );
    }
}

fn assert_carrier_tokens(
    scans: &SourceScans,
    keyword: &str,
    name: &str,
    home: &str,
    tokens: &[&str],
) {
    assert_unique_anchor(scans, keyword, name, home, 1);
    let home_src = scans
        .production
        .iter()
        .find_map(|(path, src)| (path == home).then_some(src.as_str()))
        .expect("assigned ownership module is scanned");
    let bodies = named_item_bodies(home_src, keyword, name);
    assert_eq!(
        bodies.len(),
        1,
        "src/{home} must contain one `{keyword} {name}` body"
    );
    let missing: Vec<&&str> = tokens
        .iter()
        .filter(|token| !has_keyword(bodies[0], token))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{home}'s `{keyword} {name}` must retain its complete move-only carrier shape; \
         missing fields/variants: {missing:?}"
    );
}

fn has_pub_reexport_from(src: &str, module: &str) -> bool {
    production(src).split(';').any(|statement| {
        let compact: String = statement.chars().filter(|c| !c.is_whitespace()).collect();
        if !compact.starts_with("pubuse") {
            return false;
        }
        let direct = format!("crate::sync::{module}::");
        if compact.contains(&direct) {
            return true;
        }
        let Some(group) = compact.split_once("crate::sync::{").map(|(_, group)| group) else {
            return false;
        };
        group.starts_with(&format!("{module}::")) || group.contains(&format!(",{module}::"))
    })
}

fn shell_without_comment(line: &str) -> String {
    let mut out = String::new();
    let mut quote = None;
    let mut escaped = false;
    for c in line.chars() {
        if let Some(active_quote) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == active_quote {
                quote = None;
            }
        } else if c == '#' {
            break;
        } else {
            out.push(c);
            if matches!(c, '\'' | '"') {
                quote = Some(c);
            }
        }
    }
    out
}

fn active_legacy_allowlist_entries(shell: &str) -> Vec<String> {
    let mut in_allowlist = false;
    let mut entries = Vec::new();
    for line in shell.lines() {
        let code = shell_without_comment(line);
        let trimmed = code.trim();
        if !in_allowlist {
            in_allowlist = trimmed == "LEGACY_ALLOWLIST=(";
            continue;
        }
        if trimmed == ")" {
            in_allowlist = false;
            continue;
        }
        if let Some(entry) = trimmed
            .strip_prefix("$'")
            .and_then(|entry| entry.strip_suffix('\''))
        {
            entries.push(entry.to_owned());
        }
    }
    entries
}

fn has_active_deploy_t024_allowlist(shell: &str) -> bool {
    active_legacy_allowlist_entries(shell)
        .iter()
        .any(|entry| entry == "src/deploy.rs\\tT024")
}

fn sync_production_name_sites<'a>(scans: &'a SourceScans, name: &str) -> Vec<&'a str> {
    scans
        .production
        .iter()
        .filter_map(|(path, source)| {
            (path.starts_with("sync/") && has_keyword(source, name)).then_some(path.as_str())
        })
        .collect()
}

fn assert_no_sync_production_name(scans: &SourceScans, old: &str, replacement: &str) {
    let sites = sync_production_name_sites(scans, old);
    assert!(
        sites.is_empty(),
        "T023 must replace the old internal `{old}` identifier with `{replacement}` everywhere \
         under src/sync; production sites still containing it: {sites:?}. The compatibility \
         facade at src/{DEPLOY} is intentionally outside this assertion"
    );
}

#[test]
fn sync_registers_apply_journal_and_recovery_modules() {
    let sync_mod = production(&read_src(SYNC_MOD));
    for module in ["apply", "journal", "recovery"] {
        assert!(
            names_keyword(&sync_mod, "mod", module),
            "src/{SYNC_MOD} must register the new sync::{module} module in production; comments, \
             strings, and cfg(test)-only declarations do not count"
        );
    }
}

#[test]
fn apply_module_owns_the_copy_swap_link_and_apply_cluster() {
    let scans = SourceScans::load();
    for function in [
        "copy_file",
        "copy_mtime",
        "copy_tree",
        "apply_artifact",
        "link_artifact",
        "link_nonce",
        "swap_into",
        "is_cross_device",
    ] {
        assert_unique_anchor(&scans, "fn", function, APPLY, 1);
    }
    // Unix and Windows retain mutually-exclusive definitions of the same helper.
    assert_unique_anchor(&scans, "fn", "create_symlink", APPLY, 2);
    assert_unique_anchor(&scans, "struct", "CleanupGuard", APPLY, 1);
    assert_unique_impl_methods(
        &scans,
        "impl",
        "CleanupGuard",
        APPLY,
        &["new", "track", "prune_base_if_empty"],
    );
    assert_unique_impl_methods(&scans, "impl Drop for", "CleanupGuard", APPLY, &["drop"]);
}

#[test]
fn journal_module_owns_the_journal_types_and_implementation() {
    let scans = SourceScans::load();
    assert_carrier_tokens(&scans, "struct", "Journal", JOURNAL, &["path", "mode"]);
    assert_carrier_tokens(
        &scans,
        "enum",
        "JournalMode",
        JOURNAL,
        &["Writable", "ReadOnly", "root"],
    );
    assert_carrier_tokens(
        &scans,
        "struct",
        "JournalEntry",
        JOURNAL,
        &["staging_base", "staging", "dst", "record", "swap_completed"],
    );
    assert_carrier_tokens(&scans, "struct", "JournalFile", JOURNAL, &["entries"]);
    assert_unique_impl_methods(
        &scans,
        "impl",
        "Journal",
        JOURNAL,
        &[
            "open",
            "open_readonly",
            "refuses_writes",
            "readonly_error",
            "load",
            "persist",
            "append",
            "mark_swap_completed",
            "entries",
            "remove",
            "clear",
        ],
    );
}

#[test]
fn recovery_module_owns_sweep_and_rollback_helpers() {
    let scans = SourceScans::load();
    // These helpers are shared with apply, but recovery owns them: recovery_sweep
    // must locate backups and remove paths, while apply imports the rollback vocabulary.
    for function in [
        "remove_path",
        "rollback_swap",
        "backup_path",
        "recovery_sweep",
        "remove_orphaned_staging",
    ] {
        assert_unique_anchor(&scans, "fn", function, RECOVERY, 1);
    }
}

#[test]
fn deploy_is_a_reexport_only_compatibility_facade() {
    let deploy_src = read_src(DEPLOY);
    let stripped = strip_non_code(&deploy_src);
    let forbidden_items: Vec<&&str> = [
        "fn", "struct", "enum", "union", "trait", "impl", "type", "const", "static",
    ]
    .iter()
    .filter(|keyword| has_keyword(&stripped, keyword))
    .collect();
    assert!(
        forbidden_items.is_empty(),
        "src/{DEPLOY} must be a compatibility re-export facade with no function/type \
         implementation, including under cfg(test); item keywords still present: \
         {forbidden_items:?}"
    );
    for module in ["apply", "journal", "recovery", "inspect"] {
        assert!(
            has_pub_reexport_from(&deploy_src, module),
            "src/{DEPLOY} must publicly re-export the relocated sync::{module} API for legacy \
             phora::deploy callers; comments, strings, private uses, and cfg(test) do not count"
        );
    }
}

#[test]
fn observation_remains_owned_only_by_inspect() {
    let inspect = production(&read_src(INSPECT));
    assert!(
        defines_fn(&inspect, "check_artifact_state") && defines_type(&inspect, "ArtifactState"),
        "T022 must leave check_artifact_state and ArtifactState owned by src/{INSPECT}"
    );
    for module in [APPLY, JOURNAL, RECOVERY] {
        let stripped = strip_non_code(&read_src(module));
        assert!(
            !defines_fn(&stripped, "check_artifact_state")
                && !defines_type(&stripped, "ArtifactState"),
            "T022 must not double-move or duplicate observation into src/{module}"
        );
    }
}

#[test]
fn t023_apply_artifact_replaces_internal_deploy_artifact() {
    let scans = SourceScans::load();
    assert_unique_anchor(&scans, "fn", "apply_artifact", APPLY, 1);
    assert_no_sync_production_name(&scans, "deploy_artifact", "apply_artifact");
}

#[test]
fn t023_apply_target_changes_replaces_deploy_target_orchestration() {
    let scans = SourceScans::load();
    assert_unique_anchor(&scans, "fn", "apply_target_changes", SYNC_MOD, 1);
    for old in ["deploy_target", "deploy_all_targets"] {
        assert_no_sync_production_name(&scans, old, "apply_target_changes");
    }
}

#[test]
fn t023_apply_run_replaces_deploy_run() {
    let scans = SourceScans::load();
    assert_unique_anchor(&scans, "struct", "ApplyRun", SYNC_MOD, 1);
    assert_no_sync_production_name(&scans, "DeployRun", "ApplyRun");
}

#[test]
fn t023_preserves_t024_allowlist() {
    let arch_check =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/arch-check.sh"))
            .expect("read architecture guardrail");
    assert!(
        has_active_deploy_t024_allowlist(&arch_check),
        "the exact active `$'src/deploy.rs\\tT024'` LEGACY_ALLOWLIST array entry expires in \
         T024, not during T022; parsed active entries: {:?}",
        active_legacy_allowlist_entries(&arch_check)
    );
}

#[test]
fn legacy_deploy_api_remains_importable() {
    fn accepts<T>(_: T) {}

    accepts(phora::deploy::copy_file);
    accepts(phora::deploy::copy_tree);
    accepts(phora::deploy::deploy_artifact);
    accepts(phora::deploy::link_artifact);
    accepts(phora::deploy::recovery_sweep);
    accepts(phora::deploy::Journal::open);
    accepts(phora::deploy::check_artifact_state);
    let _ = std::mem::size_of::<phora::deploy::JournalEntry>();
    let _ = std::mem::size_of::<phora::deploy::ArtifactState>();
}

#[test]
fn scanner_rejects_comment_string_and_cfg_test_ownership_decoys() {
    let decoys = r#"
        // fn deploy_artifact() {}
        const TEXT: &str = "pub struct Journal; fn recovery_sweep() {}";
        #[cfg(test)]
        mod tests {
            fn deploy_artifact() {}
            pub struct Journal;
            fn recovery_sweep() {}
        }
    "#;
    let scanned = production(decoys);
    assert!(!defines_fn(&scanned, "deploy_artifact"));
    assert!(!defines_type(&scanned, "Journal"));
    assert!(!defines_fn(&scanned, "recovery_sweep"));

    let stripped = strip_non_code(decoys);
    assert!(
        defines_fn(&stripped, "deploy_artifact")
            && defines_type(&stripped, "Journal")
            && defines_fn(&stripped, "recovery_sweep"),
        "absence scans must still see duplicate definitions hidden under cfg(test)"
    );

    let clean = SourceScans::from_raw(&[
        (APPLY, "fn deploy_artifact() {}"),
        (
            "sync/other.rs",
            r#"fn caller() { deploy_artifact(); }
               // fn deploy_artifact() {}
               const TEXT: &str = "fn deploy_artifact() {}";"#,
        ),
    ]);
    assert!(
        has_exact_unique_anchor(&clean, "fn", "deploy_artifact", APPLY, 1),
        "references, comments, and string literals are not definition sites"
    );

    let cfg_test_duplicate = SourceScans::from_raw(&[
        (APPLY, "fn deploy_artifact() {}"),
        (
            "sync/other.rs",
            "#[cfg(test)] mod tests { fn deploy_artifact() {} }",
        ),
    ]);
    assert!(
        !has_exact_unique_anchor(&cfg_test_duplicate, "fn", "deploy_artifact", APPLY, 1),
        "a duplicate definition hidden under cfg(test) anywhere in src must fail exact-one \
         ownership"
    );

    let out_of_home_duplicate = SourceScans::from_raw(&[
        (APPLY, "fn deploy_artifact() {}"),
        ("sync/other.rs", "fn deploy_artifact() {}"),
    ]);
    assert!(
        !has_exact_unique_anchor(&out_of_home_duplicate, "fn", "deploy_artifact", APPLY, 1),
        "a duplicate production definition in any out-of-home src/**/*.rs file must fail \
         exact-one ownership"
    );
}

#[test]
fn free_function_scanner_distinguishes_module_items_from_nested_items() {
    let non_free_decoys = SourceScans::from_raw(&[
        (APPLY, "fn deploy_artifact() {}"),
        (
            "sync/other.rs",
            r"
                struct Helper;
                impl Helper { fn deploy_artifact() {} }
                trait Port { fn deploy_artifact(); }
                fn outer() { fn deploy_artifact() {} }
            ",
        ),
    ]);
    assert!(
        has_exact_unique_anchor(&non_free_decoys, "fn", "deploy_artifact", APPLY, 1),
        "same-named inherent methods, trait items, and functions nested inside another \
         function are not module-level free-function definition sites"
    );

    let out_of_home_module_fn = SourceScans::from_raw(&[
        (APPLY, "fn deploy_artifact() {}"),
        ("sync/other.rs", "mod sibling { fn deploy_artifact() {} }"),
    ]);
    assert!(
        !has_exact_unique_anchor(&out_of_home_module_fn, "fn", "deploy_artifact", APPLY, 1),
        "a genuine free function in an out-of-home inline module must fail exact-one ownership"
    );

    let cfg_test_module_fn = SourceScans::from_raw(&[
        (APPLY, "fn deploy_artifact() {}"),
        (
            "sync/other.rs",
            "#[cfg(test)] mod tests { fn deploy_artifact() {} }",
        ),
    ]);
    assert!(
        !has_exact_unique_anchor(&cfg_test_module_fn, "fn", "deploy_artifact", APPLY, 1),
        "a genuine free function inside a cfg(test) inline module must remain visible to the \
         duplicate pass"
    );
}

#[test]
fn internal_name_scanner_ignores_compatibility_facade_and_decoys() {
    let clean = SourceScans::from_raw(&[
        (
            DEPLOY,
            "pub use crate::sync::apply::apply_artifact as deploy_artifact;",
        ),
        (
            APPLY,
            r#"
                // deploy_artifact is the legacy facade name.
                const MESSAGE: &str = "deploy_artifact";
                fn apply_artifact() {}
                #[cfg(test)] mod tests { fn deploy_artifact() {} }
            "#,
        ),
    ]);
    assert!(
        sync_production_name_sites(&clean, "deploy_artifact").is_empty(),
        "the retained compatibility alias, comments, strings, and cfg(test)-only decoys must not \
         count as old sync-production identifiers"
    );

    let stale_internal_use = SourceScans::from_raw(&[
        (
            DEPLOY,
            "pub use crate::sync::apply::apply_artifact as deploy_artifact;",
        ),
        ("sync/target.rs", "fn caller() { deploy_artifact(); }"),
    ]);
    assert_eq!(
        sync_production_name_sites(&stale_internal_use, "deploy_artifact"),
        ["sync/target.rs"],
        "a real old-name use inside src/sync must remain visible even while the facade retains it"
    );
}

#[test]
fn shell_allowlist_parser_rejects_comments_strings_and_unrelated_text() {
    let active = r"
LEGACY_ALLOWLIST=(
  $'src/deploy.rs\tT024' # an active entry may have a trailing comment
)
";
    assert!(has_active_deploy_t024_allowlist(active));

    for decoy in [
        r"
LEGACY_ALLOWLIST=(
  # $'src/deploy.rs\tT024'
)
",
        r#"
LEGACY_ALLOWLIST=(
  "diagnostic: $'src/deploy.rs\tT024'"
)
"#,
        r#"
LEGACY_ALLOWLIST=(
)
echo "$'src/deploy.rs\tT024'"
"#,
        r"
OTHER_ALLOWLIST=(
  $'src/deploy.rs\tT024'
)
",
    ] {
        assert!(
            !has_active_deploy_t024_allowlist(decoy),
            "shell comments, quoted diagnostics, unrelated commands, and other arrays must not \
             satisfy the active T024 allowlist pin: {decoy:?}"
        );
    }
}
