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

fn keyword_names(stripped: &str, keyword: &str, name: &str) -> bool {
    let bytes = stripped.as_bytes();
    stripped.match_indices(keyword).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let rest_all = &stripped[i + keyword.len()..];
        let rest = rest_all.trim_start();
        before_ok
            && rest_all.len() > rest.len()
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

fn defines_pub_type(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "pub struct", name) || keyword_names(stripped, "pub enum", name)
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

fn fn_signatures(stripped: &str) -> Vec<String> {
    let bytes = stripped.as_bytes();
    stripped
        .match_indices("fn")
        .filter_map(|(i, _)| {
            let before_ok =
                i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
            let after = i + "fn".len();
            let after_ok = bytes
                .get(after)
                .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_');
            if !(before_ok && after_ok) {
                return None;
            }
            let end = stripped[i..].find(['{', ';']).map(|j| i + j)?;
            let start = stripped[..i].rfind([';', '}', '{']).map_or(0, |j| j + 1);
            Some(stripped[start..end].to_owned())
        })
        .collect()
}

fn item_body(stripped: &str, keywords: &[&str], name: &str) -> Option<String> {
    keywords
        .iter()
        .find_map(|keyword| {
            let needle = format!("{keyword} {name}");
            stripped.match_indices(&needle).find_map(|(i, _)| {
                let after = &stripped[i + needle.len()..];
                after
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                    .then_some(i)
            })
        })
        .and_then(|start| balanced_body(&stripped[start..]))
}

fn enum_body(stripped: &str, name: &str) -> Option<String> {
    item_body(stripped, &["pub enum", "enum"], name)
}

fn variant_ident(part: &str) -> Option<String> {
    let mut t = part.trim_start();
    while t.starts_with('#') {
        let open = t.find('[')?;
        let bytes = t.as_bytes();
        let mut depth = 0i32;
        let mut close = None;
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        t = t[close? + 1..].trim_start();
    }
    let name: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn enum_variant_names(body: &str) -> Vec<String> {
    let inner = &body[1..body.len().saturating_sub(1)];
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '{' | '(' | '[' => {
                depth += 1;
                buf.push(c);
            }
            '}' | ')' | ']' => {
                depth -= 1;
                buf.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut buf)),
            _ => buf.push(c),
        }
    }
    parts.push(buf);
    parts
        .iter()
        .map(String::as_str)
        .filter_map(variant_ident)
        .collect()
}

fn expand_tree(tree: &str) -> Vec<String> {
    let Some(open) = tree.find('{') else {
        return vec![tree.trim().to_owned()];
    };
    let bytes = tree.as_bytes();
    let mut depth = 0i32;
    let mut close = None;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![tree.trim().to_owned()];
    };
    let prefix = &tree[..open];
    let inner = &tree[open + 1..close];
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut d = 0i32;
    for c in inner.chars() {
        match c {
            '{' => {
                d += 1;
                buf.push(c);
            }
            '}' => {
                d -= 1;
                buf.push(c);
            }
            ',' if d == 0 => parts.push(std::mem::take(&mut buf)),
            _ => buf.push(c),
        }
    }
    parts.push(buf);
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .flat_map(|p| expand_tree(&format!("{prefix}{p}")))
        .collect()
}

fn use_leaves(scanned: &str) -> Vec<String> {
    let bytes = scanned.as_bytes();
    let mut leaves = Vec::new();
    for (i, _) in scanned.match_indices("use") {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after_ws = bytes.get(i + 3).is_some_and(|&b| b.is_ascii_whitespace());
        if !(before_ok && after_ws) {
            continue;
        }
        let Some(end) = scanned[i + 3..].find(';') else {
            continue;
        };
        for leaf in expand_tree(scanned[i + 3..i + 3 + end].trim()) {
            let leaf = leaf.split(" as ").next().unwrap_or(&leaf).replace(' ', "");
            if !leaf.is_empty() {
                leaves.push(leaf);
            }
        }
    }
    leaves
}

fn collapse_colon_ws(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':' && chars.get(i + 1) == Some(&':') {
            while out.ends_with(char::is_whitespace) {
                out.pop();
            }
            out.push_str("::");
            i += 2;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_path_needle(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let starts_ident = needle
        .as_bytes()
        .first()
        .copied()
        .is_some_and(is_ident_byte);
    let ends_ident = needle.as_bytes().last().copied().is_some_and(is_ident_byte);
    haystack.match_indices(needle).any(|(i, _)| {
        let before_ok = !starts_ident || i == 0 || !is_ident_byte(bytes[i - 1]);
        let after_ok = !ends_ident
            || bytes
                .get(i + needle.len())
                .copied()
                .is_none_or(|b| !is_ident_byte(b));
        before_ok && after_ok
    })
}

fn forbidden_crate_head(haystack: &str, leaves: &[String], token: &str) -> bool {
    let bytes = haystack.as_bytes();
    let path_head = haystack.match_indices(token).any(|(i, _)| {
        let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        let not_segment = i < 2 || &haystack[i - 2..i] != "::";
        before_ok && not_segment && haystack[i + token.len()..].starts_with("::")
    });
    path_head
        || leaves.iter().any(|leaf| {
            let head = leaf.split_whitespace().next().unwrap_or_default();
            head == token
                || head
                    .strip_prefix(token)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
}

fn derive_window_before(stripped: &str, needle: &str) -> Option<String> {
    let i = stripped.find(needle)?;
    let start = stripped[..i].rfind([';', '}']).map_or(0, |j| j + 1);
    Some(stripped[start..i].to_owned())
}

fn sig_of_fn(stripped: &str, name: &str) -> Option<String> {
    fn_signatures(stripped)
        .into_iter()
        .find(|sig| keyword_names(sig, "fn", name))
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

struct ApiPin<'a> {
    name: &'a str,
    param_compact: &'a str,
    ret_token: &'a str,
    ret_compact: Option<&'a str>,
}

fn api_pin_satisfied(stripped: &str, pin: &ApiPin<'_>) -> bool {
    let Some(sig) = sig_of_fn(stripped, pin.name) else {
        return false;
    };
    if !references_token(&sig, "pub") {
        return false;
    }
    let Some((params, ret)) = sig.split_once("->") else {
        return false;
    };
    compact(params).contains(pin.param_compact)
        && references_token(ret, pin.ret_token)
        && pin
            .ret_compact
            .is_none_or(|want| compact(ret).contains(want))
}

fn cargo_dep_names() -> Vec<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = fs::read_to_string(manifest).unwrap_or_default();
    let mut in_deps = false;
    let mut names = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]"
                || trimmed == "[dev-dependencies]"
                || (trimmed.starts_with("[target.") && trimmed.ends_with(".dependencies]"));
            continue;
        }
        if !in_deps {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            let name = name.trim().trim_matches('"');
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || "-_".contains(c))
            {
                names.push(name.replace('-', "_"));
            }
        }
    }
    names
}

const INSPECT: &str = "sync/inspect.rs";
const SCAN: &str = "sync/scan.rs";
const RECONCILE: &str = "sync/reconcile.rs";
const MODEL: &str = "sync/model.rs";
const DEPLOY: &str = "deploy.rs";
const STORE: &str = "store.rs";
const SYNC_MOD: &str = "sync/mod.rs";

// The exact drift-detection cluster relocated verbatim out of deploy.rs (commit 1).
const CLUSTER_FNS: &[&str] = &[
    "check_artifact_state",
    "check_file_artifact_state",
    "artifact_record",
    "managed_under_sibling_shape",
    "classify_drift",
    "revalidate_file",
];

// Shared filesystem scan helpers relocated into sync/scan.rs (commit 1).
const SCAN_FNS: &[&str] = &["scan_dir", "scan_dir_soft", "scan_dir_strict", "mtime_secs"];
const SCAN_TYPES: &[&str] = &["ScanResult", "ScanMode"];

// Pure reconcile-facing value types that must land in sync/model.rs.
const MODEL_CHANGE_TYPES: &[&str] = &["SyncChange", "ChangeSet", "RemovalReason"];

const RECON_FORBIDDEN_SUBSTR: &[&str] = &[
    "crate::store",
    "crate::deploy",
    "crate::source",
    "crate::config",
    "crate::error",
    "store::",
    "deploy::",
    "std::fs",
    "std::io",
    "std::os",
    "std::net",
    "std::process",
    "::fs::",
    "OpenOptions",
    "File::create",
    "File::open",
];

const RECON_FORBIDDEN_TOKENS: &[&str] =
    &["gix", "chrono", "ureq", "walkdir", "tar", "zip", "flate2"];

const RECON_IO_SIBLINGS: &[&str] = &[
    "state",
    "discover",
    "resolve",
    "preview",
    "hooks",
    "plan",
    "rebuild",
    "stage",
    "verify",
    "transitive",
    "confine",
    "target",
    "prune",
    "inspect",
    "scan",
];

fn reconcile_impurities(recon_src: &str) -> Vec<String> {
    let scanned = collapse_colon_ws(&scan(recon_src));
    let leaves = use_leaves(&scanned);
    let haystack = format!("{scanned}\n{}", leaves.join("\n"));
    let mut hits: Vec<String> = RECON_FORBIDDEN_SUBSTR
        .iter()
        .filter(|needle| contains_path_needle(&haystack, needle))
        .map(|needle| (*needle).to_owned())
        .collect();
    for sibling in RECON_IO_SIBLINGS {
        for prefix in ["sync::", "super::"] {
            let needle = format!("{prefix}{sibling}");
            if contains_path_needle(&haystack, &needle) {
                hits.push(needle);
            }
        }
    }
    hits.extend(
        RECON_FORBIDDEN_TOKENS
            .iter()
            .filter(|token| forbidden_crate_head(&haystack, &leaves, token))
            .map(|token| (*token).to_owned()),
    );
    hits.extend(
        cargo_dep_names()
            .into_iter()
            .filter(|dep| forbidden_crate_head(&haystack, &leaves, dep)),
    );
    hits.sort();
    hits.dedup();
    hits
}

#[test]
fn sync_registers_inspect_scan_and_reconcile_modules() {
    let mod_rs = scan(&read_src(SYNC_MOD));
    assert!(
        keyword_names(&mod_rs, "mod", "scan"),
        "src/{SYNC_MOD} must declare `mod scan` — the shared filesystem scan helpers get their \
         own sync module; an unregistered file never compiles and pins nothing"
    );
    for module in ["inspect", "reconcile"] {
        assert!(
            keyword_names(&mod_rs, "pub mod", module),
            "src/{SYNC_MOD} must declare `pub mod {module}` — the behavioral contracts import \
             phora::sync::{module}::* from outside the crate, so the module path must resolve \
             publicly, not just crate-visibly"
        );
    }
}

#[test]
fn check_artifact_state_cluster_lives_in_inspect() {
    let scanned = scan(&read_src(INSPECT));
    let missing: Vec<&&str> = CLUSTER_FNS
        .iter()
        .filter(|name| !defines_fn(&scanned, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{INSPECT} must DEFINE the whole drift-detection cluster T018 relocates out of \
         deploy.rs — check_artifact_state plus its private helpers check_file_artifact_state, \
         artifact_record, managed_under_sibling_shape, classify_drift, and the TOCTOU-safe \
         revalidate_file; missing definitions: {missing:?}"
    );
    assert!(
        defines_pub_type(&scanned, "ArtifactState"),
        "src/{INSPECT} must define `pub enum ArtifactState` — the drift classification moves \
         WITH check_artifact_state (a `use`/`pub use` re-export does not count); it survives \
         T018 because cli/render.rs and cli/query.rs still read it via crate::deploy until \
         their T030 migration"
    );
}

#[test]
fn check_artifact_state_cluster_is_gone_from_deploy() {
    let stripped = strip(&read_src(DEPLOY));
    let leftover: Vec<&&str> = CLUSTER_FNS
        .iter()
        .filter(|name| defines_fn(&stripped, name))
        .collect();
    assert!(
        leftover.is_empty(),
        "src/{DEPLOY} must no longer DEFINE the relocated drift cluster ANYWHERE — production \
         or #[cfg(test)] (a compat `pub use` re-export is fine, a second definition is a copy, \
         not a move; the cluster's tests move with it); still defined in deploy.rs: {leftover:?}"
    );
    assert!(
        !defines_pub_type(&stripped, "ArtifactState"),
        "src/{DEPLOY} must no longer DEFINE `pub enum ArtifactState` after the move — only \
         re-export it from sync::inspect"
    );
}

#[test]
fn deploy_re_exports_check_artifact_state_for_legacy_callers() {
    let deploy_src = read_src(DEPLOY);
    let leaves = use_leaves(&collapse_colon_ws(&scan(&deploy_src)));
    let defines = defines_fn(&strip(&deploy_src), "check_artifact_state");
    assert!(
        !defines
            && leaves
                .iter()
                .any(|leaf| leaf.ends_with("inspect::check_artifact_state")),
        "src/{DEPLOY} must carry a compat re-export `pub use crate::sync::inspect::\
         check_artifact_state` (REQUIRED, not transitional): cli/query.rs reaches it through \
         crate::deploy until T030 and sync/tests.rs binds crate::deploy::check_artifact_state, \
         so the deploy-path alias must stay live; use-leaves seen: {leaves:?}"
    );
    assert!(
        leaves
            .iter()
            .any(|leaf| leaf.ends_with("inspect::ArtifactState")),
        "src/{DEPLOY} must ALSO re-export `crate::sync::inspect::ArtifactState` — cli/render.rs, \
         cli/mod.rs, and sync/target.rs import crate::deploy::ArtifactState until T030, so the \
         missing re-export must fail here structurally, not as a late compile error; use-leaves \
         seen: {leaves:?}"
    );
}

const INSPECT_PIN: ApiPin<'static> = ApiPin {
    name: "inspect",
    param_compact: "&dynStateStore",
    ret_token: "ObservedArtifact",
    ret_compact: None,
};

const RECONCILE_PIN: ApiPin<'static> = ApiPin {
    name: "reconcile",
    param_compact: "&ReconciliationPolicy",
    ret_token: "SyncError",
    ret_compact: Some("Result<ChangeSet,SyncError>"),
};

#[test]
fn inspect_exposes_the_state_store_to_observed_artifact_entry() {
    let scanned = scan(&read_src(INSPECT));
    assert!(
        api_pin_satisfied(&scanned, &INSPECT_PIN),
        "src/{INSPECT} must define the design §7.3 observation entry: `pub fn inspect` taking \
         `&dyn StateStore` as a parameter and yielding ObservedArtifact in RETURN position \
         (after `->`) — a decoy fn naming both types only in its parameters, a private fn, or \
         a differently-named fn does not satisfy the pin; signatures found: {:?}",
        fn_signatures(&scanned)
    );
}

#[test]
fn scan_helpers_live_in_scan_module() {
    let scanned = scan(&read_src(SCAN));
    let missing_fns: Vec<&&str> = SCAN_FNS
        .iter()
        .filter(|name| !defines_fn(&scanned, name))
        .collect();
    assert!(
        missing_fns.is_empty(),
        "src/{SCAN} must define the shared filesystem scan helpers relocated from deploy.rs — \
         scan_dir, scan_dir_soft, scan_dir_strict, and mtime_secs; missing: {missing_fns:?}"
    );
    let missing_types: Vec<&&str> = SCAN_TYPES
        .iter()
        .filter(|name| !defines_type(&scanned, name))
        .collect();
    assert!(
        missing_types.is_empty(),
        "src/{SCAN} must define the scan-result carrier ScanResult and the ScanMode discriminant \
         alongside the helpers; missing: {missing_types:?}"
    );
}

#[test]
fn scan_helpers_are_gone_from_deploy() {
    let stripped = strip(&read_src(DEPLOY));
    let leftover: Vec<String> = SCAN_FNS
        .iter()
        .filter(|name| defines_fn(&stripped, name))
        .chain(
            SCAN_TYPES
                .iter()
                .filter(|name| defines_type(&stripped, name)),
        )
        .map(|name| (*name).to_owned())
        .collect();
    assert!(
        leftover.is_empty(),
        "src/{DEPLOY} must no longer DEFINE the scan helpers/types ANYWHERE — production or \
         #[cfg(test)] — after the move to sync/scan.rs (copy_tree imports scan_dir_strict from \
         there); still in deploy.rs: {leftover:?}"
    );
}

#[test]
fn scanned_file_is_defined_in_model_not_store() {
    assert!(
        defines_pub_type(&scan(&read_src(MODEL)), "ScannedFile"),
        "src/{MODEL} must DEFINE `pub struct ScannedFile` (Debug+Clone, no serde) — the pure \
         stat carrier reconcile and inspect both name, relocated off store.rs so reconcile \
         imports only pure model types; a `use`/`pub use` alias does not count"
    );
    assert!(
        !defines_pub_type(&strip(&read_src(STORE)), "ScannedFile"),
        "src/{STORE} must no longer DEFINE `pub struct ScannedFile` anywhere — production or \
         #[cfg(test)] — it re-imports it from sync::model to keep compiling; a second \
         definition is a copy, not a move"
    );
}

#[test]
fn scanned_file_in_model_derives_debug_clone_and_no_serde() {
    let scanned = scan(&read_src(MODEL));
    let window = derive_window_before(&scanned, "struct ScannedFile").unwrap_or_else(|| {
        panic!(
            "src/{MODEL} does not define `struct ScannedFile` yet — the derive pin needs its \
             attribute window to scan"
        )
    });
    assert!(
        references_token(&window, "derive")
            && references_token(&window, "Debug")
            && references_token(&window, "Clone"),
        "ScannedFile in src/{MODEL} must keep its store.rs derives — #[derive(Debug, Clone)]; \
         attribute window: {window}"
    );
    assert!(
        !references_token(&window, "Serialize") && !references_token(&window, "Deserialize"),
        "ScannedFile must stay serde-free — store.rs never serialized it, so the move to \
         sync/model.rs has zero serialization impact and must not grow Serialize/Deserialize; \
         attribute window: {window}"
    );
}

#[test]
fn model_defines_the_reconcile_change_types() {
    let scanned = scan(&read_src(MODEL));
    let missing: Vec<&&str> = MODEL_CHANGE_TYPES
        .iter()
        .filter(|name| !defines_pub_type(&scanned, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{MODEL} must define the PURE reconcile change vocabulary as `pub struct`/`pub \
         enum` — the per-artifact SyncChange, the aggregate ChangeSet reconcile returns, and \
         the RemovalReason it tags prunes/moved-pin drops with (design §7.3); reconcile imports \
         only these pure model types, never sync/plan or store; missing: {missing:?}"
    );
}

#[test]
fn observed_artifact_is_the_designed_enum_with_managed_artifact() {
    let scanned = scan(&read_src(MODEL));
    let body = enum_body(&scanned, "ObservedArtifact").unwrap_or_else(|| {
        panic!(
            "src/{MODEL} must reshape ObservedArtifact into an ENUM (design §7.3) — the B16 \
             placeholder struct does not satisfy the case split Missing / Managed / Foreign / \
             Ejected; no `enum ObservedArtifact` body found"
        )
    });
    let variants = enum_variant_names(&body);
    let missing: Vec<&str> = ["Missing", "Managed", "Foreign", "Ejected"]
        .into_iter()
        .filter(|variant| !variants.iter().any(|v| v == variant))
        .collect();
    assert!(
        missing.is_empty(),
        "enum ObservedArtifact must declare the design §7.3 cases as TOP-LEVEL variants — \
         Missing, Managed(ManagedArtifact), Foreign(...), Ejected (payload shapes unpinned); \
         declared: {variants:?}; missing: {missing:?}"
    );
    assert!(
        keyword_names(&scanned, "pub struct", "ManagedArtifact"),
        "src/{MODEL} must define `pub struct ManagedArtifact` — the Managed payload carrying \
         the record and its condition (design §7.3)"
    );
    let managed = item_body(&scanned, &["pub struct", "struct"], "ManagedArtifact")
        .unwrap_or_else(|| panic!("ManagedArtifact has no body to scan in src/{MODEL}"));
    assert!(
        references_token(&managed, "record") && references_token(&managed, "condition"),
        "ManagedArtifact must carry fields named `record` and `condition` (field TYPES \
         unpinned — the model purity gate constrains what they may reference); body: {managed}"
    );
}

#[test]
fn sync_change_carries_a_conflict_variant() {
    let body = enum_body(&scan(&read_src(MODEL)), "SyncChange").unwrap_or_else(|| {
        panic!(
            "src/{MODEL} does not define `enum SyncChange` yet — reconcile emits \
             SyncChange::Conflict, so the change set needs a top-level Conflict case"
        )
    });
    let variants = enum_variant_names(&body);
    assert!(
        variants.iter().any(|v| v == "Conflict"),
        "SyncChange must declare a top-level `Conflict` variant — reconcile emits it when an \
         unforced observation collides with the desired projection (payload shape unpinned); \
         declared variants: {variants:?}"
    );
}

#[test]
fn reconciliation_policy_is_a_pure_value_carrying_force() {
    let scanned = scan(&read_src(MODEL));
    assert!(
        defines_pub_type(&scanned, "ReconciliationPolicy"),
        "src/{MODEL} must define the PURE `ReconciliationPolicy` value (design §7.3) so PR7 \
         reconcile has its full policy without importing config or PR11 SyncOptions"
    );
    let body = item_body(
        &scanned,
        &["pub struct", "struct", "pub enum", "enum"],
        "ReconciliationPolicy",
    )
    .unwrap_or_else(|| panic!("ReconciliationPolicy has no body to scan in src/{MODEL}"));
    assert!(
        references_token(&body, "force"),
        "ReconciliationPolicy must carry `force` (the matrix uses it to choose Conflict vs \
         Overwrite), alongside its prune/moved-pin/removal decisions; body: {body}"
    );
}

#[test]
fn model_defines_the_pure_sync_error() {
    assert!(
        defines_pub_type(&scan(&read_src(MODEL)), "SyncError"),
        "src/{MODEL} must define the PURE failure payload `SyncError` reconcile returns — \
         reconcile's signature is Result<ChangeSet, SyncError> and INV-8 forbids it from \
         importing crate::error, so the error type reconcile names must live in pure \
         sync::model (no separate ReconciliationError, no I/O-bearing error import)"
    );
}

#[test]
fn reconcile_fn_has_the_specced_signature() {
    let scanned = scan(&read_src(RECONCILE));
    assert!(
        api_pin_satisfied(&scanned, &RECONCILE_PIN),
        "src/{RECONCILE} must define the pure `pub fn reconcile(projection, observed, \
         &ReconciliationPolicy) -> Result<ChangeSet, SyncError>` — `&ReconciliationPolicy` as \
         a parameter and `Result<ChangeSet, SyncError>` after `->`; a decoy fn naming the \
         types only in its parameters, a private fn, or a differently-named fn does not \
         satisfy the pin; signatures found: {:?}",
        fn_signatures(&scanned)
    );
}

#[test]
fn reconcile_reaches_only_pure_dependencies() {
    let impurities = reconcile_impurities(&read_src(RECONCILE));
    assert!(
        impurities.is_empty(),
        "src/{RECONCILE} must reach only projection + sync::model + std-non-I/O (INV-8): no \
         store/deploy/source/config/error, no I/O-bearing sync sibling (state/target/stage/\
         inspect/scan/...), no std fs/io/os/net/process, no gix/chrono/ureq/walkdir/tar/zip/\
         flate2 — grouped, aliased, and fully-qualified body paths all count. Found: \
         {impurities:?}"
    );
}

#[test]
fn helper_defines_fn_and_type_are_word_bounded() {
    assert!(defines_fn("pub fn scan_dir(d: &Path) {}", "scan_dir"));
    assert!(
        !defines_fn("pub fn scan_dir_soft(d: &Path) {}", "scan_dir"),
        "scan_dir must not match the longer scan_dir_soft"
    );
    assert!(defines_fn("fn revalidate_file() {}", "revalidate_file"));
    assert!(
        !defines_fn(
            "pub use crate::sync::inspect::check_artifact_state;",
            "check_artifact_state"
        ),
        "a re-export is not a definition — the anti-copy pin depends on this"
    );
    assert!(defines_type("pub struct ScanResult { }", "ScanResult"));
    assert!(defines_type("enum ScanMode { Strict, Soft }", "ScanMode"));
    assert!(defines_pub_type(
        "pub enum ArtifactState { Clean }",
        "ArtifactState"
    ));
    assert!(
        !defines_pub_type("pub use x::ArtifactState;", "ArtifactState"),
        "a pub-use re-export must not satisfy the pub-type definition pin"
    );
}

#[test]
fn helper_use_leaves_reads_grouped_and_reexport_forms() {
    let scanned = collapse_colon_ws(&scan(
        "pub use crate::sync::inspect::{check_artifact_state, ArtifactState};\n\
         use crate::sync::scan::scan_dir_strict;\n",
    ));
    let leaves = use_leaves(&scanned);
    assert!(
        leaves
            .iter()
            .any(|l| l.ends_with("inspect::check_artifact_state")),
        "the grouped re-export leaf must expand to inspect::check_artifact_state, got: {leaves:?}"
    );
    assert!(
        leaves
            .iter()
            .any(|l| l == "crate::sync::scan::scan_dir_strict"),
        "a plain import leaf is captured whole, got: {leaves:?}"
    );
}

#[test]
fn helper_reconcile_impurity_scan_flags_bypass_classes() {
    for impure in [
        "use crate::sync::target::StageBridge;\npub fn reconcile() {}",
        "use crate::sync::inspect::inspect;\npub fn reconcile() {}",
        "use crate::sync::scan::scan_dir_soft;\npub fn reconcile() {}",
        "use crate::store::RegistryRecord;\npub fn reconcile() {}",
        "use crate::config::Config;\npub fn reconcile() {}",
        "use crate::error::Error;\npub fn reconcile() {}",
        "use std::fs;\npub fn reconcile() {}",
        "pub fn reconcile() { let _ = crate::sync::stage::stage_artifact(); }",
        "pub fn reconcile() { let _ = crate :: store :: X::default(); }",
        "use super::target::record_artifact_path;\npub fn reconcile() {}",
        "use walkdir::WalkDir;\npub fn reconcile() {}",
        "use zip as archive;\npub fn reconcile() {}",
    ] {
        assert!(
            !reconcile_impurities(impure).is_empty(),
            "reconcile impurity scan must flag: {impure:?}"
        );
    }
}

#[test]
fn helper_reconcile_impurity_scan_spares_pure_dependencies() {
    for pure in [
        "use crate::projection::Projection;\n\
         use crate::sync::model::{ChangeSet, ObservedProjectState, ReconciliationPolicy};\n\
         pub fn reconcile(_p: &Projection) -> ChangeSet { ChangeSet::default() }",
        "use super::model::SyncChange;\npub fn reconcile() {}",
        "use std::collections::BTreeMap;\npub fn reconcile() { let _ = BTreeMap::<u8, u8>::new(); }",
        "pub fn reconcile() -> crate::sync::model::ChangeSet { crate::sync::model::ChangeSet::default() }",
        "pub fn reconcile(l: &[u8], r: &[u8]) -> usize { l.iter().zip(r.iter()).count() }",
    ] {
        assert!(
            reconcile_impurities(pure).is_empty(),
            "reconcile impurity scan must spare pure code: {pure:?} -> {:?}",
            reconcile_impurities(pure)
        );
    }
}

#[test]
fn helper_absence_scan_sees_cfg_test_hidden_copies() {
    let hidden = "pub fn keep() {}\n#[cfg(test)]\nmod tests {\n    fn check_artifact_state() {}\n    \
                  pub struct ScannedFile {}\n}\n";
    let stripped = strip(hidden);
    assert!(
        defines_fn(&stripped, "check_artifact_state"),
        "the absence scan (strip only, cfg-test INTACT) must see a cluster fn hidden in a \
         #[cfg(test)] module"
    );
    assert!(
        keyword_names(&stripped, "pub struct", "ScannedFile"),
        "the absence scan must see a type definition hidden in a #[cfg(test)] module"
    );
    let scanned = scan(hidden);
    assert!(
        !defines_fn(&scanned, "check_artifact_state"),
        "the presence scan (scan = strip + cfg-test removal) still ignores test-only \
         definitions — only the ABSENCE pins tightened"
    );
}

#[test]
fn helper_api_pin_rejects_decoys_and_accepts_the_specced_shapes() {
    let real_inspect = "pub fn inspect(target: &Path, store: &dyn StateStore, key: &ArtifactKey) \
                        -> Result<ObservedArtifact, InspectError> { body }";
    assert!(
        api_pin_satisfied(&strip(real_inspect), &INSPECT_PIN),
        "the real shape — pub, &dyn StateStore parameter, ObservedArtifact after -> — satisfies \
         the inspect pin (Result-wrapped return included)"
    );
    let bare_return = "pub fn inspect(store: &dyn StateStore) -> ObservedArtifact { body }";
    assert!(
        api_pin_satisfied(&strip(bare_return), &INSPECT_PIN),
        "a bare ObservedArtifact return also satisfies the inspect pin"
    );
    let decoy_params_only =
        "pub fn inspect(store: &dyn StateStore, seed: ObservedArtifact) -> bool { body }";
    assert!(
        !api_pin_satisfied(&strip(decoy_params_only), &INSPECT_PIN),
        "a decoy naming ObservedArtifact only in its PARAMETERS must not satisfy the pin"
    );
    let private_fn = "fn inspect(store: &dyn StateStore) -> ObservedArtifact { body }";
    assert!(
        !api_pin_satisfied(&strip(private_fn), &INSPECT_PIN),
        "a non-pub inspect must not satisfy the pin"
    );
    let wrong_name = "pub fn observe(store: &dyn StateStore) -> ObservedArtifact { body }";
    assert!(
        !api_pin_satisfied(&strip(wrong_name), &INSPECT_PIN),
        "a differently-named fn must not satisfy the pin"
    );

    let real_reconcile = "pub fn reconcile(projection: &Projection, observed: \
                          &ObservedProjectState, policy: &ReconciliationPolicy) -> \
                          Result<ChangeSet, SyncError> { body }";
    assert!(
        api_pin_satisfied(&strip(real_reconcile), &RECONCILE_PIN),
        "the specced reconcile signature satisfies the pin"
    );
    let decoy_reconcile = "pub fn reconcile(policy: &ReconciliationPolicy, set: ChangeSet, \
                           err: SyncError) -> bool { body }";
    assert!(
        !api_pin_satisfied(&strip(decoy_reconcile), &RECONCILE_PIN),
        "a decoy naming ChangeSet/SyncError only in its parameters must not satisfy the pin"
    );
    let wrong_return = "pub fn reconcile(policy: &ReconciliationPolicy) -> ChangeSet { body }";
    assert!(
        !api_pin_satisfied(&strip(wrong_return), &RECONCILE_PIN),
        "a bare ChangeSet return (no Result<ChangeSet, SyncError>) must not satisfy the pin"
    );
}

#[test]
fn helper_cargo_dep_scan_bans_extern_prelude_crates_in_reconcile() {
    let deps = cargo_dep_names();
    for expected in ["blake3", "walkdir", "reflink_copy", "tempfile"] {
        assert!(
            deps.iter().any(|d| d == expected),
            "cargo_dep_names must read `{expected}` out of Cargo.toml (dash-normalized; \
             dev-dependencies included), got: {deps:?}"
        );
    }
    assert!(
        !reconcile_impurities("pub fn reconcile() { let _ = blake3::hash(b\"x\"); }").is_empty(),
        "a Cargo dependency reached via the extern prelude with NO use statement \
         (blake3::hash) must be flagged — dependency crates resolve without imports, so the \
         purity scan bans every [dependencies] name as a path head"
    );
    assert!(
        !reconcile_impurities("use blake3::Hasher;\npub fn reconcile() {}").is_empty(),
        "a use-leaf headed by a Cargo dependency must be flagged"
    );
    assert!(
        reconcile_impurities(
            "use std::collections::BTreeMap;\npub fn reconcile() { let _ = \
             BTreeMap::<u8, u8>::new(); }"
        )
        .is_empty(),
        "std non-I/O must stay spared by the dependency ban"
    );
    assert!(
        reconcile_impurities("pub fn reconcile() -> crate::projection::Projection { body() }")
            .is_empty(),
        "crate::projection paths must stay spared by the dependency ban"
    );
}

#[test]
fn helper_observed_artifact_scan_requires_enum_and_field_names() {
    let designed = "pub enum ObservedArtifact { Missing, Managed(ManagedArtifact), \
                    Foreign(ForeignArtifact), Ejected }\n\
                    pub struct ManagedArtifact { pub record: R, pub condition: ManagedCondition }";
    let body = enum_body(designed, "ObservedArtifact").expect("designed enum body");
    let variants = enum_variant_names(&body);
    for variant in ["Missing", "Managed", "Foreign", "Ejected"] {
        assert!(
            variants.iter().any(|v| v == variant),
            "the designed §7.3 enum must satisfy the variant scan, missing {variant} in \
             {variants:?}"
        );
    }
    let placeholder = "pub struct ObservedArtifact { pub condition: ManagedCondition }";
    assert!(
        enum_body(placeholder, "ObservedArtifact").is_none(),
        "the B16 placeholder STRUCT must not satisfy the enum pin"
    );
    let managed =
        item_body(designed, &["pub struct", "struct"], "ManagedArtifact").expect("managed body");
    assert!(
        references_token(&managed, "record") && references_token(&managed, "condition"),
        "the designed ManagedArtifact fields satisfy the field-name scan"
    );
    let renamed = "pub struct ManagedArtifact { pub rec: R, pub state: ManagedCondition }";
    let renamed_body =
        item_body(renamed, &["pub struct", "struct"], "ManagedArtifact").expect("renamed body");
    assert!(
        !(references_token(&renamed_body, "record")
            && references_token(&renamed_body, "condition")),
        "renamed fields must not satisfy the record/condition field pin"
    );
}

#[test]
fn helper_derive_window_reads_the_attribute_and_rejects_serde() {
    let clean = "pub struct Other { pub x: u8 }\n#[derive(Debug, Clone)]\npub struct \
                 ScannedFile { pub path: PathBuf }";
    let window = derive_window_before(clean, "struct ScannedFile").expect("clean window");
    assert!(
        references_token(&window, "derive")
            && references_token(&window, "Debug")
            && references_token(&window, "Clone")
            && !references_token(&window, "Serialize"),
        "a Debug+Clone derive satisfies the pin and the window stops at the previous item, \
         got: {window}"
    );
    let serde = "#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct ScannedFile {}";
    let serde_window = derive_window_before(serde, "struct ScannedFile").expect("serde window");
    assert!(
        references_token(&serde_window, "Serialize"),
        "a serde derive must be visible to the rejection half of the pin"
    );
    let bare = "pub struct ScannedFile {}";
    let bare_window = derive_window_before(bare, "struct ScannedFile").expect("bare window");
    assert!(
        !references_token(&bare_window, "Debug"),
        "a derive-less struct must fail the Debug+Clone half"
    );
    assert!(
        derive_window_before(clean, "struct Absent").is_none(),
        "an absent struct yields no window"
    );
}

#[test]
fn helper_sync_change_variant_scan_reads_top_level_names() {
    let body = "{ Deploy, Overwrite, Conflict { changed: Vec<PathBuf> }, Remove(RemovalReason) }";
    let variants = enum_variant_names(body);
    assert!(
        variants.iter().any(|v| v == "Conflict"),
        "a payload-carrying Conflict variant is still a top-level variant, got: {variants:?}"
    );
    assert!(
        !enum_variant_names("{ Wrapper(Conflict) }")
            .iter()
            .any(|v| v == "Conflict"),
        "a name only inside a payload must not register as a variant"
    );
}
