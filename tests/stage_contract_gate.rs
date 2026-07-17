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

fn impl_spans(stripped: &str) -> Vec<(String, String)> {
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
            let item_position = stripped[..i]
                .chars()
                .rev()
                .find(|c| !c.is_whitespace())
                .is_none_or(|c| matches!(c, ';' | '}' | ']'));
            if !(before_ok && after_ok && item_position) {
                return None;
            }
            let rest = &stripped[i..];
            let open = rest.find('{')?;
            let span = balanced_body(rest)?;
            Some((rest[..open].to_owned(), span))
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

fn contract_impls(stripped: &str) -> Vec<(String, String)> {
    impl_spans(stripped)
        .into_iter()
        .filter(|(header, _)| {
            impl_subject(header).is_some_and(|subject| subject == REQUEST || subject == STAGED)
        })
        .collect()
}

const PHYSICAL_TOKENS: &[&str] = &[
    "symlink_metadata",
    "read_link",
    "is_symlink",
    "canonicalize",
    "metadata",
    "try_exists",
    "exists",
    "read_dir",
    "OpenOptions",
    "File",
];

fn has_physical_check(span: &str) -> bool {
    span.contains("std::fs")
        || span.contains("std::io")
        || span.contains("std::os")
        || span.contains("fs::")
        || PHYSICAL_TOKENS
            .iter()
            .any(|token| references_token(span, token))
}

fn defined_type_names(stripped: &str) -> Vec<String> {
    let bytes = stripped.as_bytes();
    ["pub struct", "pub enum"]
        .into_iter()
        .flat_map(|keyword| {
            stripped.match_indices(keyword).filter_map(move |(i, _)| {
                let before_ok =
                    i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
                if !before_ok {
                    return None;
                }
                let rest = stripped[i + keyword.len()..].trim_start();
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()).then_some(name)
            })
        })
        .collect()
}

fn framed_over(stripped: &str, body: &str, token: &str) -> bool {
    if references_token(body, token) {
        return true;
    }
    defined_type_names(stripped).iter().any(|name| {
        references_token(body, name)
            && spec_type_body(stripped, name).is_some_and(|nested| references_token(&nested, token))
    })
}

fn definers_of(name: &str) -> Vec<String> {
    prod_src_files()
        .into_iter()
        .filter(|(_, content)| defines_pub_type(&scan(content), name))
        .map(|(rel, _)| rel)
        .collect()
}

const STAGE: &str = "sync/stage.rs";
const REQUEST: &str = "StageRequest";
const STAGED: &str = "StagedArtifact";

const PATH_NEWTYPES: &[&str] = &["ArtifactRelativePath", "TargetPath", "SourcePath"];

const CONTRACT_FORBIDDEN_TOKENS: &[&str] = &[
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

fn smuggled_into(body: &str) -> Vec<&'static str> {
    CONTRACT_FORBIDDEN_TOKENS
        .iter()
        .copied()
        .filter(|token| references_token(body, token))
        .collect()
}

#[test]
fn contract_stage_module_exists_and_is_registered_in_sync() {
    assert!(
        !read_src(STAGE).trim().is_empty(),
        "src/{STAGE} must exist: T032 defines the target staging contract THERE, before any \
         staging code moves, so T015 stays move-only (contract-before-move, R2-9/AD-3)"
    );
    assert!(
        keyword_names(&scan(&read_src("sync/mod.rs")), "mod", "stage"),
        "src/sync/mod.rs must declare `mod stage` — the defining task registers the module \
         (blanket scope requirement); an unregistered file never compiles and pins nothing"
    );
}

#[test]
fn contract_stage_defines_the_two_contract_types_exactly_once() {
    for name in [REQUEST, STAGED] {
        let definers = definers_of(name);
        assert_eq!(
            definers,
            [STAGE],
            "exactly src/{STAGE} must define `pub {name}` — the T032 contract lives where the \
             T015 relocation lands, and a second definer anywhere would fork the interface the \
             staged sub-moves re-assert against; found at: {definers:?}"
        );
    }
}

#[test]
fn contract_stage_request_carries_the_bridge_borrows_and_variables() {
    let body = spec_type_body(&scan(&read_src(STAGE)), REQUEST).unwrap_or_else(|| {
        panic!(
            "src/{STAGE} does not define `pub struct {REQUEST}` yet — T032's contract type: the \
             two bridge borrows plus `variables`, and variables is the ONLY extra field; the \
             flow-owned export parameters (url/root/policy/template_opt_in) are passed alongside \
             &{REQUEST} by the T015 staging execution fn, never carried as contract fields"
        )
    });
    for token in ["ProjectedArtifact", "TargetProjection"] {
        assert!(
            references_token(&body, token),
            "{REQUEST} must borrow `{token}` — the T014 StageBridge carries exactly \
             {{ artifact: &ProjectedArtifact, target: &TargetProjection }} so T015's swap from \
             bridge to request is a literal field move; body:\n{body}"
        );
    }
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    for field in ["pub artifact:", "pub target:", "pub variables:"] {
        assert!(
            flat.contains(field),
            "{REQUEST} must expose a public `{field}` field — artifact/target mirror the bridge \
             fields and variables is the run's vars map (ctx.vars at today's ExportRequest build \
             site in sync/target.rs); body:\n{body}"
        );
    }
    let smuggled = smuggled_into(&body);
    assert!(
        smuggled.is_empty(),
        "{REQUEST} is a pure projection-value carrier: config DTOs, registry/manifest types, and \
         source-I/O ports stay flow-owned parameters of the T015 staging execution, never \
         contract fields (INV-2 staging clause); smuggled: {smuggled:?}; body:\n{body}"
    );
}

#[test]
fn contract_staged_artifact_is_framed_over_the_projection_path_newtype() {
    let scanned = scan(&read_src(STAGE));
    let body = spec_type_body(&scanned, STAGED).unwrap_or_else(|| {
        panic!(
            "src/{STAGE} does not define `pub struct {STAGED}` yet — T032's result framing for \
             what export_artifact returns today (per-file manifest entries + artifact digest + \
             vars digest)"
        )
    });
    assert!(
        framed_over(&scanned, &body, "ArtifactRelativePath"),
        "{STAGED} must frame its per-file destinations over ArtifactRelativePath — directly or \
         through a stage.rs-defined entry type — so staged output speaks the PR3/T008 typed path \
         the projection already emits, not a loose PathBuf; body:\n{body}"
    );
    let smuggled = smuggled_into(&body);
    assert!(
        smuggled.is_empty(),
        "{STAGED} frames the staging result in contract terms; registry/manifest and config \
         types are conversion targets at the call site, not contract fields; smuggled: \
         {smuggled:?}; body:\n{body}"
    );
}

#[test]
fn contract_artifact_relative_path_is_reused_never_redefined() {
    let scanned = scan(&read_src(STAGE));
    for name in PATH_NEWTYPES {
        assert!(
            !(keyword_names(&scanned, "struct", name) || keyword_names(&scanned, "enum", name)),
            "src/{STAGE} must not define a type named `{name}` — the path newtypes are owned by \
             projection/source and REUSED by the stage contract (codex-R3-C1: reused, not \
             redefined); a stage-local double would let a source path satisfy a destination \
             parameter again"
        );
        assert!(
            !(keyword_names(&scanned, "type", name) || keyword_names(&scanned, "as", name)),
            "src/{STAGE} must not shadow `{name}` through a `type {name} = ...` alias or a \
             `use ... as {name}` rename — either bypass would let stage.rs name the token while \
             binding it to a different type, defeating the reuse pin (codex-R3-C1)"
        );
    }
    assert!(
        references_token(&scanned, "ArtifactRelativePath"),
        "src/{STAGE} must name ArtifactRelativePath — the contract is defined OVER the existing \
         PR3/T008 newtype from crate::projection::model (codex-R3-C1)"
    );
    let definers = definers_of("ArtifactRelativePath");
    assert_eq!(
        definers,
        ["projection/model.rs"],
        "crate-wide, ArtifactRelativePath keeps exactly one definer — src/projection/model.rs; \
         any second definition (stage.rs included) forks the newtype the whole contract chain \
         shares; found at: {definers:?}"
    );
}

#[test]
fn contract_type_impls_perform_no_physical_checks() {
    let scanned = scan(&read_src(STAGE));
    let offenders: Vec<String> = contract_impls(&scanned)
        .into_iter()
        .filter(|(_, span)| has_physical_check(span))
        .map(|(header, _)| header.trim().to_owned())
        .collect();
    assert!(
        offenders.is_empty(),
        "physical unsafe-symlink-component checks are sync-side STAGING EXECUTION (arriving \
         with T015's relocation as free functions), never behavior of the pure contract types: \
         no impl of {REQUEST}/{STAGED} may probe the filesystem, so their constructors stay \
         lexical and the T003 goldens keep pinning where rejection happens; offenders: \
         {offenders:?}"
    );
}

#[test]
fn contract_sync_reexports_the_stage_contract() {
    let stmts = pub_use_statements(&strip(&read_src("sync/mod.rs")));
    for name in [REQUEST, STAGED] {
        assert!(
            stmts.iter().any(|stmt| references_token(stmt, name)),
            "src/sync/mod.rs must `pub use` {name} so it resolves at `phora::sync::{name}` — \
             sync keeps its modules private and re-exports its API (house style; the behavioral \
             suite and the T016 driver rewire bind this path); pub use statements found: \
             {stmts:?}"
        );
    }
}

#[test]
fn helper_impl_spans_takes_item_impls_and_skips_argument_position_impl_trait() {
    let stripped = "impl<'a> StageRequest<'a> { fn read(&self) -> &str { x } }\n\
                    fn stage(req: &StageRequest, it: impl Iterator) -> StagedArtifact { \
                    std::fs::read(p) }\n\
                    impl std::fmt::Display for StagedArtifact { fn fmt(&self) { y } }\n\
                    impl Other { fn io(&self) { std::fs::write(q) } }";
    let spans = impl_spans(stripped);
    assert_eq!(
        spans.len(),
        3,
        "three item-position impls; the argument-position `impl Iterator` in the free fn must \
         not open a span, got headers: {:?}",
        spans.iter().map(|(h, _)| h.trim()).collect::<Vec<_>>()
    );
    let contract = contract_impls(stripped);
    assert_eq!(
        contract.len(),
        2,
        "the generic inherent impl and the trait impl name contract types; `impl Other` does not"
    );
    assert!(
        contract.iter().all(|(_, span)| !has_physical_check(span)),
        "the free fn's fs body must never be attributed to a contract impl — otherwise T015's \
         relocated staging functions would trip this gate"
    );
}

#[test]
fn helper_contract_impls_classify_by_subject_type_not_header_mention() {
    let drop_impl = "impl Drop for StageRequest<'_> { fn drop(&mut self) { x } }";
    assert_eq!(
        contract_impls(drop_impl).len(),
        1,
        "a trait impl whose SUBJECT is a contract type is a contract impl"
    );
    let ext_impl = "impl StageExt for StageRequest<'_> { fn label(&self) -> &str { y } }";
    assert_eq!(
        contract_impls(ext_impl).len(),
        1,
        "an extension trait implemented ON a contract type is a contract impl"
    );
    let returns = "impl Builder { fn build(&self) -> StagedArtifact { z } }";
    assert!(
        contract_impls(returns).is_empty(),
        "another type's impl merely RETURNING a contract type is not a contract impl"
    );
    let try_from = "impl<'a> TryFrom<StageRequest<'a>> for StagingArgs { \
                    fn try_from(req: StageRequest<'a>) -> R { std::fs::read(p) } }";
    assert!(
        contract_impls(try_from).is_empty(),
        "a conversion impl on ANOTHER type that only mentions a contract type in its generics — \
         the T015-sanctioned pattern — must never be classified as a contract impl, or its fs \
         body would trip the purity pin"
    );
}

#[test]
fn helper_physical_check_detector_flags_fs_probes_and_spares_pure_code() {
    assert!(has_physical_check("{ std::fs::symlink_metadata(path) }"));
    assert!(has_physical_check("{ path.metadata() }"));
    assert!(has_physical_check("{ fs::read_link(p) }"));
    assert!(
        !has_physical_check("{ self.destination.as_str().to_owned() }"),
        "pure accessors carry no physical tokens"
    );
    let stripped = strip("fn doc() { let s = \"std::fs::metadata\"; }");
    assert!(
        !has_physical_check(&stripped),
        "fs tokens inside string literals are blanked before scanning: {stripped}"
    );
}

#[test]
fn helper_framed_over_sees_direct_and_one_hop_nesting_but_not_absence() {
    let nested = strip(
        "pub struct StagedArtifact { pub files: Vec<StagedFile> }\n\
         pub struct StagedFile { pub destination: ArtifactRelativePath }",
    );
    let body = spec_type_body(&nested, STAGED).expect("nested body extracted");
    assert!(
        framed_over(&nested, &body, "ArtifactRelativePath"),
        "a per-file entry type carrying the newtype counts as framing"
    );
    let direct = strip("pub struct StagedArtifact { pub destination: ArtifactRelativePath }");
    let direct_body = spec_type_body(&direct, STAGED).expect("direct body extracted");
    assert!(
        framed_over(&direct, &direct_body, "ArtifactRelativePath"),
        "a direct field of the newtype counts as framing"
    );
    let bare = strip("pub struct StagedArtifact { pub digest: String }");
    let bare_body = spec_type_body(&bare, STAGED).expect("bare body extracted");
    assert!(
        !framed_over(&bare, &bare_body, "ArtifactRelativePath"),
        "a body with no path typing anywhere must not pass"
    );
    assert_eq!(
        defined_type_names(&nested),
        ["StagedArtifact", "StagedFile"],
        "defined-type extraction feeds the one-hop check"
    );
}

#[test]
fn helper_mod_registration_scan_matches_visibility_variants_only_in_code() {
    assert!(keyword_names(&strip("mod stage;"), "mod", "stage"));
    assert!(keyword_names(
        &strip("pub(crate) mod stage;"),
        "mod",
        "stage"
    ));
    assert!(
        !keyword_names(&strip("// mod stage;"), "mod", "stage"),
        "a commented-out declaration must not count"
    );
    assert!(
        !keyword_names(&strip("mod staged;"), "mod", "stage"),
        "a name prefix must not claim registration"
    );
}

#[test]
fn helper_shadow_scan_catches_type_alias_and_use_rename_but_spares_plain_reuse() {
    let alias = strip("pub type ArtifactRelativePath = String;");
    assert!(
        keyword_names(&alias, "type", "ArtifactRelativePath"),
        "a `pub type` alias shadow must be visible to the scan"
    );
    let rename = strip("use std::path::PathBuf as ArtifactRelativePath;");
    assert!(
        keyword_names(&rename, "as", "ArtifactRelativePath"),
        "a `use ... as` rename shadow must be visible to the scan"
    );
    let reuse = strip("use crate::projection::model::ArtifactRelativePath;");
    assert!(
        !keyword_names(&reuse, "type", "ArtifactRelativePath")
            && !keyword_names(&reuse, "as", "ArtifactRelativePath"),
        "the sanctioned plain import must pass the shadow scan"
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
