use std::fs;
use std::path::PathBuf;

fn read_src(rel: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(rel),
    )
    .unwrap_or_default()
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

fn enum_body(stripped: &str, name: &str) -> Option<String> {
    let needle = format!("enum {name}");
    stripped.match_indices(&needle).find_map(|(i, _)| {
        let after = &stripped[i + needle.len()..];
        let boundary = after
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        boundary
            .then_some(i)
            .and_then(|start| balanced_body(&stripped[start..]))
    })
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

fn struct_bodies(stripped: &str) -> Vec<String> {
    let bytes = stripped.as_bytes();
    stripped
        .match_indices("struct")
        .filter_map(|(i, _)| {
            let before_ok =
                i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
            let after = i + "struct".len();
            let after_ok = bytes.get(after).is_some_and(u8::is_ascii_whitespace);
            if !(before_ok && after_ok) {
                return None;
            }
            let rest = &stripped[i..];
            match (rest.find('{'), rest.find(';')) {
                (Some(b), Some(s)) if s < b => None,
                (Some(_), _) => balanced_body(rest),
                _ => None,
            }
        })
        .collect()
}

fn skip_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

fn top_level_for_tail(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            'f' if depth == 0
                && s[i..].starts_with("for")
                && (i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_'))
                && bytes
                    .get(i + 3)
                    .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_') =>
            {
                return Some(&s[i + 3..]);
            }
            _ => {}
        }
    }
    None
}

fn impl_subject(header: &str) -> Option<String> {
    let rest = skip_generics(header.trim_start().strip_prefix("impl")?.trim_start());
    let subject = top_level_for_tail(rest).unwrap_or(rest);
    let head = subject.find('<').map_or(subject, |i| &subject[..i]);
    head.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .rfind(|part| !part.is_empty() && *part != "dyn" && *part != "mut")
        .map(str::to_owned)
}

fn impl_headers(stripped: &str) -> Vec<String> {
    let bytes = stripped.as_bytes();
    stripped
        .match_indices("impl")
        .filter_map(|(i, _)| {
            let before_ok =
                i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
            let after = i + "impl".len();
            let after_ok = bytes
                .get(after)
                .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_' && b != b'!');
            if !(before_ok && after_ok) {
                return None;
            }
            let rest = &stripped[i..];
            let open = rest.find('{')?;
            Some(rest[..open].to_owned())
        })
        .collect()
}

fn impls_trait_for(stripped: &str, trait_token: &str, subject: &str) -> bool {
    impl_headers(stripped).into_iter().any(|header| {
        references_token(&header, trait_token) && impl_subject(&header).as_deref() == Some(subject)
    })
}

const MODEL: &str = "sync/model.rs";
const RECONCILE: &str = "sync/reconcile.rs";
const RENAMED: &[&str] = &["UnmatchedArtifact", "MissingObservation"];

#[test]
fn sync_error_renames_the_bare_unmatched_variant_to_a_subject_specific_name() {
    let stripped = strip(&read_src(MODEL));
    let body = enum_body(&stripped, "SyncError").unwrap_or_else(|| {
        panic!("src/{MODEL} must define `enum SyncError` — reconcile's pure failure payload")
    });
    let variants = enum_variant_names(&body);
    assert!(
        !references_token(&stripped, "Unmatched"),
        "no bare `Unmatched` token may survive ANYWHERE in src/{MODEL} — not as a variant, a \
         re-export alias (`... as Unmatched`), or a `type Unmatched =` reintroduction; B17 renames \
         the never-constructed placeholder to a subject-specific name and leaves nothing bare \
         behind (word boundaries keep the renamed `UnmatchedArtifact` from tripping this pin); \
         variants: {variants:?}"
    );
    assert!(
        variants.iter().any(|v| RENAMED.contains(&v.as_str())),
        "SyncError must declare the renamed correlation-failure variant (one of {RENAMED:?}) \
         carrying the desired (target, source, artifact) triplet; variants: {variants:?}"
    );
}

#[test]
fn sync_error_implements_display_and_std_error() {
    let stripped = strip(&read_src(MODEL));
    assert!(
        impls_trait_for(&stripped, "Display", "SyncError"),
        "src/{MODEL} must carry `impl ... Display for SyncError` — B17 requires the wired error to \
         render its subject once semantics land, not derive-only Debug"
    );
    assert!(
        impls_trait_for(&stripped, "Error", "SyncError"),
        "src/{MODEL} must carry `impl std::error::Error for SyncError` — the correlation failure \
         becomes a real error type the sync boundary can propagate"
    );
}

#[test]
fn reconcile_constructs_the_renamed_correlation_error() {
    let scanned = strip(&read_src(RECONCILE));
    assert!(
        RENAMED.iter().any(|name| references_token(&scanned, name)),
        "src/{RECONCILE} must CONSTRUCT the renamed correlation-failure variant (one of \
         {RENAMED:?}) — B17: a desired artifact with no keyed observation is no longer silently \
         dropped by a positional zip; the error must be wired, not a never-constructed placeholder"
    );
    assert!(
        !references_token(&scanned, "Unmatched"),
        "src/{RECONCILE} must not construct or name the bare `Unmatched` placeholder — only the \
         renamed subject-specific variant"
    );
}

#[test]
fn reconcile_correlates_by_key_not_positional_zip() {
    let scanned = strip(&read_src(RECONCILE));
    assert!(
        !references_token(&scanned, "zip"),
        "src/{RECONCILE} must NOT pair desired against observed with a positional `.zip(` — B17: \
         positional pairing silently truncates unequal tails and cross-matches misordered sets; \
         reconcile must correlate by the (target, source, artifact) key instead"
    );
}

#[test]
fn observed_project_state_carries_the_identity_triplet_per_entry() {
    let keyed = struct_bodies(&strip(&read_src(MODEL)))
        .into_iter()
        .any(|body| {
            ["target", "source", "artifact", "ObservedArtifact"]
                .iter()
                .all(|token| references_token(&body, token))
        });
    assert!(
        keyed,
        "src/{MODEL} must key each observation to its (target, source, artifact) identity — the \
         keyed correlation requires the observed side to carry the triplet (e.g. `struct \
         ObservedEntry {{ target, source, artifact, observation: ObservedArtifact }}`); a bare \
         `Vec<ObservedArtifact>` with no key cannot be correlated to the projection"
    );
}

#[test]
fn helper_enum_variant_scan_reads_top_level_variants_only() {
    let body = "{ Deploy { target: String }, UnmatchedArtifact { target: String }, Foreign }";
    let names = enum_variant_names(body);
    assert_eq!(
        names,
        ["Deploy", "UnmatchedArtifact", "Foreign"],
        "top-level variant identifiers are extracted, payload fields are not; got {names:?}"
    );
    assert!(
        !enum_variant_names("{ Unmatched, Other }")
            .iter()
            .any(|v| v == "UnmatchedArtifact"),
        "a bare `Unmatched` must not read as the renamed `UnmatchedArtifact`"
    );
}

#[test]
fn helper_bare_unmatched_token_scan_spares_the_rename() {
    assert!(
        references_token(
            "enum SyncError { Unmatched { target: String } }",
            "Unmatched"
        ),
        "a bare `Unmatched` token must be caught by the whole-file rename pin"
    );
    assert!(
        references_token(
            "pub use SyncError::UnmatchedArtifact as Unmatched;",
            "Unmatched"
        ),
        "a re-export alias reintroducing the bare `Unmatched` name must be caught"
    );
    assert!(
        !references_token(
            "enum SyncError { UnmatchedArtifact { target: String } }",
            "Unmatched"
        ),
        "the renamed `UnmatchedArtifact` must NOT trip the bare-`Unmatched` pin — `references_token` \
         word boundaries require a non-identifier char after the token"
    );
}

#[test]
fn helper_impl_detection_binds_trait_to_subject() {
    assert!(
        impls_trait_for(
            "impl std::fmt::Display for SyncError { }",
            "Display",
            "SyncError"
        ),
        "a Display impl on SyncError must be detected"
    );
    assert!(
        impls_trait_for(
            "impl std::error::Error for SyncError { }",
            "Error",
            "SyncError"
        ),
        "an Error impl on SyncError must be detected — the trait `Error` is distinct from the \
         subject `SyncError`"
    );
    assert!(
        !impls_trait_for(
            "impl std::fmt::Display for SyncError { }",
            "Error",
            "SyncError"
        ),
        "a Display impl must NOT satisfy the Error pin — `Error` inside `SyncError` is not a \
         standalone trait token"
    );
}

#[test]
fn helper_struct_body_scan_reads_a_keyed_entry_and_spares_the_bare_vec() {
    let keyed = struct_bodies(
        "pub struct ObservedEntry { pub target: String, pub source: String, \
         pub artifact: String, pub observation: ObservedArtifact }",
    );
    assert!(
        keyed
            .iter()
            .any(|body| ["target", "source", "artifact", "ObservedArtifact"]
                .iter()
                .all(|t| references_token(body, t))),
        "a keyed observed entry carrying the triplet must be detected"
    );
    let bare =
        struct_bodies("pub struct ObservedProjectState { pub artifacts: Vec<ObservedArtifact> }");
    assert!(
        !bare.iter().any(|body| references_token(body, "target")),
        "a bare `Vec<ObservedArtifact>` with no per-entry key must NOT satisfy the triplet pin"
    );
}
