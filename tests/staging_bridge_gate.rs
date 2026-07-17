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

fn spec_type_body(stripped: &str, name: &str) -> Option<String> {
    ["pub struct", "pub enum"]
        .into_iter()
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

fn item_span(stripped: &str, needle: &str) -> Option<String> {
    let start = stripped.find(needle)?;
    let after = &stripped[start..];
    let open = after.find('{')?;
    let end = matching_brace(after.as_bytes(), open)?;
    Some(after[..=end].to_string())
}

fn pub_use_statements(stripped: &str) -> Vec<String> {
    stripped
        .split(';')
        .filter_map(|stmt| stmt.find("pub use").map(|i| stmt[i..].to_owned()))
        .collect()
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

const BRIDGE: &str = "StageBridge";

const BRIDGE_HOMES: &[&str] = &["sync/target.rs", "deploy.rs"];

const DEPLOY_FLOW_SITES: &[&str] = &["struct DeployContext", "fn deploy_one"];

const BRIDGE_FORBIDDEN_TOKENS: &[&str] = &[
    "ParsedSource",
    "TemplateOptIn",
    "LayoutConfig",
    "Config",
    "RegistryRecord",
    "ManifestFile",
    "ExportRequest",
    "ExportLeaf",
    "SourceBackend",
    "Registry",
    "Journal",
];

fn bridge_definers() -> Vec<String> {
    prod_src_files()
        .into_iter()
        .filter(|(_, content)| defines_pub_type(&scan(content), BRIDGE))
        .map(|(rel, _)| rel)
        .collect()
}

#[test]
fn contract_stage_bridge_is_defined_in_the_old_orchestration_exactly_once() {
    let definers = bridge_definers();
    assert!(
        definers.len() == 1,
        "exactly one production file must define `pub struct {BRIDGE}` — the T014 orchestrator \
         bridge (review-gate C2): without it the still-old deploy.rs/target.rs cannot hand \
         T032's StageRequest its `&ProjectedArtifact` + `&TargetProjection` borrows, so PR6 \
         staging could not move green before PR8 migrates orchestration; found at: {definers:?}"
    );
    assert!(
        BRIDGE_HOMES.contains(&definers[0].as_str()),
        "the bridge must live in the still-old orchestration it serves — src/sync/target.rs \
         (the staging call sites) or src/deploy.rs — never in projection/ or source/ \
         (INV-1: the adapter pulls projection values OUT, it pushes nothing in); found in \
         src/{}",
        definers[0]
    );
}

#[test]
fn contract_stage_bridge_carries_exactly_the_two_projection_borrows() {
    let definers = bridge_definers();
    let home = definers.first().cloned().unwrap_or_else(|| {
        panic!(
            "no production file defines `pub struct {BRIDGE}` yet — T014 must add the bridge \
             before its shape can hold; expected in one of {BRIDGE_HOMES:?}"
        )
    });
    let body = spec_type_body(&scan(&read_src(&home)), BRIDGE).unwrap_or_else(|| {
        panic!("src/{home} defines {BRIDGE} but its braced body could not be extracted")
    });

    for token in ["ProjectedArtifact", "TargetProjection"] {
        assert!(
            references_token(&body, token),
            "{BRIDGE} must borrow `{token}` — T032's StageRequest sketch takes exactly \
             {{ artifact: &ProjectedArtifact, target: &TargetProjection, variables }} and the \
             bridge supplies the two projection borrows (variables the flow already owns); \
             body:\n{body}"
        );
    }
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    for field in ["pub artifact:", "pub target:"] {
        assert!(
            flat.contains(field),
            "{BRIDGE} must expose a public `{field}` field so the behavioral suite and T032's \
             StageRequest construction can consume it by struct literal; body:\n{body}"
        );
    }
    let smuggled: Vec<&str> = BRIDGE_FORBIDDEN_TOKENS
        .iter()
        .copied()
        .filter(|token| references_token(&body, token))
        .collect();
    assert!(
        smuggled.is_empty(),
        "{BRIDGE} is a pure projection-value carrier: config DTOs, registry/manifest types, and \
         source-I/O ports stay flow-owned and must never ride the bridge (they would leak into \
         sync/stage.rs at T015 and violate the INV-2 staging clause); smuggled: {smuggled:?}; \
         body:\n{body}"
    );
}

#[test]
fn contract_deploy_flow_threads_the_bridge_to_the_staging_call_path() {
    let scanned = scan(&read_src("sync/target.rs"));
    let wired: Vec<&str> = DEPLOY_FLOW_SITES
        .iter()
        .copied()
        .filter(|site| {
            item_span(&scanned, site).is_some_and(|span| references_token(&span, BRIDGE))
        })
        .collect();
    assert!(
        !wired.is_empty(),
        "src/sync/target.rs must carry {BRIDGE} into the STAGING CALL PATH itself — a \
         `struct DeployContext` field or an `fn deploy_one` reference (either site counts) — \
         because deploy_one is where backend.export_artifact runs and where T015 swaps in the \
         T032 StageRequest; constructing a {BRIDGE} up in deploy_target's loop (where plan and \
         item already co-reside) and dropping it bridges nothing: the borrows still never \
         reach the staging call"
    );
}

#[test]
fn contract_sync_reexports_the_bridge() {
    let stmts = pub_use_statements(&strip(&read_src("sync/mod.rs")));
    assert!(
        stmts.iter().any(|stmt| references_token(stmt, BRIDGE)),
        "src/sync/mod.rs must `pub use` {BRIDGE} so it resolves at `phora::sync::{BRIDGE}` — \
         the stable path the behavioral suite binds and T032's stage.rs builds on; pub use \
         statements found: {stmts:?}"
    );
}

#[test]
fn contract_bridge_stays_out_of_projection() {
    let offenders: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(rel, content)| {
            rel.starts_with("projection/") && references_token(&scan(content), BRIDGE)
        })
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        offenders.is_empty(),
        "src/projection/ must never name the sync-owned {BRIDGE}: the bridge derives values \
         FROM projection, and projection/build.rs may gain only pure helpers over existing \
         types (INV-1; arch-check enforces the import side, this pins the type-name side); \
         offenders: {offenders:?}"
    );
}

#[test]
fn tripwire_stage_bridge_retires_at_t020() {
    assert!(
        !bridge_definers().is_empty(),
        "TRIPWIRE — the T014 {BRIDGE} is TEMPORARY scaffolding, alive only for PR6–PR7: this \
         test is DESIGNED to fail at T020 (PR8) when the orchestrator consumes Projection \
         directly and the bridge is deleted. Closing it at T020 means DELETING this test file \
         (tests/staging_bridge_gate.rs) and the activated tests/staging_bridge.rs — never \
         resurrecting the bridge. If it fails BEFORE T020, the bridge was removed early and \
         PR6/PR7 staging has lost its StageRequest inputs (review-gate C2)."
    );
}

#[test]
fn helper_item_span_covers_signature_and_nested_braces() {
    let stripped = "fn deploy_one(bridge: StageBridge<'_>) -> Y { if x { inner } tail } \
                    fn other() { unrelated }";
    let span = item_span(stripped, "fn deploy_one").expect("span extracted");
    assert!(
        references_token(&span, "StageBridge"),
        "the span must include the signature, so a bridge PARAMETER counts as wiring, got:\n{span}"
    );
    assert!(
        references_token(&span, "inner") && references_token(&span, "tail"),
        "the span must include nested braces and trailing statements, got:\n{span}"
    );
    assert!(
        !references_token(&span, "unrelated"),
        "the span must stop at its own matching close brace, got:\n{span}"
    );
    assert!(
        item_span(stripped, "fn absent").is_none(),
        "a missing item yields no span"
    );
}

#[test]
fn helper_spec_type_body_handles_lifetime_generics_and_name_prefixes() {
    let stripped = strip(
        "pub struct StageBridgeX<'a> { pub wrong: &'a u8 }\n\
         pub struct StageBridge<'a> { pub artifact: &'a ProjectedArtifact, pub target: &'a \
         TargetProjection }",
    );
    let body = spec_type_body(&stripped, "StageBridge").expect("generic struct body extracted");
    assert!(
        references_token(&body, "ProjectedArtifact") && references_token(&body, "TargetProjection"),
        "the body must be the exact-named struct's, not the prefixed sibling's, got:\n{body}"
    );
    assert!(
        !references_token(&body, "wrong"),
        "a name prefix must not claim the longer-named struct's body, got:\n{body}"
    );
}

#[test]
fn helper_strip_blanks_raw_strings_without_desyncing() {
    let stripped = strip("let t = r#\"a \"quoted\" bit\"#; b.fetch(x) // b.resolve(y)");
    assert!(
        !stripped.contains("quoted"),
        "raw-string content must be blanked, embedded double quote included: {stripped}"
    );
    assert!(
        stripped.contains("b.fetch(x)"),
        "the scanner must stay in sync after a raw string: {stripped}"
    );
    assert!(
        !stripped.contains("b.resolve(y)"),
        "a trailing comment must still be stripped after a raw string: {stripped}"
    );
}

#[test]
fn helper_pub_use_scan_sees_reexports_but_never_definitions() {
    let stmts = pub_use_statements(&strip(
        "mod target;\npub use target::StageBridge;\npub struct StageBridge;",
    ));
    assert!(
        stmts
            .iter()
            .any(|stmt| references_token(stmt, "StageBridge")),
        "a re-export must be visible to the scan"
    );
    assert!(
        pub_use_statements(&strip("// pub use target::StageBridge;")).is_empty(),
        "a commented-out re-export must not count"
    );
    assert!(
        pub_use_statements(&strip("pub struct StageBridge;")).is_empty(),
        "a definition is not a re-export"
    );
}
