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

fn defines_pub_enum(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "pub enum", name)
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

fn variant_braced_payload(enum_body: &str, name: &str) -> Option<String> {
    let bytes = enum_body.as_bytes();
    enum_body.match_indices(name).find_map(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        if !before_ok {
            return None;
        }
        let after_all = &enum_body[i + name.len()..];
        let after = after_all.trim_start();
        (after_all.len() > after.len() || after.starts_with('{'))
            .then(|| after.starts_with('{'))
            .and_then(|is_braced| is_braced.then(|| balanced_body(after)).flatten())
    })
}

fn conflict_variant_payload(model_src: &str) -> Option<String> {
    let body = enum_body(&strip(model_src), "SyncChange")?;
    variant_braced_payload(&body, "Conflict")
}

fn field_type_segment(payload: &str, field: &str) -> Option<String> {
    let bytes = payload.as_bytes();
    payload.match_indices(field).find_map(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        if !before_ok {
            return None;
        }
        let after_all = &payload[i + field.len()..];
        let after = after_all.trim_start();
        let boundary = after_all.len() > after.len() || after.starts_with(':');
        let rest = boundary.then(|| after.strip_prefix(':')).flatten()?;
        let mut depth = 0i32;
        let mut seg = String::new();
        for c in rest.chars() {
            match c {
                '<' | '(' | '[' => depth += 1,
                '>' | ')' | ']' => depth -= 1,
                ',' | '}' if depth <= 0 => break,
                _ => {}
            }
            seg.push(c);
        }
        Some(seg)
    })
}

fn field_typed(payload: &str, field: &str, type_tokens: &[&str]) -> bool {
    field_type_segment(payload, field).is_some_and(|seg| {
        type_tokens
            .iter()
            .all(|token| references_token(&seg, token))
    })
}

fn pub_use_reexports_unaliased(mod_src: &str, name: &str) -> bool {
    let scanned = strip(mod_src);
    scanned.match_indices("pub use").any(|(i, _)| {
        let before_ok = i == 0
            || (!scanned.as_bytes()[i - 1].is_ascii_alphanumeric()
                && scanned.as_bytes()[i - 1] != b'_');
        let after = &scanned[i + "pub use".len()..];
        let Some(end) = after.find(';') else {
            return false;
        };
        before_ok && names_unaliased(&after[..end], name)
    })
}

fn names_unaliased(use_body: &str, name: &str) -> bool {
    let bytes = use_body.as_bytes();
    use_body.match_indices(name).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after_all = &use_body[i + name.len()..];
        let after = after_all.trim_start();
        let boundary = after_all.len() > after.len()
            || after
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let aliased = after.strip_prefix("as").is_some_and(|rest| {
            rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric() && c != '_')
        });
        before_ok && boundary && !aliased
    })
}

fn redefines_conflict_kind(stripped: &str) -> bool {
    [
        "pub enum",
        "enum",
        "pub struct",
        "struct",
        "pub type",
        "type",
    ]
    .iter()
    .any(|kw| keyword_names(stripped, kw, "ConflictKind"))
}

const MODEL: &str = "sync/model.rs";
const MOD: &str = "sync/mod.rs";
const RECONCILE: &str = "sync/reconcile.rs";

#[test]
fn conflict_kind_is_defined_in_the_pure_model_module() {
    assert!(
        defines_pub_enum(&strip(&read_src(MODEL)), "ConflictKind"),
        "src/{MODEL} must define `pub enum ConflictKind` — T033 re-homes the kind classifier into \
         the pure, std-only model module so reconcile can name it under the INV-8 allowlist \
         instead of reaching into the I/O-bearing sync::mod"
    );
}

#[test]
fn conflict_kind_carries_modified_and_foreign_variants() {
    let body = enum_body(&strip(&read_src(MODEL)), "ConflictKind").unwrap_or_else(|| {
        panic!(
            "src/{MODEL} does not define `enum ConflictKind` yet — the re-homed kind classifier \
             (design §7 two-phase conflict protocol)"
        )
    });
    let modified_payload = variant_braced_payload(&body, "Modified").unwrap_or_else(|| {
        panic!(
            "ConflictKind::Modified must be a STRUCT variant carrying the changed paths — a unit \
             `Modified` erases the drift detail the resolver renders; body: {body}"
        )
    });
    assert!(
        field_typed(&modified_payload, "changed", &["Vec", "PathBuf"]),
        "ConflictKind::Modified must carry `changed: Vec<...PathBuf...>` — a `changed: String` (or \
         any non-path type) erases the drift paths the resolver renders; payload: \
         {modified_payload}"
    );
    assert!(
        references_token(&body, "Foreign"),
        "ConflictKind must keep an EXPLICIT `Foreign` variant — a Foreign conflict is named, never \
         implied by an empty changed list; body: {body}"
    );
}

#[test]
fn sync_change_conflict_carries_a_typed_kind_field_over_the_identity_triplet() {
    let payload = conflict_variant_payload(&read_src(MODEL)).unwrap_or_else(|| {
        panic!(
            "src/{MODEL} must keep an `enum SyncChange` with a `Conflict {{ .. }}` struct variant \
             — the reconcile-emitted conflict"
        )
    });
    for field in ["target", "source", "artifact"] {
        assert!(
            references_token(&payload, field),
            "SyncChange::Conflict must keep the `{field}` identity field — identity stays the \
             string triplet (B17 ruling); payload: {payload}"
        );
    }
    assert!(
        field_typed(&payload, "kind", &["ConflictKind"]),
        "SyncChange::Conflict's `kind` field must be typed `ConflictKind` (adjacent: `kind: \
         ConflictKind`) — a `kind: u8` with a ConflictKind token elsewhere in the payload cannot \
         carry Modified-vs-Foreign into the resolver; payload: {payload}"
    );
    assert!(
        !references_token(&payload, "changed"),
        "SyncChange::Conflict must NOT carry a top-level `changed: Vec<PathBuf>` — the changed \
         paths move INSIDE ConflictKind::Modified, so an empty list can never masquerade as \
         Foreign; payload: {payload}"
    );
}

#[test]
fn sync_mod_reexports_conflict_kind_unaliased_and_stops_defining_it() {
    let mod_src = read_src(MOD);
    assert!(
        !redefines_conflict_kind(&strip(&mod_src)),
        "src/{MOD} must STOP defining ConflictKind under ANY of enum/struct/type — it is re-homed \
         into the pure src/{MODEL}; a second definition (or a shadowing `type ConflictKind =` \
         alias) forks the type the resolver and reconcile bind against"
    );
    assert!(
        pub_use_reexports_unaliased(&mod_src, "ConflictKind"),
        "src/{MOD} must `pub use` ConflictKind (UNaliased) from the model module so \
         `phora::sync::ConflictKind` keeps resolving — a `pub use ... as X` renames the export and \
         breaks the existing resolver/CLI call sites"
    );
}

#[test]
fn reconcile_never_names_resolution_or_execution_effects() {
    let scanned = strip(&read_src(RECONCILE));
    for token in ["Resolution", "ConflictResolver", "Eject", "Abort"] {
        assert!(
            !references_token(&scanned, token),
            "src/{RECONCILE} must never name `{token}` — INV-8 keeps the \
             overwrite/skip/eject/abort DECISION and its EXECUTION out of the pure reconcile \
             floor; resolution lives only in the I/O-bearing target.rs preflight"
        );
    }
    assert!(
        !references_token(&scanned, "Ejection") && !references_token(&scanned, "Journal"),
        "src/{RECONCILE} must never name eject-persistence or journal effects — Eject/Abort \
         EXECUTION stays in target.rs (INV-8)"
    );
}

#[test]
fn helper_conflict_payload_reads_the_new_and_old_shapes() {
    let new_shape = "pub enum SyncChange { Deploy { target: String }, \
        Conflict { target: String, source: String, artifact: String, kind: ConflictKind }, \
        Remove { reason: RemovalReason } }";
    let payload = conflict_variant_payload(new_shape).expect("Conflict arm found");
    assert!(
        references_token(&payload, "kind")
            && references_token(&payload, "ConflictKind")
            && !references_token(&payload, "changed"),
        "the kind-carrying arm must read as typed-kind-present, changed-absent; got {payload}"
    );

    let old_shape = "pub enum SyncChange { \
        Conflict { target: String, source: String, artifact: String, changed: Vec<PathBuf> } }";
    let payload = conflict_variant_payload(old_shape).expect("old Conflict arm found");
    assert!(
        !references_token(&payload, "kind") && references_token(&payload, "changed"),
        "the legacy flat arm must read as changed-present, kind-absent; got {payload}"
    );
}

#[test]
fn helper_field_typed_binds_the_type_adjacent_to_the_field() {
    let conflict = "{ target: String, source: String, artifact: String, kind: ConflictKind }";
    assert!(
        field_typed(conflict, "kind", &["ConflictKind"]),
        "kind: ConflictKind must bind"
    );
    let decoy =
        "{ target: String, source: String, artifact: String, kind: u8, marker: ConflictKind }";
    assert!(
        !field_typed(decoy, "kind", &["ConflictKind"]),
        "a `kind: u8` with ConflictKind smuggled into a sibling field must FAIL — the type must be \
         adjacent to `kind`"
    );

    let modified = "{ changed: Vec<PathBuf> }";
    assert!(
        field_typed(modified, "changed", &["Vec", "PathBuf"]),
        "changed: Vec<PathBuf> must bind"
    );
    let nested = "{ changed: Vec<(PathBuf, u8)> }";
    assert!(
        field_typed(nested, "changed", &["Vec", "PathBuf"]),
        "a nested generic must not truncate the type segment at its inner comma"
    );
    let wrong_type = "{ changed: String }";
    assert!(
        !field_typed(wrong_type, "changed", &["Vec", "PathBuf"]),
        "a `changed: String` decoy must FAIL the Vec<PathBuf> binding"
    );
    let bare = "{ kind, other: ConflictKind }";
    assert!(
        !field_typed(bare, "kind", &["ConflictKind"]),
        "a bare `kind` with no `:` type yields no segment and must FAIL"
    );
}

#[test]
fn helper_braced_payload_rejects_a_unit_variant() {
    let struct_variant = "{ Modified { changed: Vec<PathBuf> }, Foreign }";
    assert!(
        variant_braced_payload(struct_variant, "Modified").is_some_and(|p| p.contains("changed")),
        "a braced Modified must yield its payload"
    );
    let unit_variant = "{ Modified, Foreign }";
    assert!(
        variant_braced_payload(unit_variant, "Modified").is_none(),
        "a UNIT Modified must yield no braced payload — the contract-breaking shape must fail"
    );
    let tuple_variant = "{ Modified(Vec<PathBuf>), Foreign }";
    assert!(
        variant_braced_payload(tuple_variant, "Modified").is_none(),
        "a tuple Modified is not the required brace-with-`changed` struct payload"
    );
}

#[test]
fn helper_reexport_scan_requires_an_unaliased_pub_use() {
    assert!(
        pub_use_reexports_unaliased("pub use crate::sync::model::ConflictKind;", "ConflictKind"),
        "a direct `pub use` of ConflictKind must be detected"
    );
    assert!(
        pub_use_reexports_unaliased(
            "pub use model::{Conflict, ConflictKind, Resolution};",
            "ConflictKind"
        ),
        "a grouped `pub use` re-export of ConflictKind must be detected"
    );
    assert!(
        !pub_use_reexports_unaliased("pub use model::ConflictKind as CK;", "ConflictKind"),
        "an aliased `pub use ... as CK` renames the export — `phora::sync::ConflictKind` would not \
         resolve, so it must NOT count"
    );
    assert!(
        !pub_use_reexports_unaliased(
            "pub enum ConflictKind { Modified, Foreign }",
            "ConflictKind"
        ),
        "an in-place definition is not a re-export"
    );
    assert!(
        !pub_use_reexports_unaliased("use model::ConflictKind;", "ConflictKind"),
        "a private `use` (not `pub use`) does not re-export the type"
    );
}

#[test]
fn helper_redefinition_scan_rejects_every_shadowing_form() {
    for shadow in [
        "pub enum ConflictKind { Modified, Foreign }",
        "enum ConflictKind { Modified, Foreign }",
        "pub struct ConflictKind { kind: u8 }",
        "struct ConflictKind;",
        "pub type ConflictKind = model::ConflictKind;",
        "type ConflictKind = ();",
    ] {
        assert!(
            redefines_conflict_kind(shadow),
            "a shadowing definition must be rejected: {shadow}"
        );
    }
    assert!(
        !redefines_conflict_kind("pub use crate::sync::model::ConflictKind;"),
        "a plain `pub use` re-export is not a redefinition"
    );
    assert!(
        !redefines_conflict_kind("pub use model::{Conflict, ConflictKind};"),
        "a grouped `pub use` re-export is not a redefinition"
    );
}
