//! Placement gate for the T009 move. Scans source as text and must never
//! `use` the moved API: a pending or half-done move has to fail as a per-test
//! assertion, not break the whole test target's compile.
//!
//! Coverage boundary: a `type` alias or a `use ... as` rename evades both
//! `defines_pub_type` and the forbidden-token check;
//! `tests/projection_contract.rs` is the semantic backstop.

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

fn defines_fn(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "fn", name)
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
    "ContentTransform",
];

const PROJECTION_DIAGNOSTIC_TYPES: &[&str] = &["ProjectionWarning", "ProjectionError"];

const SPEC_INPUT_TYPES: &[&str] = &[
    "OfferSpec",
    "TakeSpec",
    "LayoutSpec",
    "LayoutStyle",
    "TemplatePolicy",
    "MaterializationPolicy",
    "CollapsePreference",
    "ResolvedSourceRef",
    "BindingProjectionInput",
];

const PATH_NEWTYPES: &[&str] = &["TargetPath", "ArtifactRelativePath"];

const MOVED_FNS: &[&str] = &[
    "project_binding",
    "project_target",
    "projected_artifact_keys",
    "reject_partial_take_collapse",
    "reject_cross_binding_dups",
    "from_entries",
];

const MOVED_HELPER_FNS: &[&str] = &[
    "other",
    "collapse_blocked",
    "classify_take_error",
    "offer_root_prefix",
    "build_leaves",
    "unsafe_leaf",
    "unsafe_target_path",
    "partial_take_collapse_diagnostic",
    "apply_deployed_name",
    "kept_leaves_under",
    "duplicate_destination",
    "ancestor_prefixes",
    "cross_binding_dup_diagnostic",
];

const MOVED_TYPES_WITH_IMPLS: &[&str] = &[
    "OfferSpec",
    "TakeSpec",
    "LayoutSpec",
    "TemplatePolicy",
    "MaterializationPolicy",
    "CollapsePreference",
    "ResolvedSourceRef",
    "TargetPath",
    "ArtifactRelativePath",
];

const KERNEL_FACADE_LEAVES: &[&str] = &[
    "OfferSelection",
    "compile_take_glob",
    "ResolvedTake",
    "Take",
    "TakeResolution",
    "TakeWarning",
    "is_take_glob",
    "resolve_take",
    "fold_dest",
    "CollapseChoice",
    "CollapseMode",
    "CollapsePlan",
    "CollapseWarning",
    "plan_collapse",
    "Materialization",
];

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

fn kernel_reference_regions(stripped: &str) -> Vec<String> {
    let bytes = stripped.as_bytes();
    stripped
        .match_indices("kernel::")
        .filter(|&(i, _)| i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_'))
        .map(|(i, hit)| {
            let rest = stripped[i + hit.len()..].trim_start();
            if rest.starts_with('{') {
                balanced_body(rest).unwrap_or_default()
            } else {
                rest.chars()
                    .take_while(|&c| c.is_alphanumeric() || c == '_' || c == ':')
                    .collect()
            }
        })
        .collect()
}

fn projection_source_files() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("projection");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            if name == "tests.rs" || name.ends_with("_tests.rs") {
                return None;
            }
            let content = fs::read_to_string(&path).unwrap_or_default();
            Some((name, content))
        })
        .collect()
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
fn contract_source_module_defines_pure_source_value_types() {
    let monolith = strip(&read_src("source.rs"));
    let model = strip(&read_src("source/model.rs"));
    let missing: Vec<&str> = SOURCE_VALUE_TYPES
        .iter()
        .copied()
        .filter(|name| !defines_pub_type(&monolith, name) && !defines_pub_type(&model, name))
        .collect();
    assert!(
        missing.is_empty(),
        "the pure source-owned value types (SourcePath/SourceEntryKind/SourceEntryMeta/\
         SourceInventory — no I/O; PR5 adds the store that populates them) must be defined in \
         src/source.rs (pre-T011) or src/source/model.rs (post-T011 split); absent from both: \
         {missing:?}"
    );
}

#[test]
fn contract_model_rs_defines_projection_output_types() {
    assert_all_defined(
        "projection/model.rs",
        PROJECTION_OUTPUT_TYPES,
        "the projection OUTPUT types (Projection/TargetProjection/BindingProjection/\
         ProjectedArtifact/ProjectedLeaf/ContentTransform), moved out of sync/plan.rs by T009 so \
         projection owns them for T014",
    );
}

#[test]
fn contract_model_rs_defines_spec_input_types() {
    assert_all_defined(
        "projection/model.rs",
        SPEC_INPUT_TYPES,
        "the config-free projection input specs (OfferSpec/TakeSpec/LayoutSpec/LayoutStyle/\
         TemplatePolicy/MaterializationPolicy/CollapsePreference) plus ResolvedSourceRef and \
         BindingProjectionInput, moved out of sync/plan.rs by T009",
    );
}

#[test]
fn contract_model_rs_defines_path_newtypes() {
    assert_all_defined(
        "projection/model.rs",
        PATH_NEWTYPES,
        "the lexical target-relative path newtypes TargetPath and ArtifactRelativePath, moved \
         out of sync/plan.rs by T009",
    );
}

#[test]
fn contract_diagnostic_rs_owns_projection_warning_and_error() {
    assert_all_defined(
        "projection/diagnostic.rs",
        PROJECTION_DIAGNOSTIC_TYPES,
        "the projection-owned diagnostics ProjectionWarning and ProjectionError (design §9), \
         moved out of sync/plan.rs by T009",
    );
}

#[test]
fn contract_spec_input_type_bodies_are_config_free() {
    let stripped = strip(&read_src("projection/model.rs"));
    for spec in SPEC_INPUT_TYPES {
        let body = spec_type_body(&stripped, spec).unwrap_or_else(|| {
            panic!("src/projection/model.rs must define `{spec}` with a braced struct/enum body")
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
fn contract_binding_projection_input_is_config_free_over_an_inventory() {
    let stripped = strip(&read_src("projection/model.rs"));
    let body = struct_body(&stripped, "BindingProjectionInput").expect(
        "src/projection/model.rs must define `pub struct BindingProjectionInput { … }` — the \
         config-free projection input carrying `inventory: &SourceInventory`",
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
fn contract_build_rs_defines_the_pure_projection_verbs() {
    let stripped = strip(&read_src("projection/build.rs"));
    for verb in ["project_binding", "project_target"] {
        assert!(
            defines_pub_fn(&stripped, verb),
            "src/projection/build.rs must define `pub fn {verb}` — T009 moves the config-free \
             projection core out of sync/plan.rs into the projection module"
        );
    }
}

#[test]
fn contract_build_rs_owns_validation_and_artifact_key_calc() {
    let stripped = strip(&read_src("projection/build.rs"));
    assert!(
        defines_pub_fn(&stripped, "projected_artifact_keys"),
        "src/projection/build.rs must define `pub fn projected_artifact_keys` — the artifact-key \
         calc moves with the projection core in T009"
    );
    for helper in ["reject_partial_take_collapse", "reject_cross_binding_dups"] {
        assert!(
            defines_fn(&stripped, helper),
            "src/projection/build.rs must define `fn {helper}` — the partial-take collapse \
             validation and cross-binding collision checks move with the projection core in T009"
        );
    }
}

#[test]
fn contract_plan_rs_retains_the_sync_side_orchestrators() {
    let stripped = strip(&read_src("sync/plan.rs"));
    for verb in ["plan_target", "project_workspace"] {
        assert!(
            defines_pub_fn(&stripped, verb),
            "src/sync/plan.rs must keep `pub fn {verb}` — the config- and discovery-consuming \
             orchestrators stay sync-owned; T009 moves only the pure projection cluster"
        );
    }
}

#[test]
fn contract_plan_rs_no_longer_defines_the_moved_projection_cluster() {
    let stripped = strip_cfg_test(&strip(&read_src("sync/plan.rs")));
    let mut leftovers: Vec<String> = SPEC_INPUT_TYPES
        .iter()
        .chain(PATH_NEWTYPES)
        .chain(PROJECTION_OUTPUT_TYPES)
        .chain(PROJECTION_DIAGNOSTIC_TYPES)
        .filter(|name| defines_pub_type(&stripped, name))
        .map(|name| format!("pub type {name}"))
        .collect();
    leftovers.extend(
        MOVED_FNS
            .iter()
            .chain(MOVED_HELPER_FNS)
            .filter(|name| defines_fn(&stripped, name))
            .map(|name| format!("fn {name}")),
    );
    leftovers.extend(
        MOVED_TYPES_WITH_IMPLS
            .iter()
            .chain(PROJECTION_DIAGNOSTIC_TYPES)
            .chain(PROJECTION_OUTPUT_TYPES)
            .filter(|name| has_impl_referencing(&stripped, name))
            .map(|name| format!("impl … {name}")),
    );
    assert!(
        leftovers.is_empty(),
        "src/sync/plan.rs must no longer DEFINE the T009-moved projection cluster (types → \
         projection/model.rs + projection/diagnostic.rs, verbs/validation/artifact-key calc and \
         their private helpers → projection/build.rs, the TakeEntry→TakeSpec conversion → \
         config/mod.rs) — a leftover definition, helper fn, or impl block means the move was \
         copied, not relocated; compat `pub use` re-exports are fine, definitions are not. \
         Still defined here: {leftovers:?}"
    );
}

#[test]
fn contract_projection_owns_the_moved_private_helpers() {
    let files = projection_source_files();
    let missing: Vec<&str> = MOVED_HELPER_FNS
        .iter()
        .copied()
        .filter(|helper| {
            !files
                .iter()
                .any(|(_, content)| defines_fn(&strip_cfg_test(&strip(content)), helper))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "every private helper of the moved cluster (project_binding's leaf/error/collapse \
         helpers, the cross-binding diagnostic helpers) must be DEFINED in a production file \
         under src/projection/ — build.rs for the core, diagnostic.rs acceptable for the \
         diagnostic renderers; absent from all projection files: {missing:?}"
    );
}

#[test]
fn contract_model_rs_owns_the_moved_type_impls() {
    let stripped = strip(&read_src("projection/model.rs"));
    let blocks = impl_blocks(&stripped);
    let missing: Vec<&str> = MOVED_TYPES_WITH_IMPLS
        .iter()
        .copied()
        .filter(|name| !has_impl_referencing(&stripped, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/projection/model.rs must carry each moved spec/newtype's impl block along with its \
         type (constructors/accessors move too, not just the bare struct/enum); no impl found \
         for: {missing:?}"
    );
    for newtype in PATH_NEWTYPES {
        for trait_name in ["FromStr", "Display"] {
            assert!(
                blocks
                    .iter()
                    .any(|(header, _)| references_token(header, newtype)
                        && references_token(header, trait_name)),
                "src/projection/model.rs must carry the `impl {trait_name} for {newtype}` block — \
                 the path newtypes move with their trait impls"
            );
        }
    }
}

#[test]
fn contract_diagnostic_rs_owns_the_projection_error_conversion() {
    let stripped = strip(&read_src("projection/diagnostic.rs"));
    assert!(
        impl_blocks(&stripped).iter().any(|(header, _)| {
            references_token(header, "From") && references_token(header, "ProjectionError")
        }),
        "src/projection/diagnostic.rs must carry the `impl From<ProjectionError> for Error` \
         unwrap-conversion alongside the ProjectionError definition it moves with"
    );
}

#[test]
fn contract_projection_never_names_a_kernel_facade_leaf() {
    let mut violations: Vec<String> = Vec::new();
    for (name, content) in projection_source_files() {
        let scanned = strip_cfg_test(&strip(&content));
        for region in kernel_reference_regions(&scanned) {
            violations.extend(
                KERNEL_FACADE_LEAVES
                    .iter()
                    .filter(|leaf| references_token(&region, leaf))
                    .map(|leaf| format!("src/projection/{name}: kernel::{leaf}")),
            );
        }
    }
    assert!(
        violations.is_empty(),
        "src/projection/ must reach the moved algorithms at their direct crate::projection:: \
         paths — the kernel facade leaves are compat re-exports outside the kernel allowance \
         (only TargetName/ArtifactName/SourceName/Commit/safe_relpath may come from kernel). \
         Facade references found: {violations:?}"
    );
}

#[test]
fn contract_projection_is_take_entry_free() {
    let offenders: Vec<String> = projection_source_files()
        .iter()
        .filter(|(_, content)| references_token(&strip_cfg_test(&strip(content)), "TakeEntry"))
        .map(|(name, _)| format!("src/projection/{name}"))
        .collect();
    assert!(
        offenders.is_empty(),
        "the TakeEntry→TakeSpec conversion lands config-side: no file under src/projection/ may \
         name the `TakeEntry` config DTO. Offenders: {offenders:?}"
    );
}

#[test]
fn contract_config_mod_converts_config_into_projection_specs() {
    let stripped = strip_cfg_test(&strip(&read_src("config/mod.rs")));
    for spec in ["OfferSpec", "LayoutSpec", "TakeSpec"] {
        assert!(
            stripped.contains(spec),
            "src/config/mod.rs must perform the one-way config->spec conversion and therefore \
             reference `{spec}` (config depends on the projection spec types, never the reverse); \
             no reference found"
        );
    }
    assert!(
        impl_blocks(&stripped).iter().any(|(header, body)| {
            references_token(header, "TakeSpec")
                && (references_token(header, "TakeEntry") || references_token(body, "TakeEntry"))
        }),
        "src/config/mod.rs must DEFINE the TakeEntry→TakeSpec conversion carved out of \
         sync/plan.rs by T009: an impl block naming TakeSpec in its header and consuming \
         TakeEntry (either `impl TakeSpec {{ fn from_entries(… TakeEntry …) }}` or \
         `impl From<… TakeEntry …> for TakeSpec`), alongside the other config→spec From impls; \
         a bare `TakeSpec` import does not satisfy this"
    );
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
fn helper_fn_matcher_counts_private_fns_but_not_calls_or_suffixed_names() {
    assert!(defines_fn(
        &strip("fn reject_cross_binding_dups() {}"),
        "reject_cross_binding_dups"
    ));
    assert!(defines_fn(
        &strip("pub(crate) fn reject_cross_binding_dups() {}"),
        "reject_cross_binding_dups"
    ));
    assert!(
        !defines_fn(
            &strip("let x = reject_cross_binding_dups(name, &bindings);"),
            "reject_cross_binding_dups"
        ),
        "a call site must not count as a definition"
    );
    assert!(
        !defines_fn(&strip("fn project_binding_input() {}"), "project_binding"),
        "a longer-named sibling must not satisfy the exact fn name"
    );
}

#[test]
fn helper_impl_matcher_extracts_headers_and_bodies() {
    let blocks = impl_blocks(&strip(
        "impl TakeSpec {\n    pub fn from_entries(entries: Option<&[TakeEntry]>) -> Self {\n        \
         Self::ProjectAll\n    }\n}\n\
         impl std::str::FromStr for TargetPath {\n    type Err = ();\n}\n",
    ));
    assert!(
        blocks
            .iter()
            .any(|(header, body)| references_token(header, "TakeSpec")
                && references_token(body, "TakeEntry")),
        "an inherent impl consuming TakeEntry in its body must be visible"
    );
    assert!(
        blocks
            .iter()
            .any(|(header, _)| references_token(header, "FromStr")
                && references_token(header, "TargetPath")),
        "a trait impl header must expose both the trait and the self type"
    );
    assert!(
        impl_blocks(&strip("// impl TakeSpec { fn from_entries() {} }")).is_empty(),
        "a commented-out impl must not count"
    );
    assert!(
        !has_impl_referencing(&strip("fn implement() { let implicit = 1; }"), "TakeSpec"),
        "`impl` inside a longer identifier must not start a block"
    );
    assert!(
        !has_impl_referencing(&strip("impl TargetPathBuf { fn x() {} }"), "TargetPath"),
        "a longer-named self type must not satisfy the exact type name"
    );
    assert!(
        has_impl_referencing(
            &strip("impl From<ProjectionError> for Error { fn from() {} }"),
            "ProjectionError"
        ),
        "a generic-argument mention in the header must count"
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
fn helper_cfg_test_stripper_blanks_only_test_code() {
    let src = "pub struct Keep;\n\
               #[cfg(test)]\nmod tests {\n    use crate::config::TakeEntry;\n    \
               fn helper() { let _ = kernel::Materialization::Leaf; }\n}\n\
               pub struct Also;";
    let scanned = strip_cfg_test(&strip(src));
    assert!(
        references_token(&scanned, "Keep") && references_token(&scanned, "Also"),
        "production items around a test module must survive the stripper"
    );
    assert!(
        !references_token(&scanned, "TakeEntry"),
        "a TakeEntry reference inside a #[cfg(test)] module must not trip the production scans"
    );
    assert!(
        kernel_reference_regions(&scanned).is_empty(),
        "a kernel:: path inside a #[cfg(test)] module must not trip the facade scan"
    );
    let use_form = strip_cfg_test(&strip(
        "#[cfg(test)]\nuse crate::config::TakeEntry;\npub struct Keep;",
    ));
    assert!(
        !references_token(&use_form, "TakeEntry") && references_token(&use_form, "Keep"),
        "a #[cfg(test)] attribute on a use statement must blank only that statement"
    );
}

#[test]
fn helper_kernel_facade_scan_flags_facade_paths_not_the_allowance() {
    let allowance = kernel_reference_regions(&strip(
        "use crate::kernel::{SourceName, safe_relpath};\nfn f() { crate::kernel::safe_relpath(d); }",
    ));
    assert!(
        allowance.iter().all(|region| KERNEL_FACADE_LEAVES
            .iter()
            .all(|leaf| !references_token(region, leaf))),
        "allowance leaves (SourceName/safe_relpath/…) must not be flagged as facade references"
    );
    let grouped = kernel_reference_regions(&strip("use crate::kernel::{OfferSelection, Take};"));
    assert!(
        grouped
            .iter()
            .any(|region| references_token(region, "OfferSelection")
                && references_token(region, "Take")),
        "facade leaves in a braced use group must be visible to the scan"
    );
    let qualified = kernel_reference_regions(&strip("let t = crate::kernel::Take::Glob(leaf);"));
    assert!(
        qualified
            .iter()
            .any(|region| references_token(region, "Take")),
        "a fully-qualified kernel::Take:: body path must be visible to the scan"
    );
    assert!(
        kernel_reference_regions(&strip("// use crate::kernel::OfferSelection;")).is_empty(),
        "a commented-out kernel path must not be scanned"
    );
    let unrelated = kernel_reference_regions(&strip("use crate::kernel::ResolvedTake;"));
    assert!(
        unrelated
            .iter()
            .all(|region| !references_token(region, "Take")),
        "the word-bounded leaf match must not mistake ResolvedTake for Take"
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
