//! Text-level `toml_edit` mutators over `phora.toml` / `phora.local.toml`.
//!
//! These preserve formatting and comments byte-for-byte on untouched regions.
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

use super::add::AddTarget;
use crate::error::{Error, Result};
use crate::source::Protocol;

fn parse_doc(doc_text: &str) -> Result<DocumentMut> {
    doc_text
        .parse::<DocumentMut>()
        .map_err(|e| Error::Config(format!("parse toml: {e}")))
}

pub(super) fn source_table(source: &AddTarget) -> Table {
    let mut table = Table::new();
    if let Some(git) = &source.git {
        table["git"] = value(git.as_str());
        return table;
    }
    if let Some(path) = &source.path {
        table["path"] = value(path.as_str());
        return table;
    }
    if let Some(host) = &source.host {
        table["host"] = value(host.as_str());
    }
    if let Some(repo) = &source.repo {
        table["repo"] = value(repo.as_str());
    }
    if let Some(Protocol::Ssh) = source.protocol {
        table["protocol"] = value("ssh");
    }
    table
}

pub(super) fn ensure_sources_table(doc: &mut DocumentMut) {
    if doc.get("sources").is_none() {
        let mut sources = Table::new();
        sources.set_implicit(true);
        doc["sources"] = Item::Table(sources);
    }
}

/// Insert or replace `[sources.<name>]` preserving surrounding formatting.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if `doc_text` is not valid TOML.
pub fn upsert_source(
    doc_text: &str,
    name: &str,
    source: &AddTarget,
    branch: Option<&str>,
    tag: Option<&str>,
    root: Option<&str>,
) -> Result<String> {
    let mut doc = parse_doc(doc_text)?;

    let mut table = source_table(source);
    if let Some(branch) = branch {
        table["branch"] = value(branch);
    }
    if let Some(tag) = tag {
        table["tag"] = value(tag);
    }
    if let Some(root) = root.or(source.root.as_deref()) {
        table["root"] = value(root);
    }

    ensure_sources_table(&mut doc);
    doc["sources"][name] = Item::Table(table);
    Ok(doc.to_string())
}

#[derive(Debug)]
pub struct ScrubResult {
    pub main: String,
    pub local: String,
}

/// Rewrite every element's decor to the canonical `, `-separated form so a
/// surviving array renders without leading-space artifacts after element removal.
fn normalize_array_decor(array: &mut Array) {
    for (i, item) in array.iter_mut().enumerate() {
        item.decor_mut().set_prefix(if i == 0 { "" } else { " " });
        item.decor_mut().set_suffix("");
    }
    array.set_trailing("");
    array.set_trailing_comma(false);
}

fn scrub_target_arrays(doc: &mut DocumentMut, name: &str) {
    let Some(targets) = doc.get_mut("targets").and_then(Item::as_table_like_mut) else {
        return;
    };
    for (_, target) in targets.iter_mut() {
        let Some(array) = target
            .get_mut("sources")
            .and_then(Item::as_value_mut)
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        let before = array.len();
        array.retain(|v| v.as_str() != Some(name));
        if array.len() != before {
            normalize_array_decor(array);
        }
    }
}

fn remove_source_table(doc: &mut DocumentMut, name: &str) -> bool {
    doc.get_mut("sources")
        .and_then(Item::as_table_like_mut)
        .is_some_and(|sources| sources.remove(name).is_some())
}

/// Remove `[sources.<name>]` from BOTH texts AND scrub `<name>` from every
/// `[targets.*].sources` array in BOTH.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if `<name>` is absent from both docs.
pub fn remove_source(main_text: &str, local_text: &str, name: &str) -> Result<ScrubResult> {
    let mut main = parse_doc(main_text)?;
    let mut local = parse_doc(local_text)?;

    let removed = remove_source_table(&mut main, name) | remove_source_table(&mut local, name);
    if !removed {
        return Err(Error::Config(format!(
            "source `{name}` is not defined in phora.toml or phora.local.toml"
        )));
    }

    scrub_target_arrays(&mut main, name);
    scrub_target_arrays(&mut local, name);

    Ok(ScrubResult {
        main: main.to_string(),
        local: local.to_string(),
    })
}

/// Insert or replace `[targets.<name>]` with path + optional layout (string form).
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if `doc_text` is not valid TOML.
pub fn upsert_target(
    doc_text: &str,
    name: &str,
    path: &str,
    layout: Option<&str>,
) -> Result<String> {
    let mut doc = parse_doc(doc_text)?;

    let mut table = Table::new();
    table["path"] = value(path);
    if let Some(layout) = layout {
        table["layout"] = value(layout);
    }

    if doc.get("targets").is_none() {
        let mut targets = Table::new();
        targets.set_implicit(true);
        doc["targets"] = Item::Table(targets);
    }
    doc["targets"][name] = Item::Table(table);
    Ok(doc.to_string())
}

/// Remove `[targets.<name>]`.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if `<name>` is absent.
pub fn remove_target(doc_text: &str, name: &str) -> Result<String> {
    let mut doc = parse_doc(doc_text)?;
    let removed = doc
        .get_mut("targets")
        .and_then(Item::as_table_like_mut)
        .is_some_and(|targets| targets.remove(name).is_some());
    if !removed {
        return Err(Error::Config(format!("target `{name}` is not defined")));
    }
    Ok(doc.to_string())
}

fn target_table_mut<'a>(doc: &'a mut DocumentMut, target: &str) -> Option<&'a mut Table> {
    doc.get_mut("targets")?
        .as_table_like_mut()?
        .get_mut(target)?
        .as_table_mut()
}

fn names_array(names: &[String]) -> Array {
    let mut array = Array::new();
    for n in names {
        array.push(n.as_str());
    }
    normalize_array_decor(&mut array);
    array
}

#[derive(Debug)]
pub struct BindResult {
    pub text: String,
}

/// Append `sources` to `[targets.<target>].sources` (dedup, stable order),
/// creating the list when the target has no `sources` key.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if the target table does not exist.
pub fn bind(doc_text: &str, target: &str, sources: &[String]) -> Result<BindResult> {
    let mut doc = parse_doc(doc_text)?;
    let table = target_table_mut(&mut doc, target)
        .ok_or_else(|| Error::Config(format!("target `{target}` is not defined")))?;

    let mut present: Vec<String> = if table.contains_key("sources") {
        let array = table["sources"]
            .as_array_mut()
            .ok_or_else(|| Error::Config(format!("`{target}.sources` is not an array")))?;
        array
            .iter()
            .map(|v| {
                v.as_str().map(str::to_owned).ok_or_else(|| {
                    Error::Config(format!(
                        "`{target}.sources` contains a non-string entry; only plain source names are supported"
                    ))
                })
            })
            .collect::<Result<_>>()?
    } else {
        Vec::new()
    };
    for s in sources {
        if !present.contains(s) {
            present.push(s.clone());
        }
    }
    table["sources"] = value(names_array(&present));

    Ok(BindResult {
        text: doc.to_string(),
    })
}

#[derive(Debug)]
pub struct UnbindResult {
    pub text: String,
    pub tombstoned: bool,
}

/// Remove `sources` from `[targets.<target>].sources`.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if the target has no `sources` list.
pub fn unbind(doc_text: &str, target: &str, sources: &[String]) -> Result<UnbindResult> {
    let mut doc = parse_doc(doc_text)?;
    let table = target_table_mut(&mut doc, target)
        .ok_or_else(|| Error::Config(format!("target `{target}` is not defined")))?;

    if !table.contains_key("sources") {
        return Err(Error::Config(format!(
            "target `{target}` has no `sources` list; nothing to unbind"
        )));
    }
    let array = table["sources"]
        .as_array_mut()
        .ok_or_else(|| Error::Config(format!("`{target}.sources` is not an array")))?;
    array.retain(|v| v.as_str().is_none_or(|s| !sources.contains(&s.to_owned())));
    let remaining: Vec<String> = array
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    table["sources"] = value(names_array(&remaining));

    Ok(UnbindResult {
        text: doc.to_string(),
        tombstoned: remaining.is_empty(),
    })
}

/// Pre-write guard: every `[targets.*].sources` entry resolves to an existing source.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] naming the dangling (target, source).
pub fn validate_source_references(merged: &crate::config::Config) -> Result<()> {
    for (target_name, target) in &merged.targets {
        let Some(sources) = &target.sources else {
            continue;
        };
        for source in sources {
            if !merged.sources.contains_key(source) {
                return Err(Error::Config(format!(
                    "target `{target_name}` references undefined source `{source}`"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::error::Error;

    fn lit_source(git: &str) -> AddTarget {
        AddTarget {
            name: String::new(),
            git: Some(git.to_owned()),
            host: None,
            repo: None,
            path: None,
            protocol: None,
            branch: None,
            root: None,
        }
    }

    fn symbolic_source(host: &str, repo: &str) -> AddTarget {
        AddTarget {
            name: String::new(),
            git: None,
            host: Some(host.to_owned()),
            repo: Some(repo.to_owned()),
            path: None,
            protocol: None,
            branch: None,
            root: None,
        }
    }

    fn path_source(path: &str) -> AddTarget {
        AddTarget {
            name: String::new(),
            git: None,
            host: None,
            repo: None,
            path: Some(path.to_owned()),
            protocol: None,
            branch: None,
            root: None,
        }
    }

    // 1. Formatting preservation: byte-identical surrounding regions.

    const DECORATED: &str = "version = 1\n\n# top comment\n[hosts.foo]\nbar = \"baz\"\n";

    #[test]
    fn upsert_source_preserves_surrounding_text_byte_for_byte() {
        let out = upsert_source(
            DECORATED,
            "loqui",
            &lit_source("https://github.com/srnnkls/loqui.git"),
            None,
            None,
            None,
        )
        .expect("upsert into decorated toml");

        let expected = "version = 1\n\n# top comment\n[hosts.foo]\nbar = \"baz\"\n\n\
             [sources.loqui]\ngit = \"https://github.com/srnnkls/loqui.git\"\n";
        assert_eq!(
            out, expected,
            "the comment, blank line, and unrelated [hosts.foo] table must survive verbatim, \
             with the new standard table appended"
        );
    }

    #[test]
    fn upsert_target_preserves_surrounding_text_byte_for_byte() {
        let out = upsert_target(DECORATED, "neovim", "~/.config/nvim", Some("flat"))
            .expect("upsert target into decorated toml");

        let expected = "version = 1\n\n# top comment\n[hosts.foo]\nbar = \"baz\"\n\n\
             [targets.neovim]\npath = \"~/.config/nvim\"\nlayout = \"flat\"\n";
        assert_eq!(
            out, expected,
            "the comment and unrelated [hosts.foo] table must survive verbatim, with the new \
             [targets.neovim] standard table appended carrying path + layout"
        );
    }

    // 2. upsert_source migration parity (matches add.rs::source_table shapes).

    #[test]
    fn upsert_source_emits_literal_git_table() {
        let out = upsert_source(
            "version = 1\n",
            "loqui",
            &lit_source("https://github.com/srnnkls/loqui.git"),
            None,
            None,
            None,
        )
        .expect("upsert literal git source");

        let expected = "version = 1\n\n[sources.loqui]\n\
             git = \"https://github.com/srnnkls/loqui.git\"\n";
        assert_eq!(
            out, expected,
            "a literal git source must emit exactly a `git = ...` table, same shape as add.rs"
        );
    }

    #[test]
    fn upsert_source_emits_symbolic_host_repo_table() {
        let out = upsert_source(
            "version = 1\n",
            "dotfiles",
            &symbolic_source("github", "me/dotfiles"),
            None,
            None,
            None,
        )
        .expect("upsert symbolic source");

        let expected = "version = 1\n\n[sources.dotfiles]\n\
             host = \"github\"\nrepo = \"me/dotfiles\"\n";
        assert_eq!(
            out, expected,
            "a symbolic source must emit host + repo keys in that order, same shape as add.rs"
        );
    }

    #[test]
    fn upsert_source_emits_path_table() {
        let out = upsert_source(
            "version = 1\n",
            "scratch",
            &path_source("~/dev/scratch"),
            None,
            None,
            None,
        )
        .expect("upsert path source");

        let expected = "version = 1\n\n[sources.scratch]\npath = \"~/dev/scratch\"\n";
        assert_eq!(
            out, expected,
            "a path source must emit exactly a `path = ...` table, same shape as add.rs"
        );
    }

    #[test]
    fn upsert_source_emits_branch_tag_root_when_some() {
        let out = upsert_source(
            "version = 1\n",
            "editor",
            &lit_source("https://github.com/company/configs.git"),
            Some("main"),
            Some("v1.0"),
            Some("editor"),
        )
        .expect("upsert with branch+tag+root");

        let expected = "version = 1\n\n[sources.editor]\n\
             git = \"https://github.com/company/configs.git\"\n\
             branch = \"main\"\ntag = \"v1.0\"\nroot = \"editor\"\n";
        assert_eq!(
            out, expected,
            "Some branch/tag/root must each be written as keys after the source kind"
        );
    }

    #[test]
    fn upsert_source_replaces_existing_table() {
        let base = "version = 1\n\n[sources.foo]\ngit = \"https://github.com/old/foo.git\"\n";
        let out = upsert_source(
            base,
            "foo",
            &lit_source("https://github.com/new/foo.git"),
            None,
            None,
            None,
        )
        .expect("replace existing source");

        let expected = "version = 1\n\n[sources.foo]\ngit = \"https://github.com/new/foo.git\"\n";
        assert_eq!(
            out, expected,
            "upserting an existing source name must overwrite ONLY the git value in place, \
             keeping [sources.foo] at its original position with every surrounding byte intact"
        );
    }

    // 3. remove_source scrub-both-files.

    #[test]
    fn remove_source_scrubs_both_files_and_target_arrays() {
        let main = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.A]\npath = \"~/a\"\nsources = [\"dotfiles\", \"other\"]\n\n\
             [sources.other]\ngit = \"h\"\n";
        let local = "version = 1\n\n\
             [targets.B]\npath = \"~/b\"\nsources = [\"dotfiles\"]\n";

        let result = remove_source(main, local, "dotfiles").expect("remove dotfiles");

        let expected_main = "version = 1\n\n\
             [targets.A]\npath = \"~/a\"\nsources = [\"other\"]\n\n\
             [sources.other]\ngit = \"h\"\n";
        assert_eq!(
            result.main, expected_main,
            "the [sources.dotfiles] table must be removed in place while [targets.A], \
             [sources.other], and all blank lines survive verbatim; [targets.A].sources goes \
             from [\"dotfiles\", \"other\"] to [\"other\"] without reformatting the rest"
        );

        let expected_local = "version = 1\n\n\
             [targets.B]\npath = \"~/b\"\nsources = []\n";
        assert_eq!(
            result.local, expected_local,
            "dotfiles was the only entry in [targets.B].sources in phora.local.toml, so the array \
             becomes empty (scrubbed, not deleted), every surrounding byte preserved"
        );
    }

    #[test]
    fn remove_source_unknown_name_errors() {
        let main = "version = 1\n\n[sources.foo]\ngit = \"g\"\n";
        let local = "version = 1\n";
        let err = remove_source(main, local, "ghost").expect_err("unknown source must error");
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("ghost")),
            "removing an absent source must Err mentioning the name"
        );
    }

    // 4 + 5. bind creates/appends a target's `sources` list.

    const NO_KEY_TARGET: &str = "version = 1\n\n[targets.t]\npath = \"~/x\"\n\n\
         [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n[sources.c]\ngit = \"i\"\n";

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn bind_to_no_key_target_creates_list() {
        let result = bind(NO_KEY_TARGET, "t", &names(&["a"]))
            .expect("binding to a no-key target must succeed, creating sources = [\"a\"]");

        let expected = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n[sources.c]\ngit = \"i\"\n";
        assert_eq!(
            result.text, expected,
            "binding to a no-key target must materialize sources = [\"a\"] in place, \
             preserving every other byte"
        );
    }

    #[test]
    fn bind_to_explicit_target_appends_stable_order() {
        let base = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        let result = bind(base, "t", &names(&["b"])).expect("bind to explicit target");

        let expected = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\", \"b\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        assert_eq!(
            result.text, expected,
            "binding to an explicit target appends b after the existing a, in given order"
        );
    }

    #[test]
    fn bind_dedups_without_reordering() {
        let base = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\", \"b\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        let result = bind(base, "t", &names(&["a"]))
            .expect("re-binding an existing source is a no-op on the list");

        let expected = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\", \"b\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        assert_eq!(
            result.text, expected,
            "binding a source already present must not duplicate it nor reorder the array"
        );
    }

    #[test]
    fn bind_to_target_with_non_string_source_entry_errors() {
        let base = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [{ name = \"a\" }]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        let err = bind(base, "t", &names(&["b"])).expect_err(
            "a non-string entry in an existing sources array must error, not be dropped",
        );
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("non-string")),
            "the error must report the non-string entry rather than silently dropping it"
        );
    }

    #[test]
    fn bind_to_missing_target_errors() {
        let err = bind(NO_KEY_TARGET, "nope", &names(&["a"]))
            .expect_err("binding to a non-existent target must error");
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("nope")),
            "a missing target table must Err mentioning the target name"
        );
    }

    // 6. unbind tombstone semantics.

    #[test]
    fn unbind_last_entry_writes_empty_array_and_tombstones() {
        let base = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\"]\n\n\
             [sources.a]\ngit = \"g\"\n";
        let result = unbind(base, "t", &names(&["a"])).expect("unbind last source");

        let expected = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = []\n\n\
             [sources.a]\ngit = \"g\"\n";
        assert_eq!(
            result.text, expected,
            "removing the last entry writes sources = [] and keeps the key, never deleting it"
        );
        assert!(
            result.tombstoned,
            "an emptied sources array must report tombstoned=true"
        );
    }

    #[test]
    fn unbind_one_of_several_keeps_remaining_no_tombstone() {
        let base = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\", \"b\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        let result = unbind(base, "t", &names(&["a"])).expect("unbind one of two");

        let expected = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"b\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        assert_eq!(
            result.text, expected,
            "unbinding one of several leaves the rest, preserving order"
        );
        assert!(
            !result.tombstoned,
            "a non-empty remaining array must report tombstoned=false"
        );
    }

    #[test]
    fn unbind_from_no_key_target_errors() {
        let err = unbind(NO_KEY_TARGET, "t", &names(&["a"]))
            .expect_err("unbind from a no-key target must error");
        assert!(
            matches!(err, Error::Config(_)),
            "a target with no `sources` key cannot be unbound from"
        );
    }

    // 7. validate_source_references.

    #[test]
    #[allow(
        clippy::single_char_pattern,
        reason = "assertion text is fixed by the spec and must not be altered"
    )]
    fn validate_source_references_flags_dangling_pair() {
        let merged = Config::parse(
            "version = 1\n\n[sources.real]\ngit = \"g\"\n\n\
             [targets.T]\npath = \"~/t\"\nsources = [\"ghost\"]\n",
        )
        .expect("config with a dangling reference still parses");

        let err = validate_source_references(&merged)
            .expect_err("a dangling [targets.T].sources entry must be rejected");
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("T") && msg.contains("ghost")),
            "the error must name both the target T and the dangling source ghost"
        );
    }

    #[test]
    fn validate_source_references_passes_clean_config() {
        let merged = Config::parse(
            "version = 1\n\n[sources.real]\ngit = \"g\"\n\n\
             [targets.T]\npath = \"~/t\"\nsources = [\"real\"]\n",
        )
        .expect("clean config parses");

        validate_source_references(&merged)
            .expect("every reference resolves, so validation must pass");
    }
}
