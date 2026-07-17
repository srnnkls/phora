//! Live RED gate for T008. The behavioral contract is in the sibling
//! `tests/projection_contract.rs.pending`; it stays `.pending` (not `.rs`)
//! because cargo compiles every `.rs` test target, so a file naming types that
//! do not exist yet would break the whole `cargo test` compile instead of
//! reporting a clean per-test RED. This gate scans the source tree as text and
//! is safe to keep live. The implementer renames the `.pending` file into place
//! once the API compiles and passes.
//!
//! Coverage boundary: the scan matches source text, so a `type` alias or a
//! `use ... as` rename of a forbidden config DTO evades both `defines_pub_type`
//! and the forbidden-token check; `projection_contract.rs.pending` is the
//! semantic backstop.

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

fn defines_pub_fn(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "pub fn", name)
}

fn balanced_body(after: &str) -> Option<String> {
    let open = after.find('{')?;
    let bytes = after.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[open..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn struct_body(stripped: &str, name: &str) -> Option<String> {
    let needle = format!("pub struct {name}");
    let start = stripped.find(&needle)?;
    balanced_body(&stripped[start..])
}

fn spec_type_body(stripped: &str, name: &str) -> Option<String> {
    ["pub struct", "pub enum"]
        .into_iter()
        .find_map(|keyword| stripped.find(&format!("{keyword} {name}")))
        .and_then(|start| balanced_body(&stripped[start..]))
}

const SOURCE_VALUE_TYPES: &[&str] = &[
    "SourcePath",
    "SourceEntryKind",
    "SourceEntryMeta",
    "SourceInventory",
];

const PROJECTION_OUTPUT_TYPES: &[&str] = &[
    "Projection",
    "TargetProjection",
    "BindingProjection",
    "ProjectedArtifact",
    "ProjectedLeaf",
    "ProjectionWarning",
];

const SPEC_INPUT_TYPES: &[&str] = &[
    "OfferSpec",
    "TakeSpec",
    "LayoutSpec",
    "TemplatePolicy",
    "MaterializationPolicy",
    "ResolvedSourceRef",
    "BindingProjectionInput",
];

const PATH_NEWTYPES: &[&str] = &["TargetPath", "ArtifactRelativePath"];

const FORBIDDEN_CONFIG_DTOS: &[&str] = &[
    "Offer<",
    "LayoutConfig",
    "TemplateOptIn",
    "DeployMode",
    "TakeEntry",
    "ParsedSource",
    "target_path",
];

const FORBIDDEN_CONFIG_TOKENS: &[&str] = &["Config", "Target"];

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

fn assert_all_defined(rel: &str, names: &[&str], what: &str) {
    let stripped = strip(&read_src(rel));
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !defines_pub_type(&stripped, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{rel} must define {what} as `pub struct`/`pub enum` items; absent: {missing:?}"
    );
}

#[test]
fn contract_source_rs_defines_pure_source_value_types() {
    assert_all_defined(
        "source.rs",
        SOURCE_VALUE_TYPES,
        "the pure source-owned value types (SourcePath/SourceEntryKind/SourceEntryMeta/\
         SourceInventory — no I/O; PR5 adds the store that populates them)",
    );
}

#[test]
fn contract_plan_rs_defines_projection_output_types() {
    assert_all_defined(
        "sync/plan.rs",
        PROJECTION_OUTPUT_TYPES,
        "the projection OUTPUT types (§6.3/§6.4 renames of TargetPlan/PlannedItem/PlanWarning)",
    );
}

#[test]
fn contract_plan_rs_defines_spec_input_types() {
    assert_all_defined(
        "sync/plan.rs",
        SPEC_INPUT_TYPES,
        "the config-free projection input specs (OfferSpec/TakeSpec/LayoutSpec/TemplatePolicy) \
         plus ResolvedSourceRef and BindingProjectionInput",
    );
}

#[test]
fn contract_spec_input_type_bodies_are_config_free() {
    let stripped = strip(&read_src("sync/plan.rs"));
    for spec in SPEC_INPUT_TYPES {
        let body = spec_type_body(&stripped, spec).unwrap_or_else(|| {
            panic!("src/sync/plan.rs must define `{spec}` with a braced struct/enum body")
        });
        let mut leaked: Vec<&str> = FORBIDDEN_CONFIG_DTOS
            .iter()
            .copied()
            .filter(|dto| body.contains(dto))
            .collect();
        leaked.extend(
            FORBIDDEN_CONFIG_TOKENS
                .iter()
                .copied()
                .filter(|token| references_token(&body, token)),
        );
        assert!(
            leaked.is_empty(),
            "the projection input spec `{spec}` must be an owned, config-free value: its fields may \
             name only pure spec/kernel/source types, never a config DTO (`Config`/`Target`/\
             `LayoutConfig`/`TemplateOptIn`/…). Leaked config references: {leaked:?}\nbody:\n{body}"
        );
    }
}

#[test]
fn contract_plan_rs_defines_path_newtypes() {
    assert_all_defined(
        "sync/plan.rs",
        PATH_NEWTYPES,
        "the lexical target-relative path newtypes TargetPath and ArtifactRelativePath",
    );
}

#[test]
fn contract_plan_rs_defines_projection_error() {
    let stripped = strip(&read_src("sync/plan.rs"));
    assert!(
        defines_pub_type(&stripped, "ProjectionError"),
        "src/sync/plan.rs must define the projection-owned `ProjectionError` (design §9), not \
         reuse a sync/config error for offer/take/collapse failures"
    );
}

#[test]
fn contract_plan_rs_renames_resolver_to_project_verbs() {
    let stripped = strip(&read_src("sync/plan.rs"));
    for verb in ["project_binding", "project_target", "project_workspace"] {
        assert!(
            defines_pub_fn(&stripped, verb),
            "src/sync/plan.rs must expose `pub fn {verb}` — the §6.4 verb rename \
             (resolve_binding_plan->project_binding, resolve_target_plan->project_target, \
             plan_targets->project_workspace)"
        );
    }
}

#[test]
fn contract_binding_projection_input_is_config_free_over_an_inventory() {
    let stripped = strip(&read_src("sync/plan.rs"));
    let body = struct_body(&stripped, "BindingProjectionInput").expect(
        "src/sync/plan.rs must define `pub struct BindingProjectionInput { … }` — the config-free \
         projection input carrying `inventory: &SourceInventory`",
    );
    assert!(
        body.contains("SourceInventory"),
        "BindingProjectionInput must carry a `SourceInventory` (the pure discovered leaf set) \
         instead of the old `candidate_leaves`/backend seam; body:\n{body}"
    );
    let mut leaked: Vec<&str> = FORBIDDEN_CONFIG_DTOS
        .iter()
        .copied()
        .filter(|dto| body.contains(dto))
        .collect();
    leaked.extend(
        FORBIDDEN_CONFIG_TOKENS
            .iter()
            .copied()
            .filter(|token| references_token(&body, token)),
    );
    assert!(
        leaked.is_empty(),
        "BindingProjectionInput must consume ONLY the pure *Spec/*Policy inputs and an inventory — \
         no config DTOs (`Config`/`Target`/`Offer<`/`LayoutConfig`/…) and no absolute target_path \
         (destinations are target-relative; sync joins the root). Leaked config references: \
         {leaked:?}\nbody:\n{body}"
    );
}

#[test]
fn contract_config_mod_converts_config_into_projection_specs() {
    let stripped = strip(&read_src("config/mod.rs"));
    for spec in ["OfferSpec", "LayoutSpec"] {
        assert!(
            stripped.contains(spec),
            "src/config/mod.rs must perform the one-way config->spec conversion and therefore \
             reference `{spec}` (config depends on the projection spec types, never the reverse); \
             no reference found"
        );
    }
}

#[test]
fn helper_pub_type_matcher_rejects_comment_and_string_and_impl() {
    assert!(defines_pub_type(
        &strip("#[derive(Debug)]\npub struct SourcePath(String);"),
        "SourcePath"
    ));
    assert!(defines_pub_type(
        &strip("pub enum SourceEntryKind { File }"),
        "SourceEntryKind"
    ));
    assert!(
        !defines_pub_type(&strip("// pub struct SourcePath(String);\n"), "SourcePath"),
        "a commented-out definition must not count"
    );
    assert!(
        !defines_pub_type(
            &strip("const S: &str = \"pub struct SourcePath(String);\";"),
            "SourcePath"
        ),
        "a definition inside a string literal must not count"
    );
    assert!(
        !defines_pub_type(&strip("impl SourcePath { fn new() {} }"), "SourcePath"),
        "an impl block is not a type definition"
    );
    assert!(
        !defines_pub_type(&strip("pub struct SourcePathBuf(String);"), "SourcePath"),
        "a longer-named sibling must not satisfy the exact type name"
    );
}

#[test]
fn helper_struct_body_extracts_balanced_braces_only() {
    let src = "pub struct BindingProjectionInput<'a> {\n    \
               pub inventory: &'a SourceInventory,\n    \
               pub offer: &'a OfferSpec,\n}\n\
               pub struct Other { pub x: u8 }\n";
    let body = struct_body(&strip(src), "BindingProjectionInput").expect("body extracted");
    assert!(
        body.contains("SourceInventory") && body.contains("OfferSpec"),
        "the extracted body must include the struct's fields"
    );
    assert!(
        !body.contains("Other") && !body.contains("pub x"),
        "brace balancing must stop at the struct's own closing brace; got:\n{body}"
    );
    assert!(
        struct_body(
            &strip("// pub struct BindingProjectionInput { x: u8 }"),
            "BindingProjectionInput"
        )
        .is_none(),
        "a commented-out struct must not yield a body"
    );
}

#[test]
fn helper_forbidden_dto_tokens_distinguish_config_from_spec() {
    assert!("offer: Offer<'a>".contains("Offer<"));
    assert!(!"offer: &'a OfferSpec".contains("Offer<"));
    assert!("layout: &'a LayoutConfig".contains("LayoutConfig"));
    assert!(!"layout: &'a LayoutSpec".contains("LayoutConfig"));
}

#[test]
fn helper_config_token_matcher_respects_word_boundaries() {
    assert!(
        references_token("pub base_config: &'a Config,", "Config"),
        "a bare `Config` field must be caught"
    );
    assert!(
        references_token("pub target: &'a Target,", "Target"),
        "a bare `Target` config DTO must be caught"
    );
    assert!(
        !references_token("pub destination: TargetPath,", "Target"),
        "the legitimate `TargetPath` spec type must NOT trip the `Target` token"
    );
    assert!(
        !references_token("pub proj: TargetProjection,", "Target"),
        "the legitimate `TargetProjection` output type must NOT trip the `Target` token"
    );
    assert!(
        !references_token("pub layout: &'a LayoutConfig,", "Config"),
        "`LayoutConfig` is a suffix match: the substring list owns it, the word-bounded token must not"
    );
}
