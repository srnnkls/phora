//! Placement gate for the T011 source split. Structure is scanned as text so a
//! pending move fails as a per-test assertion, never as this binary's compile
//! error; compile-level probes name only paths resolving on both move sides.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_rel(rel: &str) -> String {
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

fn impl_blocks(stripped: &str) -> Vec<(String, String)> {
    let bytes = stripped.as_bytes();
    stripped
        .match_indices("impl")
        .filter(|&(i, hit)| {
            let before_ok =
                i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
            let after = i + hit.len();
            let after_ok = bytes
                .get(after)
                .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_');
            before_ok && after_ok
        })
        .filter_map(|(i, _)| {
            let rest = &stripped[i..];
            let open = rest.find(['{', ';'])?;
            if rest.as_bytes()[open] == b';' {
                return None;
            }
            let body = balanced_body(rest)?;
            Some((rest[..open].to_string(), body))
        })
        .collect()
}

fn has_impl_referencing(stripped: &str, name: &str) -> bool {
    impl_blocks(stripped)
        .iter()
        .any(|(header, _)| references_token(header, name))
}

fn declares_file_module(src: &str, name: &str) -> bool {
    strip(src).split(';').any(|stmt| {
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

/// `tests.rs`/`*_tests.rs` are excluded here because their `#[cfg(test)]`
/// attribute sits on the parent's `mod` declaration, invisible to `strip_cfg_test`.
fn prod_src_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    collect_rs_files(&src_dir(), "", &mut out);
    out.retain(|(rel, _)| !(rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs")));
    out
}

fn files_defining<'a>(
    files: &'a [(String, String)],
    name: &str,
    matcher: fn(&str, &str) -> bool,
) -> Vec<&'a str> {
    files
        .iter()
        .filter(|(_, content)| matcher(&scan(content), name))
        .map(|(rel, _)| rel.as_str())
        .collect()
}

fn is_thin_source_reexport_shim(content: &str) -> bool {
    let scanned = scan(content);
    let has_definitions = [
        "fn",
        "struct",
        "enum",
        "trait",
        "impl",
        "mod",
        "macro_rules",
        "type",
        "const",
        "static",
    ]
    .iter()
    .any(|kw| references_token(&scanned, kw));
    !has_definitions && scanned.contains("pub use") && scanned.contains("crate::source::")
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

fn test_fn_names(src: &str) -> BTreeSet<String> {
    let s = strip(src);
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

fn assert_destination_defines(rel: &str, fns: &[&str], types: &[&str], what: &str) {
    let scanned = scan(&read_rel(rel));
    let mut missing: Vec<String> = fns
        .iter()
        .filter(|name| !defines_fn(&scanned, name))
        .map(|name| format!("fn {name}"))
        .collect();
    missing.extend(
        types
            .iter()
            .filter(|name| !defines_type(&scanned, name))
            .map(|name| format!("struct/enum {name}")),
    );
    assert!(
        missing.is_empty(),
        "src/{rel} must DEFINE {what} (T011 places each origin cluster in exactly one \
         destination file); absent: {missing:?}"
    );
}

const DEST_MODULES: &[&str] = &[
    "archive", "cache", "git", "http", "import", "model", "router", "worktree",
];

const HTTP_FNS: &[&str] = &["download", "verify_digest", "remove_dot_segments"];

const ARCHIVE_FNS: &[&str] = &["safe_archive_path", "extract", "strip_single_top_level"];
const ARCHIVE_TYPES: &[&str] = &["EntryKind", "ExtractedEntry"];

const GIT_FNS: &[&str] = &[
    "shallow_ref_name",
    "resolve_in",
    "tree_blobs",
    "read_blob_at",
];

const CACHE_FNS: &[&str] = &[
    "mirror_path",
    "mirror_lock_path",
    "lock_mirror",
    "sweep_orphan_staging",
    "staging_is_recent",
    "open_mirror",
    "fetch_into_mirror",
    "reclone_mirror",
];

const IMPORT_FNS: &[&str] = &[
    "to_gix_entry_kind",
    "import_tree",
    "write_import",
    "write_import_tree",
];
const IMPORT_TYPES: &[&str] = &["HttpBackend", "TempDownload", "ImportDir", "ImportNode"];

const WORKTREE_FNS: &[&str] = &["read_local_head", "is_local_path"];

const MODEL_TYPES: &[&str] = &[
    "SourcePath",
    "SourceEntryKind",
    "SourceEntryMeta",
    "SourceInventory",
];

/// `mirror_path`/`open_mirror` are excluded: `GitBackend` carries same-named
/// inherent methods that legitimately land in a second file (`source/git.rs`).
const UNIQUE_FN_ANCHORS: &[&str] = &[
    "download",
    "verify_digest",
    "remove_dot_segments",
    "safe_archive_path",
    "extract",
    "strip_single_top_level",
    "shallow_ref_name",
    "resolve_in",
    "tree_blobs",
    "read_blob_at",
    "mirror_lock_path",
    "lock_mirror",
    "sweep_orphan_staging",
    "staging_is_recent",
    "fetch_into_mirror",
    "reclone_mirror",
    "to_gix_entry_kind",
    "import_tree",
    "write_import",
    "write_import_tree",
    "read_local_head",
    "is_local_path",
];

const UNIQUE_TYPE_ANCHORS: &[&str] = &[
    "EntryKind",
    "ExtractedEntry",
    "RouterBackend",
    "GitBackend",
    "HttpBackend",
    "MirrorStaging",
    "TempDownload",
    "ImportDir",
    "ImportNode",
    "SourcePath",
    "SourceEntryKind",
    "SourceEntryMeta",
    "SourceInventory",
];

const IMPL_HOMES: &[(&str, &str)] = &[
    ("RouterBackend", "source/router.rs"),
    ("GitBackend", "source/git.rs"),
    ("HttpBackend", "source/import.rs"),
    ("MirrorStaging", "source/cache.rs"),
    ("TempDownload", "source/import.rs"),
    ("ImportDir", "source/import.rs"),
    ("SourcePath", "source/model.rs"),
    ("SourceInventory", "source/model.rs"),
];

const ORIGIN_UNIT_TEST_FLOOR: usize = 125;

#[test]
fn source_module_root_exists_and_is_registered_in_lib_rs() {
    assert!(
        src_dir().join("source/mod.rs").is_file(),
        "src/source/mod.rs must exist — T011 turns the source.rs monolith into the src/source/ \
         directory module"
    );
    assert!(
        declares_file_module(&read_rel("lib.rs"), "source"),
        "src/lib.rs must keep registering the source module with an item-level `pub mod source;` \
         declaration"
    );
}

#[test]
fn source_mod_declares_the_eight_destination_submodules() {
    let mod_rs = read_rel("source/mod.rs");
    for module in DEST_MODULES {
        let rel = format!("source/{module}.rs");
        assert!(
            src_dir().join(&rel).is_file(),
            "src/{rel} must exist — the T011 destination set is exactly http/archive/router \
             (whole-file moves) plus git/cache/import/worktree/model (carved from source.rs)"
        );
        assert!(
            declares_file_module(&mod_rs, module),
            "src/source/mod.rs must declare `mod {module};` so src/{rel} is a live file-backed \
             module, not an orphaned placeholder file"
        );
    }
}

#[test]
fn monolithic_source_rs_is_gone() {
    assert!(
        !src_dir().join("source.rs").exists(),
        "src/source.rs must no longer exist — its clusters moved into src/source/ and the module \
         root is src/source/mod.rs (a lingering monolith means the carve was copied, not \
         relocated)"
    );
}

#[test]
fn top_level_origin_files_and_compatibility_shims_are_gone() {
    let lib = read_rel("lib.rs");
    for name in ["archive", "backend", "http"] {
        let rel = format!("{name}.rs");
        let declared = declares_file_module(&lib, name);
        let exists = src_dir().join(&rel).is_file();
        assert!(
            !declared && !exists,
            "T030 must remove the top-level `{name}` compatibility module and src/{rel}; \
             currently declared={declared}, file-exists={exists}"
        );
    }
}

#[test]
fn http_destination_holds_the_download_client() {
    assert_destination_defines(
        "source/http.rs",
        HTTP_FNS,
        &[],
        "the HTTP download client moved whole-file from src/http.rs (download/verify_digest and \
         the RFC 3986 redirect helpers)",
    );
}

#[test]
fn http_destination_never_absorbs_the_synthetic_import_cluster() {
    let scanned = scan(&read_rel("source/http.rs"));
    let mut leaked: Vec<String> = IMPORT_TYPES
        .iter()
        .filter(|name| defines_type(&scanned, name))
        .map(|name| format!("struct/enum {name}"))
        .collect();
    leaked.extend(
        IMPORT_FNS
            .iter()
            .filter(|name| defines_fn(&scanned, name))
            .map(|name| format!("fn {name}")),
    );
    if has_impl_referencing(&scanned, "HttpBackend") {
        leaked.push("impl … HttpBackend".to_owned());
    }
    assert!(
        leaked.is_empty(),
        "src/source/http.rs has exactly ONE origin (the old top-level src/http.rs); the \
         synthetic-import cluster carved from source.rs (HttpBackend, TempDownload, ImportDir, \
         import_tree, …) belongs in src/source/import.rs (codex-R5-T011). Found here: {leaked:?}"
    );
}

#[test]
fn archive_destination_holds_the_extraction_cluster() {
    assert_destination_defines(
        "source/archive.rs",
        ARCHIVE_FNS,
        ARCHIVE_TYPES,
        "the archive extraction cluster moved whole-file from src/archive.rs (safe_archive_path, \
         extract, strip_single_top_level, EntryKind, ExtractedEntry)",
    );
}

#[test]
fn router_destination_holds_the_router_backend() {
    assert_destination_defines(
        "source/router.rs",
        &[],
        &["RouterBackend"],
        "the git/url routing backend moved whole-file from src/backend.rs",
    );
    assert!(
        impl_blocks(&scan(&read_rel("source/router.rs")))
            .iter()
            .any(|(header, _)| references_token(header, "SourceStore")
                && references_token(header, "RouterBackend")),
        "src/source/router.rs must carry the final `impl SourceStore for RouterBackend<…>` \
         alongside the router type"
    );
}

#[test]
fn git_destination_holds_the_git_adapter() {
    assert_destination_defines(
        "source/git.rs",
        GIT_FNS,
        &["GitBackend"],
        "the GitBackend adapter and its mirror-reading helpers carved from source.rs \
         (shallow_ref_name, resolve_in, tree_blobs, read_blob_at)",
    );
    assert!(
        impl_blocks(&scan(&read_rel("source/git.rs")))
            .iter()
            .any(|(header, _)| references_token(header, "SourceStore")
                && references_token(header, "GitBackend")),
        "src/source/git.rs must carry the final `impl SourceStore for GitBackend` block \
         alongside the Git adapter"
    );
}

#[test]
fn cache_destination_holds_the_mirror_cache_lifecycle() {
    assert_destination_defines(
        "source/cache.rs",
        CACHE_FNS,
        &["MirrorStaging"],
        "the mirror-cache lifecycle carved from source.rs (mirror path/lock helpers, orphan \
         staging sweep, open/fetch/reclone, MirrorStaging)",
    );
    assert!(
        has_impl_referencing(&scan(&read_rel("source/cache.rs")), "MirrorStaging"),
        "src/source/cache.rs must carry MirrorStaging's impl blocks (create/commit_to and the \
         Drop cleanup) alongside the type"
    );
}

#[test]
fn import_destination_holds_the_synthetic_import_cluster() {
    assert_destination_defines(
        "source/import.rs",
        IMPORT_FNS,
        IMPORT_TYPES,
        "the HTTP synthetic-import cluster carved from source.rs (HttpBackend + TempDownload and \
         the import_tree/write_import machinery) — NOT source/http.rs",
    );
    let scanned = scan(&read_rel("source/import.rs"));
    assert!(
        impl_blocks(&scanned)
            .iter()
            .any(|(header, _)| references_token(header, "SourceStore")
                && references_token(header, "HttpBackend")),
        "src/source/import.rs must carry the final `impl SourceStore for HttpBackend` block \
         alongside the HTTP adapter"
    );
    assert!(
        has_impl_referencing(&scanned, "TempDownload"),
        "src/source/import.rs must carry TempDownload's impl blocks (create and the Drop \
         cleanup) alongside the type"
    );
}

#[test]
fn worktree_destination_holds_the_local_path_helpers() {
    assert_destination_defines(
        "source/worktree.rs",
        WORKTREE_FNS,
        &[],
        "the local working-tree helpers carved from source.rs (read_local_head, is_local_path)",
    );
}

#[test]
fn model_destination_holds_the_source_value_types_with_their_impls() {
    assert_destination_defines(
        "source/model.rs",
        &[],
        MODEL_TYPES,
        "the T008 pure source value types (SourcePath, SourceEntryKind, SourceEntryMeta, \
         SourceInventory)",
    );
    let scanned = scan(&read_rel("source/model.rs"));
    for (type_name, trait_name) in [
        ("SourcePath", "FromStr"),
        ("SourcePath", "Display"),
        ("SourceInventory", ""),
        ("SourcePath", ""),
    ] {
        let found = impl_blocks(&scanned).iter().any(|(header, _)| {
            references_token(header, type_name)
                && (trait_name.is_empty() || references_token(header, trait_name))
        });
        let wanted = if trait_name.is_empty() {
            format!("an impl block for {type_name}")
        } else {
            format!("the `impl {trait_name} for {type_name}` block")
        };
        assert!(
            found,
            "src/source/model.rs must carry {wanted} — the value types move with their \
             constructors and trait impls, not as bare declarations"
        );
    }
}

#[test]
fn moved_anchors_are_defined_exactly_once_across_production_sources() {
    let files = prod_src_files();
    let mut duplicated: Vec<String> = Vec::new();
    for name in UNIQUE_FN_ANCHORS {
        let sites = files_defining(&files, name, defines_fn);
        if sites.len() > 1 {
            duplicated.push(format!("fn {name} in {sites:?}"));
        }
    }
    for name in UNIQUE_TYPE_ANCHORS {
        let sites = files_defining(&files, name, defines_type);
        if sites.len() > 1 {
            duplicated.push(format!("struct/enum {name} in {sites:?}"));
        }
    }
    assert!(
        duplicated.is_empty(),
        "every T011 anchor must have exactly one production definition site — a second site \
         means the move was copied, not relocated (compat `pub use` re-exports are fine, \
         definitions are not): {duplicated:?}"
    );
}

#[test]
fn adapter_impl_blocks_live_only_in_their_destination_files() {
    let files = prod_src_files();
    let mut misplaced: Vec<String> = Vec::new();
    for (rel, content) in &files {
        let scanned = scan(content);
        for (name, home) in IMPL_HOMES {
            if rel != home && has_impl_referencing(&scanned, name) {
                misplaced.push(format!("src/{rel}: impl … {name} (belongs in src/{home})"));
            }
        }
    }
    assert!(
        misplaced.is_empty(),
        "every production impl block of a moved type must live in that type's T011 destination \
         file — an impl left behind (or copied elsewhere) means the carve duplicated code \
         instead of relocating it: {misplaced:?}"
    );
}

#[test]
fn source_store_is_the_only_live_source_capability() {
    let files = prod_src_files();
    let source_backend_sites: Vec<&str> = files
        .iter()
        .filter(|(_, content)| references_token(&scan(content), "SourceBackend"))
        .map(|(rel, _)| rel.as_str())
        .collect();
    assert!(
        source_backend_sites.is_empty(),
        "T030 must remove every live production SourceBackend trait/import/bound/impl/call; \
         found in: {source_backend_sites:?}"
    );

    let source_store_sites: Vec<&str> = files
        .iter()
        .filter(|(_, content)| keyword_names(&scan(content), "pub trait", "SourceStore"))
        .map(|(rel, _)| rel.as_str())
        .collect();
    assert_eq!(
        source_store_sites,
        ["source/snapshot.rs"],
        "SourceStore must remain the one live source capability, defined in \
         src/source/snapshot.rs"
    );

    let source_root = scan(&read_rel("source/mod.rs"));
    let worktree_resolver = scan(&read_rel("source/resolve.rs"));
    let mut alternate_resolution_paths = Vec::new();
    if source_root.split(';').any(|statement| {
        statement.contains("pub use") && references_token(statement, "resolve_worktree")
    }) {
        alternate_resolution_paths.push("src/source/mod.rs publicly re-exports resolve_worktree");
    }
    if keyword_names(&worktree_resolver, "pub fn", "resolve_worktree") {
        alternate_resolution_paths.push("src/source/resolve.rs defines pub fn resolve_worktree");
    }
    assert!(
        alternate_resolution_paths.is_empty(),
        "SourceStore::resolve must be the sole public direct source-resolution capability; \
         resolve_worktree may remain only as a non-public backend helper: \
         {alternate_resolution_paths:?}"
    );
}

#[test]
fn source_module_tree_retains_the_origin_unit_tests() {
    let mut files = Vec::new();
    collect_rs_files(&src_dir().join("source"), "source", &mut files);
    let mut names: BTreeSet<String> = BTreeSet::new();
    for (_, content) in &files {
        names.extend(test_fn_names(content));
    }
    assert!(
        names.len() >= ORIGIN_UNIT_TEST_FLOOR,
        "the src/source/ tree must retain at least the {ORIGIN_UNIT_TEST_FLOOR} audited \
         survivor unit tests (the T011 carve relocated 158; T016 deletes/ports the 33 \
         export-driven tests WITH their machinery — 32 port to src/sync/stage.rs by name, \
         pinned in stage_deletion_gate.rs — leaving 125 source-owned survivors; any lower \
         count means source coverage was net-deleted, not migrated); found {} distinct test fns",
        names.len()
    );
}

#[test]
fn source_value_types_keep_resolving_at_their_source_paths() {
    use phora::source::{SourceEntryKind, SourceEntryMeta, SourceInventory, SourcePath};

    let inventory = SourceInventory::from_paths(["b/two", "a/one"]).expect("safe paths");
    let paths: Vec<&str> = inventory
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(
        paths,
        ["a/one", "b/two"],
        "source::SourceInventory must keep resolving and ordering entries ascending by path \
         (source/mod.rs must re-export the T008 value types so PR3 projection imports stay valid)"
    );
    let meta = SourceEntryMeta {
        path: SourcePath::new("a/one").expect("safe path"),
        kind: SourceEntryKind::File,
    };
    assert_eq!(
        inventory.entries.first(),
        Some(&meta),
        "source::{{SourcePath, SourceEntryMeta, SourceEntryKind}} must keep resolving with \
         their pre-move semantics"
    );
}

#[test]
fn source_port_surface_keeps_resolving_with_only_required_methods_implemented() {
    use std::collections::BTreeMap;

    use phora::source::{
        GitBackend, HttpBackend, MirrorKey, NormalizedUrl, ResolvedSource, RouterBackend,
        SnapshotId, SourceEntry, SourceStore,
    };

    fn pin_surface(
        _store: Option<&dyn SourceStore>,
        _resolved: Option<ResolvedSource>,
        _snapshot: Option<SnapshotId>,
        _entry: Option<SourceEntry>,
        _router: Option<RouterBackend<GitBackend, HttpBackend>>,
    ) {
    }

    let key = MirrorKey::from_url(&NormalizedUrl::parse("https://Example.com/Owner/repo.git"));
    assert_eq!(
        key.as_str().len(),
        16,
        "source::{{NormalizedUrl, MirrorKey}} must keep resolving with the 16-hex mirror key"
    );
    assert!(
        phora::source::is_local_path("/"),
        "source::is_local_path must keep resolving and treating an absolute path as local"
    );
    assert!(
        phora::source::vars_digest(&BTreeMap::new()).starts_with("blake3:"),
        "source::vars_digest must keep resolving with its blake3-prefixed digest"
    );
    let non_repo = tempfile::TempDir::new().expect("temp dir");
    let head = phora::source::read_local_head(non_repo.path().to_str().expect("utf-8 temp path"))
        .expect("a plain directory must not error");
    assert_eq!(
        head, "link",
        "source::read_local_head must keep resolving and yielding the `link` sentinel for a \
         non-repo directory"
    );
    pin_surface(None, None, None, None, None);
}

#[test]
fn helper_shim_classifier_requires_reexports_and_rejects_definitions() {
    assert!(
        is_thin_source_reexport_shim(
            "//! Compat shim; removed by T030.\npub use crate::source::http::{download, \
             verify_digest};\n"
        ),
        "a pure `pub use crate::source::…` file must classify as a thin shim"
    );
    assert!(
        !is_thin_source_reexport_shim(
            "pub use crate::source::http::download;\npub fn verify_digest() {}\n"
        ),
        "a leftover fn definition must disqualify the shim"
    );
    assert!(
        !is_thin_source_reexport_shim("pub use crate::source::archive::EntryKind;\nstruct S;\n"),
        "a leftover type definition must disqualify the shim"
    );
    assert!(
        !is_thin_source_reexport_shim("pub use crate::config::Config;\n"),
        "a re-export that does not target crate::source must not classify as the T011 shim"
    );
    assert!(
        !is_thin_source_reexport_shim(""),
        "an empty (or unreadable) file must not classify as a shim"
    );
}

#[test]
fn helper_trait_method_extractor_reads_only_the_trait_block() {
    let src = "pub trait SourceBackend {\n    fn fetch(&self) -> bool;\n    fn mirror_ready(&self) \
               -> bool {\n        false\n    }\n}\npub trait Other {\n    fn stray(&self);\n}\n";
    let got = trait_method_names(&scan(src), "SourceBackend").expect("trait body extracted");
    let want: BTreeSet<String> = ["fetch", "mirror_ready"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        got, want,
        "required and default-bodied methods must be collected; a following trait's methods \
         must not leak into the set"
    );
    assert!(
        trait_method_names(
            &scan("// pub trait SourceBackend { fn x(&self); }"),
            "SourceBackend"
        )
        .is_none(),
        "a commented-out trait must not yield a body"
    );
}

#[test]
fn helper_definition_matchers_reject_comments_strings_calls_and_impls() {
    assert!(defines_fn(
        &scan("pub(crate) fn import_tree() {}"),
        "import_tree"
    ));
    assert!(
        !defines_fn(
            &scan("let c = import_tree(dir, url, &entries);"),
            "import_tree"
        ),
        "a call site must not count as a definition"
    );
    assert!(
        !defines_fn(&scan("fn import_tree_at() {}"), "import_tree"),
        "a longer-named sibling must not satisfy the exact fn name"
    );
    assert!(
        !defines_fn(&scan("// fn import_tree() {}"), "import_tree"),
        "a commented-out definition must not count"
    );
    assert!(defines_type(
        &scan("struct MirrorStaging { path: PathBuf }"),
        "MirrorStaging"
    ));
    assert!(
        !defines_type(
            &scan("const S: &str = \"struct MirrorStaging;\";"),
            "MirrorStaging"
        ),
        "a definition inside a string literal must not count"
    );
    assert!(
        !defines_type(
            &scan("impl MirrorStaging { fn create() {} }"),
            "MirrorStaging"
        ),
        "an impl block is not a type definition"
    );
    assert!(
        !defines_fn(
            &scan("#[cfg(test)]\nmod tests {\n    fn import_tree() {}\n}"),
            "import_tree"
        ),
        "a definition inside a #[cfg(test)] module must not count as a production site"
    );
}

#[test]
fn helper_test_name_extractor_ignores_strings_and_unattributed_fns() {
    assert!(
        !test_fn_names("fn plain_helper() {}").contains("plain_helper"),
        "a fn with no #[test] attribute must not count"
    );
    assert!(
        !test_fn_names("const S: &str = \"#[test]\nfn faked() {}\";").contains("faked"),
        "a #[test] fn inside a string literal must not count"
    );
    assert!(
        test_fn_names("#[test]\n#[cfg(unix)]\nfn real_case() {}").contains("real_case"),
        "a genuine #[test] fn behind further attributes must be counted"
    );
}
