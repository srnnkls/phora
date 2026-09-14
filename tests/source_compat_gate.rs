use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
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

fn trait_method_names(stripped: &str, trait_name: &str) -> Option<BTreeSet<String>> {
    let start = stripped.find(&format!("pub trait {trait_name}"))?;
    let body = balanced_body(&stripped[start..])?;
    let bytes = body.as_bytes();
    let mut names = BTreeSet::new();
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
            names.insert(name);
        }
    }
    Some(names)
}

fn top_level_public_fn_signature(stripped: &str, fn_name: &str) -> Option<String> {
    let needle = format!("pub fn {fn_name}");
    let bytes = stripped.as_bytes();
    stripped.match_indices(&needle).find_map(|(i, _)| {
        let depth = bytes[..i].iter().fold(0i32, |depth, byte| match byte {
            b'{' => depth + 1,
            b'}' => depth - 1,
            _ => depth,
        });
        if depth != 0 {
            return None;
        }
        let after = i + needle.len();
        let after_ok = bytes
            .get(after)
            .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_');
        if !after_ok {
            return None;
        }
        let rest = &stripped[i..];
        let end = rest.find(['{', ';'])?;
        Some(rest[..end].to_string())
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

/// `tests.rs`/`*_tests.rs` are excluded: their `#[cfg(test)]` attribute sits on
/// the parent's `mod` declaration, invisible to `strip_cfg_test`.
fn prod_src_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    collect_rs_files(&src_dir(), "", &mut out);
    out.retain(|(rel, _)| !(rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs")));
    out
}

fn calls_method(stripped: &str, method: &str) -> bool {
    stripped.match_indices(method).any(|(i, _)| {
        let after = i + method.len();
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

const KEPT_TOKENS: &[&str] = &["retained", "delegates", "delegating", "delegated"];

fn line_disposition(line: &str, method: &str) -> Option<Result<&'static str, String>> {
    if !line.contains(&format!("`{method}`")) {
        return None;
    }
    let lower = line.to_lowercase();
    let removed = references_token(&lower, "removed");
    let kept = KEPT_TOKENS.iter().any(|t| references_token(&lower, t));
    match (removed, kept) {
        (true, true) => Some(Err(format!(
            "line classifies `{method}` as both removed and retained/delegating: {line}"
        ))),
        (true, false) => Some(Ok("removed")),
        (false, true) => Some(Ok("kept")),
        (false, false) => None,
    }
}

const ANCHOR_WINDOW: usize = 20;

fn table_rows(text: &str, methods: &[&str]) -> Result<BTreeMap<String, &'static str>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let anchors: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains("migration table"))
        .map(|(i, _)| i)
        .collect();
    let mut rows = BTreeMap::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let row_shaped = trimmed.starts_with('|') || trimmed.starts_with('-');
        let anchored = anchors.iter().any(|&a| i > a && i - a <= ANCHOR_WINDOW);
        if !(row_shaped || anchored) {
            continue;
        }
        for method in methods {
            let Some(row) = line_disposition(line, method) else {
                continue;
            };
            let disposition = row?;
            if let Some(prior) = rows.insert((*method).to_owned(), disposition)
                && prior != disposition
            {
                return Err(format!(
                    "the migration table classifies `{method}` twice with conflicting \
                     dispositions ({prior} vs {disposition})"
                ));
            }
        }
    }
    Ok(rows)
}

const LEGACY_METHODS: &[&str] = &[
    "fetch",
    "mirror_ready",
    "read_file_at",
    "list_source_leaves",
    "list_tree_at",
    "resolve",
    "commit_time",
    "export_artifact",
    "compute_digest",
];

fn source_files() -> Vec<(String, String)> {
    prod_src_files()
        .into_iter()
        .filter(|(rel, _)| rel.starts_with("source/"))
        .collect()
}

fn find_trait_scan(trait_name: &str) -> (String, String) {
    source_files()
        .into_iter()
        .map(|(rel, content)| (rel, scan(&content)))
        .find(|(_, scanned)| scanned.contains(&format!("pub trait {trait_name}")))
        .unwrap_or_else(|| panic!("no file under src/source/ declares `pub trait {trait_name}`"))
}

fn find_migration_table() -> (&'static str, BTreeMap<String, &'static str>) {
    let name = "docs/architecture.md";
    let text = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name))
        .unwrap_or_default();
    let rows = match table_rows(&text, LEGACY_METHODS) {
        Ok(rows) => rows,
        Err(conflict) => panic!("{name}: {conflict}"),
    };
    assert!(
        !rows.is_empty(),
        "T013 requires an explicit caller-migration table in {name} (the decision-record \
         home): rows must be table-shaped (leading `|` or `-`) or sit within {ANCHOR_WINDOW} \
         lines after a `migration table` heading. Each of {LEGACY_METHODS:?} needs one row \
         naming the backticked `method` plus its disposition (`removed`, or one of \
         {KEPT_TOKENS:?})"
    );
    let missing: Vec<&str> = LEGACY_METHODS
        .iter()
        .copied()
        .filter(|m| !rows.contains_key(*m))
        .collect();
    assert!(
        missing.is_empty(),
        "{name}: the caller-migration table must classify EVERY legacy SourceBackend method — \
         one row per method naming the backticked `method` plus `removed` or one of \
         {KEPT_TOKENS:?}; unclassified: {missing:?}"
    );
    (name, rows)
}

#[test]
fn gate_source_store_declares_the_exact_final_operation_set() {
    let (rel, scanned) = find_trait_scan("SourceStore");
    let methods = trait_method_names(&scanned, "SourceStore")
        .unwrap_or_else(|| panic!("src/{rel}: SourceStore trait body must parse"));
    let expected: BTreeSet<String> = [
        "resolve",
        "inventory",
        "read",
        "list_directory",
        "lock_worktree_mirror",
        "lock_worktree_mirror_at",
        "observe_worktree",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        methods, expected,
        "src/{rel}: SourceStore's authoritative final operation set is exactly resolve, \
         inventory, read, list_directory, lock_worktree_mirror, lock_worktree_mirror_at, and \
         observe_worktree; digest_snapshot is a source-owned free operation, not a trait method"
    );
}

#[test]
fn gate_digest_snapshot_is_a_source_owned_public_free_operation() {
    let (rel, signature) = source_files()
        .into_iter()
        .find_map(|(rel, content)| {
            top_level_public_fn_signature(&scan(&content), "digest_snapshot")
                .map(|signature| (rel, signature))
        })
        .unwrap_or_else(|| {
            panic!(
                "src/source must export a top-level `pub fn digest_snapshot(store, snapshot, \
                 leaves)`; a SourceStore trait method does not satisfy the final source contract"
            )
        });
    for required in [
        "store",
        "snapshot",
        "leaves",
        "SourceStore",
        "SnapshotId",
        "SourcePath",
    ] {
        assert!(
            references_token(&signature, required),
            "src/{rel}: public free digest_snapshot must name its store, snapshot, and explicit \
             leaf inputs using the source contract types; missing `{required}` in: {signature}"
        );
    }
    for banned in ["OfferSelection", "include", "exclude"] {
        assert!(
            !references_token(&signature, banned),
            "src/{rel}: digest_snapshot must NEVER accept selection (`{banned}`): projection \
             chooses leaves and the source operation digests exactly those leaves (INV-2); \
             signature: {signature}"
        );
    }
    assert!(
        signature.starts_with("pub fn digest_snapshot"),
        "src/{rel}: digest_snapshot must be a public top-level source operation; \
         signature: {signature}"
    );
}

#[test]
fn gate_migration_table_classifies_every_legacy_method() {
    let (name, rows) = find_migration_table();
    assert_eq!(
        rows.len(),
        LEGACY_METHODS.len(),
        "{name}: the migration table must carry exactly one disposition per legacy method"
    );
}

#[test]
fn gate_final_source_capability_replaces_the_legacy_trait() {
    let source_backend_sites: Vec<String> = source_files()
        .into_iter()
        .filter(|(_, content)| references_token(&scan(content), "SourceBackend"))
        .map(|(rel, _)| format!("src/{rel}"))
        .collect();
    assert!(
        source_backend_sites.is_empty(),
        "T030 must remove SourceBackend from the live source capability tree; found: \
         {source_backend_sites:?}"
    );
    let _ = find_trait_scan("SourceStore");
}

#[test]
fn gate_source_backend_has_zero_production_callers() {
    let callers: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(_, content)| references_token(&scan(content), "SourceBackend"))
        .map(|(rel, _)| format!("src/{rel}"))
        .collect();
    assert!(
        callers.is_empty(),
        "T030 must leave zero live SourceBackend imports, bounds, impls, UFCS calls, or \
         consumers anywhere in production; found: {callers:?}"
    );
}

#[test]
fn gate_source_store_surface_excludes_legacy_operations() {
    let (rel, scanned) = find_trait_scan("SourceStore");
    let methods = trait_method_names(&scanned, "SourceStore")
        .unwrap_or_else(|| panic!("src/{rel}: SourceStore trait body must parse"));
    let legacy_operations: Vec<&str> = LEGACY_METHODS
        .iter()
        .copied()
        .filter(|method| *method != "resolve" && methods.contains(*method))
        .collect();
    assert!(
        legacy_operations.is_empty(),
        "SourceStore is the final source capability and must not absorb SourceBackend's \
         readiness/routing/metadata/export operations; found in src/{rel}: \
         {legacy_operations:?}"
    );
}

#[test]
fn gate_lock_byte_oracle_stays_wired() {
    let compat = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/compat_serialized.rs"),
    )
    .expect("tests/compat_serialized.rs is the T002 byte-oracle and must exist");
    for golden in ["lock.toml.golden", "url_synthetic_commit.golden"] {
        assert!(
            compat.contains(&format!("assert_golden(\"{golden}\"")),
            "tests/compat_serialized.rs must keep asserting {golden}: the locked T002 goldens \
             are the byte-parity oracle for T013's lock compatibility — weakening the driver \
             would let the compat SourceBackend drift the lock bytes unnoticed"
        );
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/compat/serialized")
            .join(golden);
        assert!(
            path.is_file() && fs::read_to_string(&path).is_ok_and(|s| !s.is_empty()),
            "golden fixture {golden} must exist non-empty (regeneration is LOCKED)"
        );
    }
    let synthetic = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/compat/serialized/url_synthetic_commit.golden"),
    )
    .expect("url synthetic commit golden");
    assert_eq!(
        synthetic.trim(),
        "d27db2740b93bb1f970ec2d67fd33cb0d750bb77",
        "the url-source synthetic commit is content-addressed and pinned; T013 must not \
         change how url sources materialize commits"
    );
}

#[test]
fn helper_line_disposition_requires_backticks_and_one_token() {
    assert_eq!(
        line_disposition("| `fetch` | removed | callers moved to … |", "fetch"),
        Some(Ok("removed"))
    );
    assert_eq!(
        line_disposition("- `compute_digest`: delegating-retained", "compute_digest"),
        Some(Ok("kept"))
    );
    assert_eq!(
        line_disposition("fetch was removed long ago", "fetch"),
        None,
        "a method named without backticks is prose, not a table row"
    );
    assert_eq!(
        line_disposition("| `fetch` | still routed |", "fetch"),
        None,
        "a row without a disposition token classifies nothing"
    );
    assert!(
        matches!(
            line_disposition("| `fetch` | retained then removed |", "fetch"),
            Some(Err(_))
        ),
        "one row carrying both dispositions is a conflict, not a silent pick"
    );
    assert_eq!(
        line_disposition("| `mirror_ready` | removed |", "fetch"),
        None,
        "a row classifies only the method it names"
    );
}

#[test]
fn helper_table_rows_require_row_shape_or_a_migration_table_anchor() {
    let prose = "the `fetch` method was removed in T013";
    assert!(
        table_rows(prose, &["fetch"]).expect("scan").is_empty(),
        "scattered prose without row shape or a nearby anchor must not classify"
    );
    assert_eq!(
        table_rows("| `fetch` | removed |", &["fetch"])
            .expect("scan")
            .get("fetch"),
        Some(&"removed"),
        "a `|`-shaped row classifies wherever it sits"
    );
    let anchored = format!("## Migration table\n\n{prose}");
    assert_eq!(
        table_rows(&anchored, &["fetch"])
            .expect("scan")
            .get("fetch"),
        Some(&"removed"),
        "a line shortly after a `migration table` heading classifies"
    );
    let far = format!(
        "## Migration table\n{}{prose}",
        "\n".repeat(ANCHOR_WINDOW + 5)
    );
    assert!(
        table_rows(&far, &["fetch"]).expect("scan").is_empty(),
        "the anchor window is bounded: a distant prose line must not classify"
    );
}

#[test]
fn helper_strip_blanks_raw_strings_without_desyncing() {
    let stripped = strip("let t = r#\"a \"quoted\" bit\"#; b.fetch(x) // b.resolve(y)");
    assert!(
        !references_token(&stripped, "quoted"),
        "raw-string content must be blanked, embedded double quote included: {stripped}"
    );
    assert!(
        calls_method(&stripped, "fetch"),
        "the scanner must stay in sync after a raw string: {stripped}"
    );
    assert!(
        !calls_method(&stripped, "resolve"),
        "a trailing comment must still be stripped after a raw string: {stripped}"
    );
}

#[test]
fn helper_calls_method_sees_calls_but_not_definitions_or_prefixes() {
    assert!(calls_method("backend.fetch(name, url)", "fetch"));
    assert!(calls_method("SourceBackend::fetch(&b, name, url)", "fetch"));
    assert!(calls_method("self . fetch (n)", "fetch"));
    assert!(
        !calls_method("fn fetch(&self) {}", "fetch"),
        "a definition is not a call site"
    );
    assert!(
        !calls_method("counter.fetch_add(1, Relaxed)", "fetch"),
        "`fetch_add` must not satisfy a word-bounded `fetch` scan"
    );
    assert!(
        !calls_method("cache.prefetch(url)", "fetch"),
        "`prefetch` must not satisfy the scan"
    );
    assert!(
        !calls_method(&scan("// backend.fetch(name, url)"), "fetch"),
        "a call inside a comment must not count once scanned"
    );
}

#[test]
fn helper_top_level_public_fn_signature_rejects_trait_methods_and_stops_before_the_body() {
    let stripped = strip(
        "pub trait SourceStore {\n    fn read(&self, p: &SourcePath) -> Result<SourceEntry>;\n\
         \n    fn digest_snapshot(&self, source: &ResolvedSource, leaves: &[SourcePath]) \
         -> Result<String> {\n        unimplemented!()\n    }\n}\n\
         pub fn digest_snapshot(store: &dyn SourceStore, snapshot: &SnapshotId, \
         leaves: &[SourcePath]) -> Result<String> {\n    unimplemented!()\n}",
    );
    let signature =
        top_level_public_fn_signature(&stripped, "digest_snapshot").expect("signature extracted");
    assert!(
        references_token(&signature, "store")
            && references_token(&signature, "snapshot")
            && references_token(&signature, "leaves")
            && !signature.contains("unimplemented"),
        "the top-level public signature must be selected over the trait method and end before \
         its body, got: {signature}"
    );
    assert!(
        top_level_public_fn_signature(
            &strip("pub trait SourceStore { fn digest_snapshot(&self, leaves: &[SourcePath]); }"),
            "digest_snapshot"
        )
        .is_none(),
        "a SourceStore trait method must not satisfy the public free-operation scan"
    );
}
