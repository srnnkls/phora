use phora::config::Config;

const README: &str = include_str!("../README.md");
const EXAMPLE: &str = include_str!("../phora.example.toml");
const LOCAL_EXAMPLE: &str = include_str!("../phora.local.example.toml");

fn strip_scrut_prompt(line: &str) -> &str {
    line.trim()
        .strip_prefix("> ")
        .unwrap_or_else(|| line.trim())
}

fn fenced_lines(contents: &str) -> Vec<&str> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in contents.lines() {
        if strip_scrut_prompt(line).starts_with("```") {
            inside = !inside;
            continue;
        }
        if inside {
            out.push(strip_scrut_prompt(line));
        }
    }
    out
}

fn fenced_blocks(contents: &str) -> Vec<String> {
    let mut inside = false;
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in contents.lines() {
        if strip_scrut_prompt(line).starts_with("```") {
            if inside {
                blocks.push(current.join("\n"));
                current.clear();
            }
            inside = !inside;
            continue;
        }
        if inside {
            current.push(strip_scrut_prompt(line));
        }
    }
    blocks
}

fn is_sources_array_element(line: &str) -> bool {
    let line = strip_scrut_prompt(line).trim_start();
    line.starts_with('{') || line.contains("{ source")
}

fn has_multiline_sources_array_of_tables(lines: &[&str]) -> bool {
    let mut open = false;
    for line in lines {
        let line = strip_scrut_prompt(line);
        if open {
            if is_sources_array_element(line) {
                return true;
            }
            if line.contains(']') {
                open = false;
            }
            continue;
        }
        if line.contains("sources = [") && !line.contains(']') {
            open = true;
        }
    }
    false
}

fn is_legacy_as_binding(line: &str) -> bool {
    line.contains("as =") && (line.contains("source") || line.contains('{'))
}

fn is_legacy_inline_sources(line: &str) -> bool {
    line.contains("sources = [{")
}

fn assert_no_legacy_binding_forms_in(file: &str, toml_lines: &[&str]) {
    assert!(
        !has_multiline_sources_array_of_tables(toml_lines),
        "{file}: must not document the legacy multiline array-of-tables binding form \
         (a `{{`/`{{ source` table element inside an open `sources = [` array)"
    );
    for line in toml_lines {
        assert!(
            !is_legacy_inline_sources(line),
            "{file}: must not document the legacy inline array-of-tables \
             binding form `sources = [{{` (found in TOML: `{line}`)"
        );
        assert!(
            !is_legacy_as_binding(line),
            "{file}: must not use the legacy binding identity field `as =` \
             (keyed by table key now) (found in TOML: `{line}`)"
        );
    }
}

fn is_keyed_sources_header(line: &str) -> bool {
    let line = strip_scrut_prompt(line);
    let Some(rest) = line.strip_prefix("[targets.") else {
        return false;
    };
    let Some(id) = rest.strip_suffix(".sources]") else {
        return false;
    };
    !id.is_empty()
}

fn documents_keyed_sources_table(lines: impl Iterator<Item = impl AsRef<str>>) -> bool {
    lines
        .into_iter()
        .any(|line| is_keyed_sources_header(line.as_ref()))
}

#[test]
fn readme_drops_legacy_binding_forms() {
    assert_no_legacy_binding_forms_in("README.md", &fenced_lines(README));
}

#[test]
fn readme_documents_keyed_sources_table() {
    assert!(
        documents_keyed_sources_table(fenced_lines(README).into_iter()),
        "README.md: must document the keyed binding model with a contiguous \
         `[targets.<t>.sources]` table header inside a TOML code fence"
    );
}

#[test]
fn example_toml_drops_legacy_binding_forms() {
    let lines: Vec<&str> = EXAMPLE.lines().collect();
    assert_no_legacy_binding_forms_in("phora.example.toml", &lines);
}

#[test]
fn example_toml_documents_keyed_sources_table() {
    assert!(
        documents_keyed_sources_table(EXAMPLE.lines()),
        "phora.example.toml: must use a contiguous keyed `[targets.<t>.sources]` table header"
    );
}

#[test]
fn local_example_toml_drops_legacy_binding_forms() {
    let lines: Vec<&str> = LOCAL_EXAMPLE.lines().collect();
    assert_no_legacy_binding_forms_in("phora.local.example.toml", &lines);
}

#[test]
fn readme_self_contained_fences_parse_and_validate() {
    let complete: Vec<String> = fenced_blocks(README)
        .into_iter()
        .filter(|block| block.trim_start().starts_with("version = 1"))
        .collect();
    assert!(
        !complete.is_empty(),
        "README.md: expected at least one self-contained `version = 1` config fence"
    );
    for block in &complete {
        let config = Config::parse(block).unwrap_or_else(|err| {
            panic!("README.md: self-contained config fence must parse: {err}\n---\n{block}")
        });
        config.validate().unwrap_or_else(|err| {
            panic!("README.md: self-contained config fence must validate: {err}\n---\n{block}")
        });
    }
}

#[test]
fn readme_drops_multiline_array_of_tables() {
    assert!(
        !has_multiline_sources_array_of_tables(&fenced_lines(README)),
        "README.md: must not document the legacy multiline array-of-tables binding form"
    );
}

#[test]
fn readme_documents_network_filesystem_lock_limitation() {
    let doc = README.to_lowercase();

    assert!(
        doc.contains("nfs")
            || doc.contains("network filesystem")
            || doc.contains("network file system"),
        "README.md: locking docs must name the network-filesystem (NFS) limitation"
    );
    assert!(
        doc.contains("smb") || doc.contains("cifs"),
        "README.md: locking docs must name SMB/CIFS alongside NFS as an unreliable-lock filesystem"
    );
    assert!(
        doc.contains("advisory") || doc.contains("unreliable") || doc.contains("best-effort"),
        "README.md: locking docs must state the lock is advisory / unreliable over network mounts"
    );
    assert!(
        doc.contains("shared")
            && (doc.contains("$home")
                || doc.contains("home directory")
                || doc.contains("home dir")),
        "README.md: locking docs must carry the shared-$HOME concurrent-sync guidance"
    );
}
