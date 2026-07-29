use std::collections::BTreeSet;
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

fn defines_pub_type(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "pub struct", name) || keyword_names(stripped, "pub enum", name)
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

fn variant_body(enum_body: &str, variant: &str) -> Option<String> {
    let bytes = enum_body.as_bytes();
    enum_body.match_indices(variant).find_map(|(i, hit)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after = i + hit.len();
        let after_ok = bytes
            .get(after)
            .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_');
        if !(before_ok && after_ok) {
            return None;
        }
        let rest = enum_body[after..].trim_start();
        if rest.starts_with('{') {
            balanced_body(rest)
        } else {
            None
        }
    })
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

fn pub_use_statements(stripped: &str) -> Vec<String> {
    stripped
        .split(';')
        .filter_map(|stmt| stmt.find("pub use").map(|i| stmt[i..].to_owned()))
        .collect()
}

fn paragraph_documents_link_exception(paragraph: &str) -> bool {
    let words: Vec<String> = paragraph
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let contains_words = |expected: &[&str]| {
        words.windows(expected.len()).any(|window| {
            window
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual == expected)
        })
    };

    let denies_exception = [
        &["link", "mode", "artifacts", "are", "not", "an", "exception"][..],
        &[
            "link",
            "mode",
            "artifacts",
            "are",
            "not",
            "the",
            "exception",
        ][..],
        &["no", "link", "mode", "artifacts", "are", "the", "exception"][..],
        &[
            "snapshot",
            "immutability",
            "has",
            "no",
            "link",
            "mode",
            "exception",
        ][..],
    ]
    .iter()
    .any(|denial| contains_words(denial));

    !denies_exception
        && contains_words(&[
            "link",
            "mode",
            "artifacts",
            "are",
            "the",
            "exception",
            "to",
            "snapshot",
            "immutability",
        ])
        && contains_words(&[
            "link",
            "artifacts",
            "materialize",
            "as",
            "symlinks",
            "into",
            "the",
            "live",
            "worktree",
        ])
        && contains_words(&[
            "snapshotid",
            "worktree",
            "freezes",
            "inventory",
            "and",
            "copy",
            "mode",
            "reads",
        ])
}

fn markdown_docs_link_exception(text: &str) -> bool {
    let mut paragraph = String::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if paragraph_documents_link_exception(&paragraph) {
                return true;
            }
            paragraph.clear();
        } else {
            if !paragraph.is_empty() {
                paragraph.push('\n');
            }
            paragraph.push_str(line);
        }
    }
    false
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

/// `tests.rs`/`*_tests.rs` are excluded: their `#[cfg(test)]` attribute sits on
/// the parent's `mod` declaration, invisible to `strip_cfg_test`.
fn prod_src_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    collect_rs_files(&src_dir(), "", &mut out);
    out.retain(|(rel, _)| !(rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs")));
    out
}

const T012_FILES: &[&str] = &["snapshot", "inventory", "resolve"];

const MODEL_TYPES: &[&str] = &[
    "SourcePath",
    "SourceEntryKind",
    "SourceEntryMeta",
    "SourceInventory",
];

const SNAPSHOT_TYPES: &[&str] = &["SnapshotId", "ResolvedSource", "SourceEntry"];

const SNAPSHOT_EXPORTS: &[&str] = &[
    "SnapshotId",
    "ResolvedSource",
    "SourceEntry",
    "SourceStore",
    "capture_worktree",
];

#[test]
fn contract_snapshot_module_files_exist_and_are_declared() {
    let mod_rs = read_src("source/mod.rs");
    for module in T012_FILES {
        let rel = format!("source/{module}.rs");
        assert!(
            src_dir().join(&rel).is_file(),
            "src/{rel} must exist — T012 adds the snapshot layer as snapshot.rs (value types + \
             SourceStore), inventory.rs (the I/O that populates SourceInventory), and resolve.rs \
             (source → ResolvedSource resolution incl. the eager worktree capture)"
        );
        assert!(
            declares_file_module(&mod_rs, module),
            "src/source/mod.rs must declare `mod {module};` so src/{rel} is a live file-backed \
             module, not an orphaned placeholder file"
        );
    }
}

#[test]
fn contract_snapshot_rs_defines_the_snapshot_value_types() {
    let scanned = scan(&read_src("source/snapshot.rs"));
    let missing: Vec<&str> = SNAPSHOT_TYPES
        .iter()
        .copied()
        .filter(|name| !defines_pub_type(&scanned, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/source/snapshot.rs must define the T012 snapshot value types as `pub struct`/\
         `pub enum` items (SnapshotId — the one snapshot representation; ResolvedSource — \
         name + url + snapshot; SourceEntry — captured bytes behind an inventory entry); \
         absent: {missing:?}"
    );
}

#[test]
fn contract_snapshot_id_unifies_git_and_worktree_variants() {
    let scanned = scan(&read_src("source/snapshot.rs"));
    let body = spec_type_body(&scanned, "SnapshotId").unwrap_or_else(|| {
        panic!(
            "src/source/snapshot.rs must define `pub enum SnapshotId {{ … }}` — the single \
             snapshot representation every Git, HTTP, and worktree source resolves to (INV-6)"
        )
    });
    let git = variant_body(&body, "Git").unwrap_or_else(|| {
        panic!(
            "SnapshotId must carry a braced `Git {{ commit }}` variant — git AND url sources \
             share this one representation with unchanged (synthetic) commit ids (INV-6); \
             enum body:\n{body}"
        )
    });
    assert!(
        references_token(&git, "commit"),
        "SnapshotId::Git must carry a `commit` field; variant body:\n{git}"
    );
    let worktree = variant_body(&body, "Worktree").unwrap_or_else(|| {
        panic!(
            "SnapshotId must carry a braced `Worktree {{ … }}` variant — the eager worktree \
             freeze (INV-6); enum body:\n{body}"
        )
    });
    let missing: Vec<&str> = ["root", "head", "capture_digest"]
        .into_iter()
        .filter(|field| !references_token(&worktree, field))
        .collect();
    assert!(
        missing.is_empty(),
        "SnapshotId::Worktree must carry the `root`, `head`, and `capture_digest` fields \
         (design: SnapshotId::Worktree{{root, head, capture_digest}}); absent: {missing:?}; \
         variant body:\n{worktree}"
    );
}

#[test]
fn contract_source_entry_couples_reused_meta_with_bytes() {
    let scanned = scan(&read_src("source/snapshot.rs"));
    let body = spec_type_body(&scanned, "SourceEntry").unwrap_or_else(|| {
        panic!(
            "src/source/snapshot.rs must define `pub struct SourceEntry {{ … }}` — one captured \
             entry: its PR3 SourceEntryMeta plus the frozen bytes"
        )
    });
    assert!(
        references_token(&body, "bytes"),
        "SourceEntry must carry a `bytes` field (the captured content); body:\n{body}"
    );
    assert!(
        references_token(&body, "SourceEntryMeta"),
        "SourceEntry must reuse the PR3 `SourceEntryMeta` value type from source/model.rs \
         (R4-C1: reuse, never redefine); body:\n{body}"
    );
}

#[test]
fn contract_source_store_trait_reads_inventories_and_entries() {
    let scanned = scan(&read_src("source/snapshot.rs"));
    let methods = trait_method_names(&scanned, "SourceStore").unwrap_or_else(|| {
        panic!(
            "src/source/snapshot.rs must define `pub trait SourceStore` — the read port over \
             resolved snapshots that populates the PR3 value types"
        )
    });
    for required in ["inventory", "read"] {
        assert!(
            methods.contains(required),
            "SourceStore must declare fn `{required}` (inventory: snapshot → SourceInventory; \
             read: snapshot + SourcePath → SourceEntry); declared methods: {methods:?}"
        );
    }
    let start = scanned
        .find("pub trait SourceStore")
        .expect("presence checked above");
    let body = balanced_body(&scanned[start..]).expect("trait body checked above");
    for name in [
        "ResolvedSource",
        "SourceInventory",
        "SourceEntry",
        "SourcePath",
    ] {
        assert!(
            references_token(&body, name),
            "SourceStore's method signatures must speak the snapshot/value vocabulary — \
             `{name}` is absent from the trait body:\n{body}"
        );
    }
}

#[test]
fn contract_new_files_reuse_not_redefine_the_model_value_types() {
    for module in T012_FILES {
        let rel = format!("source/{module}.rs");
        let scanned = scan(&read_src(&rel));
        let redefined: Vec<&str> = MODEL_TYPES
            .iter()
            .copied()
            .filter(|name| defines_type(&scanned, name))
            .collect();
        assert!(
            redefined.is_empty(),
            "src/{rel} must REUSE the PR3 value types defined in src/source/model.rs, never \
             redefine them (R4-C1); redefined here: {redefined:?}"
        );
    }
    let snapshot = scan(&read_src("source/snapshot.rs"));
    for name in ["SourceInventory", "SourceEntryMeta", "SourcePath"] {
        assert!(
            references_token(&snapshot, name),
            "src/source/snapshot.rs must consume the PR3 `{name}` from source/model.rs — T012 \
             adds only the I/O that POPULATES the existing value types"
        );
    }
    assert!(
        references_token(&scan(&read_src("source/inventory.rs")), "SourceInventory"),
        "src/source/inventory.rs must reference `SourceInventory` — it holds the I/O that \
         populates the PR3 inventory from a snapshot"
    );
    assert!(
        references_token(&scan(&read_src("source/resolve.rs")), "ResolvedSource"),
        "src/source/resolve.rs must reference `ResolvedSource` — it owns source → \
         ResolvedSource resolution"
    );
}

#[test]
fn contract_capture_worktree_is_a_public_source_fn() {
    let files = prod_src_files();
    let sites: Vec<&str> = files
        .iter()
        .filter(|(_, content)| keyword_names(&scan(content), "pub fn", "capture_worktree"))
        .map(|(rel, _)| rel.as_str())
        .collect();
    assert!(
        sites.len() == 1,
        "exactly one production file must define `pub fn capture_worktree` — the eager, \
         offer-agnostic worktree freeze into the content-addressed cache yielding \
         SnapshotId::Worktree; found at: {sites:?}"
    );
    assert!(
        sites[0] == "source/worktree.rs" || sites[0] == "source/resolve.rs",
        "capture_worktree must live beside the worktree helpers (src/source/worktree.rs) or the \
         resolution layer (src/source/resolve.rs); found in src/{}",
        sites[0]
    );
}

#[test]
fn contract_mod_reexports_the_snapshot_api() {
    let stmts = pub_use_statements(&strip(&read_src("source/mod.rs")));
    let missing: Vec<&str> = SNAPSHOT_EXPORTS
        .iter()
        .copied()
        .filter(|name| !stmts.iter().any(|stmt| references_token(stmt, name)))
        .collect();
    assert!(
        missing.is_empty(),
        "src/source/mod.rs must `pub use` the T012 snapshot API so it resolves at \
         `crate::source::…` (arch-check names `crate::source::SourceStore` as the source-io \
         seam); absent from every pub use statement: {missing:?}"
    );
}

#[test]
fn contract_capture_digest_stays_internal_to_source() {
    let files = prod_src_files();
    let outside: Vec<&str> = files
        .iter()
        .filter(|(rel, content)| {
            !rel.starts_with("source/") && references_token(&scan(content), "capture_digest")
        })
        .map(|(rel, _)| rel.as_str())
        .collect();
    assert!(
        outside.is_empty(),
        "capture_digest is INTERNAL to source: commit ids and lock bytes stay byte-identical \
         (INV-4/INV-6), so no production file outside src/source/ may name it — destructure \
         SnapshotId::Worktree with `..` instead. Offenders: {outside:?}"
    );
    assert!(
        files.iter().any(|(rel, content)| rel.starts_with("source/")
            && references_token(&scan(content), "capture_digest")),
        "src/source/ must carry the capture_digest of the frozen worktree tree \
         (SnapshotId::Worktree{{…, capture_digest}}); no source file names it yet"
    );
}

#[test]
fn contract_link_mode_immutability_exception_is_documented() {
    let doc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/architecture.md");
    let architecture = fs::read_to_string(doc).unwrap_or_default();
    assert!(
        markdown_docs_link_exception(&architecture),
        "the link-mode immutability EXCEPTION (link artifacts materialize as symlinks into the \
         LIVE worktree; SnapshotId::Worktree freezes inventory and copy-mode reads, link \
         artifacts do not freeze) must be documented: one paragraph naming `link` and \
         `exception` in the context of snapshot immutability in docs/architecture.md \
         (codex-R4-H3); none found"
    );
}

#[test]
fn helper_variant_body_extracts_only_the_named_braced_variant() {
    let body = spec_type_body(
        &strip(
            "pub enum SnapshotId {\n    Git { commit: String },\n    Worktree { root: PathBuf, \
             head: String, capture_digest: String },\n}",
        ),
        "SnapshotId",
    )
    .expect("enum body extracted");
    let worktree = variant_body(&body, "Worktree").expect("braced variant extracted");
    assert!(
        references_token(&worktree, "capture_digest") && !references_token(&worktree, "commit"),
        "a variant body must hold its own fields only, got:\n{worktree}"
    );
    let git = variant_body(&body, "Git").expect("braced variant extracted");
    assert!(
        references_token(&git, "commit") && !references_token(&git, "root"),
        "a variant body must stop at its own closing brace, got:\n{git}"
    );
    assert!(
        variant_body(&body, "Work").is_none(),
        "a variant-name prefix must not match a longer-named variant"
    );
    assert!(
        variant_body("Unit, Braced { x: u8 }", "Unit").is_none(),
        "a unit variant has no braced body; the next variant's braces must not be claimed"
    );
}

#[test]
fn helper_pub_use_scan_sees_reexports_but_never_definitions() {
    let stmts = pub_use_statements(&strip(
        "mod snapshot;\npub use snapshot::{SnapshotId, SourceStore};\n\
         pub fn capture_worktree() {}\npub use worktree::read_local_head;",
    ));
    assert!(
        stmts
            .iter()
            .any(|stmt| references_token(stmt, "SnapshotId")
                && references_token(stmt, "SourceStore")),
        "grouped re-exports must be visible to the scan"
    );
    assert!(
        !stmts
            .iter()
            .any(|stmt| references_token(stmt, "capture_worktree")),
        "a definition is not a re-export: `pub fn capture_worktree` must not satisfy the scan"
    );
    assert!(
        pub_use_statements(&strip("// pub use snapshot::SnapshotId;")).is_empty(),
        "a commented-out re-export must not count"
    );
}

#[test]
fn helper_link_exception_scan_is_word_bounded_and_paragraph_scoped() {
    assert!(markdown_docs_link_exception(
        "Link-mode artifacts are the exception to snapshot immutability: link artifacts \
         materialize as symlinks into the live worktree, while SnapshotId::Worktree freezes \
         inventory and copy-mode reads."
    ));
    assert!(
        !markdown_docs_link_exception(
            "Link-mode artifacts are not an exception to snapshot immutability: link artifacts \
             materialize as symlinks into the live worktree, while SnapshotId::Worktree freezes \
             inventory and copy-mode reads."
        ),
        "explicitly denying the link-mode exception must not satisfy the contract"
    );
    assert!(
        !markdown_docs_link_exception(
            "No link-mode artifacts are the exception to snapshot immutability: link artifacts \
             materialize as symlinks into the live worktree, while SnapshotId::Worktree freezes \
             inventory and copy-mode reads."
        ),
        "a `no ... exception` denial must not satisfy the contract"
    );
    assert!(
        !markdown_docs_link_exception(
            "Link-mode artifacts are the exception to snapshot immutability: link artifacts \
             materialize as symlinks into the frozen worktree, while SnapshotId::Worktree keeps \
             inventory and copy-mode reads live."
        ),
        "inverting live link artifacts and frozen copy reads must not satisfy the contract"
    );
    assert!(
        !markdown_docs_link_exception(
            "Symlink-mode artifacts are the exception to snapshot immutability: symlink artifacts \
             materialize as symlinks into the live worktree, while SnapshotId::Worktree freezes \
             inventory and copy-mode reads."
        ),
        "`symlink` must not satisfy the word-bounded `link`"
    );
    assert!(
        !markdown_docs_link_exception(
            "Link-mode artifacts govern snapshot immutability: link artifacts materialize as \
             symlinks into the live worktree, while SnapshotId::Worktree freezes inventory and \
             copy-mode reads."
        ),
        "deleting the `exception` concept must not count"
    );
    assert!(
        !markdown_docs_link_exception(
            "Local artifacts are the exception to snapshot immutability: local artifacts \
             materialize as symlinks into the live worktree, while SnapshotId::Worktree freezes \
             inventory and copy-mode reads."
        ),
        "deleting the `link` concept must not count"
    );
    assert!(
        !markdown_docs_link_exception("a link parser reports an exception"),
        "unrelated adjacent `link` and `exception` tokens must not count"
    );
    assert!(
        !markdown_docs_link_exception(
            "Link-mode artifacts are the exception to snapshot immutability: link artifacts \
             materialize as symlinks into the live worktree.\n\nSnapshotId::Worktree freezes \
             inventory and copy-mode reads."
        ),
        "all three semantic clauses must share one paragraph"
    );
}
