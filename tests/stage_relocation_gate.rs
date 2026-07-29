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

fn impl_body_ranges(stripped: &str) -> Vec<(usize, usize)> {
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
            let open = stripped[i..].find('{')? + i;
            let close = matching_brace(bytes, open)?;
            Some((open, close))
        })
        .collect()
}

fn blank_impl_bodies(stripped: &str) -> String {
    let mut bytes = stripped.as_bytes().to_vec();
    for (open, close) in impl_body_ranges(stripped) {
        for b in &mut bytes[open + 1..close] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

fn defines_free_fn(stripped: &str, name: &str) -> bool {
    keyword_names(&blank_impl_bodies(stripped), "fn", name)
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

fn files_defining(name: &str, matcher: fn(&str, &str) -> bool) -> Vec<String> {
    prod_src_files()
        .into_iter()
        .filter(|(_, content)| matcher(&scan(content), name))
        .map(|(rel, _)| rel)
        .collect()
}

const STAGE: &str = "sync/stage.rs";

const MOVED_FREE_FNS: &[&str] = &[
    "dest_has_vcs_component",
    "set_deterministic_mtime",
    "symlink_target_escapes",
    "materialize_symlink",
];

const MOVED_METHOD_FNS: &[&str] = &["register_deployed_name"];

const MOVED_TYPES: &[&str] = &["Renderer"];

const SHARED_DIGEST_FNS: &[&str] = &["hash_framed_entry", "vars_digest"];

const TEMPLATE_POLICY_TOKENS: &[&str] = &[
    "set_fuel",
    "UndefinedBehavior",
    "Strict",
    "keep_trailing_newline",
    "render_str",
];

const MATERIALIZATION_TOKENS: &[&str] = &["create_dir_all", "0o111", "set_file_mtime"];

const GIT_READ_TOKENS: &[&str] = &["gix", "GitBackend", "find_blob_data"];

const OLD_PORT_TOKENS: &[&str] = &[
    "ExportRequest",
    "ExportLeaf",
    "ExportResult",
    "ManifestFile",
    "SourceBackend",
    "StateStore",
    "Journal",
    "ArtifactRecord",
    "StageBridge",
];

#[test]
fn contract_stage_defines_the_renderer_and_its_template_policy() {
    let scanned = scan(&read_src(STAGE));
    for name in MOVED_TYPES {
        assert!(
            defines_type(&scanned, name),
            "src/{STAGE} must define `{name}` — T015 relocates the template machinery (the \
             minijinja environment with strict-undefined variables, the 1_000_000 fuel bound, \
             and trailing-newline preservation) out of src/source/mod.rs into the sync-side \
             staging module"
        );
    }
    let missing: Vec<&&str> = TEMPLATE_POLICY_TOKENS
        .iter()
        .filter(|token| !references_token(&scanned, token))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{STAGE} must carry the template policy IN SUBSTANCE, not as a delegating shell: \
         strict undefined variables (UndefinedBehavior::Strict), the render fuel bound \
         (set_fuel), trailing-newline preservation (keep_trailing_newline), and the render call \
         (render_str) all live here after T015; missing tokens: {missing:?}"
    );
}

#[test]
fn contract_stage_defines_the_relocated_staging_helpers() {
    let scanned = scan(&read_src(STAGE));
    let missing: Vec<&&str> = MOVED_FREE_FNS
        .iter()
        .chain(MOVED_METHOD_FNS)
        .filter(|name| !defines_fn(&scanned, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{STAGE} must define the relocated staging helpers — the deployed-name collision \
         check (register_deployed_name), the vcs destination gate (dest_has_vcs_component), \
         deterministic mtimes (set_deterministic_mtime), and the physical symlink validation + \
         materialization pair (symlink_target_escapes, materialize_symlink) — carved from \
         src/source/mod.rs by T015; missing: {missing:?}"
    );
}

#[test]
fn contract_stage_execution_is_a_pub_fn_from_request_to_staged_artifact() {
    let scanned = scan(&read_src(STAGE));
    let sigs = fn_signatures(&scanned);
    let hit = sigs.iter().find(|sig| {
        references_token(sig, "StageRequest") && references_token(sig, "StagedArtifact")
    });
    let Some(sig) = hit else {
        panic!(
            "src/{STAGE} must define the T015 staging execution: a fn whose SIGNATURE takes \
             &StageRequest (with the flow-owned url/root/policy/template_opt_in parameters \
             alongside, per the T032 gate) and yields StagedArtifact — the relocated machinery \
             lands ON the T032 interface, not beside it; signatures found: {sigs:?}"
        );
    };
    assert!(
        references_token(sig, "pub"),
        "the staging execution fn must be `pub` (and re-exported at phora::sync, house style) so \
         the T015 old-vs-new equivalence suite and the T016 driver rewire can bind it; got \
         signature: {sig}"
    );
}

#[test]
fn contract_stage_materializes_bytes_modes_mtimes_and_dirs() {
    let scanned = scan(&read_src(STAGE));
    let missing: Vec<&&str> = MATERIALIZATION_TOKENS
        .iter()
        .filter(|token| !references_token(&scanned, token))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{STAGE} must perform the physical staging after T015: parent-dir materialization \
         (create_dir_all — the staging-dir cluster), the exec-bit mask (0o111), and the \
         deterministic mtime write (set_file_mtime). These live in free fns or non-contract \
         impls — never in an impl of StageRequest/StagedArtifact (T032 purity pin, \
         stage_contract_gate); missing tokens: {missing:?}"
    );
}

#[test]
fn contract_stage_frames_digests_through_the_shared_source_helpers() {
    let scanned = scan(&read_src(STAGE));
    for name in SHARED_DIGEST_FNS {
        assert!(
            references_token(&scanned, name),
            "src/{STAGE} must frame its digests through `{name}` — the SAME fn compute_digest, \
             digest_snapshot, and check_artifact_state use — so the artifact and vars digests \
             stay byte-identical to the T003 goldens by construction, never re-derived from a \
             second framing implementation"
        );
    }
    assert!(
        references_token(&scanned, "blake3"),
        "src/{STAGE} must compute the artifact and per-file digests (blake3) as part of the \
         relocated manifest framing"
    );
}

#[test]
fn contract_relocated_anchors_define_exactly_once_crate_wide() {
    let mut duplicated: Vec<String> = Vec::new();
    for name in MOVED_FREE_FNS.iter().chain(SHARED_DIGEST_FNS) {
        let sites = files_defining(name, defines_free_fn);
        if sites.len() != 1 {
            duplicated.push(format!("free fn {name} in {sites:?}"));
        }
    }
    for name in MOVED_METHOD_FNS {
        let sites = files_defining(name, defines_fn);
        if sites.len() != 1 {
            duplicated.push(format!("fn {name} in {sites:?}"));
        }
    }
    for name in MOVED_TYPES {
        let sites = files_defining(name, defines_type);
        if sites.len() != 1 {
            duplicated.push(format!("struct/enum {name} in {sites:?}"));
        }
    }
    assert!(
        duplicated.is_empty(),
        "every T015 anchor keeps exactly ONE production definition site at every sub-move — \
         mid-relocation it lives in src/source/mod.rs OR src/{STAGE}, never both (a second site \
         means the machinery was copied, not relocated; INV-10's `git log --follow` spot-check \
         is the blame-side twin of this pin). Free fns are counted at module level only, so \
         Renderer's PRIVATE vars_digest method landing in stage.rs never false-duplicates the \
         free source::vars_digest it delegates to: {duplicated:?}"
    );
}

#[test]
fn contract_relocated_anchors_end_in_stage() {
    let mut misplaced: Vec<String> = Vec::new();
    for name in MOVED_FREE_FNS {
        let sites = files_defining(name, defines_free_fn);
        if sites != [STAGE] {
            misplaced.push(format!("free fn {name} at {sites:?}"));
        }
    }
    for name in MOVED_METHOD_FNS {
        let sites = files_defining(name, defines_fn);
        if sites != [STAGE] {
            misplaced.push(format!("fn {name} at {sites:?}"));
        }
    }
    for name in MOVED_TYPES {
        let sites = files_defining(name, defines_type);
        if sites != [STAGE] {
            misplaced.push(format!("struct/enum {name} at {sites:?}"));
        }
    }
    assert!(
        misplaced.is_empty(),
        "the T015 end state places every moved anchor in src/{STAGE} (this pin stays RED \
         through the intermediate sub-moves and goes green at the last one; each sub-move is \
         separately gated by the T003 goldens staying green): {misplaced:?}"
    );
}

#[test]
fn contract_shared_digest_helpers_stay_out_of_sync() {
    for name in SHARED_DIGEST_FNS {
        let sites = files_defining(name, defines_free_fn);
        for site in &sites {
            assert!(
                !site.starts_with("sync/"),
                "`{name}` must NOT be defined under src/sync/ (found in src/{site}): \
                 source-side digest_snapshot (source/snapshot.rs) and compute_digest \
                 (source/git.rs) frame through it FOREVER, and after T016 source may no longer \
                 import sync (INV-2 staging clause) — a sync-side home makes T016 unsatisfiable \
                 without a second move. Stage.rs imports it FROM source (sync → source is the \
                 legal direction); source/ or kernel/ are the valid homes"
            );
        }
    }
}

#[test]
fn contract_template_engine_has_left_the_source_tree() {
    let offenders: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(rel, content)| {
            rel.starts_with("source/") && references_token(&scan(content), "minijinja")
        })
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        offenders.is_empty(),
        "no production file under src/source/ may reference minijinja once T015 completes — the \
         template fuel/strict-variable policy relocates to src/{STAGE} in substance, and the \
         old export path stays byte-identical by DELEGATING to the relocated machinery (a \
         transitional pub(crate) re-export in source/mod.rs is fine; a leftover engine use is a \
         copy): {offenders:?}"
    );
}

#[test]
fn contract_stage_never_reads_git() {
    let scanned = scan(&read_src(STAGE));
    let offenders: Vec<&&str> = GIT_READ_TOKENS
        .iter()
        .filter(|token| references_token(&scanned, token))
        .collect();
    assert!(
        offenders.is_empty(),
        "src/{STAGE} must never read the mirror: T015's source-read commit narrows the boundary \
         so SOURCE resolves each leaf to raw bytes + kind and staging consumes them (AD-3) — \
         this pin holds at EVERY sub-move, so a wholesale ExportWalk move that drags the gix \
         tree lookup into sync is rejected even transitionally; offending tokens: {offenders:?}"
    );
}

#[test]
fn contract_stage_speaks_the_t032_interface_not_the_export_port() {
    let scanned = scan(&read_src(STAGE));
    let offenders: Vec<&&str> = OLD_PORT_TOKENS
        .iter()
        .filter(|token| references_token(&scanned, token))
        .collect();
    assert!(
        offenders.is_empty(),
        "src/{STAGE} must speak only the T032 interface: ExportRequest/ExportLeaf/ExportResult/\
         ManifestFile conversion happens SOURCE-side in the delegating old path (alive until \
         T016), registry/journal/backend ports stay orchestration-owned, and the T014 \
         StageBridge stays in target.rs — this holds at every sub-move; offending tokens: \
         {offenders:?}"
    );
}

#[test]
fn helper_fn_signatures_extracts_item_fns_with_modifiers() {
    let stripped = "use x::Y; pub fn stage(req: &StageRequest<'_>, p: &Policy) -> \
                    Result<StagedArtifact, E> { body } impl T { fn helper(&self) { x } } \
                    trait Q { fn decl(&self); }";
    let sigs = fn_signatures(stripped);
    assert_eq!(
        sigs.len(),
        3,
        "three fn items (free, inherent method, trait decl) must yield three signatures, got: \
         {sigs:?}"
    );
    let execution = sigs
        .iter()
        .find(|sig| {
            references_token(sig, "StageRequest") && references_token(sig, "StagedArtifact")
        })
        .expect("the request→staged signature is found");
    assert!(
        references_token(execution, "pub"),
        "modifiers preceding the fn keyword are part of the extracted signature: {execution}"
    );
    assert!(
        !execution.contains("body"),
        "the body must not leak into the signature: {execution}"
    );
    assert!(
        sigs.iter().any(|sig| references_token(sig, "decl")),
        "a semicolon-terminated trait method declaration still yields its signature: {sigs:?}"
    );
}

#[test]
fn helper_free_fn_matcher_skips_impl_methods_but_keeps_free_fns() {
    let method_file = strip(
        "struct Renderer<'a> { vars: &'a V }\n\
         impl<'a> Renderer<'a> {\n    fn vars_digest(&self) -> String { vars_digest(self.vars) }\n}",
    );
    assert!(
        !defines_free_fn(&method_file, "vars_digest"),
        "an fn inside an impl block is a METHOD, not a module-level definition — Renderer's \
         private vars_digest must not register as a free-fn site when the cluster lands in \
         stage.rs"
    );
    assert!(
        defines_fn(&method_file, "vars_digest"),
        "the any-fn matcher still sees the method (the method-anchored pins rely on it)"
    );

    let free_file = strip("pub fn vars_digest(vars: &BTreeMap<String, String>) -> String { x() }");
    assert!(
        defines_free_fn(&free_file, "vars_digest"),
        "a module-level fn is a free-fn definition site"
    );

    let files = [
        ("source/mod.rs", &free_file),
        ("sync/stage.rs", &method_file),
    ];
    let sites: Vec<&str> = files
        .iter()
        .filter(|(_, content)| defines_free_fn(content, "vars_digest"))
        .map(|(rel, _)| *rel)
        .collect();
    assert_eq!(
        sites,
        ["source/mod.rs"],
        "the exact T015 collision — the free source::vars_digest staying put while Renderer \
         (carrying a same-named private method) relocates to stage.rs — must count exactly ONE \
         free definer, or a pure move would false-fail the exactly-once pin"
    );

    let trait_impl = strip("impl std::fmt::Display for Renderer<'_> { fn fmt(&self) { y } }");
    assert!(
        !defines_free_fn(&trait_impl, "fmt"),
        "trait-impl methods are blanked exactly like inherent ones"
    );
    let free_after_impl = strip("impl R { fn inner(&self) { a } }\nfn outer() { b }");
    assert!(
        defines_free_fn(&free_after_impl, "outer") && !defines_free_fn(&free_after_impl, "inner"),
        "blanking an impl body must not swallow the free fn that follows it"
    );
}

#[test]
fn helper_token_matcher_handles_numeric_and_pathless_tokens() {
    assert!(
        references_token("perms.mode() | 0o111;", "0o111"),
        "the exec-bit mask must match as a standalone token"
    );
    assert!(
        !references_token("let x = 0o1110;", "0o111"),
        "a longer literal must not satisfy the mask token"
    );
    assert!(
        references_token("env.set_fuel(Some(1_000_000));", "set_fuel"),
        "a method call is a token reference"
    );
    let stripped = strip("let s = \"set_fuel inside a string\";");
    assert!(
        !references_token(&stripped, "set_fuel"),
        "tokens inside string literals are blanked before scanning: {stripped}"
    );
    assert!(
        references_token("use minijinja::UndefinedBehavior;", "minijinja"),
        "a use-path segment is a token reference"
    );
}
