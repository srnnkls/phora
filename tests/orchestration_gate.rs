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
                let mut depth = 1i32;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
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

fn normalize(s: &str) -> String {
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
        if chars[i].is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn scan(src: &str) -> String {
    normalize(&strip(src))
}

fn despace(s: &str) -> String {
    let flat: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    flat.replace(",)", ")")
}

fn strip_comments(src: &str) -> String {
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
                let mut depth = 1i32;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                out.push(' ');
            }
            'r' if is_raw_string_open(&chars, i) => {
                let mut hashes = 0;
                let mut j = i + 1;
                while chars.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                out.push('r');
                for _ in 0..hashes {
                    out.push('#');
                }
                out.push('"');
                j += 1;
                while j < chars.len() {
                    if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
                        break;
                    }
                    out.push(chars[j]);
                    j += 1;
                }
                out.push('"');
                for _ in 0..hashes {
                    out.push('#');
                }
                i = (j + 1 + hashes).min(chars.len());
            }
            '"' => {
                out.push('"');
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < chars.len() {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < chars.len() {
                    out.push('"');
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
    for (i, _) in scanned.match_indices("use ") {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        if !before_ok {
            continue;
        }
        let Some(end) = scanned[i + 4..].find(';') else {
            continue;
        };
        for leaf in expand_tree(scanned[i + 4..i + 4 + end].trim()) {
            let head = leaf.split(" as ").next().unwrap_or(&leaf).trim();
            if !head.is_empty() {
                leaves.push(head.to_owned());
            }
        }
    }
    leaves
}

fn ends_with_word(head: &str, word: &str) -> bool {
    head.strip_suffix(word).is_some_and(|rest| {
        rest.chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
    })
}

fn is_pub_vis(head: &str) -> bool {
    let head = head.trim_end();
    if ends_with_word(head, "pub") {
        return true;
    }
    if !head.ends_with(')') {
        return false;
    }
    let bytes = head.as_bytes();
    let mut depth = 0i32;
    let mut open = None;
    for i in (0..head.len()).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    open.is_some_and(|open| ends_with_word(head[..open].trim_end(), "pub"))
}

fn pub_use_leaves(scanned: &str) -> Vec<String> {
    let mut leaves = Vec::new();
    let bytes = scanned.as_bytes();
    for (i, _) in scanned.match_indices("use ") {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        if !before_ok {
            continue;
        }
        if !is_pub_vis(&scanned[..i]) {
            continue;
        }
        let Some(end) = scanned[i + 4..].find(';') else {
            continue;
        };
        for leaf in expand_tree(scanned[i + 4..i + 4 + end].trim()) {
            let head = leaf.split(" as ").next().unwrap_or(&leaf).trim();
            if !head.is_empty() {
                leaves.push(head.to_owned());
            }
        }
    }
    leaves
}

fn module_aliases(scanned: &str) -> Vec<(String, String)> {
    let bytes = scanned.as_bytes();
    let mut out = Vec::new();
    for (i, _) in scanned.match_indices("use ") {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        if !before_ok {
            continue;
        }
        let Some(end) = scanned[i + 4..].find(';') else {
            continue;
        };
        for leaf in expand_tree(scanned[i + 4..i + 4 + end].trim()) {
            let Some((path, alias)) = leaf.split_once(" as ") else {
                continue;
            };
            let path = path.trim().strip_suffix("::self").unwrap_or(path.trim());
            let alias = alias.trim();
            if !path.is_empty() && !alias.is_empty() {
                out.push((alias.to_owned(), path.to_owned()));
            }
        }
    }
    out
}

fn references_call(scanned: &str, name: &str) -> bool {
    reference_call_count(scanned, name) > 0
}

fn reference_call_count(scanned: &str, name: &str) -> usize {
    let bytes = scanned.as_bytes();
    let needle = format!("{name}(");
    scanned
        .match_indices(&needle)
        .filter(|(i, _)| {
            let before_ok =
                *i == 0 || (!bytes[*i - 1].is_ascii_alphanumeric() && bytes[*i - 1] != b'_');
            before_ok && !ends_with_word(scanned[..*i].trim_end(), "fn")
        })
        .count()
}

fn references_call_or_alias(scanned: &str, name: &str) -> bool {
    references_call(scanned, name)
        || module_aliases(scanned)
            .iter()
            .any(|(alias, path)| final_segment(path) == name && references_call(scanned, alias))
}

fn reference_call_count_or_alias(scanned: &str, name: &str) -> usize {
    reference_call_count(scanned, name)
        + module_aliases(scanned)
            .iter()
            .filter(|(_, path)| final_segment(path) == name)
            .map(|(alias, _)| reference_call_count(scanned, alias))
            .sum::<usize>()
}

fn final_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

fn sync_prod_files(exclude: &[&str]) -> Vec<(String, String)> {
    let dir = src_dir().join("sync");
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        if name == "tests.rs" || name.ends_with("_tests.rs") || exclude.contains(&name.as_str()) {
            continue;
        }
        out.push((name, fs::read_to_string(&path).unwrap_or_default()));
    }
    out
}

const MOVED_PROJECTION_TYPES: &[&str] = &[
    "CollapsePreference",
    "LayoutSpec",
    "LayoutStyle",
    "MaterializationPolicy",
    "OfferSpec",
    "TakeSpec",
];

fn projection_reexport_offenders(scanned: &str) -> Vec<String> {
    let projection_aliases: Vec<String> = module_aliases(scanned)
        .into_iter()
        .filter(|(_, path)| path == "crate::projection" || path.starts_with("crate::projection::"))
        .map(|(alias, _)| alias)
        .collect();
    pub_use_leaves(scanned)
        .into_iter()
        .filter(|leaf| {
            leaf == "crate::projection"
                || leaf.starts_with("crate::projection::")
                || projection_aliases
                    .iter()
                    .any(|alias| leaf.split("::").next() == Some(alias.as_str()))
        })
        .collect()
}

#[test]
fn no_sync_module_re_exports_the_projection_facade() {
    let mut offenders = Vec::new();
    for (name, body) in sync_prod_files(&[]) {
        for leaf in projection_reexport_offenders(&scan(&body)) {
            offenders.push(format!("src/sync/{name}: pub use {leaf}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "T020 retires the sync→projection compat facade: no production sync module may re-export \
         the moved projection symbols (obligation 1 — consumers repoint to crate::projection \
         directly; library-API breaks are allowed). This catches BOTH a direct `pub use \
         crate::projection::…` and a module-alias-laundered `use crate::projection::build as \
         projection_build; pub use projection_build::{{…}}` (the exact form live in src/sync/plan.rs). \
         Found: {offenders:?}"
    );
}

fn types_sourced_from(scanned: &str, root: &str, types: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for leaf in use_leaves(scanned) {
        if leaf.starts_with(&format!("{root}::")) && types.contains(&final_segment(&leaf)) {
            hits.push(leaf);
        }
    }
    for (alias, path) in module_aliases(scanned) {
        if path == root || path.starts_with(&format!("{root}::")) {
            for ty in types {
                if references_token(scanned, &format!("{alias}::{ty}")) {
                    hits.push(format!("{alias}::{ty} (alias -> {path})"));
                }
            }
        }
    }
    hits
}

#[test]
fn config_imports_moved_projection_types_from_projection_not_sync() {
    let scanned = scan_prod(&read_src("config/mod.rs"));

    let sync_sourced = types_sourced_from(&scanned, "crate::sync", MOVED_PROJECTION_TYPES);
    assert!(
        sync_sourced.is_empty(),
        "src/config/mod.rs must not import the moved projection value types (TakeSpec and its \
         layout/offer siblings) from crate::sync — that path is the retiring facade (obligation \
         1), including any module-aliased `use crate::sync as …; …::TakeSpec` laundering. Repoint \
         them to crate::projection::model; still sync-sourced: {sync_sourced:?}"
    );

    let take_spec_from_projection =
        !types_sourced_from(&scanned, "crate::projection", &["TakeSpec"]).is_empty();
    assert!(
        take_spec_from_projection,
        "src/config/mod.rs must import TakeSpec from crate::projection::model (its post-move \
         home) — the `impl TakeSpec` block in config binds against the relocated type, not the \
         sync facade."
    );
}

#[test]
fn reconcile_has_a_production_caller_in_sync() {
    let callers: Vec<String> = sync_prod_files(&["reconcile.rs"])
        .into_iter()
        .filter(|(_, body)| references_call_or_alias(&scan_prod(body), "reconcile"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        !callers.is_empty(),
        "T020 wires the pure `reconcile` live: some production sync module (the orchestrator, \
         not reconcile.rs itself) must CALL `reconcile(…)`. Today reconcile has no production \
         caller and SyncError::UnmatchedArtifact is production-unreachable (obligation 5). No \
         `reconcile(` call was found outside src/sync/reconcile.rs"
    );
}

#[test]
fn inspect_has_a_production_caller_in_sync() {
    let callers: Vec<String> = sync_prod_files(&["inspect.rs"])
        .into_iter()
        .filter(|(_, body)| references_call_or_alias(&scan_prod(body), "inspect"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        !callers.is_empty(),
        "T020 makes sync/inspect.rs's `inspect` the single observation pass feeding reconcile \
         (obligation 2): some production sync module must CALL `inspect(…)`. Today no production \
         module wires it — the double walk still classifies via check_artifact_state. No \
         `inspect(` call was found outside src/sync/inspect.rs"
    );
}

#[test]
fn helper_pub_use_leaf_scan_separates_pub_from_plain_and_expands_groups() {
    let src = "use crate::projection::model::Projection;\n\
               pub use crate::projection::build::{project_binding, project_target};\n\
               pub(crate) use crate::projection::model::TargetPath;\n";
    let leaves = pub_use_leaves(&scan(src));
    assert!(
        leaves.contains(&"crate::projection::build::project_binding".to_owned())
            && leaves.contains(&"crate::projection::build::project_target".to_owned())
            && leaves.contains(&"crate::projection::model::TargetPath".to_owned()),
        "pub use group members and pub(crate) leaves must be extracted, got {leaves:?}"
    );
    assert!(
        !leaves.contains(&"crate::projection::model::Projection".to_owned()),
        "a PLAIN (non-pub) use must not be reported as a facade re-export, got {leaves:?}"
    );
}

#[test]
fn helper_whitespace_split_fq_path_normalizes() {
    let leaves = pub_use_leaves(&scan("pub use crate :: projection :: model :: TakeSpec;"));
    assert_eq!(
        leaves,
        ["crate::projection::model::TakeSpec"],
        "`::`-whitespace must collapse so a spaced facade path is still caught, got {leaves:?}"
    );
}

#[test]
fn helper_call_scan_ignores_module_declarations_and_matches_calls() {
    assert!(
        !references_call(&scan("pub mod reconcile;"), "reconcile"),
        "a `mod reconcile;` declaration is not a call"
    );
    assert!(
        !references_call(&scan("use crate::sync::reconcile::reconcile;"), "reconcile"),
        "a use-import of the name is not a call"
    );
    assert!(
        references_call(&scan("let set = reconcile(&p, &o, &policy)?;"), "reconcile"),
        "a bare `reconcile(` call must be detected"
    );
    assert!(
        references_call(&scan("reconcile :: reconcile(&p)"), "reconcile"),
        "a path-qualified `reconcile::reconcile(` call must be detected after normalization"
    );
    assert!(
        !references_call(&scan("myreconcile(x)"), "reconcile"),
        "a longer identifier ending in the name must not match (word boundary before)"
    );
}

#[test]
fn helper_use_leaf_alias_and_final_segment() {
    let leaves = use_leaves(&scan("use crate::sync::{TakeSpec, OfferSpec as Offer};"));
    assert!(
        leaves.contains(&"crate::sync::TakeSpec".to_owned())
            && leaves.contains(&"crate::sync::OfferSpec".to_owned()),
        "grouped leaves with an `as` alias stripped must be extracted, got {leaves:?}"
    );
    assert_eq!(
        final_segment("crate::projection::model::TakeSpec"),
        "TakeSpec"
    );
    assert_eq!(final_segment("TakeSpec"), "TakeSpec");
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
            None => {
                out.truncate(start);
            }
        }
    }
    out
}

fn scan_prod(src: &str) -> String {
    normalize(&strip_cfg_test(&strip(src)))
}

fn references_token(scanned: &str, token: &str) -> bool {
    let bytes = scanned.as_bytes();
    scanned.match_indices(token).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after = i + token.len();
        let after_ok =
            after >= bytes.len() || (!bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_');
        before_ok && after_ok
    })
}

fn keyword_names(scanned: &str, keyword: &str, name: &str) -> bool {
    let bytes = scanned.as_bytes();
    scanned.match_indices(keyword).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let rest_all = &scanned[i + keyword.len()..];
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

fn constructs_struct(scanned: &str, name: &str) -> bool {
    let bytes = scanned.as_bytes();
    let needle = format!("{name} {{");
    let glued = format!("{name}{{");
    [needle, glued].iter().any(|n| {
        scanned
            .match_indices(n.as_str())
            .any(|(i, _)| i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_'))
    })
}

fn sync_file_exists(name: &str) -> bool {
    src_dir().join("sync").join(name).is_file()
}

const S1_PURE_BUILDER: &str = "build_workspace";
const S2_OBSERVER: &str = "observe_workspace";

#[test]
fn pure_workspace_builder_exists_in_projection_build() {
    let build = scan(&read_src("projection/build.rs"));
    assert!(
        keyword_names(&build, "pub fn", S1_PURE_BUILDER),
        "S1: src/projection/build.rs must define `pub fn {S1_PURE_BUILDER}` — the separately-named \
         PURE workspace builder taking only values+inventories (arch_check already enforces its \
         purity). Not found."
    );
    let build_ds = despace(&build);
    let sig = despace(
        "pub fn build_workspace(targets: &[WorkspaceTargetInput<'_>]) \
         -> Result<Projection, ProjectionError>",
    );
    assert!(
        build_ds.contains(&sig),
        "S1/H: build.rs's builder must carry the ruled signature (whitespace-normalized): \
         `pub fn build_workspace(targets: &[WorkspaceTargetInput<'_>]) -> Result<Projection, \
         ProjectionError>` — takes only the resolved per-target value/inventory inputs (no config, \
         no backend, no registry) and returns the pure Projection."
    );

    let model = scan(&read_src("projection/model.rs"));
    assert!(
        keyword_names(&model, "pub struct", "WorkspaceTargetInput"),
        "S1: src/projection/model.rs must define `pub struct WorkspaceTargetInput` — the per-target \
         value/inventory input the pure builder consumes. Not found."
    );
    let model_ds = despace(&model);
    for fragment in [
        "pub struct WorkspaceTargetInput<'a>",
        "pub target: &'a str",
        "pub bindings: Vec<BindingProjectionInput<'a>>",
    ] {
        assert!(
            model_ds.contains(&despace(fragment)),
            "S1/H: WorkspaceTargetInput must be `pub struct WorkspaceTargetInput<'a>` carrying \
             `pub target: &'a str` and `pub bindings: Vec<BindingProjectionInput<'a>>` — missing \
             `{fragment}`."
        );
    }
}

#[test]
fn sync_orchestrator_calls_the_pure_builder_and_builds_no_projection_itself() {
    let callers: Vec<String> = sync_prod_files(&[])
        .into_iter()
        .filter(|(_, body)| references_call_or_alias(&scan_prod(body), S1_PURE_BUILDER))
        .map(|(name, _)| name)
        .collect();
    assert!(
        !callers.is_empty(),
        "S1: sync's project_workspace (the I/O orchestrator) must CALL `{S1_PURE_BUILDER}(` — \
         resolve+discover+build inputs sync-side, then delegate the pure mapping to \
         projection::build. No caller found in production sync."
    );
    let constructors: Vec<String> = sync_prod_files(&[])
        .into_iter()
        .filter(|(_, body)| constructs_struct(&scan_prod(body), "Projection"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        constructors.is_empty(),
        "S1: no production sync module may construct the top-level `Projection {{ … }}` (nor \
         aggregate its warnings) — the workspace Projection is assembled purely inside \
         {S1_PURE_BUILDER}. Still sync-constructed in: {constructors:?}"
    );
}

#[test]
fn observation_producer_module_exists_and_is_wired() {
    assert!(
        sync_file_exists("observe.rs"),
        "S2: the single observation pass lives in a NEW src/sync/observe.rs (observe_workspace). \
         The file does not exist."
    );
    let callers: Vec<String> = sync_prod_files(&["observe.rs"])
        .into_iter()
        .filter(|(_, body)| references_call_or_alias(&scan_prod(body), S2_OBSERVER))
        .map(|(name, _)| name)
        .collect();
    assert!(
        !callers.is_empty(),
        "S2: some production sync module must CALL `{S2_OBSERVER}(` — the one observation pass \
         producing ObservedProjectState for reconcile. No caller found."
    );
}

#[test]
fn check_artifact_state_is_called_only_from_inspect_in_production_sync() {
    let offenders: Vec<String> = sync_prod_files(&[])
        .into_iter()
        .filter(|(name, _)| name != "inspect.rs")
        .filter(|(_, body)| references_call(&scan_prod(body), "check_artifact_state"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "S2 double-walk retirement: in production sync (cfg(test) excluded), \
         `check_artifact_state(` may be called ONLY from src/sync/inspect.rs — the single \
         observation pass. Today target.rs's preflight_entry and apply_entry both call it \
         (the two walks that must collapse). Still calling outside inspect.rs: {offenders:?}"
    );
}

#[test]
fn stage_bridge_is_retired_from_target_and_sync_surface() {
    let target = scan(&read_src("sync/target.rs"));
    assert!(
        !keyword_names(&target, "pub struct", "StageBridge"),
        "S3: src/sync/target.rs must define no `pub struct StageBridge` — the PR6 adapter retires; \
         deploy/apply consume Projection values directly."
    );
    assert!(
        !references_token(
            &scan_prod(&read_src("sync/target.rs")),
            "deploy_artifact_entry"
        ),
        "S3: the cfg(test) `deploy_artifact_entry` shim in target.rs must be gone once its call \
         site dies with the bridge."
    );
    let mod_reexports: Vec<String> = pub_use_leaves(&scan(&read_src("sync/mod.rs")))
        .into_iter()
        .filter(|leaf| final_segment(leaf) == "StageBridge")
        .collect();
    assert!(
        mod_reexports.is_empty(),
        "S3: src/sync/mod.rs must not `pub use …StageBridge` — the bridge is not on sync's public \
         surface after retirement. Still re-exported: {mod_reexports:?}"
    );
}

#[test]
fn apply_side_missing_decision_errs_with_unresolved_conflict() {
    let code = strip_comments(&read_src("sync/target.rs"));
    assert!(
        code.contains("unresolved conflict"),
        "S5/R6: the apply-side consumption of a Conflict row with no preflight decision must map \
         to an Err whose message contains \"unresolved conflict\" — never warn+skip. That \
         diagnostic string literal is absent from src/sync/target.rs's code (comments stripped; \
         string interiors preserved, so a comment mention cannot satisfy this — the real Err \
         message must exist). Textual pin: it proves the Err path was ADDED, not that the old \
         warn+skip fallback was fully removed; the behavioral R6 test is scheduled for the \
         post-Phase-B tester touch once the single-pass apply seam exists."
    );
}

const REPROJECTION_ENTRY_POINTS: &[&str] = &[
    "plan_target",
    "project_workspace",
    "project_binding",
    "project_target",
];

fn reprojection_calls(rel: &str) -> Vec<&'static str> {
    let scanned = scan_prod(&read_src(rel));
    REPROJECTION_ENTRY_POINTS
        .iter()
        .copied()
        .filter(|entry| references_call_or_alias(&scanned, entry))
        .collect()
}

#[test]
fn target_no_longer_reprojects_the_workspace() {
    let reprojectors = reprojection_calls("sync/target.rs");
    assert!(
        reprojectors.is_empty(),
        "G/consumer-seam: src/sync/target.rs production code must not re-project — today \
         walk_target calls plan_target; post-T020 `target` consumes the ONE threaded Projection \
         (INV-7). No re-projection entry point ({REPROJECTION_ENTRY_POINTS:?}, build_workspace \
         excepted) may be called (alias-resolved). Still re-projecting via: {reprojectors:?}"
    );
}

#[test]
fn prune_no_longer_reprojects_the_workspace() {
    let reprojectors = reprojection_calls("sync/prune.rs");
    assert!(
        reprojectors.is_empty(),
        "G/consumer-seam: src/sync/prune.rs production code must not re-project — today \
         expected_live_paths and prune_orphans each call project_workspace; post-T020 prune's key \
         set derives from the one shared Projection (INV-7). No re-projection entry point \
         ({REPROJECTION_ENTRY_POINTS:?}, build_workspace excepted) may be called (alias-resolved). \
         Still re-projecting via: {reprojectors:?}"
    );
}

#[test]
fn preview_no_longer_reprojects_the_workspace() {
    let reprojectors = reprojection_calls("sync/preview.rs");
    assert!(
        reprojectors.is_empty(),
        "G/consumer-seam: src/sync/preview.rs production code must not re-project — today it calls \
         project_binding; post-T020 preview builds WorkspaceTargetInputs from the resolvable \
         bindings and consumes build_workspace's Projection (keeping its NotLocked/NeedsSync \
         annotations for the rest). No re-projection entry point ({REPROJECTION_ENTRY_POINTS:?}, \
         build_workspace excepted) may be called (alias-resolved). Still re-projecting via: \
         {reprojectors:?}"
    );
}

#[test]
fn one_workspace_projection_is_threaded_across_the_entire_sync_run() {
    let call_sites: Vec<(String, usize)> = sync_prod_files(&[])
        .into_iter()
        .filter_map(|(name, body)| {
            let calls = reference_call_count_or_alias(&scan_prod(&body), "project_workspace");
            (calls > 0).then_some((name, calls))
        })
        .collect();
    let total: usize = call_sites.iter().map(|(_, calls)| calls).sum();
    assert_eq!(
        total, 1,
        "T021 one-Projection invariant: a sync run must call project_workspace exactly once, then \
         thread that Projection through fast-forward/drop guarding, deploy/observe/reconcile/apply, \
         and prune. Function/module aliases are resolved. Found {total} production call sites: \
         {call_sites:?}"
    );
}

#[test]
fn test_only_legacy_orchestration_surface_is_fully_retired() {
    const LEGACY: &[&str] = &[
        "deploy_artifact_entry",
        "preflight_entry",
        "apply_entry",
        "conflict_kind_for",
        "skips_redeploy",
        "prune_orphans",
    ];
    let dir = src_dir().join("sync");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read src/sync")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    paths.sort();

    let mut offenders = Vec::new();
    for path in paths {
        let scanned = scan(&fs::read_to_string(&path).expect("read sync Rust source"));
        for symbol in LEGACY {
            if references_token(&scanned, symbol) {
                offenders.push(format!(
                    "src/sync/{}: {symbol}",
                    path.file_name()
                        .expect("Rust file has a name")
                        .to_string_lossy()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "T021 must migrate tests onto the live projection/reconcile seams and delete the legacy \
         orchestration surface. Whole-word scanning catches direct declarations/calls plus \
         `use … as` and `pub use … as` laundering. Still present: {offenders:?}"
    );

    let target = scan(&read_src("sync/target.rs"));
    let start = target
        .find("struct TargetRun")
        .expect("target.rs defines TargetRun");
    let open = target[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("TargetRun has a body");
    let close = matching_brace(target.as_bytes(), open).expect("TargetRun body is balanced");
    let body = &target[open + 1..close];
    let dead_fields: Vec<&str> = ["force", "interactive", "resolver"]
        .into_iter()
        .filter(|field| references_token(body, field))
        .collect();
    assert!(
        dead_fields.is_empty(),
        "TargetRun must not retain cfg_attr(not(test))-laundered fields used only by the retired \
         single-entry shim. Still present: {dead_fields:?}"
    );
}

#[test]
fn helper_keyword_and_construct_scans_are_word_bounded() {
    assert!(keyword_names(
        &scan("pub fn build_workspace(x: u8) {}"),
        "pub fn",
        "build_workspace"
    ));
    assert!(!keyword_names(
        &scan("pub fn build_workspaces() {}"),
        "pub fn",
        "build_workspace"
    ));
    assert!(constructs_struct(
        &scan("Ok(Projection { targets, warnings })"),
        "Projection"
    ));
    assert!(
        !constructs_struct(&scan("let t = TargetProjection { target };"), "Projection"),
        "TargetProjection construction must not read as a top-level Projection construction"
    );
    assert!(
        references_token(
            &scan("let _ = check_artifact_state;"),
            "check_artifact_state"
        ),
        "references_token matches a whole-word token"
    );
    assert!(
        !references_token(
            &scan("let _ = my_check_artifact_state_wrapper;"),
            "check_artifact_state"
        ),
        "references_token must not match inside a longer identifier"
    );
}

#[test]
fn helper_strip_cfg_test_removes_gated_calls() {
    let src = "fn f() { real_call(); }\n#[cfg(test)]\nmod t { fn g() { gated_call(); } }\n";
    let scanned = scan_prod(src);
    assert!(
        references_call(&scanned, "real_call"),
        "a production call must survive strip_cfg_test"
    );
    assert!(
        !references_call(&scanned, "gated_call"),
        "a call inside a #[cfg(test)] module must be stripped"
    );
}

#[test]
fn helper_caller_pin_ignores_cfg_test_calls() {
    let gated = "pub fn f() {}\n#[cfg(test)]\nmod t {\n    fn g() { build_workspace(&x); }\n}\n";
    assert!(
        !references_call(&scan_prod(gated), "build_workspace"),
        "a caller pin over scan_prod must NOT be satisfied by a #[cfg(test)]-gated call — this is \
         the cfg(test)-laundering hole the caller pins close by using scan_prod"
    );
    let prod = "pub fn f() { build_workspace(&x); }\n";
    assert!(
        references_call(&scan_prod(prod), "build_workspace"),
        "a genuine production call must still be seen"
    );
}

#[test]
fn helper_pub_use_recognizes_all_visibility_forms() {
    for vis in [
        "pub",
        "pub(crate)",
        "pub(super)",
        "pub(in crate)",
        "pub(in crate::foo)",
    ] {
        let src = format!("{vis} use crate::projection::model::TakeSpec;");
        assert!(
            pub_use_leaves(&scan(&src))
                .iter()
                .any(|leaf| leaf == "crate::projection::model::TakeSpec"),
            "visibility form `{vis} use` must count as a re-export"
        );
    }
    assert!(
        pub_use_leaves(&scan("use crate::projection::model::TakeSpec;")).is_empty(),
        "a plain (private) use is not a re-export"
    );
    assert!(
        pub_use_leaves(&scan("let pubx = use_case();")).is_empty(),
        "an identifier ending in `pub` must not read as a `pub use`"
    );
}

#[test]
fn helper_module_alias_laundering_is_resolved() {
    let sync_alias = scan_prod(
        "use crate::sync as compat;\npub fn f() { let _ = compat::TakeSpec::ProjectAll; }\n",
    );
    assert!(
        !types_sourced_from(&sync_alias, "crate::sync", &["TakeSpec"]).is_empty(),
        "`use crate::sync as compat; …; compat::TakeSpec` must resolve as sourcing TakeSpec from \
         crate::sync"
    );
    let projection_alias = scan_prod(
        "use crate::projection::model as pm;\npub fn f() { let _ = pm::TakeSpec::ProjectAll; }\n",
    );
    assert!(
        !types_sourced_from(&projection_alias, "crate::projection", &["TakeSpec"]).is_empty(),
        "an alias of crate::projection::model must resolve as sourcing from crate::projection"
    );
    assert!(
        types_sourced_from(&sync_alias, "crate::projection", &["TakeSpec"]).is_empty(),
        "a crate::sync alias must not be miscounted as a projection source"
    );
}

#[test]
fn helper_strip_handles_nested_block_comments() {
    let one = scan("/* outer /* inner */ still comment */ real_call();");
    assert!(
        references_call(&one, "real_call"),
        "a nested block comment must be fully consumed, leaving the trailing call visible"
    );
    let two = scan("/* /* */ hidden_call(); */ after_call();");
    assert!(
        !references_call(&two, "hidden_call"),
        "a call nested inside a doubled block comment must be stripped (depth-tracked), not \
         exposed by stopping at the first `*/`"
    );
    assert!(
        references_call(&two, "after_call"),
        "the call after the nested comment closes must remain visible"
    );
}

#[test]
fn helper_strip_comments_keeps_strings_drops_comments() {
    assert!(
        !strip_comments("// unresolved conflict\npub fn f() {}\n").contains("unresolved conflict"),
        "a comment-only occurrence must be removed"
    );
    assert!(
        !strip_comments("/* unresolved conflict */\npub fn f() {}\n")
            .contains("unresolved conflict"),
        "a block-comment occurrence must be removed"
    );
    assert!(
        strip_comments("pub fn f() -> E { err(\"unresolved conflict\") }\n")
            .contains("unresolved conflict"),
        "a real string-literal message must survive strip_comments"
    );
}

#[test]
fn helper_references_call_rejects_declarations_and_resolves_aliases() {
    assert!(
        !references_call(&scan("pub fn build_workspace(x: u8) {}"), "build_workspace"),
        "a `fn name(` DECLARATION must not count as a call"
    );
    assert!(
        !references_call(
            &scan("trait T { fn observe_workspace(&self); }"),
            "observe_workspace"
        ),
        "a trait method DECLARATION must not count as a call"
    );
    assert!(
        references_call(
            &scan("let _ = build_workspace(&inputs);"),
            "build_workspace"
        ),
        "a genuine call must still count"
    );

    let aliased_prod = scan_prod(
        "use crate::sync::reconcile::reconcile as rc;\npub fn f() { let _ = rc(&p, &o, &pol); }\n",
    );
    assert!(
        references_call_or_alias(&aliased_prod, "reconcile"),
        "a call through `use …::reconcile as rc; rc(…)` must count for a positive caller pin"
    );
    assert_eq!(
        reference_call_count_or_alias(
            &scan_prod(
                "use crate::sync::plan::project_workspace as pw;\n\
                 pub fn f() { let _ = pw(&a); let _ = project_workspace(&b); }\n"
            ),
            "project_workspace"
        ),
        2,
        "the one-projection gate must count both a direct call and an alias-laundered call"
    );

    let aliased_banned =
        scan_prod("use crate::sync::plan::plan_target as pt;\npub fn f() { let _ = pt(&t); }\n");
    assert!(
        references_call_or_alias(&aliased_banned, "plan_target"),
        "a banned call laundered through `use …::plan_target as pt; pt(…)` must be caught"
    );

    let grouped_alias =
        scan_prod("use crate::sync::plan::{plan_target as pt};\npub fn f() { let _ = pt(&t); }\n");
    assert!(
        references_call_or_alias(&grouped_alias, "plan_target"),
        "a grouped `use …::{{plan_target as pt}}` call alias must be caught"
    );

    assert!(
        !references_call_or_alias(&scan("pub fn f() {}"), "plan_target"),
        "no call and no alias means no match"
    );
}

#[test]
fn helper_grouped_and_self_module_aliases_are_resolved() {
    let grouped = scan_prod(
        "use crate::{sync as compat};\npub fn f() { let _ = compat::TakeSpec::ProjectAll; }\n",
    );
    assert!(
        !types_sourced_from(&grouped, "crate::sync", &["TakeSpec"]).is_empty(),
        "a grouped `use crate::{{sync as compat}}; compat::TakeSpec` must resolve as sync-sourced"
    );
    let self_alias = scan_prod(
        "use crate::sync::{self as compat};\npub fn f() { let _ = compat::TakeSpec::ProjectAll; }\n",
    );
    assert!(
        !types_sourced_from(&self_alias, "crate::sync", &["TakeSpec"]).is_empty(),
        "a `use crate::sync::{{self as compat}}; compat::TakeSpec` must resolve as sync-sourced"
    );
}

#[test]
fn helper_projection_reexport_catches_alias_laundering() {
    let laundered = scan(
        "use crate::projection::model as projection_model;\n\
         use crate::projection::{build as projection_build, diagnostic as projection_diagnostic};\n\
         pub use projection_build::{project_binding, project_target, projected_artifact_keys};\n\
         pub use projection_diagnostic::{ProjectionError, ProjectionWarning};\n\
         pub use projection_model::{Projection, TargetProjection, TakeSpec};\n",
    );
    let offenders = projection_reexport_offenders(&laundered);
    assert!(
        offenders
            .iter()
            .any(|o| o == "projection_build::project_binding")
            && offenders
                .iter()
                .any(|o| o == "projection_model::Projection"),
        "the exact laundered facade in src/sync/plan.rs (`use crate::projection::build as \
         projection_build; pub use projection_build::{{…}}`) must be flagged, got {offenders:?}"
    );

    assert!(
        !projection_reexport_offenders(&scan("pub use crate::projection::model::TargetPath;"))
            .is_empty(),
        "a DIRECT `pub use crate::projection::…` facade must still be flagged"
    );

    let clean = scan(
        "use crate::projection::model::Projection;\n\
         pub use crate::sync::model::ConflictKind;\n\
         pub use super::stage::StageRequest;\n",
    );
    assert!(
        projection_reexport_offenders(&clean).is_empty(),
        "a PRIVATE projection import plus pub re-exports of SYNC-owned items must not be flagged, \
         got {:?}",
        projection_reexport_offenders(&clean)
    );
}
