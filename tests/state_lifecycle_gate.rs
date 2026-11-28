use std::fs;
use std::path::{Path, PathBuf};

const MOUNT: &str = "mount.rs";
const LIFECYCLE: &str = "sync/state/lifecycle/mod.rs";
const MODEL: &str = "sync/state/lifecycle/model.rs";
const WIRE: &str = "sync/state/lifecycle/wire.rs";

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(rel: &str) -> String {
    fs::read_to_string(src_dir().join(rel))
        .unwrap_or_else(|err| panic!("src/{rel} must exist for the lifecycle data model: {err}"))
}

fn tokens(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c == '\'' && (chars.get(i + 1) == Some(&'\\') || chars.get(i + 2) == Some(&'\''))
        {
            i += 2;
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            i += 1;
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            if !c.is_whitespace() {
                out.push(c.to_string());
            }
            i += 1;
        }
    }
    out
}

fn declaration(tokens: &[String], keyword: &str, name: &str) -> Option<usize> {
    tokens
        .windows(2)
        .position(|pair| pair[0] == keyword && pair[1] == name)
}

fn closing(tokens: &[String], open: usize) -> usize {
    let mut depth = 0_i32;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        match token.as_str() {
            "{" => depth += 1,
            "}" => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src tree must be readable") {
        let path = entry.expect("src entry must be readable").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn wire_dtos_stay_private_and_domain_types_derive_no_deserialize() {
    let wire = tokens(&read_src(WIRE));
    for (keyword, name) in [
        ("struct", "ProjectMetaWire"),
        ("enum", "PathWire"),
        ("struct", "StateRootIndexWire"),
        ("struct", "RegisteredRootWire"),
    ] {
        let at = declaration(&wire, keyword, name)
            .unwrap_or_else(|| panic!("{WIRE} must declare `{keyword} {name}`"));
        let before = at.checked_sub(1).map(|i| wire[i].as_str());
        assert!(
            !matches!(before, Some("pub" | ")")),
            "wire DTO `{name}` must stay private to {WIRE}"
        );
    }

    for rel in [MOUNT, LIFECYCLE, MODEL] {
        assert!(
            !tokens(&read_src(rel)).iter().any(|t| t == "Deserialize"),
            "domain types in src/{rel} must not derive or implement Deserialize; parse through {WIRE}"
        );
    }
}

#[test]
fn validated_newtypes_keep_private_fields_and_deletion_proof_is_unforgeable() {
    let domain: Vec<(&str, Vec<String>)> = [MOUNT, LIFECYCLE, MODEL]
        .into_iter()
        .map(|rel| (rel, tokens(&read_src(rel))))
        .collect();
    let find = |name: &str| {
        domain
            .iter()
            .find_map(|(rel, toks)| declaration(toks, "struct", name).map(|at| (*rel, toks, at)))
            .unwrap_or_else(|| panic!("the lifecycle data model must declare `struct {name}`"))
    };

    for name in [
        "NormalPath",
        "MountId",
        "AnchorDir",
        "ProjectRoot",
        "StateRoot",
        "TrashSuffix",
    ] {
        let (rel, toks, at) = find(name);
        assert_eq!(
            toks.get(at + 2).map(String::as_str),
            Some("("),
            "`{name}` in src/{rel} must be a tuple newtype"
        );
        assert_ne!(
            toks.get(at + 3).map(String::as_str),
            Some("pub"),
            "the field of validated newtype `{name}` in src/{rel} must stay private"
        );
    }

    for name in ["DeletionProof", "StrandedEvidence"] {
        let (rel, toks, at) = find(name);
        let open = at + 2;
        assert_eq!(
            toks.get(open).map(String::as_str),
            Some("{"),
            "`{name}` has named fields"
        );
        assert!(
            !toks[open..closing(toks, open)].iter().any(|t| t == "pub"),
            "fields of validated `{name}` in src/{rel} must stay private"
        );
    }

    let (rel, mount, at) = find("DeletionProof");
    assert_eq!(rel, MOUNT, "DeletionProof must be declared in src/{MOUNT}");
    let attrs_start = mount[..at]
        .iter()
        .rposition(|t| t == ";" || t == "}")
        .map_or(0, |i| i + 1);
    assert!(
        !mount[attrs_start..at].iter().any(|t| t == "Default"),
        "DeletionProof must not derive Default"
    );
    let mut search = 0;
    while let Some(offset) = mount[search..]
        .windows(2)
        .position(|pair| pair[0] == "impl" && pair[1] == "DeletionProof")
    {
        let open = search + offset + 2;
        let close = closing(mount, open);
        assert!(
            !mount[open..close]
                .iter()
                .any(|t| t == "Self" || t == "DeletionProof"),
            "inherent methods of DeletionProof must not construct one; only `locate` proves deletion"
        );
        search = close;
    }

    let mut files = Vec::new();
    rust_files(&src_dir(), &mut files);
    for file in files {
        let source = fs::read_to_string(&file).expect("src file must be readable");
        let toks = tokens(&source);
        for (i, window) in toks.windows(2).enumerate() {
            if window[0] != "for" || window[1] != "DeletionProof" {
                continue;
            }
            let header_start = toks[..i].iter().rposition(|t| t == "impl").unwrap_or(0);
            assert!(
                !toks[header_start..i]
                    .iter()
                    .any(|t| matches!(t.as_str(), "From" | "TryFrom" | "Default")),
                "{} must not implement a constructing trait for DeletionProof",
                file.display()
            );
        }
    }
}
