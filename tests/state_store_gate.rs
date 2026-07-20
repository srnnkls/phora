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

fn collapse_colon_ws(s: &str) -> String {
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
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn keyword_names(stripped: &str, keyword: &str, name: &str) -> bool {
    let bytes = stripped.as_bytes();
    stripped.match_indices(keyword).any(|(i, _)| {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let rest_all = &stripped[i + keyword.len()..];
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

fn defines_pub_type(stripped: &str, name: &str) -> bool {
    keyword_names(stripped, "pub struct", name) || keyword_names(stripped, "pub enum", name)
}

fn item_body(stripped: &str, keywords: &[&str], name: &str) -> Option<String> {
    keywords
        .iter()
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

fn enum_body(stripped: &str, name: &str) -> Option<String> {
    item_body(stripped, &["pub enum", "enum"], name)
}

fn trait_body(stripped: &str, name: &str) -> Option<String> {
    item_body(stripped, &["pub trait", "trait"], name)
}

fn trait_fn_names(body: &str) -> Vec<String> {
    let inner = &body[1..body.len().saturating_sub(1)];
    let bytes = inner.as_bytes();
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            b'f' if depth == 0 => {
                let before_ok =
                    i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
                let after_ok = inner[i..].starts_with("fn")
                    && bytes.get(i + 2).is_some_and(|&b| b.is_ascii_whitespace());
                if before_ok && after_ok {
                    let name: String = inner[i + 2..]
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        names.push(name);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    names
}

fn variant_ident(part: &str) -> Option<String> {
    let mut t = part.trim_start();
    while t.starts_with('#') {
        let open = t.find('[')?;
        let bytes = t.as_bytes();
        let mut depth = 0i32;
        let mut close = None;
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        t = t[close? + 1..].trim_start();
    }
    let name: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn enum_variant_names(body: &str) -> Vec<String> {
    let inner = &body[1..body.len().saturating_sub(1)];
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '{' | '(' | '[' => {
                depth += 1;
                buf.push(c);
            }
            '}' | ')' | ']' => {
                depth -= 1;
                buf.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut buf)),
            _ => buf.push(c),
        }
    }
    parts.push(buf);
    parts
        .iter()
        .map(String::as_str)
        .filter_map(variant_ident)
        .collect()
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
    for (i, _) in scanned.match_indices("use") {
        let before_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
        let after_ws = bytes.get(i + 3).is_some_and(|&b| b.is_ascii_whitespace());
        if !(before_ok && after_ws) {
            continue;
        }
        let Some(end) = scanned[i + 3..].find(';') else {
            continue;
        };
        leaves.extend(expand_tree(scanned[i + 3..i + 3 + end].trim()));
    }
    leaves
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn hollow_markers(body: &str) -> Vec<&'static str> {
    let bytes = body.as_bytes();
    ["unimplemented", "todo", "panic"]
        .into_iter()
        .filter(|name| {
            body.match_indices(name).any(|(i, _)| {
                let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
                let rest = body[i + name.len()..].trim_start();
                before_ok
                    && rest.strip_prefix('!').is_some_and(|after_bang| {
                        matches!(
                            after_bang.trim_start().chars().next(),
                            Some('(' | '{' | '[')
                        )
                    })
            })
        })
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
            let body = balanced_body(rest)?;
            Some((rest[..open].to_owned(), body))
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

fn trait_impl_body(stripped: &str, trait_name: &str, subject: &str) -> Option<String> {
    impl_spans(stripped)
        .into_iter()
        .find(|(header, _)| {
            references_token(header, trait_name) && impl_subject(header).as_deref() == Some(subject)
        })
        .map(|(_, body)| body)
}

fn impls_trait_for(stripped: &str, trait_name: &str, subject: &str) -> bool {
    trait_impl_body(stripped, trait_name, subject).is_some()
}

fn trait_aliases(stripped: &str, trait_name: &str) -> Vec<String> {
    let normalized = collapse_colon_ws(stripped);
    let mut aliases: Vec<String> = use_leaves(&normalized)
        .into_iter()
        .filter_map(|leaf| {
            let words: Vec<&str> = leaf.split_whitespace().collect();
            let [path, "as", alias] = words.as_slice() else {
                return None;
            };
            let imported = path.rsplit("::").next()?;
            (imported == trait_name && *alias != "_").then(|| (*alias).to_owned())
        })
        .collect();
    aliases.sort();
    aliases.dedup();
    aliases
}

fn trait_impl_subjects(stripped: &str, trait_name: &str) -> Vec<String> {
    let mut trait_names = vec![trait_name.to_owned()];
    trait_names.extend(trait_aliases(stripped, trait_name));
    let mut subjects: Vec<String> = impl_spans(stripped)
        .into_iter()
        .filter_map(|(header, _)| {
            let rest = skip_generics(header.trim_start().strip_prefix("impl")?.trim_start());
            let subject = top_level_for_tail(rest)?;
            let trait_side = &rest[..rest.len() - subject.len() - "for".len()];
            trait_names
                .iter()
                .any(|name| references_token(trait_side, name))
                .then(|| impl_subject(&header))?
        })
        .collect();
    subjects.sort();
    subjects
}

fn production_trait_impl_sites(trait_name: &str) -> Vec<String> {
    let mut sites: Vec<String> = prod_src_files()
        .into_iter()
        .flat_map(|(rel, source)| {
            trait_impl_subjects(&scan(&source), trait_name)
                .into_iter()
                .map(move |subject| format!("{rel}:{subject}"))
        })
        .collect();
    sites.sort();
    sites
}

fn production_trait_alias_sites(trait_name: &str) -> Vec<String> {
    let mut sites: Vec<String> = prod_src_files()
        .into_iter()
        .flat_map(|(rel, source)| {
            trait_aliases(&scan(&source), trait_name)
                .into_iter()
                .map(move |alias| format!("{rel}:{alias}"))
        })
        .collect();
    sites.sort();
    sites
}

fn top_level_fn_items(body: &str) -> Vec<(String, bool)> {
    let inner = body
        .strip_prefix('{')
        .and_then(|body| body.strip_suffix('}'))
        .unwrap_or(body);
    let bytes = inner.as_bytes();
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            b'f' if depth == 0 => {
                let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
                let after_ok = inner[i..].starts_with("fn")
                    && bytes.get(i + 2).is_some_and(|&b| b.is_ascii_whitespace());
                if !(before_ok && after_ok) {
                    i += 1;
                    continue;
                }
                let name: String = inner[i + 2..]
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let prefix = inner[..i].trim_end();
                let is_public = prefix.strip_suffix("pub").is_some_and(|before| {
                    before.chars().next_back().is_none_or(char::is_whitespace)
                });
                if !name.is_empty() {
                    items.push((name, is_public));
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    items
}

fn locking_seam_sites(rel: &str, source: &str) -> Vec<(String, bool, bool)> {
    const SEAMS: &[&str] = &["lock_exclusive", "lock_advisory"];
    let scanned = scan(source);
    let mut sites: Vec<(String, bool, bool)> = top_level_fn_items(&format!("{{{scanned}}}"))
        .into_iter()
        .filter(|(name, _)| SEAMS.contains(&name.as_str()))
        .map(|(name, is_public)| (format!("{rel}:<free>::{name}"), is_public, false))
        .collect();
    for (header, body) in impl_spans(&scanned) {
        let subject = impl_subject(&header).unwrap_or_else(|| "<unknown>".to_owned());
        let rest = skip_generics(
            header
                .trim_start()
                .strip_prefix("impl")
                .unwrap_or_default()
                .trim_start(),
        );
        let is_inherent = top_level_for_tail(rest).is_none();
        sites.extend(
            top_level_fn_items(&body)
                .into_iter()
                .filter(|(name, _)| SEAMS.contains(&name.as_str()))
                .map(|(name, is_public)| {
                    (format!("{rel}:{subject}::{name}"), is_public, is_inherent)
                }),
        );
    }
    sites.sort();
    sites
}

fn production_locking_seam_sites() -> Vec<(String, bool, bool)> {
    let mut sites: Vec<(String, bool, bool)> = prod_src_files()
        .into_iter()
        .flat_map(|(rel, source)| locking_seam_sites(&rel, &source))
        .collect();
    sites.sort();
    sites
}

fn strip_attributes(stripped: &str) -> String {
    let chars: Vec<char> = stripped.chars().collect();
    let mut out = String::with_capacity(stripped.len());
    let mut i = 0;
    while i < chars.len() {
        let bracket = match (chars.get(i), chars.get(i + 1), chars.get(i + 2)) {
            (Some('#'), Some('['), _) => Some(i + 1),
            (Some('#'), Some('!'), Some('[')) => Some(i + 2),
            _ => None,
        };
        let Some(bracket) = bracket else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        while i <= bracket {
            out.push(blank(chars[i]));
            i += 1;
        }
        let mut depth = 1i32;
        while i < chars.len() && depth > 0 {
            match chars[i] {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            out.push(blank(chars[i]));
            i += 1;
        }
    }
    out
}

fn keyword_at(source: &str, start: usize, keyword: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    source[start..]
        .starts_with(keyword)
        .then_some(start + keyword.len())
        .filter(|&end| bytes.get(end).is_none_or(|&b| !is_ident_byte(b)))
}

fn facade_non_reexport_constructs(source: &str) -> Vec<String> {
    let stripped = strip_attributes(&scan(source));
    let bytes = stripped.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if i == bytes.len() {
            return Vec::new();
        }

        let Some(after_pub) = keyword_at(&stripped, i, "pub") else {
            break;
        };
        if !bytes.get(after_pub).is_some_and(u8::is_ascii_whitespace) {
            break;
        }
        let mut after_pub_ws = after_pub;
        while bytes.get(after_pub_ws).is_some_and(u8::is_ascii_whitespace) {
            after_pub_ws += 1;
        }
        let Some(after_use) = keyword_at(&stripped, after_pub_ws, "use") else {
            break;
        };
        if !bytes.get(after_use).is_some_and(u8::is_ascii_whitespace) {
            break;
        }

        let mut brace_depth = 0i32;
        let mut bracket_depth = 0i32;
        let mut paren_depth = 0i32;
        let mut end = None;
        let mut j = after_use;
        while j < bytes.len() {
            match bytes[j] {
                b'{' => brace_depth += 1,
                b'}' => brace_depth -= 1,
                b'[' => bracket_depth += 1,
                b']' => bracket_depth -= 1,
                b'(' => paren_depth += 1,
                b')' => paren_depth -= 1,
                b';' if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 => {
                    end = Some(j + 1);
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        let Some(next) = end else {
            break;
        };
        i = next;
    }

    let unexpected = stripped[i..].trim();
    (!unexpected.is_empty())
        .then(|| unexpected.chars().take(120).collect())
        .into_iter()
        .collect()
}

fn definition_sites(name: &str) -> Vec<String> {
    let mut sites: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(_, source)| {
            let scanned = scan(source);
            ["trait", "struct", "enum", "type"]
                .into_iter()
                .any(|keyword| keyword_names(&scanned, keyword, name))
        })
        .map(|(rel, _)| rel)
        .collect();
    sites.sort();
    sites
}

const TRAIT: &str = "StateStore";
const STATE_MODULE: &str = "sync/state/mod.rs";
const STATE_FILE: &str = "sync/state/file.rs";
const STATE_LOCKING: &str = "sync/state/locking.rs";
const MODEL_MODULE: &str = "sync/model.rs";
const STORE: &str = "store.rs";

const MODEL_TYPES: &[&str] = &[
    "ObservedProjectState",
    "ObservedArtifact",
    "ManagedCondition",
];

const MANAGED_CONDITION_VARIANTS: &[&str] = &[
    "Clean",
    "MetadataChangedButContentClean",
    "Outdated",
    "Modified",
    "Linked",
];

const SKETCH_METHODS: &[&str] = &[
    "artifact",
    "put_artifact",
    "remove_artifact",
    "target_artifacts",
    "all_artifacts",
    "ejections",
    "save_ejections",
    "hook_state",
    "record_hook_success",
    "acquire_lock",
    "journal_root",
];

const MODEL_FORBIDDEN_SUBSTR: &[&str] = &[
    "crate::store",
    "crate::deploy",
    "crate::source",
    "store::",
    "deploy::",
    "std::fs",
    "std::io",
    "std::os",
    "std::net",
    "std::process",
    "::fs::",
    "OpenOptions",
    "File::create",
    "File::open",
];

const MODEL_FORBIDDEN_TOKENS: &[&str] =
    &["gix", "chrono", "ureq", "walkdir", "tar", "zip", "flate2"];

const IO_SYNC_SIBLINGS: &[&str] = &[
    "state",
    "discover",
    "resolve",
    "preview",
    "hooks",
    "plan",
    "rebuild",
    "stage",
    "verify",
    "transitive",
    "confine",
    "target",
    "prune",
];

fn slot_mismatch(trait_src: &str) -> (Vec<&'static str>, Vec<String>) {
    let Some(body) = trait_body(&scan(trait_src), TRAIT) else {
        return (SKETCH_METHODS.to_vec(), Vec::new());
    };
    let declared = trait_fn_names(&body);
    let missing = SKETCH_METHODS
        .iter()
        .copied()
        .filter(|name| !declared.iter().any(|d| d == name))
        .collect();
    let extra = declared
        .into_iter()
        .filter(|d| !SKETCH_METHODS.contains(&d.as_str()))
        .collect();
    (missing, extra)
}

fn contains_path_needle(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let starts_ident = needle
        .as_bytes()
        .first()
        .copied()
        .is_some_and(is_ident_byte);
    let ends_ident = needle.as_bytes().last().copied().is_some_and(is_ident_byte);
    haystack.match_indices(needle).any(|(i, _)| {
        let before_ok = !starts_ident || i == 0 || !is_ident_byte(bytes[i - 1]);
        let after_ok = !ends_ident
            || bytes
                .get(i + needle.len())
                .copied()
                .is_none_or(|b| !is_ident_byte(b));
        before_ok && after_ok
    })
}

fn forbidden_crate_head(haystack: &str, leaves: &[String], token: &str) -> bool {
    let bytes = haystack.as_bytes();
    let path_head = haystack.match_indices(token).any(|(i, _)| {
        let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        let not_segment = i < 2 || &haystack[i - 2..i] != "::";
        before_ok && not_segment && haystack[i + token.len()..].starts_with("::")
    });
    path_head
        || leaves.iter().any(|leaf| {
            let head = leaf.split_whitespace().next().unwrap_or_default();
            head == token
                || head
                    .strip_prefix(token)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
}

fn model_impurities(model_src: &str) -> Vec<String> {
    let scanned = collapse_colon_ws(&scan(model_src));
    let leaves = use_leaves(&scanned);
    let haystack = format!("{scanned}\n{}", leaves.join("\n"));
    let mut hits: Vec<String> = MODEL_FORBIDDEN_SUBSTR
        .iter()
        .filter(|needle| contains_path_needle(&haystack, needle))
        .map(|needle| (*needle).to_owned())
        .collect();
    for sibling in IO_SYNC_SIBLINGS {
        for prefix in ["sync::", "super::"] {
            let needle = format!("{prefix}{sibling}");
            if contains_path_needle(&haystack, &needle) {
                hits.push(needle);
            }
        }
    }
    hits.extend(
        MODEL_FORBIDDEN_TOKENS
            .iter()
            .filter(|token| forbidden_crate_head(&haystack, &leaves, token))
            .map(|token| (*token).to_owned()),
    );
    hits
}

fn definers_of_trait() -> Vec<String> {
    prod_src_files()
        .into_iter()
        .filter(|(_, content)| keyword_names(&scan(content), "pub trait", TRAIT))
        .map(|(rel, _)| rel)
        .collect()
}

#[test]
fn state_and_model_modules_are_registered_in_sync() {
    let mod_rs = scan(&read_src("sync/mod.rs"));
    assert!(
        keyword_names(&mod_rs, "mod", "state"),
        "src/sync/mod.rs must declare `mod state` (T017 registers the new trait home); an \
         unregistered file never compiles and pins nothing"
    );
    assert!(
        keyword_names(&mod_rs, "mod", "model"),
        "src/sync/mod.rs must declare `mod model` — the PURE reconcile-facing value types live \
         in src/{MODEL_MODULE}, separate from the I/O-bearing sync::state so reconcile can import \
         them without tripping the INV-8 lint"
    );
}

#[test]
fn state_module_registers_file_and_locking_implementation_modules() {
    let state = scan(&read_src(STATE_MODULE));
    for module in ["file", "locking"] {
        assert!(
            keyword_names(&state, "mod", module),
            "src/{STATE_MODULE} must register its `{module}` implementation module; merely \
             creating src/sync/state/{module}.rs without a module declaration compiles no \
             production code"
        );
    }
}

#[test]
fn state_store_trait_is_defined_only_in_the_state_module() {
    assert!(
        keyword_names(&scan(&read_src(STATE_MODULE)), "pub trait", TRAIT),
        "src/{STATE_MODULE} must define `pub trait {TRAIT}` — T017's full StateStore port \
         (design §8) lives here (namespace sync::state::{TRAIT}), the home T025 later relocates \
         the FileRegistry impl beside, unchanged"
    );
    let definers = definers_of_trait();
    assert_eq!(
        definers,
        [STATE_MODULE],
        "exactly src/{STATE_MODULE} must define `pub trait {TRAIT}`; a second definer anywhere \
         would fork the port the reconcile suite and the T025 relocation bind against; found \
         at: {definers:?}"
    );
}

#[test]
fn state_store_trait_declares_exactly_the_design_eight_methods() {
    let (missing, extra) = slot_mismatch(&read_src(STATE_MODULE));
    assert!(
        missing.is_empty(),
        "src/{STATE_MODULE}'s `pub trait {TRAIT}` must declare every design §8 method under its \
         sketch name — artifact, put_artifact, remove_artifact, target_artifacts, all_artifacts, \
         ejections, save_ejections, hook_state, record_hook_success, acquire_lock, journal_root \
         (the trait is NEW; the legacy Registry keeps its own names on its own trait); missing: \
         {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "src/{STATE_MODULE}'s `pub trait {TRAIT}` must declare NOTHING beyond the 11 design §8 \
         methods — T025 relocates the impl unchanged, no widening (a copied-in \
         refuses_writes/readonly_error is exactly the evasion this rejects); extra fns: {extra:?}"
    );
}

#[test]
fn file_registry_implements_state_store_only_in_state_file() {
    let implementation_sites: Vec<String> = prod_src_files()
        .into_iter()
        .filter(|(_, source)| impls_trait_for(&scan(source), TRAIT, "FileRegistry"))
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        impls_trait_for(&scan(&read_src(STATE_FILE)), TRAIT, "FileRegistry"),
        "src/{STATE_FILE} must carry the real `impl {TRAIT} for FileRegistry` after T025 moves \
         file-backed state ownership out of the compatibility facade"
    );
    assert_eq!(
        implementation_sites,
        [STATE_FILE],
        "exactly src/{STATE_FILE} must implement {TRAIT} for FileRegistry; a retained impl in \
         src/{STORE} or a second adapter elsewhere forks the state boundary; found: \
         {implementation_sites:?}"
    );
}

#[test]
fn file_registry_state_store_impl_is_not_hollow() {
    let body =
        trait_impl_body(&scan(&read_src(STATE_FILE)), TRAIT, "FileRegistry").unwrap_or_else(|| {
            panic!(
                "src/{STATE_FILE} does not carry `impl {TRAIT} for FileRegistry` yet — the \
                 hollow-impl pin needs the relocated production impl body to scan"
            )
        });
    let markers = hollow_markers(&body);
    assert!(
        markers.is_empty(),
        "the production `impl {TRAIT} for FileRegistry` in src/{STATE_FILE} must be REAL — no \
         unimplemented!/todo!/panic! stubs; panic stubs are legal only in the in-memory fake on \
         methods the reconcile suite never exercises; found: {markers:?}"
    );
}

#[test]
fn file_module_owns_the_relocated_state_implementation() {
    let file = scan(&read_src(STATE_FILE));
    assert!(
        defines_pub_type(&file, "FileRegistry")
            && defines_pub_type(&file, "RegistryRecord")
            && keyword_names(&file, "pub trait", "Registry"),
        "src/{STATE_FILE} must own FileRegistry, RegistryRecord, and the compatibility Registry \
         port after the whole-file-first move from src/{STORE}"
    );
}

#[test]
fn locking_module_owns_file_registry_locking_seams() {
    let expected_locking_sites = [
        (
            format!("{STATE_LOCKING}:FileRegistry::lock_advisory"),
            true,
            true,
        ),
        (
            format!("{STATE_LOCKING}:FileRegistry::lock_exclusive"),
            true,
            true,
        ),
    ];
    let locking_sites = production_locking_seam_sites();
    assert_eq!(
        locking_sites, expected_locking_sites,
        "the public lock_exclusive and lock_advisory seams must be inherent methods on \
         FileRegistry in src/{STATE_LOCKING}, with no old-owner, free-function, or decoy-type \
         definitions anywhere in production; found: {locking_sites:?}"
    );
}

#[test]
fn registry_impl_subjects_exclude_renamed_state_store_adapters() {
    let alias_sites = production_trait_alias_sites("Registry");
    assert!(
        alias_sites.is_empty(),
        "production code must not rename Registry before implementing it; a trait alias lets a \
         renamed StateStore-to-Registry bridge evade a literal trait-subject gate; found: \
         {alias_sites:?}"
    );
    let registry_sites = production_trait_impl_sites("Registry");
    assert_eq!(
        registry_sites,
        [
            format!("{STATE_FILE}:FileRegistry"),
            format!("{STATE_FILE}:FrozenReadOnlyRegistry"),
        ],
        "Registry may be implemented only by the real file owner and its frozen wrapper after \
         T025; any renamed StateStore-to-Registry bridge is an unexpected impl subject; found: \
         {registry_sites:?}"
    );
}

#[test]
fn state_store_impl_subjects_exclude_renamed_registry_adapters() {
    let alias_sites = production_trait_alias_sites(TRAIT);
    assert!(
        alias_sites.is_empty(),
        "production code must not rename {TRAIT} before implementing it; a trait alias lets a \
         renamed Registry-to-StateStore bridge evade a literal trait-subject gate; found: \
         {alias_sites:?}"
    );
    let state_store_sites = production_trait_impl_sites(TRAIT);
    assert_eq!(
        state_store_sites,
        [format!("{STATE_FILE}:FileRegistry")],
        "StateStore may be implemented only by the real file owner after T025; any renamed \
         Registry-to-StateStore bridge is an unexpected impl subject; found: \
         {state_store_sites:?}"
    );
}

#[test]
fn store_is_a_reexport_only_compatibility_facade() {
    let forbidden = facade_non_reexport_constructs(&read_src(STORE));
    assert!(
        forbidden.is_empty(),
        "src/{STORE} must be a re-export-only T030 compatibility facade: after comments, \
         strings, attributes, and cfg(test) items are stripped, only public use declarations \
         and whitespace are allowed; unexpected construct: {forbidden:?}"
    );
}

#[test]
fn relocated_public_state_definitions_have_unique_production_owners() {
    let expected_owners = [
        ("ArtifactKey", STATE_FILE),
        ("EjectedEntry", STATE_FILE),
        ("FileRegistry", STATE_FILE),
        ("FrozenReadOnlyRegistry", STATE_FILE),
        ("HookState", STATE_FILE),
        ("ManifestFile", STATE_FILE),
        ("ProjectedRecord", STATE_FILE),
        ("RecordKind", STATE_FILE),
        ("Registry", STATE_FILE),
        ("RegistryRecord", STATE_FILE),
        ("StateLockGuard", STATE_LOCKING),
        ("StoreError", STATE_FILE),
    ];
    for (name, expected) in expected_owners {
        let sites = definition_sites(name);
        assert_eq!(
            sites,
            [expected],
            "{name} must have exactly one production definition in src/{expected}; \
             compatibility through src/{STORE} is by pub use, never a duplicate definition; \
             found: {sites:?}"
        );
    }
}

#[test]
fn model_module_defines_the_three_reconcile_value_types() {
    let scanned = scan(&read_src(MODEL_MODULE));
    let missing: Vec<&&str> = MODEL_TYPES
        .iter()
        .filter(|name| !defines_pub_type(&scanned, name))
        .collect();
    assert!(
        missing.is_empty(),
        "src/{MODEL_MODULE} must define the reconcile-facing value types as `pub struct`/`pub \
         enum` (design §7.3): the per-artifact observation aggregate ObservedProjectState, the \
         ObservedArtifact case split, and the ManagedCondition classification — a `type` alias or \
         `use ... as` shadow does not count; missing: {missing:?}"
    );
}

#[test]
fn managed_condition_carries_the_five_condition_variants() {
    let body = enum_body(&scan(&read_src(MODEL_MODULE)), "ManagedCondition").unwrap_or_else(|| {
        panic!(
            "src/{MODEL_MODULE} does not define `enum ManagedCondition` yet — design §7.3's \
             classification of a managed artifact's condition"
        )
    });
    let variants = enum_variant_names(&body);
    let missing: Vec<&&str> = MANAGED_CONDITION_VARIANTS
        .iter()
        .filter(|variant| !variants.iter().any(|v| v == *variant))
        .collect();
    assert!(
        missing.is_empty(),
        "ManagedCondition must declare the design §7.3 variants as TOP-LEVEL variants — Clean, \
         MetadataChangedButContentClean, Outdated, Modified, Linked (payload shapes stay \
         unpinned; a name appearing only inside a payload type does not count); declared \
         variants: {variants:?}; missing: {missing:?}"
    );
}

#[test]
fn model_module_imports_no_io_bearing_module() {
    let impurities = model_impurities(&read_src(MODEL_MODULE));
    assert!(
        impurities.is_empty(),
        "src/{MODEL_MODULE} must be PURE: reconcile imports it under the INV-8 allowlist, so it \
         may reach no I/O-bearing module — not crate::store (ManagedArtifact references the \
         record type, but purity is satisfied by importing pure value types, restructuring \
         fields, or deferring internals to T018, never by importing store), not source/deploy, \
         not the I/O-bearing sync siblings (state/target/stage/...), not std \
         fs/io/os/net/process, not gix/chrono/ureq/walkdir/tar/zip/flate2. Found forbidden \
         references: {impurities:?}"
    );
}

#[test]
fn helper_slot_scan_requires_exactly_the_sketch_names() {
    let sketch = "pub trait StateStore {\n\
        fn artifact(&self) {}\n fn put_artifact(&self) {}\n fn remove_artifact(&self) {}\n\
        fn target_artifacts(&self) {}\n fn all_artifacts(&self) {}\n fn ejections(&self) {}\n\
        fn save_ejections(&self) {}\n fn hook_state(&self) {}\n fn record_hook_success(&self) {}\n\
        fn acquire_lock(&self) {}\n fn journal_root(&self) {}\n}";
    let (missing, extra) = slot_mismatch(sketch);
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the exact design §8 sketch names must satisfy the scan cleanly, got missing {missing:?} \
         extra {extra:?}"
    );

    let legacy = "pub trait StateStore {\n\
        fn get(&self) {}\n fn put(&self) {}\n fn remove(&self) {}\n fn list_target(&self) {}\n\
        fn list_all(&self) {}\n fn load_ejected(&self) {}\n fn save_ejected(&self) {}\n\
        fn load_hook_state(&self) {}\n fn record_hook_success(&self) {}\n\
        fn lock_exclusive(&self) {}\n fn locks_dir(&self) {}\n}";
    let (missing, extra) = slot_mismatch(legacy);
    assert!(
        !missing.is_empty() && !extra.is_empty(),
        "legacy Registry names must NOT satisfy the canonized sketch vocabulary — they are both \
         missing sketch names and extra fns, got missing {missing:?} extra {extra:?}"
    );

    let widened = "pub trait StateStore {\n\
        fn artifact(&self) {}\n fn put_artifact(&self) {}\n fn remove_artifact(&self) {}\n\
        fn target_artifacts(&self) {}\n fn all_artifacts(&self) {}\n fn ejections(&self) {}\n\
        fn save_ejections(&self) {}\n fn hook_state(&self) {}\n fn record_hook_success(&self) {}\n\
        fn acquire_lock(&self) {}\n fn journal_root(&self) {}\n fn refuses_writes(&self) -> bool { false }\n}";
    let (missing, extra) = slot_mismatch(widened);
    assert!(
        missing.is_empty() && extra == ["refuses_writes"],
        "a copied-in refuses_writes must surface as trait WIDENING, got missing {missing:?} \
         extra {extra:?}"
    );

    let short = "pub trait StateStore {\n fn artifact(&self) {}\n fn put_artifact(&self) {}\n}";
    let (missing, _) = slot_mismatch(short);
    assert!(
        missing.contains(&"record_hook_success") && missing.contains(&"acquire_lock"),
        "a trait missing methods must report them, got: {missing:?}"
    );

    let (missing, extra) = slot_mismatch("pub fn not_a_trait() {}");
    assert!(
        missing.len() == SKETCH_METHODS.len() && extra.is_empty(),
        "no trait at all means every method is missing"
    );
}

#[test]
fn helper_trait_fn_scan_reads_top_level_items_not_default_bodies() {
    let body = "{ fn artifact(&self); fn acquire_lock(&self) -> Lock { fn sneaky() {} \
                helper(self) } }";
    let names = trait_fn_names(body);
    assert_eq!(
        names,
        ["artifact", "acquire_lock"],
        "only trait-item fns count — an fn nested inside a default method body must not \
         register, got: {names:?}"
    );
    assert!(
        trait_fn_names("{ }").is_empty(),
        "an empty trait body declares nothing"
    );
}

#[test]
fn helper_hollow_marker_scan_flags_stub_macros_and_spares_real_code() {
    assert_eq!(
        hollow_markers("{ fn artifact(&self) { unimplemented!() } }"),
        ["unimplemented"],
        "an unimplemented! stub must be flagged"
    );
    assert_eq!(
        hollow_markers("{ todo!(\"later\") }"),
        ["todo"],
        "a todo! stub must be flagged"
    );
    assert_eq!(
        hollow_markers("{ panic!(\"boom\") }"),
        ["panic"],
        "a panic! stub must be flagged"
    );
    assert_eq!(
        hollow_markers("{ panic! (\"boom\") }"),
        ["panic"],
        "space between the bang and the paren must not evade the scan"
    );
    assert_eq!(
        hollow_markers("{ panic! { \"boom\" } }"),
        ["panic"],
        "brace-delimited macro form `panic! {{ .. }}` must be flagged"
    );
    assert_eq!(
        hollow_markers("{ todo !() }"),
        ["todo"],
        "space between the name and the bang must not evade the scan"
    );
    assert_eq!(
        hollow_markers("{ unimplemented ! [] }"),
        ["unimplemented"],
        "bracket-delimited macro form with spaced bang must be flagged"
    );
    assert!(
        hollow_markers("{ self.get(key) }").is_empty(),
        "a real delegating body carries no hollow markers"
    );
    assert!(
        hollow_markers("{ my_todo!() }").is_empty(),
        "a macro merely ending in todo! must not be flagged (word-boundary before)"
    );
    assert!(
        hollow_markers("{ let panic_free = a != b; }").is_empty(),
        "an identifier starting with a marker name and a bare != must not be flagged"
    );
    let stripped = strip("fn doc() { let s = \"panic!(never)\"; }");
    assert!(
        hollow_markers(&stripped).is_empty(),
        "hollow tokens inside string literals are blanked before scanning: {stripped}"
    );
}

#[test]
fn helper_purity_scan_catches_bypass_classes_and_spares_pure_imports() {
    assert!(
        !model_impurities("use crate::store::RegistryRecord;\npub struct X;").is_empty(),
        "a plain `use crate::store::...` import must be flagged"
    );
    assert!(
        !model_impurities("pub fn f() { let _ = crate :: store :: RegistryRecord::default(); }")
            .is_empty(),
        "a whitespace-separated fully-qualified `crate :: store` body path must be flagged"
    );
    assert!(
        !model_impurities("pub fn f() {\n let _ = crate::\n    store::X; }").is_empty(),
        "a newline-split `crate::\\n store` body path must be flagged"
    );
    assert!(
        !model_impurities("use crate::{store::RegistryRecord};\npub struct X;").is_empty(),
        "a braced use-group hiding crate::store must be flagged"
    );
    assert!(
        !model_impurities("use std::{fs};\npub struct X;").is_empty(),
        "a braced `use std::{{fs}};` must be flagged (group expansion)"
    );
    assert!(
        !model_impurities("use std::{collections::BTreeMap, fs};\npub struct X;").is_empty(),
        "std::fs smuggled into a multi-member group must be flagged"
    );
    assert!(
        !model_impurities("use crate::sync::state::StateStore;\npub struct X;").is_empty(),
        "laundering I/O through the sync::state sibling must be flagged"
    );
    assert!(
        !model_impurities("use super::state::StateStore;\npub struct X;").is_empty(),
        "laundering I/O through `super::state` must be flagged"
    );
    assert!(
        !model_impurities("use super::target::record_artifact_path;\npub struct X;").is_empty(),
        "an I/O-bearing sync sibling (super::target) must be flagged"
    );
    assert!(
        !model_impurities("use crate::sync::stage::stage_artifact;\npub struct X;").is_empty(),
        "an I/O-bearing sync sibling (sync::stage) must be flagged"
    );
    for impure in [
        "use std::fs;\n",
        "pub fn f() { let _ = std::fs::read(\"x\"); }\n",
        "use gix::Repository;\n",
        "use chrono::Utc;\n",
        "use walkdir::WalkDir;\n",
        "use crate::source::SourceStore;\n",
        "use crate::deploy::Journal;\n",
    ] {
        assert!(
            !model_impurities(impure).is_empty(),
            "an I/O-bearing reference must be flagged: {impure:?}"
        );
    }
    assert!(
        !model_impurities("use zip::ZipArchive;\npub struct X;").is_empty(),
        "an import of the zip crate must be flagged"
    );
    assert!(
        !model_impurities("pub fn f() { let _ = zip::read(\"x\"); }").is_empty(),
        "a zip:: path head in a body must be flagged"
    );
    assert!(
        !model_impurities("pub fn f() { let _ = crate::sync::stage::stage_artifact(); }")
            .is_empty(),
        "a fully-qualified crate::sync::stage body path must still be flagged"
    );
    assert!(
        !model_impurities("use zip as archive;\npub struct X;").is_empty(),
        "an aliased crate import (`use zip as archive;`) must be flagged — the rename must not \
         hide the forbidden crate"
    );
    assert!(
        !model_impurities("use crate::{store as s};\npub struct X;").is_empty(),
        "a grouped-and-aliased `use crate::{{store as s}};` must be flagged"
    );
    assert!(
        !model_impurities("use crate::store as s;\npub struct X;").is_empty(),
        "an aliased `use crate::store as s;` must be flagged"
    );
    assert!(
        !model_impurities("use super::state as st;\npub struct X;").is_empty(),
        "an aliased `use super::state as st;` must be flagged"
    );
}

#[test]
fn helper_purity_scan_spares_pure_code_and_boundary_lookalikes() {
    let pure = "use std::collections::BTreeMap;\n\
        use serde::{Deserialize, Serialize};\n\
        use crate::projection::model::ArtifactRelativePath;\n\
        use crate::kernel::TargetName;\n\
        pub enum ManagedCondition { Clean }\n";
    assert!(
        model_impurities(pure).is_empty(),
        "pure imports — std::collections, serde, projection value types, kernel identities — \
         must pass"
    );
    let zipped = "pub fn f(left: &[u8], right: &[u8]) -> usize {\n    \
        left.iter().zip(right.iter()).count()\n}\n";
    assert!(
        model_impurities(zipped).is_empty(),
        "the iterator method .zip( must not trip the zip-crate token (path heads only)"
    );
    assert!(
        model_impurities("pub fn f(v: &keystore::Value) -> u8 { v.byte() }").is_empty(),
        "keystore:: must not trip the store:: needle (identifier-segment boundary before)"
    );
    assert!(
        model_impurities("use super::statement::Parsed;\npub struct X;").is_empty(),
        "super::statement must not trip super::state (identifier-segment boundary after)"
    );
    assert!(
        model_impurities("use std::collections::BTreeMap as Map;\npub struct X;").is_empty(),
        "an aliased pure std import must not be flagged"
    );
    assert!(
        model_impurities("use serde::Serialize as Ser;\npub struct X;").is_empty(),
        "an aliased serde import must not be flagged"
    );
    let commented = "// use crate::store::RegistryRecord;\npub struct X;\n";
    assert!(
        model_impurities(commented).is_empty(),
        "a forbidden import inside a comment must not be flagged (strip runs first)"
    );
    let cfg_test = "pub struct X;\n#[cfg(test)]\nmod t { use crate::store::RegistryRecord; }\n";
    assert!(
        model_impurities(cfg_test).is_empty(),
        "a forbidden import confined to a #[cfg(test)] module is not a production impurity"
    );
}

#[test]
fn helper_impl_detection_binds_to_subject_type_not_header_mention() {
    assert!(
        impls_trait_for(
            "impl StateStore for FileRegistry { }",
            TRAIT,
            "FileRegistry"
        ),
        "the plain trait impl on FileRegistry must be detected"
    );
    assert!(
        impls_trait_for(
            "impl<'a> StateStore for FileRegistry<'a> { }",
            TRAIT,
            "FileRegistry"
        ),
        "a generic trait impl on FileRegistry must be detected"
    );
    assert!(
        !impls_trait_for("impl Registry for FileRegistry { }", TRAIT, "FileRegistry"),
        "an impl of a DIFFERENT trait on FileRegistry must not satisfy the StateStore pin"
    );
    assert!(
        !impls_trait_for(
            "impl StateStore for FrozenReadOnlyRegistry<'_> { }",
            TRAIT,
            "FileRegistry"
        ),
        "a StateStore impl on another subject must not satisfy the FileRegistry pin"
    );
    assert!(
        !impls_trait_for(
            "impl Builder { fn make(&self) -> StateStore { helper() } }",
            TRAIT,
            "FileRegistry"
        ),
        "merely naming StateStore in another type's method must not count as an impl"
    );
}

#[test]
fn helper_trait_impl_allowlist_rejects_renamed_adapters_and_spares_real_owners() {
    let renamed_registry_bridge = scan(
        "use crate::store::Registry as LegacyRegistry;\n\
         struct AnyBridge<'a> { store: &'a dyn StateStore }\n\
         impl LegacyRegistry for AnyBridge<'_> { }",
    );
    assert_eq!(
        trait_aliases(&renamed_registry_bridge, "Registry"),
        ["LegacyRegistry"],
        "a Registry alias is itself a forbidden production escape hatch"
    );
    assert_eq!(
        trait_impl_subjects(&renamed_registry_bridge, "Registry"),
        ["AnyBridge"],
        "an aliased Registry impl on a renamed StateStore bridge must still surface by subject"
    );

    let renamed_store_bridge = scan(
        "use crate::sync::{\n\
             inspect::Other,\n\
             state::{StateStore as LegacyStateStore, StateError},\n\
         };\n\
         struct OtherBridge<'a> { registry: &'a dyn Registry }\n\
         impl LegacyStateStore for OtherBridge<'_> { }",
    );
    assert_eq!(
        trait_aliases(&renamed_store_bridge, TRAIT),
        ["LegacyStateStore"],
        "a StateStore alias nested in a grouped use tree must be detected"
    );
    assert_eq!(
        trait_impl_subjects(&renamed_store_bridge, TRAIT),
        ["OtherBridge"],
        "an aliased StateStore impl on a renamed Registry bridge must still surface by subject"
    );

    let owners = scan(
        "impl Registry for FileRegistry { }\n\
         impl crate::store::Registry for FrozenReadOnlyRegistry<'_> { }\n\
         impl crate::sync::state::StateStore for FileRegistry { }\n\
         #[cfg(test)] impl StateStore for TestFake { }",
    );
    assert_eq!(
        trait_impl_subjects(&owners, "Registry"),
        ["FileRegistry", "FrozenReadOnlyRegistry"],
        "the two legitimate Registry owners must satisfy the exact subject allowlist"
    );
    assert_eq!(
        trait_impl_subjects(&owners, TRAIT),
        ["FileRegistry"],
        "the real FileRegistry StateStore owner must pass and a cfg(test) fake must be excluded"
    );
}

#[test]
fn helper_locking_seam_scan_rejects_free_functions_and_decoy_impl_subjects() {
    let decoys = locking_seam_sites(
        STATE_LOCKING,
        "pub fn lock_exclusive() {}\n\
         struct LockLookalike;\n\
         impl LockLookalike { pub fn lock_advisory(&self) {} }",
    );
    assert_eq!(
        decoys,
        [
            (
                format!("{STATE_LOCKING}:<free>::lock_exclusive"),
                true,
                false,
            ),
            (
                format!("{STATE_LOCKING}:LockLookalike::lock_advisory"),
                true,
                true,
            ),
        ],
        "free functions and methods on another impl subject must not masquerade as \
         FileRegistry's locking seams"
    );

    let owner = locking_seam_sites(
        STATE_LOCKING,
        "impl FileRegistry {\n\
             pub fn lock_exclusive(&self) {}\n\
             pub fn lock_advisory(&self) {}\n\
         }",
    );
    assert_eq!(
        owner,
        [
            (
                format!("{STATE_LOCKING}:FileRegistry::lock_advisory"),
                true,
                true,
            ),
            (
                format!("{STATE_LOCKING}:FileRegistry::lock_exclusive"),
                true,
                true,
            ),
        ],
        "both public inherent methods on FileRegistry are the legitimate ownership shape"
    );
}

#[test]
fn helper_facade_grammar_rejects_every_non_reexport_construct() {
    let forbidden = [
        ("include!(\"legacy_store.rs\");", "include macro"),
        (
            "macro_rules! legacy { () => { impl FileRegistry {} } }\nlegacy!();",
            "macro definition and invocation",
        ),
        ("legacy_items!();", "ordinary macro invocation"),
        ("mod legacy;", "module declaration"),
        ("pub trait Port {}", "trait definition"),
        ("pub struct Owner;", "struct definition"),
        ("pub fn helper() {}", "function definition"),
        ("impl Owner {}", "implementation block"),
    ];
    for (source, construct) in forbidden {
        assert!(
            !facade_non_reexport_constructs(source).is_empty(),
            "a top-level {construct} must make the compatibility facade substantive: {source}"
        );
    }

    let reexports = r#"
        //! compatibility only
        #![allow(deprecated)]
        #[doc = "legacy state names"]
        pub use crate::sync::state::{
            ArtifactKey,
            file::{FileRegistry, Registry},
            RegistryRecord,
        };

        #[cfg(test)]
        mod tests {
            struct Fixture;
            impl Fixture { fn helper() {} }
        }
    "#;
    assert!(
        facade_non_reexport_constructs(reexports).is_empty(),
        "grouped/multiline public re-exports, harmless attributes, and cfg(test)-only helpers \
         are allowed in the thin facade"
    );
}

#[test]
fn helper_type_alias_and_rename_do_not_satisfy_the_value_type_pins() {
    assert!(
        defines_pub_type("pub enum ManagedCondition { Clean }", "ManagedCondition"),
        "a real enum definition is a definition site"
    );
    assert!(
        defines_pub_type(
            "pub struct ObservedProjectState { }",
            "ObservedProjectState"
        ),
        "a real struct definition is a definition site"
    );
    assert!(
        !defines_pub_type("pub type ManagedCondition = ();", "ManagedCondition"),
        "a `pub type` alias must not satisfy the struct/enum definition pin"
    );
    assert!(
        !defines_pub_type(
            "use crate::other::Thing as ObservedArtifact;",
            "ObservedArtifact"
        ),
        "a `use ... as` rename must not satisfy the struct/enum definition pin"
    );
}

#[test]
fn helper_keyword_scan_requires_whitespace_after_the_keyword() {
    assert!(
        keyword_names("mod state;", "mod", "state"),
        "a real declaration with a single space matches"
    );
    assert!(
        keyword_names("mod\n    state;", "mod", "state"),
        "newline whitespace after the keyword still matches"
    );
    assert!(
        !keyword_names("fn modstate() {}", "mod", "state"),
        "an identifier merely containing the keyword+name (`fn modstate`) must not match — a \
         whitespace gap after the keyword is required"
    );
    assert!(
        !keyword_names(
            "pub structObservedProjectState: u8,",
            "pub struct",
            "ObservedProjectState"
        ),
        "a field named structObservedProjectState must not satisfy the type pin"
    );
    assert!(
        !keyword_names("pub fnartifact() {}", "fn", "artifact"),
        "fnartifact glued together must not satisfy the fn pin"
    );
}

#[test]
fn helper_variant_scan_reads_top_level_variants_only() {
    let body = "{ Clean, MetadataChangedButContentClean { refreshed: Vec<ScannedFile> }, \
                Outdated, Modified { changed: Vec<TargetPath> }, Linked }";
    let names = enum_variant_names(body);
    assert_eq!(
        names,
        [
            "Clean",
            "MetadataChangedButContentClean",
            "Outdated",
            "Modified",
            "Linked"
        ],
        "top-level variant identifiers are extracted; payload fields are not variants"
    );
    let payload_only = "{ Wrapper(Clean), Outdated }";
    let names = enum_variant_names(payload_only);
    assert!(
        !names.iter().any(|n| n == "Clean"),
        "a name appearing only inside a payload type must not register as a variant, got: \
         {names:?}"
    );
    let attributed = "{ #[serde(rename = \"c\")] Clean, Outdated }";
    let names = enum_variant_names(&strip(attributed));
    assert!(
        names.iter().any(|n| n == "Clean"),
        "an attribute before a variant must not hide it, got: {names:?}"
    );
    let compound_only = "{ MetadataChangedButContentClean, Outdated }";
    let names = enum_variant_names(compound_only);
    assert!(
        !names.iter().any(|n| n == "Clean"),
        "the compound variant must not stand in for standalone Clean"
    );
}
