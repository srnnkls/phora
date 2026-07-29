use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Ident(String),
    Literal(String),
    Punct(char),
}

fn raw_literal_bounds(bytes: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    let prefix_len = [b"br".as_slice(), b"rb", b"cr", b"rc", b"r"]
        .into_iter()
        .find_map(|prefix| bytes[start..].starts_with(prefix).then_some(prefix.len()))?;
    let mut quote = start + prefix_len;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }

    let hashes = quote - start - prefix_len;
    let content_start = quote + 1;
    let mut content_end = content_start;
    while content_end < bytes.len()
        && !(bytes[content_end] == b'"'
            && (0..hashes).all(|n| bytes.get(content_end + 1 + n) == Some(&b'#')))
    {
        content_end += 1;
    }
    let literal_end = (content_end + 1 + hashes).min(bytes.len());
    Some((content_start, content_end, literal_end))
}

fn quoted_literal_bounds(bytes: &[u8], quote: usize, delimiter: u8) -> (usize, usize, usize) {
    let content_start = quote + 1;
    let mut content_end = content_start;
    while content_end < bytes.len() && bytes[content_end] != delimiter {
        if bytes[content_end] == b'\\' {
            content_end = (content_end + 2).min(bytes.len());
        } else {
            content_end += 1;
        }
    }
    let literal_end = (content_end + 1).min(bytes.len());
    (content_start, content_end, literal_end)
}

fn utf8_scalar_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn character_literal_bounds(bytes: &[u8], quote: usize) -> Option<(usize, usize, usize)> {
    let content_start = quote + 1;
    let first = *bytes.get(content_start)?;
    let closing_quote = if first == b'\\' {
        let (_, content_end, _) = quoted_literal_bounds(bytes, quote, b'\'');
        content_end
    } else {
        content_start + utf8_scalar_width(first)
    };
    (bytes.get(closing_quote) == Some(&b'\'')).then_some((
        content_start,
        closing_quote,
        closing_quote + 1,
    ))
}

fn push_literal(tokens: &mut Vec<Token>, bytes: &[u8], content_start: usize, content_end: usize) {
    tokens.push(Token::Literal(
        String::from_utf8_lossy(&bytes[content_start..content_end]).into_owned(),
    ));
}

fn lex(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if let Some((content_start, content_end, literal_end)) = raw_literal_bounds(bytes, i)
        {
            push_literal(&mut tokens, bytes, content_start, content_end);
            i = literal_end;
        } else if matches!(bytes.get(i), Some(b'b' | b'c')) && bytes.get(i + 1) == Some(&b'"') {
            let (content_start, content_end, literal_end) =
                quoted_literal_bounds(bytes, i + 1, b'"');
            push_literal(&mut tokens, bytes, content_start, content_end);
            i = literal_end;
        } else if bytes.get(i) == Some(&b'b')
            && bytes.get(i + 1) == Some(&b'\'')
            && let Some((content_start, content_end, literal_end)) =
                character_literal_bounds(bytes, i + 1)
        {
            push_literal(&mut tokens, bytes, content_start, content_end);
            i = literal_end;
        } else if bytes[i] == b'"' {
            let (content_start, content_end, literal_end) = quoted_literal_bounds(bytes, i, b'"');
            push_literal(&mut tokens, bytes, content_start, content_end);
            i = literal_end;
        } else if bytes[i] == b'\''
            && let Some((content_start, content_end, literal_end)) =
                character_literal_bounds(bytes, i)
        {
            push_literal(&mut tokens, bytes, content_start, content_end);
            i = literal_end;
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(Token::Ident(source[start..i].to_owned()));
        } else {
            tokens.push(Token::Punct(char::from(bytes[i])));
            i += 1;
        }
    }
    tokens
}

fn is_ident(token: Option<&Token>, expected: &str) -> bool {
    matches!(token, Some(Token::Ident(actual)) if actual == expected)
}

fn is_punct(token: Option<&Token>, expected: char) -> bool {
    matches!(token, Some(Token::Punct(actual)) if *actual == expected)
}

fn matching_brace(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        if token == &Token::Punct('{') {
            depth += 1;
        } else if token == &Token::Punct('}') {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn top_level_fn_bodies(tokens: &[Token], name: &str) -> Option<Vec<Vec<Token>>> {
    let mut bodies = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        match tokens.get(i) {
            Some(Token::Punct('{')) => depth += 1,
            Some(Token::Punct('}')) => depth = depth.checked_sub(1)?,
            Some(Token::Ident(word))
                if depth == 0 && word == "fn" && is_ident(tokens.get(i + 1), name) =>
            {
                let open = (i + 2..tokens.len()).find(|&j| is_punct(tokens.get(j), '{'))?;
                let close = matching_brace(tokens, open)?;
                bodies.push(tokens[open + 1..close].to_vec());
                i = close;
            }
            _ => {}
        }
        i += 1;
    }
    (depth == 0).then_some(bodies)
}

fn unique_top_level_fn_body(tokens: &[Token], name: &str) -> Option<Vec<Token>> {
    let mut bodies = top_level_fn_bodies(tokens, name)?;
    (bodies.len() == 1).then(|| bodies.pop().expect("one function body"))
}

fn inherent_method_body(source: &str, subject: &str, method: &str) -> Option<Vec<Token>> {
    let tokens = lex(source);
    let mut bodies = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        match tokens.get(i) {
            Some(Token::Punct('{')) => depth += 1,
            Some(Token::Punct('}')) => depth = depth.checked_sub(1)?,
            Some(Token::Ident(word)) if depth == 0 && word == "impl" => {
                let open = (i + 1..tokens.len()).find(|&j| is_punct(tokens.get(j), '{'))?;
                let close = matching_brace(&tokens, open)?;
                if tokens[i + 1..open] == [Token::Ident(subject.to_owned())] {
                    bodies.extend(top_level_fn_bodies(&tokens[open + 1..close], method)?);
                }
                i = close;
            }
            _ => {}
        }
        i += 1;
    }
    (depth == 0 && bodies.len() == 1).then(|| bodies.pop().expect("one inherent method body"))
}

fn lock_advisory_composes_detection(source: &str) -> bool {
    inherent_method_body(source, "FileStateStore", "lock_advisory").is_some_and(|body| {
        body == lex("statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory)")
    })
}

fn cli_sync_emits_positive_advisory(source: &str) -> bool {
    let Some(body) = unique_top_level_fn_body(&lex(source), "run_sync") else {
        return false;
    };
    let mut depth = 0usize;
    let mut i = 0;
    while i < body.len() {
        match body.get(i) {
            Some(Token::Punct('{')) => depth += 1,
            Some(Token::Punct('}')) => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
            }
            Some(Token::Ident(word)) if depth == 0 && word == "if" => {
                let Some(open) = (i + 1..body.len()).find(|&j| is_punct(body.get(j), '{')) else {
                    return false;
                };
                let Some(close) = matching_brace(&body, open) else {
                    return false;
                };
                let condition = &body[i + 1..open];
                if condition.len() == 15
                    && is_punct(condition.first(), '!')
                    && is_ident(condition.get(1), "lockless")
                    && is_punct(condition.get(2), '&')
                    && is_punct(condition.get(3), '&')
                    && is_ident(condition.get(4), "let")
                    && is_ident(condition.get(5), "Some")
                    && is_punct(condition.get(6), '(')
                    && matches!(condition.get(7), Some(Token::Ident(_)))
                    && is_punct(condition.get(8), ')')
                    && is_punct(condition.get(9), '=')
                    && is_ident(condition.get(10), "registry")
                    && is_punct(condition.get(11), '.')
                    && is_ident(condition.get(12), "lock_advisory")
                    && is_punct(condition.get(13), '(')
                    && is_punct(condition.get(14), ')')
                {
                    let Token::Ident(binding) = &condition[7] else {
                        unreachable!();
                    };
                    let expected = lex(&format!("eprintln!(\"{{{binding}}}\");"));
                    if body[open + 1..close] == expected {
                        return true;
                    }
                }
                i = close;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn source(rel: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(rel),
    )
    .unwrap_or_else(|error| panic!("read src/{rel}: {error}"))
}

#[test]
fn file_registry_lock_advisory_composes_state_root_detection_with_formatter() {
    assert!(
        lock_advisory_composes_detection(&source("sync/state/locking.rs")),
        "FileStateStore::lock_advisory must return statfs_fstype(&self.state_root), dereferenced, \
         and composed directly into network_lock_advisory; an always-None implementation must \
         fail this contract"
    );
}

#[test]
fn cli_sync_prints_a_positive_registry_advisory_to_stderr_unless_lockless() {
    assert!(
        cli_sync_emits_positive_advisory(&source("cli/sync.rs")),
        "run_sync must guard registry.lock_advisory() with !lockless and pass the bound Some \
         value to eprintln!; omission, another receiver, or a decoy print must fail"
    );
}

#[test]
fn lexer_consumes_rust_literal_forms_without_exposing_payloads_or_lifetimes() {
    for literal in [
        r####"r###"bare \" } fn run_sync() {}"###"####,
        r####"br###"bare \" } impl FileStateStore {}"###"####,
        r####"rb###"bare \" } impl FileStateStore {}"###"####,
        r####"cr###"bare \" } fn run_sync() {}"###"####,
        r####"rc###"bare \" } fn run_sync() {}"###"####,
        r#""escaped \" quote and } brace""#,
        r#"b"escaped \" quote and } brace""#,
        r#"c"escaped \" quote and } brace""#,
        r"'}'",
        r"b'}'",
        r#"'\"'"#,
        r#"b'\"'"#,
        r"'\''",
        r"b'\''",
    ] {
        assert!(
            matches!(lex(literal).as_slice(), [Token::Literal(_)]),
            "lexer exposed tokens from literal: {literal}"
        );
    }

    assert_eq!(
        lex("'a 'input '_"),
        [
            Token::Punct('\''),
            Token::Ident("a".to_owned()),
            Token::Punct('\''),
            Token::Ident("input".to_owned()),
            Token::Punct('\''),
            Token::Ident("_".to_owned()),
        ],
        "lifetimes must remain syntax rather than being consumed as character literals"
    );
}

#[test]
fn scanners_reject_always_none_omission_and_lexical_decoys() {
    let lock_positive = "impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { \
        statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory) } }";
    assert!(lock_advisory_composes_detection(lock_positive));
    for mutant in [
        "impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { None } }",
        "impl Other { pub fn lock_advisory(&self) { \
            statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory); } }",
        "impl FileStateStore { pub fn outer(&self) { fn lock_advisory() { \
            statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory); } } }",
        r#"impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { None } }
            // impl FileStateStore { pub fn lock_advisory(&self) { statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory) } }
            const DECOY: &str = "statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory)";"#,
        r####"fn wrapper() {
            let decoy = br###"bare " } impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory) } }"###;
        }
        impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { None } }"####,
        r"#[cfg(any())]
        impl FileStateStore {
            pub fn lock_advisory(&self) -> Option<String> {
                statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory)
            }
        }
        impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { None } }",
        r"fn wrapper() {
            let promoted = '}';
            impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { statfs_fstype(&self.state_root).as_deref().and_then(network_lock_advisory) } }
        }
        impl FileStateStore { pub fn lock_advisory(&self) -> Option<String> { None } }",
    ] {
        assert!(
            !lock_advisory_composes_detection(mutant),
            "lock composition scanner accepted mutant: {mutant}"
        );
    }

    let cli_positive = r#"fn run_sync() {
        if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!("{warning}"); }
    }"#;
    assert!(cli_sync_emits_positive_advisory(cli_positive));
    for mutant in [
        r"fn run_sync() { if !lockless && let Some(warning) = registry.lock_advisory() {} }",
        r#"fn run_sync() { if !lockless && let Some(warning) = other.lock_advisory() { eprintln!("{warning}"); } }"#,
        r#"fn run_sync() { if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!("constant"); } }"#,
        r#"fn run_sync() { { if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!("{warning}"); } } }"#,
        r#"fn run_sync() {
            // if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!("{warning}"); }
            let decoy = "if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!(\"{warning}\"); }";
        }"#,
        r####"fn wrapper() {
            let decoy = cr###"bare " } fn run_sync() { if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!("{warning}"); } }"###;
        }
        fn run_sync() {}"####,
        r#"#[cfg(any())]
        fn run_sync() {
            if !lockless && let Some(warning) = registry.lock_advisory() {
                eprintln!("{warning}");
            }
        }
        fn run_sync() {}"#,
        r#"fn wrapper() {
            let promoted = b'}';
            fn run_sync() { if !lockless && let Some(warning) = registry.lock_advisory() { eprintln!("{warning}"); } }
        }
        fn run_sync() {}"#,
    ] {
        assert!(
            !cli_sync_emits_positive_advisory(mutant),
            "CLI scanner accepted mutant: {mutant}"
        );
    }
}
