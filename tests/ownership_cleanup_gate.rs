//! T029 ownership gate. Structural checks use a top-level Rust item scan that
//! excludes comments, literals, macro bodies, nested items, and cfg-gated items.
//! A child `rustc` probe verifies the final public owner paths without making
//! this test target fail to compile before the move lands.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    text: String,
    depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Type(String),
    Function(String),
    Impl,
    Module {
        name: String,
        file: bool,
        public: bool,
    },
    Use,
    Other(String),
}

#[derive(Debug, Default)]
struct LiveItems {
    items: Vec<Item>,
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(rel: &str) -> String {
    fs::read_to_string(src_dir().join(rel)).unwrap_or_default()
}

fn mask_range(bytes: &mut [u8], start: usize, end: usize) {
    for byte in &mut bytes[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

fn line_comment_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| start + offset)
}

fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1usize;
    let mut index = start + 2;
    while index + 1 < bytes.len() {
        match (bytes[index], bytes[index + 1]) {
            (b'/', b'*') => {
                depth += 1;
                index += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    bytes.len()
}

fn quoted_end(bytes: &[u8], quote: usize, delimiter: u8) -> Option<usize> {
    let mut index = quote + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == delimiter {
            return Some(index + 1);
        } else {
            index += 1;
        }
    }
    None
}

fn char_literal_end(bytes: &[u8], quote: usize) -> Option<usize> {
    if bytes.get(quote + 1) == Some(&b'\\') {
        return quoted_end(bytes, quote, b'\'');
    }
    let tail = std::str::from_utf8(bytes.get(quote + 1..)?).ok()?;
    let width = tail.chars().next()?.len_utf8();
    let closing = quote + 1 + width;
    (bytes.get(closing) == Some(&b'\'')).then_some(closing + 1)
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let hashes = cursor - hashes_start;
    cursor += 1;
    while cursor < bytes.len() {
        let suffix_end = cursor + 1 + hashes;
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..suffix_end)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(bytes.len())
}

fn mask_non_code(source: &str) -> String {
    let original = source.as_bytes();
    let mut masked = original.to_vec();
    let mut index = 0usize;
    while index < original.len() {
        let end = match (original[index], original.get(index + 1).copied()) {
            (b'/', Some(b'/')) => Some(line_comment_end(original, index)),
            (b'/', Some(b'*')) => Some(block_comment_end(original, index)),
            (b'r', _) | (b'b', Some(b'r')) => raw_string_end(original, index),
            (b'"', _) => quoted_end(original, index, b'"'),
            (b'b', Some(b'"')) => quoted_end(original, index + 1, b'"'),
            (b'\'', _) => char_literal_end(original, index),
            _ => None,
        };
        if let Some(end) = end {
            mask_range(&mut masked, index, end);
            index = end;
        } else {
            index += 1;
        }
    }
    String::from_utf8(masked).expect("masking preserves UTF-8 byte structure")
}

fn tokens(source: &str) -> Vec<Token> {
    let masked = mask_non_code(source);
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            out.push(Token {
                text: masked[start..index].to_owned(),
                depth,
            });
        } else {
            let symbol = char::from(bytes[index]);
            if matches!(symbol, ')' | ']' | '}') {
                depth = depth.saturating_sub(1);
            }
            out.push(Token {
                text: symbol.to_string(),
                depth,
            });
            if matches!(symbol, '(' | '[' | '{') {
                depth += 1;
            }
            index += 1;
        }
    }
    out
}

fn next_top(tokens: &[Token], from: usize) -> Option<(usize, &Token)> {
    tokens
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, token)| token.depth == 0)
}

fn previous_top(tokens: &[Token], before: usize) -> Option<&Token> {
    tokens[..before].iter().rev().find(|token| token.depth == 0)
}

fn next_top_ident(tokens: &[Token], from: usize) -> Option<(usize, &str)> {
    tokens
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, token)| {
            token.depth == 0
                && token
                    .text
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic)
        })
        .map(|(index, token)| (index, token.text.as_str()))
}

fn attribute_end(tokens: &[Token], from: usize) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, token)| token.depth == 0 && token.text == "]")
        .map(|(index, _)| index)
}

#[derive(Debug)]
enum CfgPredicate {
    Atom(String),
    Call(String, Vec<Self>),
}

struct CfgParser<'a> {
    tokens: &'a [Token],
    index: usize,
}

impl CfgParser<'_> {
    fn parse_predicate(&mut self) -> Option<CfgPredicate> {
        let name = self.tokens.get(self.index)?.text.clone();
        if !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }
        self.index += 1;
        if self
            .tokens
            .get(self.index)
            .is_none_or(|token| token.text != "(")
        {
            return Some(CfgPredicate::Atom(name));
        }
        self.index += 1;
        let mut arguments = Vec::new();
        while self
            .tokens
            .get(self.index)
            .is_some_and(|token| token.text != ")")
        {
            arguments.push(self.parse_predicate()?);
            if self
                .tokens
                .get(self.index)
                .is_some_and(|token| token.text == ",")
            {
                self.index += 1;
            } else if self
                .tokens
                .get(self.index)
                .is_some_and(|token| token.text != ")")
            {
                return None;
            }
        }
        if self
            .tokens
            .get(self.index)
            .is_none_or(|token| token.text != ")")
        {
            return None;
        }
        self.index += 1;
        Some(CfgPredicate::Call(name, arguments))
    }
}

#[derive(Clone, Copy)]
struct TruthPossibilities {
    can_be_true: bool,
    can_be_false: bool,
}

fn production_possibilities(predicate: &CfgPredicate) -> TruthPossibilities {
    match predicate {
        CfgPredicate::Atom(name) if name == "test" => TruthPossibilities {
            can_be_true: false,
            can_be_false: true,
        },
        CfgPredicate::Call(name, arguments) if name == "all" => TruthPossibilities {
            can_be_true: arguments
                .iter()
                .all(|argument| production_possibilities(argument).can_be_true),
            can_be_false: arguments
                .iter()
                .any(|argument| production_possibilities(argument).can_be_false),
        },
        CfgPredicate::Call(name, arguments) if name == "any" => TruthPossibilities {
            can_be_true: arguments
                .iter()
                .any(|argument| production_possibilities(argument).can_be_true),
            can_be_false: arguments
                .iter()
                .all(|argument| production_possibilities(argument).can_be_false),
        },
        CfgPredicate::Call(name, arguments) if name == "not" && arguments.len() == 1 => {
            let inner = production_possibilities(&arguments[0]);
            TruthPossibilities {
                can_be_true: inner.can_be_false,
                can_be_false: inner.can_be_true,
            }
        }
        CfgPredicate::Atom(_) | CfgPredicate::Call(_, _) => TruthPossibilities {
            can_be_true: true,
            can_be_false: true,
        },
    }
}

fn attribute_excludes_production(tokens: &[Token]) -> bool {
    if tokens.first().is_none_or(|token| token.text != "cfg")
        || tokens.get(1).is_none_or(|token| token.text != "(")
        || tokens.last().is_none_or(|token| token.text != ")")
    {
        return false;
    }
    let mut parser = CfgParser {
        tokens: &tokens[2..tokens.len() - 1],
        index: 0,
    };
    parser.parse_predicate().is_some_and(|predicate| {
        parser.index == parser.tokens.len() && !production_possibilities(&predicate).can_be_true
    })
}

fn scan_live_items(source: &str) -> LiveItems {
    let tokens = tokens(source);
    let mut items = Vec::new();
    let mut excluded_from_production = false;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.depth != 0 {
            index += 1;
            continue;
        }
        if token.text == "#"
            && tokens.get(index + 1).is_some_and(|next| next.text == "[")
            && let Some(end) = attribute_end(&tokens, index + 2)
        {
            excluded_from_production |= attribute_excludes_production(&tokens[index + 2..end]);
            index = end + 1;
            continue;
        }
        match token.text.as_str() {
            "struct" | "enum" | "union" | "trait" | "type" => {
                if let Some((_, name)) = next_top_ident(&tokens, index + 1)
                    && !excluded_from_production
                {
                    items.push(Item::Type(name.to_owned()));
                }
                excluded_from_production = false;
            }
            "fn" => {
                if let Some((_, name)) = next_top_ident(&tokens, index + 1)
                    && !excluded_from_production
                {
                    items.push(Item::Function(name.to_owned()));
                }
                excluded_from_production = false;
            }
            "impl" => {
                if !excluded_from_production {
                    items.push(Item::Impl);
                }
                excluded_from_production = false;
            }
            "mod" => {
                if let Some((name_index, name)) = next_top_ident(&tokens, index + 1) {
                    let file =
                        next_top(&tokens, name_index + 1).is_some_and(|(_, next)| next.text == ";");
                    if !excluded_from_production {
                        items.push(Item::Module {
                            name: name.to_owned(),
                            file,
                            public: previous_top(&tokens, index)
                                .is_some_and(|previous| previous.text == "pub"),
                        });
                    }
                }
                excluded_from_production = false;
            }
            "use" => {
                if !excluded_from_production {
                    items.push(Item::Use);
                }
                excluded_from_production = false;
            }
            "const" => {
                if next_top(&tokens, index + 1).is_none_or(|(_, next)| next.text != "fn") {
                    if !excluded_from_production {
                        items.push(Item::Other("const".to_owned()));
                    }
                    excluded_from_production = false;
                }
            }
            "static" | "macro_rules" => {
                if !excluded_from_production {
                    items.push(Item::Other(token.text.clone()));
                }
                excluded_from_production = false;
            }
            _ if token
                .text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                && next_top(&tokens, index + 1).is_some_and(|(_, next)| next.text == "!") =>
            {
                if !excluded_from_production {
                    items.push(Item::Other(format!(
                        "unsupported item macro invocation: {}!",
                        token.text
                    )));
                }
                excluded_from_production = false;
            }
            _ => {}
        }
        index += 1;
    }
    LiveItems { items }
}

impl LiveItems {
    fn defines_type(&self, name: &str) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, Item::Type(found) if found == name))
    }

    fn defines_function(&self, name: &str) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, Item::Function(found) if found == name))
    }

    fn declares_file_module(&self, name: &str) -> bool {
        self.items.iter().any(
            |item| matches!(item, Item::Module { name: found, file: true, .. } if found == name),
        )
    }

    fn declares_public_file_module(&self, name: &str) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item,
                Item::Module {
                    name: found,
                    file: true,
                    public: true,
                } if found == name
            )
        })
    }
}

fn rust_files(dir: &Path, base: &Path, files: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, base, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(
                path.strip_prefix(base)
                    .expect("source file remains below src/")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

fn production_files() -> Vec<String> {
    let base = src_dir();
    let mut files = Vec::new();
    rust_files(&base, &base, &mut files);
    files.sort();
    files
}

fn type_definition_sites(name: &str) -> Vec<String> {
    production_files()
        .into_iter()
        .filter(|relative| scan_live_items(&read_src(relative)).defines_type(name))
        .collect()
}

fn function_definition_sites(name: &str) -> Vec<String> {
    production_files()
        .into_iter()
        .filter(|relative| scan_live_items(&read_src(relative)).defines_function(name))
        .collect()
}

fn item_macro_sites() -> Vec<String> {
    production_files()
        .into_iter()
        .filter(|relative| {
            scan_live_items(&read_src(relative)).items.iter().any(|item| {
                matches!(item, Item::Other(description) if description.starts_with("unsupported item macro invocation:"))
            })
        })
        .collect()
}

fn compile_owner_probe(source: &str) -> Output {
    let fixture = tempfile::TempDir::new().expect("probe tempdir");
    let source_dir = fixture.path().join("src");
    fs::create_dir(&source_dir).expect("create probe src");
    fs::write(source_dir.join("main.rs"), source).expect("write owner probe");
    let package_path = env!("CARGO_MANIFEST_DIR")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        fixture.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"t029-owner-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             \n[dependencies]\nphora = {{ path = \"{package_path}\" }}\n"
        ),
    )
    .expect("write probe manifest");
    Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--offline", "--manifest-path"])
        .arg(fixture.path().join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", fixture.path().join("target"))
        .output()
        .expect("run exact path-dependency owner probe")
}

const SOURCE_BOUNDARY_PROBE: &str = r#"

#[cfg(test)]
pub(crate) use transitive::{
    T029_ACQUIRE_ARGS, T029_ACQUIRE_CALLS, T029_ACQUIRE_OVERRIDE, T029_VALIDATE_ARGS,
    T029_VALIDATE_CALLS, T029_VALIDATE_OVERRIDE,
};

#[cfg(test)]
mod t029_callable_boundary_probe {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{SourceBackend, SourceName};
    use crate::config::{ParsedSource, Refspec, Source};

    struct ManifestBackend {
        fetches: AtomicUsize,
        resolves: AtomicUsize,
        reads: AtomicUsize,
    }

    impl SourceBackend for ManifestBackend {
        fn fetch(&self, source: &SourceName, url: &str) -> super::Result<()> {
            assert_eq!(source.as_str(), "dep");
            assert_eq!(url, "https://example.test/dep.git");
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn read_file_at(
            &self,
            source: &SourceName,
            url: &str,
            commit: &str,
            path: &Path,
        ) -> super::Result<Vec<u8>> {
            assert_eq!(source.as_str(), "dep");
            assert_eq!(url, "https://example.test/dep.git");
            assert_eq!(commit, "a".repeat(40));
            assert_eq!(path, Path::new("phora.toml"));
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(b"version = 1\n\n[sources.leaf]\ngit = \"https://example.test/leaf.git\"\n".to_vec())
        }

        fn resolve(
            &self,
            source: &SourceName,
            url: &str,
            _refspec: &Refspec,
        ) -> super::Result<String> {
            assert_eq!(source.as_str(), "dep");
            assert_eq!(url, "https://example.test/dep.git");
            self.resolves.fetch_add(1, Ordering::SeqCst);
            Ok("a".repeat(40))
        }

        fn commit_time(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
        ) -> super::Result<u64> {
            Ok(0)
        }

        fn compute_digest(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
            _root: Option<&Path>,
            _include: &[String],
            _exclude: &[String],
        ) -> super::Result<String> {
            Ok("blake3:probe".to_owned())
        }
    }

    #[test]
    fn t029_boundary_callable_contract() {
        super::T029_ACQUIRE_OVERRIDE.store(false, Ordering::SeqCst);
        super::T029_VALIDATE_OVERRIDE.store(false, Ordering::SeqCst);
        super::T029_ACQUIRE_CALLS.store(0, Ordering::SeqCst);
        super::T029_VALIDATE_CALLS.store(0, Ordering::SeqCst);
        super::T029_ACQUIRE_ARGS.lock().expect("acquire args").clear();
        super::T029_VALIDATE_ARGS.lock().expect("validate args").clear();

        let backend = ManifestBackend {
            fetches: AtomicUsize::new(0),
            resolves: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        };
        let raw: Source = toml::from_str(
            "git = \"https://example.test/dep.git\"\ntransitive = true\n",
        )
        .expect("source DTO parses");
        let parsed = ParsedSource::parse("dep", &raw).expect("source parses");
        let name = SourceName::trusted("dep".to_owned());
        let (commit, manifest) = super::transitive::acquire_dependency_manifest(
            &backend,
            &name,
            &parsed,
            "https://example.test/dep.git",
            None,
        )
        .expect("the source boundary acquires and decodes the dependency manifest");
        assert_eq!(commit, "a".repeat(40));
        assert!(manifest.sources.contains_key("leaf"));
        assert_eq!(backend.fetches.load(Ordering::SeqCst), 1);
        assert_eq!(backend.resolves.load(Ordering::SeqCst), 1);
        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
        assert_eq!(super::T029_ACQUIRE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            *super::T029_ACQUIRE_ARGS.lock().expect("acquire args"),
            [("dep".to_owned(), "https://example.test/dep.git".to_owned(), true, true)]
        );

        let raw: Source = toml::from_str("path = \"/etc\"\ntransitive = true\n")
            .expect("path source DTO parses");
        let parsed = ParsedSource::parse("escape", &raw).expect("path source parses");
        let error = super::transitive::validate_dependency_remote(
            "escape", &parsed, "/etc", 1,
        )
        .expect_err("an escaping transitive path must be rejected by the source boundary");
        assert!(error.to_string().contains("transitive remote not allowed"));
        assert_eq!(super::T029_VALIDATE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            *super::T029_VALIDATE_ARGS.lock().expect("validate args"),
            [("escape".to_owned(), "/etc".to_owned(), 1, true)]
        );
    }
}
"#;

const SYNC_DELEGATION_PROBE: &str = r#"

#[cfg(test)]
mod t029_source_delegation_probe {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::Refspec;
    use crate::kernel::SourceName;
    use crate::source::{
        SourceBackend, SourceError, T029_ACQUIRE_ARGS, T029_ACQUIRE_CALLS, T029_ACQUIRE_OVERRIDE,
        T029_VALIDATE_ARGS, T029_VALIDATE_CALLS, T029_VALIDATE_OVERRIDE,
    };

    struct NoManifestReadBackend {
        fetches: AtomicUsize,
        resolves: AtomicUsize,
        reads: AtomicUsize,
    }

    impl SourceBackend for NoManifestReadBackend {
        fn fetch(
            &self,
            _source: &SourceName,
            _url: &str,
        ) -> std::result::Result<(), SourceError> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn read_file_at(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
            _path: &Path,
        ) -> std::result::Result<Vec<u8>, SourceError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Err(SourceError::Source(
                "sync duplicated the source-owned manifest read".to_owned(),
            ))
        }

        fn resolve(
            &self,
            _source: &SourceName,
            _url: &str,
            _refspec: &Refspec,
        ) -> std::result::Result<String, SourceError> {
            self.resolves.fetch_add(1, Ordering::SeqCst);
            Ok("b".repeat(40))
        }

        fn commit_time(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
        ) -> std::result::Result<u64, SourceError> {
            Ok(0)
        }

        fn compute_digest(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
            _root: Option<&Path>,
            _include: &[String],
            _exclude: &[String],
        ) -> std::result::Result<String, SourceError> {
            Ok("blake3:probe".to_owned())
        }
    }

    fn consumer(source: &str) -> (Config, BTreeMap<String, ParsedSource>) {
        let text = format!(
            "version = 1\n\n[sources.dep]\n{source}\ntransitive = true\n\n\
             [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n"
        );
        let config = Config::parse(&text).expect("consumer config parses");
        let parsed = config.parsed_sources().expect("consumer sources parse");
        (config, parsed)
    }

    #[test]
    fn t029_boundary_sync_delegates_without_duplicate_source_logic() {
        let backend = NoManifestReadBackend {
            fetches: AtomicUsize::new(0),
            resolves: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        };
        T029_ACQUIRE_CALLS.store(0, Ordering::SeqCst);
        T029_VALIDATE_CALLS.store(0, Ordering::SeqCst);
        T029_ACQUIRE_ARGS.lock().expect("acquire args").clear();
        T029_VALIDATE_ARGS.lock().expect("validate args").clear();
        T029_ACQUIRE_OVERRIDE.store(true, Ordering::SeqCst);
        T029_VALIDATE_OVERRIDE.store(false, Ordering::SeqCst);

        let (config, parsed) = consumer("git = \"https://example.test/dep.git\"");
        let graph = resolve_transitive_graph(&config, &parsed, &backend, false, None)
            .expect("sync must consume the manifest supplied by the source boundary");
        assert_eq!(graph.targets.len(), 1, "the injected boundary manifest must drive composition");
        assert!(
            graph.targets[0].target.path.ends_with("boundary"),
            "the source-owned sentinel target must drive sync composition: {:?}",
            graph.targets[0].target.path
        );
        assert_eq!(T029_ACQUIRE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(T029_VALIDATE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            *T029_ACQUIRE_ARGS.lock().expect("acquire args"),
            [("dep".to_owned(), "https://example.test/dep.git".to_owned(), true, true)]
        );
        assert_eq!(
            *T029_VALIDATE_ARGS.lock().expect("validate args"),
            [("dep".to_owned(), "https://example.test/dep.git".to_owned(), 1, true)]
        );
        assert_eq!(backend.fetches.load(Ordering::SeqCst), 0, "sync must not duplicate the source-owned fetch");
        assert_eq!(backend.resolves.load(Ordering::SeqCst), 0, "sync must not duplicate the source-owned resolve");
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0, "sync must not duplicate the manifest read");

        T029_ACQUIRE_CALLS.store(0, Ordering::SeqCst);
        T029_VALIDATE_CALLS.store(0, Ordering::SeqCst);
        T029_ACQUIRE_ARGS.lock().expect("acquire args").clear();
        T029_VALIDATE_ARGS.lock().expect("validate args").clear();
        T029_VALIDATE_OVERRIDE.store(true, Ordering::SeqCst);
        let (config, parsed) = consumer("path = \"/etc\"");
        let graph = resolve_transitive_graph(&config, &parsed, &backend, false, None)
            .expect("the source boundary override must be authoritative; duplicate sync confinement would reject");
        assert!(graph.targets[0].target.path.ends_with("boundary"));
        assert_eq!(T029_ACQUIRE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(T029_VALIDATE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            *T029_ACQUIRE_ARGS.lock().expect("acquire args"),
            [("dep".to_owned(), "/etc".to_owned(), true, true)]
        );
        assert_eq!(
            *T029_VALIDATE_ARGS.lock().expect("validate args"),
            [("dep".to_owned(), "/etc".to_owned(), 1, true)]
        );
        assert_eq!(backend.fetches.load(Ordering::SeqCst), 0);
        assert_eq!(backend.resolves.load(Ordering::SeqCst), 0);
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);

        T029_ACQUIRE_OVERRIDE.store(false, Ordering::SeqCst);
        T029_VALIDATE_OVERRIDE.store(false, Ordering::SeqCst);
    }
}
"#;

const SYNC_DUPLICATE_AST_PROBE: &str = r#"
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    AngleBracketedGenericArguments, Attribute, Expr, ExprCall, ExprField, ExprMacro,
    ExprMethodCall, ExprPath, FnArg, GenericArgument, ImplItemFn, Item, ItemFn, ItemImpl,
    ItemMacro, ItemMod, ItemStruct, ItemUse, Local, Macro, Member, Meta, Pat, Path,
    PathArguments, Signature, Token, Type, TypeParamBound, UseTree,
};

fn cfg_predicate_implies_test(predicate: &Meta) -> bool {
    match predicate {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .is_ok_and(|predicates| predicates.iter().any(cfg_predicate_implies_test)),
        Meta::List(list) if list.path.is_ident("any") => list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .is_ok_and(|predicates| {
                !predicates.is_empty() && predicates.iter().all(cfg_predicate_implies_test)
            }),
        // `not(test)` is live in production. Unknown predicates fail open into the scan.
        Meta::List(_) | Meta::NameValue(_) => false,
    }
}

fn test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Meta>()
                .is_ok_and(|predicate| cfg_predicate_implies_test(&predicate))
    })
}

fn path_ends_with(path: &Path, suffix: &[&str]) -> bool {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    segments.len() >= suffix.len()
        && segments[segments.len() - suffix.len()..]
            .iter()
            .map(String::as_str)
            .eq(suffix.iter().copied())
}

fn path_mentions(path: &Path, expected: &str) -> bool {
    path.segments
        .iter()
        .any(|segment| segment.ident == expected)
}

#[derive(Clone, Default)]
struct DecoderAliases {
    bindings: BTreeMap<String, Option<Vec<String>>>,
}

impl DecoderAliases {
    fn collect(&mut self, import: &ItemUse) {
        self.collect_tree(&import.tree, &mut Vec::new());
    }

    fn collect_tree(&mut self, tree: &UseTree, prefix: &mut Vec<String>) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.collect_tree(&path.tree, prefix);
                prefix.pop();
            }
            UseTree::Name(name) => {
                let imported = name.ident.to_string();
                let canonical = if imported == "self" {
                    prefix.clone()
                } else {
                    prefix
                        .iter()
                        .cloned()
                        .chain(std::iter::once(imported.clone()))
                        .collect()
                };
                let local = if imported == "self" {
                    canonical.last().cloned()
                } else {
                    Some(imported)
                };
                if let Some(local) = local {
                    self.bindings.insert(local, Some(canonical));
                }
            }
            UseTree::Rename(rename) => {
                let imported = rename.ident.to_string();
                let canonical = if imported == "self" {
                    prefix.clone()
                } else {
                    prefix
                        .iter()
                        .cloned()
                        .chain(std::iter::once(imported))
                        .collect()
                };
                self.bindings
                    .insert(rename.rename.to_string(), Some(canonical));
            }
            UseTree::Group(group) => {
                for item in &group.items {
                    self.collect_tree(item, prefix);
                }
            }
            UseTree::Glob(_) => {}
        }
    }

    fn shadow(&mut self, name: String) {
        self.bindings.insert(name, None);
    }
}

#[derive(Clone, Default)]
struct DecoderAliasEnvironment {
    scopes: Vec<DecoderAliases>,
}

impl DecoderAliasEnvironment {
    fn new(module_aliases: &DecoderAliases, signature: &Signature) -> Self {
        let mut environment = Self {
            scopes: vec![module_aliases.clone(), DecoderAliases::default()],
        };
        for argument in &signature.inputs {
            if let FnArg::Typed(argument) = argument {
                for binding in pattern_bindings(&argument.pat) {
                    environment.shadow(binding);
                }
            }
        }
        environment
    }

    fn enter_block(&mut self, block: &syn::Block) {
        self.scopes.push(decoder_aliases_in_statements(&block.stmts));
    }

    fn leave_block(&mut self) {
        self.scopes.pop().expect("decoder alias block scope");
    }

    fn shadow(&mut self, name: String) {
        self.scopes
            .last_mut()
            .expect("decoder alias scope")
            .shadow(name);
    }

    fn resolves_manifest_decoder(&self, path: &Path) -> bool {
        let mut segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        let mut expanded = BTreeSet::new();
        while let Some(first) = segments.first().cloned() {
            let Some(binding) = self
                .scopes
                .iter()
                .rev()
                .find_map(|scope| scope.bindings.get(&first))
            else {
                break;
            };
            let Some(prefix) = binding else {
                return false;
            };
            if !expanded.insert(first) {
                break;
            }
            segments = prefix
                .iter()
                .cloned()
                .chain(segments.into_iter().skip(1))
                .collect();
        }
        segments.iter().any(|segment| segment == "toml")
            && segments
                .last()
                .is_some_and(|segment| matches!(segment.as_str(), "from_slice" | "from_str"))
    }
}

fn decoder_aliases_in_items(items: &[Item]) -> DecoderAliases {
    let mut aliases = DecoderAliases::default();
    for item in items {
        if let Item::Use(import) = item
            && !test_only(&import.attrs)
        {
            aliases.collect(import);
        }
    }
    aliases
}

fn decoder_aliases_in_statements(statements: &[syn::Stmt]) -> DecoderAliases {
    let mut aliases = DecoderAliases::default();
    for statement in statements {
        match statement {
            syn::Stmt::Item(Item::Use(import)) if !test_only(&import.attrs) => {
                aliases.collect(import);
            }
            syn::Stmt::Item(Item::Fn(function)) if !test_only(&function.attrs) => {
                aliases.shadow(function.sig.ident.to_string());
            }
            _ => {}
        }
    }
    aliases
}

fn type_mentions(r#type: &Type, expected: &str) -> bool {
    struct TypePaths<'a> {
        expected: &'a str,
        found: bool,
    }

    impl<'ast> Visit<'ast> for TypePaths<'_> {
        fn visit_path(&mut self, path: &'ast Path) {
            self.found |= path_mentions(path, self.expected);
            if !self.found {
                visit::visit_path(self, path);
            }
        }
    }

    let mut paths = TypePaths {
        expected,
        found: false,
    };
    paths.visit_type(r#type);
    paths.found
}

fn angle_arguments_mention(
    arguments: &AngleBracketedGenericArguments,
    expected: &str,
) -> bool {
    arguments.args.iter().any(|argument| {
        matches!(argument, GenericArgument::Type(r#type) if type_mentions(r#type, expected))
    })
}

fn path_arguments_mention(path: &Path, expected: &str) -> bool {
    path.segments.iter().any(|segment| match &segment.arguments {
        PathArguments::AngleBracketed(arguments) => angle_arguments_mention(arguments, expected),
        PathArguments::Parenthesized(arguments) => {
            arguments
                .inputs
                .iter()
                .any(|r#type| type_mentions(r#type, expected))
                || matches!(
                    &arguments.output,
                    syn::ReturnType::Type(_, r#type) if type_mentions(r#type, expected)
                )
        }
        PathArguments::None => false,
    })
}

fn named_type(r#type: &Type) -> Option<String> {
    match r#type {
        Type::Reference(reference) => named_type(&reference.elem),
        Type::Group(group) => named_type(&group.elem),
        Type::Paren(paren) => named_type(&paren.elem),
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        _ => None,
    }
}

#[derive(Default)]
struct TypeFacts {
    source_backend_fields: BTreeMap<String, BTreeSet<String>>,
}

impl<'ast> Visit<'ast> for TypeFacts {
    fn visit_item_struct(&mut self, structure: &'ast ItemStruct) {
        if test_only(&structure.attrs) {
            return;
        }
        let backend_parameters: BTreeSet<String> = structure
            .generics
            .type_params()
            .filter(|parameter| {
                parameter.bounds.iter().any(|bound| {
                    matches!(
                        bound,
                        TypeParamBound::Trait(bound) if path_mentions(&bound.path, "SourceBackend")
                    )
                })
            })
            .map(|parameter| parameter.ident.to_string())
            .collect();
        let fields: BTreeSet<String> = structure
            .fields
            .iter()
            .filter(|field| {
                type_mentions(&field.ty, "SourceBackend")
                    || backend_parameters
                        .iter()
                        .any(|parameter| type_mentions(&field.ty, parameter))
            })
            .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
            .collect();
        if !fields.is_empty() {
            self.source_backend_fields
                .insert(structure.ident.to_string(), fields);
        }
        visit::visit_item_struct(self, structure);
    }
}

fn binding_name(pattern: &Pat) -> Option<String> {
    match pattern {
        Pat::Ident(binding) => Some(binding.ident.to_string()),
        Pat::Reference(reference) => binding_name(&reference.pat),
        Pat::Type(typed) => binding_name(&typed.pat),
        _ => None,
    }
}

fn pattern_bindings(pattern: &Pat) -> BTreeSet<String> {
    #[derive(Default)]
    struct Bindings {
        names: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for Bindings {
        fn visit_pat_ident(&mut self, binding: &'ast syn::PatIdent) {
            self.names.insert(binding.ident.to_string());
            visit::visit_pat_ident(self, binding);
        }
    }

    let mut bindings = Bindings::default();
    bindings.visit_pat(pattern);
    bindings.names
}

fn pattern_type(pattern: &Pat) -> Option<&Type> {
    match pattern {
        Pat::Type(typed) => Some(&typed.ty),
        Pat::Reference(reference) => pattern_type(&reference.pat),
        _ => None,
    }
}

fn typed_bindings(signature: &Signature, expected: &str) -> BTreeSet<String> {
    signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Typed(argument) if type_mentions(&argument.ty, expected) => {
                binding_name(&argument.pat)
            }
            FnArg::Receiver(_) | FnArg::Typed(_) => None,
        })
        .collect()
}

fn text_bindings(signature: &Signature) -> BTreeSet<String> {
    signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Typed(argument)
                if named_type(&argument.ty)
                    .is_some_and(|name| matches!(name.as_str(), "str" | "String")) =>
            {
                binding_name(&argument.pat)
            }
            FnArg::Receiver(_) | FnArg::Typed(_) => None,
        })
        .collect()
}

fn binding_types(signature: &Signature) -> BTreeMap<String, String> {
    signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            FnArg::Typed(argument) => {
                Some((binding_name(&argument.pat)?, named_type(&argument.ty)?))
            }
            FnArg::Receiver(_) => None,
        })
        .collect()
}

fn return_type_mentions(signature: &Signature, expected: &str) -> bool {
    matches!(
        &signature.output,
        syn::ReturnType::Type(_, r#type) if type_mentions(r#type, expected)
    )
}

fn expression_references(expression: &Expr, bindings: &BTreeSet<String>) -> bool {
    struct References<'a> {
        bindings: &'a BTreeSet<String>,
        found: bool,
    }

    impl<'ast> Visit<'ast> for References<'_> {
        fn visit_expr_path(&mut self, expression: &'ast ExprPath) {
            self.found |= expression
                .path
                .segments
                .last()
                .is_some_and(|segment| self.bindings.contains(&segment.ident.to_string()));
            if !self.found {
                visit::visit_expr_path(self, expression);
            }
        }
    }

    let mut references = References {
        bindings,
        found: false,
    };
    references.visit_expr(expression);
    references.found
}

fn macro_name(invocation: &Macro) -> Option<String> {
    invocation
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

struct ExpressionArguments {
    expressions: Punctuated<Expr, Token![,]>,
}

impl Parse for ExpressionArguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self {
            expressions: Punctuated::parse_terminated(input)?,
        })
    }
}

struct RepeatedExpression {
    value: Expr,
    _semicolon: Token![;],
    length: Expr,
}

impl Parse for RepeatedExpression {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self {
            value: input.parse()?,
            _semicolon: input.parse()?,
            length: input.parse()?,
        })
    }
}

struct MatchesArguments {
    expression: Expr,
    _comma: Token![,],
    _pattern: Pat,
    guard: Option<Expr>,
}

impl Parse for MatchesArguments {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let expression = input.parse()?;
        let comma = input.parse()?;
        let pattern = Pat::parse_multi_with_leading_vert(input)?;
        let guard = if input.peek(Token![if]) {
            input.parse::<Token![if]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        Ok(Self {
            expression,
            _comma: comma,
            _pattern: pattern,
            guard,
        })
    }
}

fn macro_expressions(invocation: &Macro) -> Result<Vec<Expr>, ()> {
    let name = macro_name(invocation).ok_or(())?;
    if name == "cfg" {
        return Ok(Vec::new());
    }
    if name == "matches" {
        let arguments = syn::parse2::<MatchesArguments>(invocation.tokens.clone()).map_err(|_| ())?;
        return Ok(std::iter::once(arguments.expression)
            .chain(arguments.guard)
            .collect());
    }
    if name == "vec" {
        if let Ok(arguments) = syn::parse2::<ExpressionArguments>(invocation.tokens.clone()) {
            return Ok(arguments.expressions.into_iter().collect());
        }
        let repeated = syn::parse2::<RepeatedExpression>(invocation.tokens.clone()).map_err(|_| ())?;
        return Ok(vec![repeated.value, repeated.length]);
    }
    if matches!(
        name.as_str(),
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "dbg"
            | "eprint"
            | "eprintln"
            | "format"
            | "format_args"
            | "panic"
            | "print"
            | "println"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "write"
            | "writeln"
    ) {
        return syn::parse2::<ExpressionArguments>(invocation.tokens.clone())
            .map(|arguments| arguments.expressions.into_iter().collect())
            .map_err(|_| ());
    }
    Err(())
}

fn expression_has_nonboundary_call(expression: &Expr) -> bool {
    #[derive(Default)]
    struct Calls {
        found: bool,
    }

    impl<'ast> Visit<'ast> for Calls {
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            let approved = matches!(
                call.func.as_ref(),
                Expr::Path(function)
                    if path_ends_with(&function.path, &["acquire_dependency_manifest"])
            );
            if !approved {
                self.found = true;
            }
            if !self.found {
                visit::visit_expr_call(self, call);
            }
        }

        fn visit_expr_method_call(&mut self, _call: &'ast ExprMethodCall) {
            self.found = true;
        }

        fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
            match macro_expressions(&expression.mac) {
                Ok(expressions) => {
                    for expression in &expressions {
                        self.visit_expr(expression);
                    }
                }
                Err(()) => self.found = true,
            }
        }
    }

    let mut calls = Calls::default();
    calls.visit_expr(expression);
    calls.found
}

fn is_manifest_decoder_call(
    call: &ExprCall,
    text_inputs: &BTreeSet<String>,
    decoder_aliases: &DecoderAliasEnvironment,
) -> bool {
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    decoder_aliases.resolves_manifest_decoder(&function.path)
        && call
            .args
            .iter()
            .any(|argument| expression_references(argument, text_inputs))
}

fn expression_produces_decode(
    expression: &Expr,
    text_inputs: &BTreeSet<String>,
    decoder_aliases: &DecoderAliasEnvironment,
) -> bool {
    match expression {
        Expr::Call(call) => {
            let function_name = match call.func.as_ref() {
                Expr::Path(function) => function
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string()),
                _ => None,
            };
            if function_name.as_deref() == Some("Err") {
                return false;
            }
            if function_name.as_deref() == Some("acquire_dependency_manifest") {
                return false;
            }
            is_manifest_decoder_call(call, text_inputs, decoder_aliases)
                || call
                    .args
                .iter()
                .any(|argument| {
                    expression_produces_decode(argument, text_inputs, decoder_aliases)
                })
        }
        Expr::MethodCall(call) => {
            expression_produces_decode(&call.receiver, text_inputs, decoder_aliases)
        }
        Expr::Await(awaited) => {
            expression_produces_decode(&awaited.base, text_inputs, decoder_aliases)
        }
        Expr::Group(group) => {
            expression_produces_decode(&group.expr, text_inputs, decoder_aliases)
        }
        Expr::Paren(paren) => {
            expression_produces_decode(&paren.expr, text_inputs, decoder_aliases)
        }
        Expr::Reference(reference) => {
            expression_produces_decode(&reference.expr, text_inputs, decoder_aliases)
        }
        Expr::Try(tried) => {
            expression_produces_decode(&tried.expr, text_inputs, decoder_aliases)
        }
        Expr::Macro(expression) => macro_expressions(&expression.mac).is_ok_and(|expressions| {
            expressions
                .iter()
                .any(|expression| {
                    expression_produces_decode(expression, text_inputs, decoder_aliases)
                })
        }),
        _ => false,
    }
}

fn returned_decode_provenance(
    signature: &Signature,
    block: &syn::Block,
    text_inputs: &BTreeSet<String>,
    decode_results: &BTreeSet<String>,
    module_aliases: &DecoderAliases,
) -> bool {
    struct Returns<'a> {
        text_inputs: &'a BTreeSet<String>,
        decode_results: &'a BTreeSet<String>,
        decoder_aliases: DecoderAliasEnvironment,
        depth: usize,
        found: bool,
    }

    impl Returns<'_> {
        fn expression_is_decode(&self, expression: &Expr) -> bool {
            expression_produces_decode(expression, self.text_inputs, &self.decoder_aliases)
                || expression_references(expression, self.decode_results)
        }
    }

    impl<'ast> Visit<'ast> for Returns<'_> {
        fn visit_block(&mut self, block: &'ast syn::Block) {
            let is_function_body = self.depth == 0;
            self.depth += 1;
            self.decoder_aliases.enter_block(block);
            for statement in &block.stmts {
                if is_function_body
                    && matches!(statement, syn::Stmt::Expr(expression, None) if self.expression_is_decode(expression))
                {
                    self.found = true;
                }
                if !self.found {
                    self.visit_stmt(statement);
                }
            }
            self.decoder_aliases.leave_block();
            self.depth -= 1;
        }

        fn visit_local(&mut self, local: &'ast Local) {
            if test_only(&local.attrs) {
                return;
            }
            visit::visit_local(self, local);
            for binding in pattern_bindings(&local.pat) {
                self.decoder_aliases.shadow(binding);
            }
        }

        fn visit_expr_return(&mut self, expression: &'ast syn::ExprReturn) {
            self.found |= expression
                .expr
                .as_ref()
                .is_some_and(|value| self.expression_is_decode(value));
            if !self.found {
                visit::visit_expr_return(self, expression);
            }
        }

        fn visit_expr_closure(&mut self, _expression: &'ast syn::ExprClosure) {}

        fn visit_item_fn(&mut self, _function: &'ast ItemFn) {}

        fn visit_impl_item_fn(&mut self, _function: &'ast ImplItemFn) {}
    }

    let mut returns = Returns {
        text_inputs,
        decode_results,
        decoder_aliases: DecoderAliasEnvironment::new(module_aliases, signature),
        depth: 0,
        found: false,
    };
    returns.visit_block(block);
    returns.found
}

fn block_rejects(block: &syn::Block) -> bool {
    #[derive(Default)]
    struct Rejections {
        found: bool,
    }

    impl<'ast> Visit<'ast> for Rejections {
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            self.found |= matches!(
                call.func.as_ref(),
                Expr::Path(function) if path_ends_with(&function.path, &["Err"])
            );
            if !self.found {
                visit::visit_expr_call(self, call);
            }
        }
    }

    let mut rejections = Rejections::default();
    rejections.visit_block(block);
    rejections.found
}

#[derive(Default)]
struct PredicateFacts {
    inspects_source_kind: bool,
    classifies_remote: bool,
}

struct PredicateVisitor<'a> {
    parsed_sources: &'a BTreeSet<String>,
    text_inputs: &'a BTreeSet<String>,
    remote_predicates: &'a BTreeSet<String>,
    facts: PredicateFacts,
}

impl PredicateVisitor<'_> {
    fn inspect_macro(&mut self, invocation: &Macro) {
        if let Ok(expressions) = macro_expressions(invocation) {
            for expression in &expressions {
                self.visit_expr(expression);
            }
        }
    }
}

impl<'ast> Visit<'ast> for PredicateVisitor<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        self.facts.inspects_source_kind |= expression_references(
            &call.receiver,
            self.parsed_sources,
        ) && call.method == "mode";
        self.facts.classifies_remote |= expression_references(&call.receiver, self.text_inputs)
            && matches!(
                call.method.to_string().as_str(),
                "components"
                    | "contains"
                    | "ends_with"
                    | "find"
                    | "has_root"
                    | "is_absolute"
                    | "parent"
                    | "starts_with"
                    | "strip_prefix"
            );
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let summarized_remote_predicate = matches!(
            call.func.as_ref(),
            Expr::Path(function)
                if function.path.segments.last().is_some_and(|segment| {
                    self.remote_predicates.contains(&segment.ident.to_string())
                })
        );
        if summarized_remote_predicate {
            self.facts.inspects_source_kind |= call
                .args
                .iter()
                .any(|argument| expression_references(argument, self.parsed_sources));
        }
        self.facts.classifies_remote |= call
            .args
            .iter()
            .any(|argument| expression_references(argument, self.text_inputs));
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_field(&mut self, field: &'ast ExprField) {
        self.facts.inspects_source_kind |= expression_references(
            &field.base,
            self.parsed_sources,
        ) && matches!(&field.member, Member::Named(member) if member == "remote");
        visit::visit_expr_field(self, field);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        self.inspect_macro(&expression.mac);
    }
}

#[derive(Default)]
struct LocalHelpers {
    remote_predicates: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for LocalHelpers {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if test_only(&function.attrs) || !return_type_mentions(&function.sig, "bool") {
            return;
        }
        let parsed_sources = typed_bindings(&function.sig, "ParsedSource");
        let text_inputs = text_bindings(&function.sig);
        if parsed_sources.is_empty() || text_inputs.is_empty() {
            return;
        }
        let no_summaries = BTreeSet::new();
        let mut visitor = PredicateVisitor {
            parsed_sources: &parsed_sources,
            text_inputs: &text_inputs,
            remote_predicates: &no_summaries,
            facts: PredicateFacts::default(),
        };
        visitor.visit_block(&function.block);
        if visitor.facts.inspects_source_kind && visitor.facts.classifies_remote {
            self.remote_predicates
                .insert(function.sig.ident.to_string());
        }
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        if !test_only(&module.attrs) {
            visit::visit_item_mod(self, module);
        }
    }
}

struct FunctionFacts<'a> {
    types: &'a TypeFacts,
    helpers: &'a LocalHelpers,
    decoder_aliases: DecoderAliasEnvironment,
    source_backends: BTreeSet<String>,
    binding_types: BTreeMap<String, String>,
    parsed_sources: BTreeSet<String>,
    text_inputs: BTreeSet<String>,
    confinement_predicates: BTreeSet<String>,
    decode_results: BTreeSet<String>,
    returns_transitive_manifest: bool,
    reads_source_file: bool,
    decodes_transitive_manifest: bool,
    implements_remote_confinement: bool,
    unsupported_macro: bool,
}

impl<'a> FunctionFacts<'a> {
    fn new(
        signature: &Signature,
        types: &'a TypeFacts,
        helpers: &'a LocalHelpers,
        decoder_aliases: &'a DecoderAliases,
        self_type: Option<&str>,
    ) -> Self {
        let mut binding_types = binding_types(signature);
        if let Some(self_type) = self_type {
            binding_types.insert("self".to_owned(), self_type.to_owned());
        }
        Self {
            types,
            helpers,
            decoder_aliases: DecoderAliasEnvironment::new(decoder_aliases, signature),
            source_backends: typed_bindings(signature, "SourceBackend"),
            binding_types,
            parsed_sources: typed_bindings(signature, "ParsedSource"),
            text_inputs: text_bindings(signature),
            confinement_predicates: BTreeSet::new(),
            decode_results: BTreeSet::new(),
            returns_transitive_manifest: return_type_mentions(signature, "TransitiveManifest"),
            reads_source_file: false,
            decodes_transitive_manifest: false,
            implements_remote_confinement: false,
            unsupported_macro: false,
        }
    }

    fn expression_is_source_backend(&self, expression: &Expr) -> bool {
        if expression_references(expression, &self.source_backends) {
            return true;
        }
        match expression {
            Expr::Reference(reference) => {
                return self.expression_is_source_backend(&reference.expr);
            }
            Expr::Group(group) => return self.expression_is_source_backend(&group.expr),
            Expr::Paren(paren) => return self.expression_is_source_backend(&paren.expr),
            _ => {}
        }
        let Expr::Field(field) = expression else {
            return false;
        };
        let Member::Named(member) = &field.member else {
            return false;
        };
        let Expr::Path(base) = field.base.as_ref() else {
            return false;
        };
        let Some(binding) = base.path.segments.last() else {
            return false;
        };
        self.binding_types
            .get(&binding.ident.to_string())
            .and_then(|r#type| self.types.source_backend_fields.get(r#type))
            .is_some_and(|fields| fields.contains(&member.to_string()))
    }

    fn predicate_facts(&self, expression: &Expr) -> PredicateFacts {
        let mut visitor = PredicateVisitor {
            parsed_sources: &self.parsed_sources,
            text_inputs: &self.text_inputs,
            remote_predicates: &self.helpers.remote_predicates,
            facts: PredicateFacts::default(),
        };
        visitor.visit_expr(expression);
        visitor.facts
    }

    fn is_confinement_predicate(&self, expression: &Expr) -> bool {
        if expression_references(expression, &self.confinement_predicates) {
            return true;
        }
        let facts = self.predicate_facts(expression);
        facts.inspects_source_kind && facts.classifies_remote
    }

    fn inspect_macro(&mut self, invocation: &Macro) {
        let Ok(expressions) = macro_expressions(invocation) else {
            self.unsupported_macro = true;
            return;
        };
        for expression in &expressions {
            self.visit_expr(expression);
        }
    }
}

impl<'ast> Visit<'ast> for FunctionFacts<'_> {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.decoder_aliases.enter_block(block);
        visit::visit_block(self, block);
        self.decoder_aliases.leave_block();
    }

    fn visit_local(&mut self, local: &'ast Local) {
        if test_only(&local.attrs) {
            return;
        }
        if let Some(initializer) = &local.init
            && let Some(name) = binding_name(&local.pat)
        {
            if self.expression_is_source_backend(&initializer.expr)
                || pattern_type(&local.pat)
                    .is_some_and(|r#type| type_mentions(r#type, "SourceBackend"))
            {
                self.source_backends.insert(name.clone());
            }
            if self.is_confinement_predicate(&initializer.expr) {
                self.confinement_predicates.insert(name.clone());
            }
            if self.returns_transitive_manifest
                && (expression_produces_decode(
                    &initializer.expr,
                    &self.text_inputs,
                    &self.decoder_aliases,
                )
                    || expression_references(&initializer.expr, &self.decode_results))
            {
                self.decode_results.insert(name);
            }
        }
        if pattern_type(&local.pat).is_some_and(|r#type| {
            type_mentions(r#type, "TransitiveManifest")
                && local
                    .init
                    .as_ref()
                    .is_some_and(|initializer| expression_has_nonboundary_call(&initializer.expr))
        }) {
            self.decodes_transitive_manifest = true;
        }
        visit::visit_local(self, local);
        for binding in pattern_bindings(&local.pat) {
            self.decoder_aliases.shadow(binding);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if test_only(&call.attrs) {
            return;
        }
        if call.method == "read_file_at"
            && self.expression_is_source_backend(&call.receiver)
        {
            self.reads_source_file = true;
        }
        if call
            .turbofish
            .as_ref()
            .is_some_and(|arguments| angle_arguments_mention(arguments, "TransitiveManifest"))
        {
            self.decodes_transitive_manifest = true;
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if test_only(&call.attrs) {
            return;
        }
        if let Expr::Path(function) = call.func.as_ref() {
            let source_backend_ufcs = path_ends_with(&function.path, &["read_file_at"])
                && (path_mentions(&function.path, "SourceBackend")
                    || function
                        .qself
                        .as_ref()
                        .is_some_and(|qself| type_mentions(&qself.ty, "SourceBackend")));
            let source_backend_argument = path_ends_with(&function.path, &["read_file_at"])
                && call
                    .args
                    .iter()
                    .any(|argument| self.expression_is_source_backend(argument));
            self.reads_source_file |= source_backend_ufcs || source_backend_argument;
            self.decodes_transitive_manifest |= path_mentions(&function.path, "TransitiveManifest")
                || path_arguments_mention(&function.path, "TransitiveManifest")
                || function
                    .qself
                    .as_ref()
                    .is_some_and(|qself| type_mentions(&qself.ty, "TransitiveManifest"));
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
        if !test_only(&expression.attrs)
            && self.is_confinement_predicate(&expression.cond)
            && block_rejects(&expression.then_branch)
        {
            self.implements_remote_confinement = true;
        }
        visit::visit_expr_if(self, expression);
    }

    fn visit_expr_match(&mut self, expression: &'ast syn::ExprMatch) {
        if !test_only(&expression.attrs)
            && self.is_confinement_predicate(&expression.expr)
            && expression.arms.iter().any(|arm| {
                let block = syn::Block {
                    brace_token: Default::default(),
                    stmts: vec![syn::Stmt::Expr((*arm.body).clone(), None)],
                };
                block_rejects(&block)
            })
        {
            self.implements_remote_confinement = true;
        }
        visit::visit_expr_match(self, expression);
    }

    fn visit_expr_struct(&mut self, structure: &'ast syn::ExprStruct) {
        if !test_only(&structure.attrs) && path_mentions(&structure.path, "TransitiveManifest") {
            self.decodes_transitive_manifest = true;
        }
        visit::visit_expr_struct(self, structure);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        if !test_only(&expression.attrs) {
            self.inspect_macro(&expression.mac);
        }
    }

    fn visit_stmt_macro(&mut self, statement: &'ast syn::StmtMacro) {
        if !test_only(&statement.attrs) {
            self.inspect_macro(&statement.mac);
        }
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if !test_only(&function.attrs) {
            visit::visit_item_fn(self, function);
        }
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        if !test_only(&function.attrs) {
            visit::visit_impl_item_fn(self, function);
        }
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        if !test_only(&module.attrs) {
            visit::visit_item_mod(self, module);
        }
    }

    fn visit_item_macro(&mut self, invocation: &'ast ItemMacro) {
        if invocation.ident.is_none() && !test_only(&invocation.attrs) {
            self.unsupported_macro = true;
        }
    }
}

struct ProductionVisitor<'a> {
    file: &'a FsPath,
    types: &'a TypeFacts,
    helpers: &'a LocalHelpers,
    decoder_aliases: DecoderAliases,
    current_impl: Option<String>,
    violations: Vec<String>,
}

impl ProductionVisitor<'_> {
    fn inspect(&mut self, signature: &Signature, block: &syn::Block) {
        let name = signature.ident.to_string();
        let mut facts = FunctionFacts::new(
            signature,
            self.types,
            self.helpers,
            &self.decoder_aliases,
            self.current_impl.as_deref(),
        );
        facts.visit_block(block);
        if facts.returns_transitive_manifest {
            facts.decodes_transitive_manifest |= returned_decode_provenance(
                signature,
                block,
                &facts.text_inputs,
                &facts.decode_results,
                &self.decoder_aliases,
            );
        }
        if facts.reads_source_file {
            self.violations.push(format!(
                "{}::{name}: directly reads source content instead of delegating acquisition",
                self.file.display()
            ));
        }
        if facts.decodes_transitive_manifest {
            self.violations.push(format!(
                "{}::{name}: decodes TransitiveManifest inside sync",
                self.file.display()
            ));
        }
        if facts.implements_remote_confinement {
            self.violations.push(format!(
                "{}::{name}: implements transitive remote confinement inside sync",
                self.file.display()
            ));
        }
        if facts.unsupported_macro {
            self.violations.push(format!(
                "{}::{name}: unsupported live macro could hide a duplicate implementation",
                self.file.display()
            ));
        }
    }
}

impl<'ast> Visit<'ast> for ProductionVisitor<'_> {
    fn visit_file(&mut self, file: &'ast syn::File) {
        let previous = std::mem::replace(
            &mut self.decoder_aliases,
            decoder_aliases_in_items(&file.items),
        );
        for item in &file.items {
            self.visit_item(item);
        }
        self.decoder_aliases = previous;
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        if !test_only(&function.attrs) {
            self.inspect(&function.sig, &function.block);
        }
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        if !test_only(&function.attrs) {
            self.inspect(&function.sig, &function.block);
        }
    }

    fn visit_item_impl(&mut self, implementation: &'ast ItemImpl) {
        if test_only(&implementation.attrs) {
            return;
        }
        let previous = self.current_impl.take();
        self.current_impl = named_type(&implementation.self_ty);
        visit::visit_item_impl(self, implementation);
        self.current_impl = previous;
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        if test_only(&module.attrs) {
            return;
        }
        if let Some((_, items)) = &module.content {
            let previous = std::mem::replace(
                &mut self.decoder_aliases,
                decoder_aliases_in_items(items),
            );
            for item in items {
                self.visit_item(item);
            }
            self.decoder_aliases = previous;
        }
    }

    fn visit_item_macro(&mut self, invocation: &'ast ItemMacro) {
        if invocation.ident.is_none() && !test_only(&invocation.attrs) {
            self.violations.push(format!(
                "{}: unsupported live item macro could hide a duplicate implementation",
                self.file.display()
            ));
        }
    }
}

#[derive(Default)]
struct DirectManifestReader {
    reads_source_file: bool,
    decodes_utf8: bool,
}

impl<'ast> Visit<'ast> for DirectManifestReader {
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        self.reads_source_file |= call.method == "read_file_at";
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            self.reads_source_file |= path_ends_with(&function.path, &["read_file_at"]);
            self.decodes_utf8 |= function
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident.to_string().starts_with("from_utf8"));
        }
        visit::visit_expr_call(self, call);
    }
}

struct AllFunctionManifestReaders<'a> {
    file: &'a FsPath,
    violations: Vec<String>,
}

impl AllFunctionManifestReaders<'_> {
    fn inspect(&mut self, signature: &Signature, block: &syn::Block) {
        let mut operations = DirectManifestReader::default();
        operations.visit_block(block);
        if operations.reads_source_file && operations.decodes_utf8 {
            self.violations.push(format!(
                "{}::{}: source manifest acquisition and UTF-8 decoding must stay source-owned",
                self.file.display(),
                signature.ident,
            ));
        }
    }
}

impl<'ast> Visit<'ast> for AllFunctionManifestReaders<'_> {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.inspect(&function.sig, &function.block);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast ImplItemFn) {
        self.inspect(&function.sig, &function.block);
    }
}

fn main() {
    let mut files = Vec::new();
    for argument in std::env::args_os().skip(1) {
        let path = std::path::PathBuf::from(argument);
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            eprintln!("{}: read failed: {error}", path.display());
            std::process::exit(2);
        });
        let syntax = syn::parse_file(&source).unwrap_or_else(|error| {
            eprintln!("{}: Rust parse failed: {error}", path.display());
            std::process::exit(2);
        });
        files.push((path, syntax));
    }
    let mut types = TypeFacts::default();
    for (_, syntax) in &files {
        types.visit_file(syntax);
    }
    let mut helpers = LocalHelpers::default();
    for (_, syntax) in &files {
        helpers.visit_file(syntax);
    }
    let mut violations = Vec::new();
    for (path, syntax) in &files {
        let mut visitor = ProductionVisitor {
            file: path,
            types: &types,
            helpers: &helpers,
            decoder_aliases: DecoderAliases::default(),
            current_impl: None,
            violations: Vec::new(),
        };
        visitor.visit_file(syntax);
        violations.extend(visitor.violations);

        let mut all_functions = AllFunctionManifestReaders {
            file: path,
            violations: Vec::new(),
        };
        all_functions.visit_file(syntax);
        violations.extend(all_functions.violations);
    }
    if !violations.is_empty() {
        for violation in violations {
            eprintln!("{violation}");
        }
        std::process::exit(1);
    }
}
"#;

fn sync_production_files() -> Vec<PathBuf> {
    let sync = src_dir().join("sync");
    let mut relative = Vec::new();
    rust_files(&sync, &sync, &mut relative);
    relative
        .into_iter()
        .filter(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| name != "tests.rs" && !name.ends_with("_tests.rs"))
        })
        .map(|path| sync.join(path))
        .collect()
}

fn run_duplicate_ast_probe(files: &[PathBuf]) -> Output {
    let fixture = tempfile::TempDir::new().expect("AST probe tempdir");
    fs::create_dir(fixture.path().join("src")).expect("create AST probe src");
    fs::write(
        fixture.path().join("Cargo.toml"),
        "[package]\nname = \"t029-sync-duplicate-probe\"\nversion = \"0.0.0\"\nedition = \
         \"2024\"\n\n[dependencies]\nsyn = { version = \"2\", features = [\"full\", \"visit\"] \
         }\n",
    )
    .expect("write AST probe manifest");
    fs::write(fixture.path().join("src/main.rs"), SYNC_DUPLICATE_AST_PROBE)
        .expect("write AST probe source");

    let mut command = Command::new(env!("CARGO"));
    command
        .args(["run", "--quiet", "--offline", "--manifest-path"])
        .arg(fixture.path().join("Cargo.toml"))
        .arg("--")
        .args(files)
        .env("CARGO_TARGET_DIR", fixture.path().join("target"));
    command
        .output()
        .expect("run Rust-aware sync duplicate probe")
}

fn run_sync_duplicate_ast_probe() -> Output {
    run_duplicate_ast_probe(&sync_production_files())
}

fn copy_probe_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create probe directory");
    for entry in fs::read_dir(from).expect("read probe source directory") {
        let entry = entry.expect("read probe source entry");
        let destination = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_probe_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy probe source file");
        }
    }
}

fn append_probe(path: &Path, probe: &str) {
    let mut source = fs::read_to_string(path).expect("read probe module");
    source.push_str(probe);
    fs::write(path, source).expect("append white-box probe");
}

fn inject_probe_statement(source: &mut String, function: &str, statement: &str) -> bool {
    let needle = format!("fn {function}");
    let Some(function_start) = source.find(&needle) else {
        return false;
    };
    let Some(opening_offset) = source[function_start..].find('{') else {
        return false;
    };
    source.insert_str(function_start + opening_offset + 1, statement);
    true
}

fn function_parameter_names(source: &str, function: &str) -> Option<Vec<String>> {
    let function_start = source.find(&format!("fn {function}"))?;
    let opening = function_start + source[function_start..].find('(')?;
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut closing = None;
    for (offset, byte) in bytes[opening..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    closing = Some(opening + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let parameters = &source[opening + 1..closing?];
    let mut names = Vec::new();
    let mut start = 0usize;
    let mut nested = 0usize;
    for (index, byte) in parameters.bytes().chain(std::iter::once(b',')).enumerate() {
        match byte {
            b'(' | b'[' | b'{' | b'<' => nested += 1,
            b')' | b']' | b'}' | b'>' => nested = nested.saturating_sub(1),
            b',' if nested == 0 => {
                let parameter = parameters[start..index].trim();
                if parameter.is_empty() {
                    start = index + 1;
                    continue;
                }
                let binding = parameter.split_once(':')?.0.trim();
                let name = binding
                    .split_whitespace()
                    .last()?
                    .trim_start_matches(['&', '*'])
                    .to_owned();
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return None;
                }
                names.push(name);
                start = index + 1;
            }
            _ => {}
        }
    }
    Some(names)
}

const BOUNDARY_ATTRIBUTE_PROBE: &str = r#"
fn is_rustfmt_skip(attribute: &syn::Attribute) -> bool {
    let segments: Vec<String> = attribute
        .path()
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    segments == ["rustfmt", "skip"]
}

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .expect("source path");
    let source = std::fs::read_to_string(&path).expect("read source");
    let syntax = syn::parse_file(&source).expect("parse Rust source");
    let expected = ["acquire_dependency_manifest", "validate_dependency_remote"];
    let mut found = std::collections::BTreeSet::new();
    let mut failed = false;
    for item in &syntax.items {
        let syn::Item::Fn(function) = item else {
            continue;
        };
        let name = function.sig.ident.to_string();
        if !expected.contains(&name.as_str()) {
            continue;
        }
        found.insert(name.clone());
        if function.attrs.iter().any(is_rustfmt_skip) {
            eprintln!("{name}: boundary function must not use #[rustfmt::skip]");
            failed = true;
        }
    }
    for name in expected {
        if !found.contains(name) {
            eprintln!("{name}: boundary function missing from parsed source");
            failed = true;
        }
    }
    if failed {
        std::process::exit(1);
    }
}
"#;

fn run_boundary_attribute_probe(path: &Path) -> Output {
    let fixture = tempfile::TempDir::new().expect("boundary attribute probe tempdir");
    fs::create_dir(fixture.path().join("src")).expect("create probe src");
    fs::write(
        fixture.path().join("Cargo.toml"),
        "[package]\nname = \"t029-boundary-attribute-probe\"\nversion = \"0.0.0\"\n\
         edition = \"2024\"\n\n[dependencies]\nsyn = { version = \"2\", features = [\"full\"] }\n",
    )
    .expect("write boundary attribute probe manifest");
    fs::write(fixture.path().join("src/main.rs"), BOUNDARY_ATTRIBUTE_PROBE)
        .expect("write boundary attribute probe");
    Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--offline", "--manifest-path"])
        .arg(fixture.path().join("Cargo.toml"))
        .arg("--")
        .arg(path)
        .env("CARGO_TARGET_DIR", fixture.path().join("target"))
        .output()
        .expect("run Rust-aware boundary attribute probe")
}

fn instrument_source_boundary(path: &Path) {
    let Ok(mut source) = fs::read_to_string(path) else {
        return;
    };
    if let Some(parameters) = function_parameter_names(&source, "acquire_dependency_manifest")
        && let [_, source_name, parsed_source, remote, pinned_commit] = parameters.as_slice()
    {
        let statement = r#"
    T029_ACQUIRE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    T029_ACQUIRE_ARGS.lock().expect("acquire args").push((
        __SOURCE_NAME__.as_str().to_owned(),
        __REMOTE__.to_owned(),
        __PARSED_SOURCE__.is_transitive(),
        __PINNED_COMMIT__.is_none(),
    ));
    if T029_ACQUIRE_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst) {
        let manifest = crate::config::transitive::TransitiveManifest::parse(
            "version = 1\n\n[targets.boundary]\npath = \"boundary\"\n",
        )
        .expect("the static boundary sentinel manifest parses");
        return Ok(("c".repeat(40), manifest));
    }
"#
        .replace("__SOURCE_NAME__", source_name)
        .replace("__PARSED_SOURCE__", parsed_source)
        .replace("__REMOTE__", remote)
        .replace("__PINNED_COMMIT__", pinned_commit);
        inject_probe_statement(&mut source, "acquire_dependency_manifest", &statement);
    }
    if let Some(parameters) = function_parameter_names(&source, "validate_dependency_remote")
        && let [name, parsed_source, remote, depth] = parameters.as_slice()
    {
        let statement = r#"
    T029_VALIDATE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    T029_VALIDATE_ARGS.lock().expect("validate args").push((
        __NAME__.to_owned(),
        __REMOTE__.to_owned(),
        __DEPTH__,
        __PARSED_SOURCE__.is_transitive(),
    ));
    if T029_VALIDATE_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
"#
        .replace("__NAME__", name)
        .replace("__PARSED_SOURCE__", parsed_source)
        .replace("__REMOTE__", remote)
        .replace("__DEPTH__", depth);
        inject_probe_statement(&mut source, "validate_dependency_remote", &statement);
    }
    source.push_str(
        r"

pub(crate) static T029_ACQUIRE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
pub(crate) static T029_VALIDATE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
pub(crate) static T029_ACQUIRE_ARGS: std::sync::Mutex<
    Vec<(String, String, bool, bool)>,
> = std::sync::Mutex::new(Vec::new());
pub(crate) static T029_VALIDATE_ARGS: std::sync::Mutex<
    Vec<(String, String, usize, bool)>,
> = std::sync::Mutex::new(Vec::new());
pub(crate) static T029_ACQUIRE_OVERRIDE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(crate) static T029_VALIDATE_OVERRIDE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
",
    );
    fs::write(path, source).expect("instrument source boundary probe");
}

fn run_source_boundary_probe() -> Output {
    let fixture = tempfile::TempDir::new().expect("boundary probe tempdir");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::copy(root.join("Cargo.toml"), fixture.path().join("Cargo.toml"))
        .expect("copy probe Cargo.toml");
    fs::copy(root.join("Cargo.lock"), fixture.path().join("Cargo.lock"))
        .expect("copy probe Cargo.lock");
    fs::copy(
        root.join("phora.example.toml"),
        fixture.path().join("phora.example.toml"),
    )
    .expect("copy probe example config");
    fs::copy(
        root.join("phora.local.example.toml"),
        fixture.path().join("phora.local.example.toml"),
    )
    .expect("copy probe local example config");
    copy_probe_tree(&root.join("src"), &fixture.path().join("src"));
    copy_probe_tree(&root.join("benches"), &fixture.path().join("benches"));

    instrument_source_boundary(&fixture.path().join("src/source/transitive.rs"));
    append_probe(
        &fixture.path().join("src/source/mod.rs"),
        SOURCE_BOUNDARY_PROBE,
    );
    append_probe(
        &fixture.path().join("src/sync/transitive.rs"),
        SYNC_DELEGATION_PROBE,
    );

    Command::new(env!("CARGO"))
        .args([
            "test",
            "--offline",
            "--lib",
            "t029_boundary_",
            "--manifest-path",
        ])
        .arg(fixture.path().join("Cargo.toml"))
        .args(["--", "--test-threads=1", "--nocapture"])
        .env(
            "CARGO_TARGET_DIR",
            root.join("target/t029-source-boundary-probe"),
        )
        .output()
        .expect("run callable source boundary probe")
}

struct TransitiveFixture {
    _home: tempfile::TempDir,
    cwd: tempfile::TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn transitive_fixture() -> TransitiveFixture {
    let home = tempfile::TempDir::new().expect("home tempdir");
    let cwd = tempfile::TempDir::new().expect("cwd tempdir");
    let home_path = home.path().to_path_buf();
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");
    TransitiveFixture {
        _home: home,
        cwd,
        home_path,
        xdg_cache,
        xdg_state,
    }
}

fn write_fixture(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, body).expect("write fixture file");
}

fn fixture_git(cwd: &Path, args: &[&str]) {
    common::assert_sandboxed(cwd);
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
        .output()
        .expect("fixture git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_transitive_manifest(dir: &Path, manifest: &str, shipped_lock: &str) {
    fixture_git(dir, &["init", "-b", "main", "."]);
    fixture_git(dir, &["config", "user.email", "test@example.com"]);
    fixture_git(dir, &["config", "user.name", "Test"]);
    write_fixture(&dir.join("phora.toml"), manifest.as_bytes());
    write_fixture(&dir.join("phora.lock"), shipped_lock.as_bytes());
    fixture_git(dir, &["add", "-A"]);
    fixture_git(dir, &["commit", "-m", "transitive fixture"]);
}

fn run_phora(fixture: &TransitiveFixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(fixture.cwd.path())
        .env("HOME", &fixture.home_path)
        .env("XDG_CACHE_HOME", &fixture.xdg_cache)
        .env("XDG_STATE_HOME", &fixture.xdg_state)
        .output()
        .expect("phora binary runs")
}

#[test]
fn live_item_scanner_rejects_inert_and_unrelated_decoys() {
    let decoys = r##"
        // pub struct Digest;
        /* pub struct ProjectId; */
        const TEXT: &str = "pub struct Commit; pub mod digest;";
        const MULTILINE: &str = "first line
            pub struct Algo;
            pub mod transitive;
            final line";
        const BYTE_MULTILINE: &[u8] = b"first line
            pub struct KernelError;
            pub fn safe_component() {}
            final line";
        const RAW: &str = r#"pub fn safe_relpath() {}"#;
        macro_rules! fake { () => { pub struct SourceName; impl SourceName {} } }
        fake!();
        #[cfg(test)] pub struct TargetName;
        #[cfg(all(test, unix))] pub struct ArtifactName;
        #[cfg(any())] pub fn safe_component() {}
        #[cfg(test)] impl DigestSuffix { fn hidden() {} }
        pub struct DigestSuffix;
        pub mod digest_extra;
        mod nested { pub struct ArtifactName; pub mod transitive; }
    "##;
    let scanned = scan_live_items(decoys);
    for name in [
        "Digest",
        "ProjectId",
        "Commit",
        "SourceName",
        "TargetName",
        "ArtifactName",
    ] {
        assert!(!scanned.defines_type(name), "decoy defined `{name}`");
    }
    assert!(!scanned.defines_function("safe_component"));
    assert!(!scanned.defines_function("safe_relpath"));
    assert!(!scanned.items.iter().any(|item| matches!(item, Item::Impl)));
    assert!(scanned.items.iter().any(
        |item| matches!(item, Item::Other(description) if description == "unsupported item macro invocation: fake!")
    ));
    assert!(!scanned.declares_file_module("digest"));
    assert!(!scanned.declares_file_module("transitive"));

    let live = scan_live_items(
        r"
        pub struct Digest;
        #[cfg(not(test))] pub struct Commit;
        #[cfg_attr(test, allow(dead_code))] pub struct SourceName;
        #[cfg_attr(test, allow(dead_code))] impl Digest {}
        #[cfg(any(test, unix))] pub(crate) fn safe_component() {}
        #[cfg_attr(test, allow(dead_code))] pub(crate) fn safe_relpath() {}
        pub mod digest;
        mod transitive;
    ",
    );
    assert!(live.defines_type("Digest"));
    assert!(live.defines_type("Commit"));
    assert!(live.defines_type("SourceName"));
    assert!(live.defines_function("safe_component"));
    assert!(live.defines_function("safe_relpath"));
    assert!(live.items.iter().any(|item| matches!(item, Item::Impl)));
    assert!(live.declares_public_file_module("digest"));
    assert!(live.declares_file_module("transitive"));
}

fn assert_ast_violation(diagnostics: &str, function: &str, responsibility: &str) {
    assert!(
        diagnostics.contains(&format!("::{function}: {responsibility}")),
        "missing `{function}: {responsibility}` from AST diagnostics: {diagnostics}"
    );
}

const INERT_DUPLICATE_AST_CONTROL: &str = r#"
// backend.read_file_at(); TransitiveManifest::parse(text);
const TEXT: &str = "Remote::Path plus is_local_path(remote)";
#[cfg(test)]
fn cfg_only() {
    backend.read_file_at();
    TransitiveManifest::parse(text);
    matches!(remote, Remote::Path(_));
    is_local_path(remote);
}
macro_rules! inert_macro {
    () => {
        SourceBackend::read_file_at(backend);
        TransitiveManifest::parse(text);
    };
}
#[cfg(test)]
inert_macro!();
fn production_with_cfg_decoys() {
    #[cfg(test)]
    backend.read_file_at();
    #[cfg(test)]
    let _ = toml::from_str::<TransitiveManifest>(text);
}
fn delegates() {
    acquire_dependency_manifest();
    validate_dependency_remote();
}
struct UnrelatedReader;
struct UnrelatedContext {
    backend: UnrelatedReader,
}
impl UnrelatedContext {
    fn unrelated_self_field(&self) {
        self.backend.read_file_at();
    }
}
fn unrelated_same_method(reader: &UnrelatedReader) {
    reader.read_file_at();
}
fn unrelated_same_field_and_method(ctx: &UnrelatedContext) {
    ctx.backend.read_file_at();
}
fn literal_only_macro_mentions(reader: &UnrelatedReader) {
    println!("SourceBackend::read_file_at({reader:?}) and TransitiveManifest::parse");
    dbg!("backend.read_file_at() is only a literal");
}
fn unrelated_toml_decode(text: &str) {
    let _config: Config = toml::from_str(text).unwrap();
}
fn unrelated_typed_return(text: &str) -> Result<Config, Error> {
    toml::from_str(text)
}
fn decode_manifest(_payload: &str) -> Result<TransitiveManifest, Error> {
    Err(Error)
}
fn unrelated_named_decoder(payload: &str) -> Result<TransitiveManifest, Error> {
    let value = decode_manifest(payload)?;
    Ok(value)
}
fn benign_manifest_forwarder(
    manifests: &BTreeMap<String, TransitiveManifest>,
    name: &str,
) -> Result<TransitiveManifest, Error> {
    manifests.get(name).cloned().ok_or(Error)
}
fn source_boundary_manifest_wrapper(
    backend: &dyn SourceBackend,
    name: &str,
    source: &ParsedSource,
    remote: &str,
) -> Result<TransitiveManifest, Error> {
    let source_name = SourceName::trusted(name.to_owned());
    source::transitive::acquire_dependency_manifest(
        backend,
        &source_name,
        source,
        remote,
        None,
    )
    .map(|(_, manifest)| manifest)
}
fn benign_remote_orchestration(source: &ParsedSource, remote: &str) -> String {
    let mode = source.mode();
    let normalized = remote.trim().to_owned();
    format!("{mode:?}:{normalized}")
}
fn normalize_remote(source: &ParsedSource, remote: &str) -> String {
    format!("{:?}:{}", source.mode(), remote.trim())
}
fn benign_helper_composition(source: &ParsedSource, remote: &str) -> Result<(), Error> {
    let normalized = normalize_remote(source, remote);
    if normalized.is_empty() {
        return Err(Error);
    }
    Ok(())
}
fn benign_source_predicate(source: &ParsedSource, remote: &str) -> bool {
    let _ = remote;
    matches!(source.mode(), SourceMode::Git)
}
fn benign_predicate_composition(source: &ParsedSource, remote: &str) -> Result<(), Error> {
    if benign_source_predicate(source, remote) {
        return Err(Error);
    }
    Ok(())
}
"#;

const LIVE_DUPLICATE_AST_CONTROL: &str = r#"
use toml::from_str as decode_manifest;
use toml as manifest_format;

fn dot_acquire(backend: &dyn SourceBackend) {
    backend.read_file_at();
}
#[cfg(not(test))]
fn ufcs_acquire(backend: &dyn SourceBackend) {
    SourceBackend::read_file_at(backend);
}
struct ReadContext<'a> {
    backend: &'a dyn SourceBackend,
}
struct SelfReadContext<'a> {
    backend: &'a dyn SourceBackend,
}
#[cfg_attr(test, allow(dead_code))]
impl SelfReadContext<'_> {
    fn self_acquire(&self) {
        self.backend.read_file_at();
    }
}
fn field_acquire(ctx: &ReadContext<'_>) {
    ctx.backend.read_file_at();
}
fn aliased_acquire(ctx: &ReadContext<'_>) {
    let reader = &ctx.backend;
    reader.read_file_at();
}
fn macro_acquire(ctx: &ReadContext<'_>) {
    let reader = &ctx.backend;
    dbg!(reader.read_file_at());
}
#[cfg_attr(test, allow(dead_code))]
fn alternative_decode(text: &str) {
    let _ = toml::from_str::<TransitiveManifest>(text);
}
fn inferred_decode(text: &str) -> Result<(), Error> {
    let _manifest: TransitiveManifest = toml::from_str(text)?;
    Ok(())
}
fn macro_inferred_decode(text: &str) -> Result<(), Error> {
    let _manifest: TransitiveManifest = dbg!(toml::from_str(text)?);
    Ok(())
}
fn returned_decode(text: &str) -> Result<TransitiveManifest, Error> {
    toml::from_str(text)
}
fn local_then_return_decode(payload: &str) -> Result<TransitiveManifest, Error> {
    let value = toml::from_str(payload)?;
    Ok(value)
}
fn aliased_local_then_return_decode(payload: &str) -> Result<TransitiveManifest, Error> {
    let value = decode_manifest(payload)?;
    Ok(value)
}
fn module_aliased_local_then_return_decode(
    payload: &str,
) -> Result<TransitiveManifest, Error> {
    let value = manifest_format::from_str(payload)?;
    Ok(value)
}
fn inline_confine(source: &ParsedSource, remote: &str) -> Result<(), Error> {
    if matches!(source.remote, Remote::Path(_)) && remote.starts_with("file://") {
        return Err(Error);
    }
    Ok(())
}
fn looks_on_disk(candidate: &str) -> bool {
    std::path::Path::new(candidate).is_absolute()
}
fn renamed_confine(source: &ParsedSource, candidate: &str) -> Result<(), Error> {
    let escapes = source.mode() == SourceMode::Git && looks_on_disk(candidate);
    if escapes {
        return Err(Error);
    }
    Ok(())
}
fn split_escape_predicate(source: &ParsedSource, remote: &str) -> bool {
    matches!(source.remote, Remote::Path(_)) && remote.starts_with("file://")
}
fn split_confine(source: &ParsedSource, remote: &str) -> Result<(), Error> {
    if split_escape_predicate(source, remote) {
        return Err(Error);
    }
    Ok(())
}
fn hidden_by_expansion() {
    duplicate_source_logic!();
}
"#;

#[test]
fn sync_duplicate_ast_probe_rejects_live_and_ignores_inert_decoys() {
    let fixture = tempfile::TempDir::new().expect("AST control tempdir");
    let inert = fixture.path().join("inert.rs");
    fs::write(&inert, INERT_DUPLICATE_AST_CONTROL).expect("write inert AST control");
    let inert_output = run_duplicate_ast_probe(std::slice::from_ref(&inert));
    assert!(
        inert_output.status.success(),
        "comments, literals, cfg-only items, inert macro definitions, and calls to the approved \
         source boundary must not be reported:\n{}{}",
        String::from_utf8_lossy(&inert_output.stdout),
        String::from_utf8_lossy(&inert_output.stderr)
    );

    let live = fixture.path().join("live.rs");
    fs::write(&live, LIVE_DUPLICATE_AST_CONTROL).expect("write live AST control");
    let live_output = run_duplicate_ast_probe(std::slice::from_ref(&live));
    let live_diagnostics = String::from_utf8_lossy(&live_output.stderr);
    assert!(!live_output.status.success(), "live duplicates must fail");
    for (function, responsibility) in [
        ("dot_acquire", "directly reads source content"),
        ("ufcs_acquire", "directly reads source content"),
        ("field_acquire", "directly reads source content"),
        ("self_acquire", "directly reads source content"),
        ("aliased_acquire", "directly reads source content"),
        ("macro_acquire", "directly reads source content"),
        ("alternative_decode", "decodes TransitiveManifest"),
        ("inferred_decode", "decodes TransitiveManifest"),
        ("macro_inferred_decode", "decodes TransitiveManifest"),
        ("returned_decode", "decodes TransitiveManifest"),
        ("local_then_return_decode", "decodes TransitiveManifest"),
        (
            "aliased_local_then_return_decode",
            "decodes TransitiveManifest",
        ),
        (
            "module_aliased_local_then_return_decode",
            "decodes TransitiveManifest",
        ),
        ("inline_confine", "implements transitive remote confinement"),
        (
            "renamed_confine",
            "implements transitive remote confinement",
        ),
        ("split_confine", "implements transitive remote confinement"),
        (
            "hidden_by_expansion",
            "unsupported live macro could hide a duplicate implementation",
        ),
    ] {
        assert_ast_violation(&live_diagnostics, function, responsibility);
    }
}

#[test]
fn sync_duplicate_ast_probe_respects_lexical_decoder_alias_scopes() {
    let fixture = tempfile::TempDir::new().expect("AST alias-scope control tempdir");
    let within_scope = fixture.path().join("within_scope.rs");
    fs::write(
        &within_scope,
        r"
fn function_local_decoder_alias(
    payload: &str,
) -> Result<TransitiveManifest, Error> {
    use toml::from_str as decode_manifest;
    let value = decode_manifest(payload)?;
    Ok(value)
}

fn block_local_module_alias(
    payload: &str,
) -> Result<TransitiveManifest, Error> {
    {
        use toml as manifest_format;
        let value = manifest_format::from_str(payload)?;
        return Ok(value);
    }
}
",
    )
    .expect("write within-scope decoder alias control");
    let within_output = run_duplicate_ast_probe(std::slice::from_ref(&within_scope));
    let within_diagnostics = String::from_utf8_lossy(&within_output.stderr);
    assert!(
        !within_output.status.success(),
        "live block-local decoder aliases must be diagnosed"
    );
    for function in ["function_local_decoder_alias", "block_local_module_alias"] {
        assert_ast_violation(&within_diagnostics, function, "decodes TransitiveManifest");
    }

    let outer_decoder = fixture.path().join("outer_decoder.rs");
    fs::write(
        &outer_decoder,
        r"
use toml::from_str as decode_manifest;

fn outer_decoder_survives_inner_shadow(
    payload: &str,
) -> Result<TransitiveManifest, Error> {
    {
        use crate::helpers::decode_manifest;
    }
    let value = decode_manifest(payload)?;
    Ok(value)
}
",
    )
    .expect("write outer decoder alias control");
    let outer_output = run_duplicate_ast_probe(std::slice::from_ref(&outer_decoder));
    let outer_diagnostics = String::from_utf8_lossy(&outer_output.stderr);
    assert!(
        !outer_output.status.success(),
        "an unrelated inner shadow must not hide the outer decoder alias"
    );
    assert_ast_violation(
        &outer_diagnostics,
        "outer_decoder_survives_inner_shadow",
        "decodes TransitiveManifest",
    );

    let inner_decoder = fixture.path().join("inner_decoder.rs");
    fs::write(
        &inner_decoder,
        r"
fn decode_manifest(_payload: &str) -> Result<TransitiveManifest, Error> {
    Err(Error)
}

fn inner_decoder_does_not_taint_outer_call(
    payload: &str,
) -> Result<TransitiveManifest, Error> {
    {
        use toml::from_str as decode_manifest;
    }
    let value = decode_manifest(payload)?;
    Ok(value)
}
",
    )
    .expect("write inner decoder alias control");
    let inner_output = run_duplicate_ast_probe(std::slice::from_ref(&inner_decoder));
    assert!(
        inner_output.status.success(),
        "an inner decoder alias must not taint an unrelated outer call:\n{}{}",
        String::from_utf8_lossy(&inner_output.stdout),
        String::from_utf8_lossy(&inner_output.stderr)
    );
}

#[test]
fn sync_manifest_reader_gate_includes_cfg_test_helpers_without_name_coupling() {
    let fixture = tempfile::TempDir::new().expect("manifest-reader control tempdir");
    let separate = fixture.path().join("separate.rs");
    fs::write(
        &separate,
        r"
#[cfg(test)]
fn backend_passthrough(backend: &dyn SourceBackend) {
    backend.read_file_at();
}
#[cfg(test)]
fn unrelated_utf8(bytes: Vec<u8>) {
    let _ = String::from_utf8(bytes);
}
",
    )
    .expect("write separate-operation control");
    let separate_output = run_duplicate_ast_probe(std::slice::from_ref(&separate));
    assert!(
        separate_output.status.success(),
        "unrelated test helpers must not be conflated into a manifest reader:\n{}{}",
        String::from_utf8_lossy(&separate_output.stdout),
        String::from_utf8_lossy(&separate_output.stderr)
    );

    let duplicate = fixture.path().join("duplicate.rs");
    fs::write(
        &duplicate,
        r"
#[cfg(test)]
fn arbitrarily_renamed_source_reader(backend: &dyn SourceBackend) -> Result<String, Error> {
    let bytes = backend.read_file_at()?;
    String::from_utf8(bytes).map_err(Error::from)
}
",
    )
    .expect("write cfg-test manifest-reader mutation");
    let duplicate_output = run_duplicate_ast_probe(std::slice::from_ref(&duplicate));
    let diagnostics = String::from_utf8_lossy(&duplicate_output.stderr);
    assert!(
        !duplicate_output.status.success(),
        "a renamed cfg(test) duplicate manifest reader must fail"
    );
    assert!(
        diagnostics.contains(
            "arbitrarily_renamed_source_reader: source manifest acquisition and UTF-8 decoding must stay source-owned"
        ),
        "missing cfg(test) duplicate diagnostic: {diagnostics}"
    );
}

#[test]
fn callable_boundary_probe_accepts_normal_rustfmt_signatures() {
    let normal = r"
pub(crate) fn acquire_dependency_manifest(
    backend: &(dyn SourceBackend + Sync),
    source_name: &SourceName,
    parsed_source: &ParsedSource,
    remote: &str,
    pinned_commit: Option<&str>,
) -> Result<(String, TransitiveManifest)> {
    todo!()
}

pub(crate) fn validate_dependency_remote(
    name: &str,
    parsed_source: &ParsedSource,
    remote: &str,
    depth: usize,
) -> Result<()> {
    todo!()
}
";
    assert_eq!(
        function_parameter_names(normal, "acquire_dependency_manifest"),
        Some(vec![
            "backend".to_owned(),
            "source_name".to_owned(),
            "parsed_source".to_owned(),
            "remote".to_owned(),
            "pinned_commit".to_owned(),
        ]),
        "the callable probe must parse rustfmt's multiline trailing-comma form"
    );
    assert_eq!(
        function_parameter_names(normal, "validate_dependency_remote"),
        Some(vec![
            "name".to_owned(),
            "parsed_source".to_owned(),
            "remote".to_owned(),
            "depth".to_owned(),
        ])
    );

    let fixture = tempfile::TempDir::new().expect("boundary attribute control tempdir");
    let unrelated_skip = fixture.path().join("unrelated_skip.rs");
    fs::write(
        &unrelated_skip,
        format!("{normal}\n#[rustfmt::skip]\nfn unrelated_private_helper() {{}}\n"),
    )
    .expect("write unrelated-skip control");
    let unrelated_output = run_boundary_attribute_probe(&unrelated_skip);
    assert!(
        unrelated_output.status.success(),
        "a rustfmt skip on an unrelated helper must not fail the boundary-scoped gate:\n{}{}",
        String::from_utf8_lossy(&unrelated_output.stdout),
        String::from_utf8_lossy(&unrelated_output.stderr)
    );

    let skipped_boundary = fixture.path().join("skipped_boundary.rs");
    fs::write(
        &skipped_boundary,
        normal.replacen(
            "pub(crate) fn acquire_dependency_manifest",
            "#[rustfmt::skip]\npub(crate) fn acquire_dependency_manifest",
            1,
        ),
    )
    .expect("write skipped-boundary mutation");
    let skipped_output = run_boundary_attribute_probe(&skipped_boundary);
    assert!(
        !skipped_output.status.success(),
        "a rustfmt skip attached to the acquisition boundary must fail"
    );
    assert!(
        String::from_utf8_lossy(&skipped_output.stderr)
            .contains("acquire_dependency_manifest: boundary function must not use"),
        "missing acquisition-boundary skip diagnostic: {}",
        String::from_utf8_lossy(&skipped_output.stderr)
    );

    let production = read_src("source/transitive.rs");
    assert_eq!(
        function_parameter_names(&production, "acquire_dependency_manifest")
            .as_deref()
            .map(<[_]>::len),
        Some(5),
        "the production acquisition boundary must remain instrumentable after rustfmt"
    );
    assert_eq!(
        function_parameter_names(&production, "validate_dependency_remote")
            .as_deref()
            .map(<[_]>::len),
        Some(4),
        "the production validation boundary must remain instrumentable after rustfmt"
    );
    let production_output = run_boundary_attribute_probe(&src_dir().join("source/transitive.rs"));
    assert!(
        production_output.status.success(),
        "the two source::transitive callable boundaries must use normal rustfmt formatting; unrelated helpers are out of scope:\n{}{}",
        String::from_utf8_lossy(&production_output.stdout),
        String::from_utf8_lossy(&production_output.stderr)
    );
}

#[test]
fn remaining_kernel_definitions_have_exactly_their_final_owners() {
    let unsupported_macros = item_macro_sites();
    assert!(
        unsupported_macros.is_empty(),
        "the exact ownership scan fails closed when production uses top-level item-producing \
         macros it cannot expand safely; unsupported sites: {unsupported_macros:?}"
    );
    let expected = [
        ("Algo", "digest.rs"),
        ("Digest", "digest.rs"),
        ("ProjectId", "sync/state/mod.rs"),
        ("Commit", "source/model.rs"),
        ("SourceName", "source/model.rs"),
        ("TargetName", "projection/model.rs"),
        ("ArtifactName", "projection/model.rs"),
        ("KernelError", "source/model.rs"),
    ];
    for (name, owner) in expected {
        assert_eq!(
            type_definition_sites(name),
            [owner],
            "T029 must leave the live `{name}` definition exactly at src/{owner}; cfg/test, \
             macro, literal, nested, and longer-name decoys do not count"
        );
    }
}

#[test]
fn lexical_path_guards_have_one_live_source_owner() {
    for name in ["safe_component", "safe_relpath"] {
        assert_eq!(
            function_definition_sites(name),
            ["source/model.rs"],
            "T029 must leave `{name}` defined exactly once in src/source/model.rs"
        );
    }
}

#[test]
fn final_owner_paths_compile_and_preserve_identity_guard_behavior() {
    let output = compile_owner_probe(
        r#"
        use std::str::FromStr as _;
        use phora::digest::{Algo, Digest};
        use phora::projection::model::{ArtifactName, TargetName};
        use phora::source::{Commit, SourceName, SourcePath};
        use phora::sync::state::ProjectId;

        fn main() {
            assert_eq!(SourceName::from_str("dotfiles").unwrap().as_str(), "dotfiles");
            for bad in ["", ".", "..", "a/b", "a\\b", "a:b", "NUL"] {
                assert!(SourceName::from_str(bad).is_err(), "unsafe source name: {bad:?}");
            }
            assert_eq!(SourcePath::from_str("dir/file").unwrap().as_str(), "dir/file");
            for bad in ["", "/a", "a/../b", "a//b", "a\\b", "C:/x", "NUL"] {
                assert!(SourcePath::from_str(bad).is_err(), "unsafe source path: {bad:?}");
            }
            assert!(TargetName::from_str("target").is_ok());
            assert!(TargetName::from_str("../target").is_err());
            assert!(ArtifactName::from_str("artifact").is_ok());
            assert!(ArtifactName::from_str("a/b").is_err());
            assert_eq!(Commit::from_str(&"A".repeat(40)).unwrap().as_str(), "a".repeat(40));
            let digest = Digest::from_str(&format!("sha256:{}", "0".repeat(64))).unwrap();
            assert_eq!(digest.algo(), Algo::Sha256);
            let _: Option<ProjectId> = None;
        }
        "#,
    );
    assert!(
        output.status.success(),
        "the final owner paths must compile and their source-owned component/relpath guards \
         must preserve behavior:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn t029_files_are_exact_live_module_declarations() {
    let lib = scan_live_items(&read_src("lib.rs"));
    for module in [
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
    ] {
        assert!(
            lib.declares_public_file_module(module),
            "src/lib.rs must contain the live file-module item `pub mod {module};`"
        );
    }
    assert!(
        scan_live_items(&read_src("source/mod.rs")).declares_file_module("transitive"),
        "src/source/mod.rs must contain a live `mod transitive;` file-module item"
    );
}

#[test]
fn sync_has_no_duplicate_source_transitive_implementation() {
    let output = run_sync_duplicate_ast_probe();
    assert!(
        output.status.success(),
        "live sync production items must not retain an independent dependency-manifest read, \
         TransitiveManifest decode, or transitive remote-confinement implementation after the \
         callable source boundary exists; comments, literals, cfg/test items, and inert macros \
         are excluded by the Rust AST scan:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_transitive_carve_preserves_manifest_and_remote_behavior() {
    let rejected = transitive_fixture();
    let escaping = "version = 1\n\n[sources.dep]\npath = \"/etc\"\ntransitive = true\n\n\
                    [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n";
    write_fixture(&rejected.cwd.path().join("phora.toml"), escaping.as_bytes());
    let rejected_output = run_phora(&rejected, &["sync"]);
    let rejected_stderr = String::from_utf8_lossy(&rejected_output.stderr);
    assert!(
        !rejected_output.status.success(),
        "an escaping transitive remote must fail; stderr: {rejected_stderr}"
    );
    assert!(
        rejected_stderr.contains("transitive remote not allowed"),
        "the escaping-remote contract must retain its named diagnostic; stderr: \
         {rejected_stderr}"
    );
    assert!(
        !rejected.cwd.path().join("phora.lock").exists(),
        "rejecting an escaping transitive remote must not write a lock"
    );

    let dep = tempfile::TempDir::new().expect("dep repo");
    commit_transitive_manifest(
        dep.path(),
        "version = 1\n",
        "this is deliberately not a valid lock\npreimage = \"blake3:evil\"\n",
    );
    let isolated = transitive_fixture();
    let consumer = format!(
        "version = 1\n\n[sources.dep]\ngit = {remote:?}\ntransitive = true\n\n\
         [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n",
        remote = dep.path().to_string_lossy(),
    );
    write_fixture(&isolated.cwd.path().join("phora.toml"), consumer.as_bytes());
    let isolated_output = run_phora(&isolated, &["sync"]);
    let isolated_stderr = String::from_utf8_lossy(&isolated_output.stderr);
    assert!(
        isolated_output.status.success(),
        "transitive manifest loading must ignore a dep-shipped phora.lock; stderr: \
         {isolated_stderr}"
    );
    let consumer_lock = fs::read_to_string(isolated.cwd.path().join("phora.lock"))
        .expect("successful transitive sync writes the consumer lock");
    assert!(
        !consumer_lock.contains("blake3:evil"),
        "a dep-shipped lock must not contaminate the consumer lock: {consumer_lock}"
    );

    let unsupported_macros = item_macro_sites();
    assert!(
        unsupported_macros.is_empty(),
        "the capability ownership scan fails closed for unsupported live item macros: \
         {unsupported_macros:?}"
    );
    let boundary = run_source_boundary_probe();
    assert!(
        boundary.status.success(),
        "source::transitive must expose the callable T029 contracts \
         `acquire_dependency_manifest` and `validate_dependency_remote`; direct fixtures must \
         exercise their real behavior, and the live sync graph path must delegate to their \
         instrumented results with zero duplicate fetch, resolve, manifest-read, or \
         remote-confinement operations in sync:\n\
         {}{}",
        String::from_utf8_lossy(&boundary.stdout),
        String::from_utf8_lossy(&boundary.stderr)
    );
    assert!(
        scan_live_items(&read_src("source/mod.rs")).declares_file_module("transitive"),
        "the code that passed both CLI contracts must move behind source::transitive"
    );
    let transitive = scan_live_items(&read_src("source/transitive.rs"));
    assert!(
        transitive
            .items
            .iter()
            .any(|item| { matches!(item, Item::Type(_) | Item::Function(_) | Item::Impl) }),
        "src/source/transitive.rs must contain the real implementation, not an empty module stub"
    );
}

#[test]
fn kernel_compatibility_files_are_reexport_only_when_retained() {
    for relative in [
        "kernel/commit.rs",
        "kernel/digest.rs",
        "kernel/name.rs",
        "kernel/project_id.rs",
    ] {
        if !src_dir().join(relative).exists() {
            continue;
        }
        let scanned = scan_live_items(&read_src(relative));
        let unexpected: Vec<&Item> = scanned
            .items
            .iter()
            .filter(|item| !matches!(item, Item::Use))
            .collect();
        assert!(
            unexpected.is_empty(),
            "src/{relative} may remain until T030 only as a re-export-only compatibility shim; \
             live implementations/definitions found: {unexpected:?}"
        );
    }
}
