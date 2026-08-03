use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_lib_rs() -> String {
    fs::read_to_string(manifest().join("src/lib.rs")).expect("src/lib.rs must be readable")
}

fn rust_tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index += 2;
            let mut depth = 1_u32;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            continue;
        }
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == b'"' {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            continue;
        }
        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(source[start..index].to_string());
            continue;
        }
        if matches!(bytes[index], b'{' | b'}' | b';') {
            tokens.push(char::from(bytes[index]).to_string());
        }
        index += 1;
    }
    tokens
}

fn forbidden_top_level_public_reexports(source: &str) -> Vec<String> {
    const FORBIDDEN: &[&str] = &["backend", "deploy", "store", "kernel", "SourceBackend"];

    let tokens = rust_tokens(source);
    let mut violations = Vec::new();
    let mut brace_depth = 0_u32;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "{" => brace_depth += 1,
            "}" => brace_depth = brace_depth.saturating_sub(1),
            "pub"
                if brace_depth == 0
                    && tokens.get(index + 1).is_some_and(|token| token == "use") =>
            {
                let end = tokens[index..]
                    .iter()
                    .position(|token| token == ";")
                    .map_or(tokens.len(), |offset| index + offset + 1);
                let statement = &tokens[index..end];
                if statement
                    .iter()
                    .any(|token| FORBIDDEN.contains(&token.as_str()))
                {
                    violations.push(statement.join(" "));
                }
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    violations
}

fn public_top_level_modules(source: &str) -> BTreeSet<String> {
    let tokens = rust_tokens(source);
    let mut modules = BTreeSet::new();
    let mut brace_depth = 0_u32;
    for window in tokens.windows(3) {
        match window[0].as_str() {
            "{" => brace_depth += 1,
            "}" => brace_depth = brace_depth.saturating_sub(1),
            "pub" if brace_depth == 0 && window[1] == "mod" => {
                modules.insert(window[2].clone());
            }
            _ => {}
        }
    }
    modules
}

fn crate_docs(source: &str) -> String {
    let mut docs = Vec::new();
    let mut started = false;
    for line in source.lines() {
        let line = line.trim_start();
        if let Some(doc) = line.strip_prefix("//!") {
            started = true;
            docs.push(doc);
        } else if line.trim().is_empty() {
            if started {
                docs.push("");
            }
        } else {
            break;
        }
    }
    docs.join("\n")
}

const CANONICAL_CRATE_DOCS: &str = "\
Phora is organized as source → projection → sync.
Source obtains immutable content.
Projection is a pure, I/O-free calculation of desired target structure.
Sync is the sole owner of target-side machine state.
Compatibility is guaranteed for the command-line interface and all serialized formats.
The Rust library API is intentionally unstable and may change between releases.";

fn normalize_doc(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn crate_docs_are_canonical(source: &str) -> bool {
    normalize_doc(&crate_docs(source)) == normalize_doc(CANONICAL_CRATE_DOCS)
}

fn as_inner_doc_source(docs: &str) -> String {
    let mut source = String::new();
    for line in docs.lines() {
        source.push_str("//! ");
        source.push_str(line);
        source.push('\n');
    }
    source.push_str("pub mod cli;\n");
    source
}

#[test]
fn t031_lib_rs_exposes_exactly_the_final_ten_modules() {
    let lib = read_lib_rs();
    let actual = public_top_level_modules(&lib);
    let expected: BTreeSet<String> = [
        "cli",
        "config",
        "diagnostic",
        "digest",
        "error",
        "lock",
        "paths",
        "projection",
        "source",
        "sync",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert_eq!(
        actual, expected,
        "src/lib.rs must expose exactly the ten end-state modules; compatibility modules \
         backend/deploy/store/kernel and any other public module are outside the final surface"
    );
    let forbidden_reexports = forbidden_top_level_public_reexports(&lib);
    assert!(
        forbidden_reexports.is_empty(),
        "src/lib.rs must not recreate a retired public surface through aliases or re-exports; \
         forbidden top-level `pub use` statements: {forbidden_reexports:?}"
    );
}

#[test]
fn t031_public_surface_oracle_rejects_forbidden_aliases_and_reexports() {
    for source in [
        "pub use source as backend;",
        "pub use crate::sync as deploy;",
        "pub use crate::source::{self as store};",
        "pub use crate::source::SourceBackend;",
    ] {
        assert!(
            !forbidden_top_level_public_reexports(source).is_empty(),
            "forbidden public compatibility surface must be detected: {source}"
        );
    }
    assert!(
        forbidden_top_level_public_reexports(
            "// pub use source as backend;\n\
         const TEXT: &str = \"pub use source as backend;\";\n\
         pub(crate) use source as backend;\n\
         mod private { pub use crate::source as backend; }"
        )
        .is_empty()
    );
}

#[test]
fn t031_retired_top_level_capability_paths_are_absent() {
    let src = manifest().join("src");
    let lingering: Vec<String> = [
        "backend.rs",
        "deploy.rs",
        "store.rs",
        "backend/mod.rs",
        "deploy/mod.rs",
        "store/mod.rs",
        "kernel.rs",
        "kernel/mod.rs",
    ]
    .into_iter()
    .filter(|path| src.join(path).exists())
    .map(str::to_string)
    .collect();
    assert!(
        lingering.is_empty(),
        "the final source tree must not retain top-level backend/deploy/store/kernel \
         compatibility paths; still present: {lingering:?}"
    );
}

#[test]
fn t031_crate_docs_match_the_canonical_final_contract() {
    let actual = crate_docs(&read_lib_rs());
    assert_eq!(
        normalize_doc(&actual),
        normalize_doc(CANONICAL_CRATE_DOCS),
        "the leading src/lib.rs crate-doc block must exactly match the canonical T031 \
         capability and compatibility contract after whitespace normalization"
    );
}

#[test]
fn t031_crate_doc_golden_rejects_contradictions_swaps_and_nested_docs() {
    let canonical = as_inner_doc_source(CANONICAL_CRATE_DOCS);
    assert!(crate_docs_are_canonical(&canonical));

    let contradiction = as_inner_doc_source(&format!(
        "{CANONICAL_CRATE_DOCS}\nProjection performs target-side I/O."
    ));
    assert!(!crate_docs_are_canonical(&contradiction));

    let swapped = CANONICAL_CRATE_DOCS
        .replace("Source obtains immutable content.", "__SOURCE__")
        .replace(
            "Sync is the sole owner of target-side machine state.",
            "Source obtains immutable content.",
        )
        .replace(
            "__SOURCE__",
            "Sync is the sole owner of target-side machine state.",
        );
    assert!(!crate_docs_are_canonical(&as_inner_doc_source(&swapped)));

    let nested_only = format!(
        "pub mod private {{\n{}\n}}\n",
        CANONICAL_CRATE_DOCS
            .lines()
            .map(|line| format!("    //! {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(crate_docs(&nested_only).is_empty());
    assert!(!crate_docs_are_canonical(&nested_only));
}
