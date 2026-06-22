//! Text-level `toml_edit` mutators over `phora.toml` / `phora.local.toml`.
//!
//! These preserve formatting and comments byte-for-byte on untouched regions.
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value, value};

use super::add::AddTarget;
use crate::config::Config;
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

fn string_array(items: &[String]) -> Array {
    let mut array = Array::new();
    for item in items {
        array.push(item.as_str());
    }
    array
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
    if !source.include.is_empty() {
        table["include"] = value(string_array(&source.include));
    }
    if !source.exclude.is_empty() {
        table["exclude"] = value(string_array(&source.exclude));
    }

    ensure_sources_table(&mut doc);
    doc["sources"][name] = Item::Table(table);
    Ok(doc.to_string())
}

/// Write `root` onto every named source's `[sources.<name>]` table. Source
/// selection is source-owned, so a bind's `--root` lands here, never on the
/// binding.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if `doc_text` is not valid TOML, or
/// if a named source has no `[sources.<name>]` table in this document — the
/// source is declared in the other config file, so writing here would silently
/// drop the requested `root`.
pub fn set_source_roots(doc_text: &str, names: &[String], root: &str) -> Result<String> {
    let mut doc = parse_doc(doc_text)?;
    for name in names {
        let table = doc
            .get_mut("sources")
            .and_then(Item::as_table_like_mut)
            .and_then(|sources| sources.get_mut(name))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                Error::Config(format!(
                    "cannot set `root` on source `{name}`: it is not declared in this config file \
                     (source selection is source-owned; declare the source here, or set its `root` \
                     where the source is defined rather than via a `--local` bind)"
                ))
            })?;
        table.insert("root", value(root));
    }
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

fn keyed_effective_source<'a>(key: &'a str, item: &'a Item) -> &'a str {
    item.get("source").and_then(Item::as_str).unwrap_or(key)
}

fn scrub_target_bindings(doc: &mut DocumentMut, name: &str) -> Result<()> {
    let Some(targets) = doc.get_mut("targets").and_then(Item::as_table_like_mut) else {
        return Ok(());
    };
    for (target_name, target) in targets.iter_mut() {
        let Some(sources) = target.get_mut("sources") else {
            continue;
        };
        if let Some(array) = sources.as_value_mut().and_then(Value::as_array_mut) {
            if array.iter().any(|v| v.as_str().is_none()) {
                return Err(legacy_array_error(target_name.get()));
            }
            let before = array.len();
            array.retain(|v| v.as_str() != Some(name));
            if array.len() != before {
                normalize_array_decor(array);
            }
        } else if let Some(table) = sources.as_table_like_mut() {
            let doomed: Vec<String> = table
                .iter()
                .filter(|(key, item)| keyed_effective_source(key, item) == name)
                .map(|(key, _)| key.to_owned())
                .collect();
            for key in doomed {
                table.remove(&key);
            }
        }
    }
    Ok(())
}

fn remove_source_table(doc: &mut DocumentMut, name: &str) -> bool {
    doc.get_mut("sources")
        .and_then(Item::as_table_like_mut)
        .is_some_and(|sources| sources.remove(name).is_some())
}

/// Remove `[sources.<name>]` from BOTH texts and scrub every `[targets.*].sources`
/// binding whose effective source is `<name>` (matched by source, not by key).
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

    scrub_target_bindings(&mut main, name)?;
    scrub_target_bindings(&mut local, name)?;

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

fn target_sources_item<'a>(doc: &'a mut DocumentMut, target: &str) -> Option<&'a mut Item> {
    doc.get_mut("targets")?
        .as_table_like_mut()?
        .get_mut(target)?
        .as_table_mut()?
        .get_mut("sources")
}

/// One `--take` entry. Its TOML form mirrors the config `take` construct's
/// untagged `String | { src = dest }` so emitted bindings re-parse through
/// [`crate::config::Binding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeArg {
    Pattern(String),
    Rename { src: String, dest: String },
}

impl TakeArg {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.split_once('=') {
            Some((src, dest)) => TakeArg::Rename {
                src: src.to_owned(),
                dest: dest.to_owned(),
            },
            None => TakeArg::Pattern(raw.to_owned()),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            TakeArg::Pattern(p) => p.as_str().into(),
            TakeArg::Rename { src, dest } => {
                let mut table = InlineTable::new();
                table.insert(src, dest.as_str().into());
                Value::InlineTable(table)
            }
        }
    }
}

/// Per-binding refinement carried by `bind`/`add --to`: an optional `as`
/// identity, optional ref pins (`branch`/`tag`/`rev`), and a binding-level
/// `take` array. `root` is source-owned; it is carried here only to route to
/// the SOURCE table, never written onto the binding.
#[derive(Debug, Default)]
pub struct BindRefinement {
    pub r#as: Option<String>,
    pub root: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub take: Vec<TakeArg>,
}

impl BindRefinement {
    pub(super) fn is_bare(&self) -> bool {
        self.r#as.is_none()
            && self.branch.is_none()
            && self.tag.is_none()
            && self.rev.is_none()
            && self.take.is_empty()
    }
}

#[derive(Debug)]
pub struct BindResult {
    pub text: String,
    pub changed: bool,
}

fn keyed_binding_value(source: &str, identity: &str, refinement: &BindRefinement) -> Value {
    let mut table = InlineTable::new();
    if source != identity {
        table.insert("source", source.into());
    }
    if let Some(branch) = &refinement.branch {
        table.insert("branch", branch.as_str().into());
    }
    if let Some(tag) = &refinement.tag {
        table.insert("tag", tag.as_str().into());
    }
    if let Some(rev) = &refinement.rev {
        table.insert("rev", rev.as_str().into());
    }
    if !refinement.take.is_empty() {
        let mut take = Array::new();
        for entry in &refinement.take {
            take.push(entry.to_value());
        }
        table.insert("take", Value::Array(take));
    }
    Value::InlineTable(table)
}

fn list_to_keyed_table(array: &Array, target: &str) -> Result<Table> {
    let mut table = Table::new();
    table.set_implicit(false);
    for element in array {
        let name = element.as_str().ok_or_else(|| legacy_array_error(target))?;
        if table.contains_key(name) {
            return Err(duplicate_source_error(target, name));
        }
        table.insert(name, Item::Value(Value::InlineTable(InlineTable::new())));
    }
    Ok(table)
}

fn legacy_array_error(target: &str) -> Error {
    Error::Config(format!(
        "target `{target}`: table entries in a `sources` list are no longer supported; \
         use a keyed `[targets.{target}.sources]` table instead"
    ))
}

fn duplicate_source_error(target: &str, name: &str) -> Error {
    Error::Config(format!(
        "target `{target}`: duplicate source `{name}` in the `sources` list"
    ))
}

fn append_bare_to_list(array: &mut Array, source: &str) -> bool {
    if array.iter().any(|b| b.as_str() == Some(source)) {
        return false;
    }
    array.push(source);
    normalize_array_decor(array);
    true
}

#[derive(Clone, Copy)]
enum UpsertMode {
    SkipIfPresent,
    Overwrite,
}

fn keyed_value_eq(a: &Value, b: &Value) -> bool {
    fn content(v: &Value) -> Option<toml::Value> {
        format!("x = {v}").parse::<toml::Table>().ok()?.remove("x")
    }
    content(a) == content(b)
}

fn upsert_keyed_entry(
    table: &mut dyn toml_edit::TableLike,
    identity: &str,
    value: Value,
    mode: UpsertMode,
) -> bool {
    if let Some(existing) = table.get(identity) {
        match mode {
            UpsertMode::SkipIfPresent => return false,
            UpsertMode::Overwrite => {
                if existing
                    .as_value()
                    .is_some_and(|e| keyed_value_eq(e, &value))
                {
                    return false;
                }
            }
        }
    }
    table.insert(identity, Item::Value(value));
    true
}

/// Tolerates an undefined source (it may live in the sibling file; the caller's
/// merged-view guard catches a truly dangling one); other rejections propagate.
fn validate_edited(text: &str) -> Result<()> {
    match Config::parse(text)?.validate() {
        Err(Error::Config(msg)) if msg.contains("undefined source") => Ok(()),
        other => other,
    }
}

/// Bind `sources` into `[targets.<target>].sources`. A bare bind keeps the flat
/// `sources = [...]` list; the first `--as` alias or refinement promotes it to a
/// keyed `[targets.<t>.sources]` table, never the reverse.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if `doc_text` is not valid TOML, the
/// target table does not exist, `--as` is paired with more than one source
/// (ambiguous identity), or the edited document would fail [`Config::validate`]
/// for a reason other than an undefined source (left to the caller's merged
/// guard).
pub fn bind(
    doc_text: &str,
    target: &str,
    sources: &[String],
    refinement: &BindRefinement,
) -> Result<BindResult> {
    if refinement.r#as.is_some() && sources.len() > 1 {
        return Err(Error::Config(
            "`--as` sets a single binding identity and cannot apply to multiple sources".to_owned(),
        ));
    }

    let mut doc = parse_doc(doc_text)?;
    {
        let table = doc
            .get_mut("targets")
            .and_then(Item::as_table_like_mut)
            .and_then(|targets| targets.get_mut(target))
            .and_then(Item::as_table_mut)
            .ok_or_else(|| Error::Config(format!("target `{target}` is not defined")))?;
        if !table.contains_key("sources") {
            table["sources"] = value(Array::new());
        }
    }

    if !refinement.is_bare() {
        promote_sources_to_table(&mut doc, target)?;
    }

    let mut changed = false;
    let item = target_sources_item(&mut doc, target)
        .ok_or_else(|| Error::Config(format!("`{target}.sources` is missing")))?;
    for source in sources {
        let identity = refinement.r#as.as_deref().unwrap_or(source);
        if let Some(array) = item.as_value_mut().and_then(Value::as_array_mut) {
            changed |= append_bare_to_list(array, source);
        } else if let Some(table) = item.as_table_like_mut() {
            let value = keyed_binding_value(source, identity, refinement);
            let mode = if refinement.is_bare() {
                UpsertMode::SkipIfPresent
            } else {
                UpsertMode::Overwrite
            };
            changed |= upsert_keyed_entry(table, identity, value, mode);
        } else {
            return Err(Error::Config(format!(
                "`{target}.sources` is not a list or table"
            )));
        }
    }

    let text = doc.to_string();
    validate_edited(&text)?;

    Ok(BindResult { text, changed })
}

fn promote_sources_to_table(doc: &mut DocumentMut, target: &str) -> Result<()> {
    let item = target_sources_item(doc, target)
        .ok_or_else(|| Error::Config(format!("`{target}.sources` is missing")))?;
    if let Some(array) = item.as_value().and_then(Value::as_array) {
        *item = Item::Table(list_to_keyed_table(array, target)?);
    }
    Ok(())
}

#[derive(Debug)]
pub struct UnbindResult {
    pub text: String,
    pub tombstoned: bool,
}

/// Remove every binding from `[targets.<target>].sources` whose identity is in
/// `identities`. Never demotes a keyed table back to a list.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] if the target has no `sources` list.
pub fn unbind(doc_text: &str, target: &str, identities: &[String]) -> Result<UnbindResult> {
    let mut doc = parse_doc(doc_text)?;
    let item = target_sources_item(&mut doc, target).ok_or_else(|| {
        Error::Config(format!(
            "target `{target}` has no `sources` list; nothing to unbind"
        ))
    })?;

    let tombstoned = if let Some(array) = item.as_value_mut().and_then(Value::as_array_mut) {
        array.retain(|b| {
            b.as_str()
                .is_none_or(|id| !identities.iter().any(|i| i == id))
        });
        normalize_array_decor(array);
        array.is_empty()
    } else if let Some(table) = item.as_table_like_mut() {
        for identity in identities {
            table.remove(identity);
        }
        table.is_empty()
    } else {
        return Err(Error::Config(format!(
            "target `{target}` has no `sources` list; nothing to unbind"
        )));
    };

    Ok(UnbindResult {
        text: doc.to_string(),
        tombstoned,
    })
}

/// Pre-write guard: every `[targets.*].sources` binding resolves to an existing
/// source by its underlying source name.
///
/// # Errors
///
/// Returns [`crate::error::Error::Config`] naming the dangling (target, source).
pub fn validate_source_references(merged: &Config) -> Result<()> {
    for (target_name, target) in &merged.targets {
        for (identity, binding) in target.sources.iter().flatten() {
            let effective = binding.effective_source(identity);
            if !merged.sources.contains_key(effective) {
                return Err(Error::Config(format!(
                    "target `{target_name}` references undefined source `{effective}`"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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
            include: Vec::new(),
            exclude: Vec::new(),
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
            include: Vec::new(),
            exclude: Vec::new(),
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
            include: Vec::new(),
            exclude: Vec::new(),
        }
    }

    fn bare() -> BindRefinement {
        BindRefinement::default()
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
    fn upsert_source_emits_include_exclude_root_on_the_source_and_round_trips() {
        let mut source = lit_source("https://github.com/me/dots.git");
        source.include = vec!["skills/**".to_owned(), "*.lua".to_owned()];
        source.exclude = vec!["skills/private/**".to_owned()];
        source.root = Some("nvim".to_owned());

        let out = upsert_source("version = 1\n", "dots", &source, None, None, None)
            .expect("upsert with source-owned include/exclude/root");

        let cfg = Config::parse(&out).expect("the rooted, scoped source re-parses through Config");
        let parsed = &cfg.sources["dots"];
        assert_eq!(
            parsed.root.as_deref(),
            Some(Path::new("nvim")),
            "`--root` must land on `[sources.dots].root`, got:\n{out}"
        );
        assert_eq!(
            parsed.include.as_deref(),
            Some(&["skills/**".to_owned(), "*.lua".to_owned()][..]),
            "repeatable `--include` must emit every pattern on the SOURCE table, got:\n{out}"
        );
        assert_eq!(
            parsed.exclude.as_deref(),
            Some(&["skills/private/**".to_owned()][..]),
            "`--exclude` must emit on the SOURCE table, got:\n{out}"
        );
        assert!(
            !out.contains("[targets."),
            "include/exclude/root are source-owned: no binding table may be written, got:\n{out}"
        );
    }

    #[test]
    fn upsert_source_omits_include_exclude_when_empty() {
        let out = upsert_source(
            "version = 1\n",
            "dots",
            &lit_source("https://github.com/me/dots.git"),
            None,
            None,
            None,
        )
        .expect("upsert with no include/exclude");
        assert!(
            !out.contains("include") && !out.contains("exclude"),
            "an empty include/exclude must emit no key, got:\n{out}"
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
    fn remove_source_scrubs_aliased_table_binding_by_underlying_source() {
        let main = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.A]\npath = \"~/a\"\n\n[targets.A.sources]\ndots = { source = \"dotfiles\" }\n";
        let local = "version = 1\n";

        let result = remove_source(main, local, "dotfiles").expect("remove dotfiles");

        assert!(
            !result.main.contains("dotfiles"),
            "an aliased table binding whose `source` is the removed name must be scrubbed, \
             not orphaned: {}",
            result.main
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
        let result = bind(NO_KEY_TARGET, "t", &names(&["a"]), &bare())
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
        let result = bind(base, "t", &names(&["b"]), &bare()).expect("bind to explicit target");

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
        let result = bind(base, "t", &names(&["a"]), &bare())
            .expect("re-binding an existing source is a no-op on the list");

        let expected = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"a\", \"b\"]\n\n\
             [sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n";
        assert_eq!(
            result.text, expected,
            "binding a source already present must not duplicate it nor reorder the array"
        );
    }

    #[test]
    fn bind_to_missing_target_errors() {
        let err = bind(NO_KEY_TARGET, "nope", &names(&["a"]), &bare())
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

    // 7. Refinement-aware bind/unbind by identity.

    #[test]
    fn bind_with_refinement_writes_table_keyed_by_identity() {
        let base = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let refinement = BindRefinement {
            r#as: Some("dots".to_owned()),
            root: Some("nvim".to_owned()),
            ..BindRefinement::default()
        };
        let result =
            bind(base, "t", &names(&["dotfiles"]), &refinement).expect("refined bind succeeds");
        assert!(result.changed, "a fresh refined bind changes the bindings");

        let cfg = Config::parse(&result.text).expect("bind output parses");
        let target = &cfg.targets["t"];
        let bindings = target.sources.as_ref().unwrap();
        let (identity, binding) = bindings.iter().next().unwrap();
        assert_eq!(identity, "dots");
        assert_eq!(binding.effective_source(identity), "dotfiles");
    }

    #[test]
    fn bind_as_with_multiple_sources_errors() {
        let base = "version = 1\n\n[sources.a]\ngit = \"g\"\n\n[sources.b]\ngit = \"h\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let refinement = BindRefinement {
            r#as: Some("x".to_owned()),
            ..BindRefinement::default()
        };
        let err = bind(base, "t", &names(&["a", "b"]), &refinement)
            .expect_err("`--as` with multiple sources is ambiguous");
        assert!(matches!(err, Error::Config(msg) if msg.contains("--as")));
    }

    #[test]
    fn bind_reads_keyed_table_and_dedups_by_identity() {
        let base = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n[sources.a]\ngit = \"h\"\n\n\
             [targets.t]\npath = \"~/x\"\n\n\
             [targets.t.sources]\na = {}\ndots = { source = \"dotfiles\" }\n";
        let result = bind(base, "t", &names(&["a"]), &bare())
            .expect("re-binding a bare source in a keyed table is a no-op");
        assert!(
            !result.changed,
            "binding an already-present identity over a keyed table changes nothing"
        );
    }

    #[test]
    fn bind_as_with_unsafe_identity_is_rejected() {
        let base = "version = 1\n\n[sources.real]\ngit = \"g\"\n\n[targets.t]\npath = \"~/x\"\n";
        let refinement = BindRefinement {
            r#as: Some("../evil".to_owned()),
            ..BindRefinement::default()
        };
        let err = bind(base, "t", &names(&["real"]), &refinement)
            .expect_err("an `--as` identity that escapes the target dir must be rejected");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("../evil"),
                "the rejection must name the unsafe identity, got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn refined_rebind_with_identical_entry_reports_unchanged() {
        let base = "version = 1\n\n[sources.fzf]\ngit = \"g\"\n\n[targets.t]\npath = \"~/x\"\n";
        let refinement = BindRefinement {
            tag: Some("v1".to_owned()),
            ..BindRefinement::default()
        };
        let first = bind(base, "t", &names(&["fzf"]), &refinement).expect("first refined bind");
        assert!(first.changed, "the first refined bind changes the document");
        let second =
            bind(&first.text, "t", &names(&["fzf"]), &refinement).expect("re-bind identical entry");
        assert!(
            !second.changed,
            "re-binding a byte-identical refined entry must report unchanged"
        );
        assert_eq!(
            second.text, first.text,
            "an unchanged re-bind must not rewrite the document"
        );
    }

    #[test]
    fn promoting_a_duplicate_flat_list_reports_duplicate_not_legacy() {
        let base = "version = 1\n\n[sources.a]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"a\", \"a\"]\n";
        let refinement = BindRefinement {
            take: vec![TakeArg::Pattern("skills/".to_owned())],
            ..BindRefinement::default()
        };
        let err = bind(base, "t", &names(&["a"]), &refinement)
            .expect_err("a duplicate flat list cannot promote to a keyed table");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("duplicate source") && !msg.contains("no longer supported"),
                "the error must report the duplicate, not the array-of-tables migration hint, \
                 got: {msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn text_and_dto_layers_agree_on_effective_source() {
        use crate::config::Binding;

        let diverged: DocumentMut = "alias = { source = \"real\" }\n".parse().unwrap();
        let item = diverged.as_table().get("alias").expect("alias entry");
        let dto: Binding = toml::from_str("source = \"real\"").expect("divergent binding parses");
        assert_eq!(
            keyed_effective_source("alias", item),
            dto.effective_source("alias")
        );

        let bare_doc: DocumentMut = "real = {}\n".parse().unwrap();
        let bare_item = bare_doc.as_table().get("real").expect("bare entry");
        let bare_dto: Binding = toml::from_str("").expect("bare binding parses");
        assert_eq!(
            keyed_effective_source("real", bare_item),
            bare_dto.effective_source("real")
        );
    }

    #[test]
    fn unbind_removes_aliased_entry_by_identity() {
        let base = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\n\n\
             [targets.t.sources]\ndots = { source = \"dotfiles\" }\n";
        let result = unbind(base, "t", &names(&["dots"])).expect("unbind by alias identity");
        assert!(result.tombstoned, "removing the only entry tombstones");
        let cfg = Config::parse(&result.text).expect("output parses");
        assert!(cfg.targets["t"].sources.as_ref().unwrap().is_empty());
    }

    // PTV-006: binding-level ref flags (--branch/--tag/--rev) write a TABLE entry.

    fn refined_binding<'a>(cfg: &'a Config, target: &str) -> (&'a str, &'a crate::config::Binding) {
        let bindings = cfg.targets[target]
            .sources
            .as_ref()
            .expect("target has a sources table");
        bindings
            .iter()
            .next()
            .map(|(identity, binding)| (identity.as_str(), binding))
            .expect("at least one binding")
    }

    #[test]
    fn take_arg_parse_splits_rename_on_first_equals_else_pattern() {
        assert_eq!(
            TakeArg::parse("skills/"),
            TakeArg::Pattern("skills/".to_owned()),
            "a value with no `=` is a verbatim pattern"
        );
        assert_eq!(
            TakeArg::parse("a/X.md=a/x.md"),
            TakeArg::Rename {
                src: "a/X.md".to_owned(),
                dest: "a/x.md".to_owned(),
            },
            "a `src=dest` value splits on the first `=` into a rename"
        );
        assert_eq!(
            TakeArg::parse("k=v=w"),
            TakeArg::Rename {
                src: "k".to_owned(),
                dest: "v=w".to_owned(),
            },
            "only the first `=` splits; later ones belong to the destination"
        );
    }

    #[test]
    fn bind_take_emits_array_that_round_trips_through_binding() {
        let base = "version = 1\n\n[sources.skills]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let refinement = BindRefinement {
            take: vec![
                TakeArg::Pattern("skills/gestalt/skill.md".to_owned()),
                TakeArg::Pattern("skills/**".to_owned()),
                TakeArg::Rename {
                    src: "a/X.md".to_owned(),
                    dest: "a/x.md".to_owned(),
                },
            ],
            ..BindRefinement::default()
        };
        let result = bind(base, "t", &names(&["skills"]), &refinement)
            .expect("a take-only bind promotes and succeeds");

        let cfg = Config::parse(&result.text).expect("emitted take re-parses through Config");
        let binding = &cfg.targets["t"].sources.as_ref().unwrap()["skills"];
        let take = binding
            .take
            .as_deref()
            .expect("the emitted binding carries a `take` array");
        assert_eq!(
            take.len(),
            3,
            "all three take entries emit and re-parse, got:\n{}",
            result.text
        );

        let resolved = cfg.targets["t"].resolve_sources(&cfg.sources);
        let skills = resolved
            .iter()
            .find(|b| b.identity == "skills")
            .expect("the skills binding resolves");
        let renames: Vec<(&str, &str)> = skills.renames().collect();
        assert_eq!(
            renames,
            vec![("a/X.md", "a/x.md")],
            "only the `src=dest` arg re-parses as a rename; the two plain patterns are leaves, \
             got:\n{}",
            result.text
        );
        assert!(
            result.text.contains("\"skills/gestalt/skill.md\"")
                && result.text.contains("\"skills/**\""),
            "both plain patterns emit verbatim as leaf strings, got:\n{}",
            result.text
        );
        assert!(
            result.text.contains("\"a/X.md\" = \"a/x.md\""),
            "the rename emits as a `{{ src = dest }}` inline table, got:\n{}",
            result.text
        );
    }

    #[test]
    fn bind_empty_take_is_bare_and_emits_no_take_key() {
        let refinement = BindRefinement::default();
        assert!(
            refinement.is_bare(),
            "an empty take list keeps the refinement bare"
        );

        let base = "version = 1\n\n[sources.skills]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let result =
            bind(base, "t", &names(&["skills"]), &refinement).expect("a bare bind succeeds");
        assert!(
            !result.text.contains("take"),
            "a refinement with no take entries must emit no `take` key, got:\n{}",
            result.text
        );
    }

    #[test]
    fn bind_tag_writes_table_binding_with_tag() {
        let base = "version = 1\n\n[sources.fzf]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let refinement = BindRefinement {
            r#as: Some("canary".to_owned()),
            tag: Some("v0.56.0".to_owned()),
            ..BindRefinement::default()
        };
        let result =
            bind(base, "t", &names(&["fzf"]), &refinement).expect("a tag-pinned bind succeeds");

        let cfg = Config::parse(&result.text).expect("bind output parses");
        let (identity, refined) = refined_binding(&cfg, "t");
        assert_eq!(identity, "canary");
        assert_eq!(refined.effective_source(identity), "fzf");
        assert_eq!(
            refined.tag.as_deref(),
            Some("v0.56.0"),
            "a `--tag` pin must be written into the binding table's `tag` field"
        );
    }

    #[test]
    fn bind_rev_writes_table_binding_with_rev() {
        let base = "version = 1\n\n[sources.fzf]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let refinement = BindRefinement {
            rev: Some("deadbeef".to_owned()),
            ..BindRefinement::default()
        };
        let result =
            bind(base, "t", &names(&["fzf"]), &refinement).expect("a rev-pinned bind succeeds");

        let cfg = Config::parse(&result.text).expect("bind output parses");
        let (identity, refined) = refined_binding(&cfg, "t");
        assert_eq!(refined.effective_source(identity), "fzf");
        assert_eq!(
            refined.rev.as_deref(),
            Some("deadbeef"),
            "a `--rev` pin must be written into the binding table's `rev` field"
        );
    }

    #[test]
    fn bind_ref_only_refinement_is_not_bare_and_writes_table() {
        let refinement = BindRefinement {
            branch: Some("develop".to_owned()),
            ..BindRefinement::default()
        };
        assert!(
            !refinement.is_bare(),
            "a refinement that pins only a branch must NOT be bare; a ref forces a table entry"
        );

        let base = "version = 1\n\n[sources.fzf]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let result = bind(base, "t", &names(&["fzf"]), &refinement)
            .expect("a branch-only refined bind succeeds");

        let cfg = Config::parse(&result.text).expect("bind output parses");
        let (_, refined) = refined_binding(&cfg, "t");
        assert_eq!(
            refined.branch.as_deref(),
            Some("develop"),
            "a branch-only refinement must emit a table entry carrying `branch`, not a bare string"
        );
    }

    #[test]
    fn bind_tag_on_url_source_errors() {
        let base = "version = 1\n\n[sources.fonts]\nurl = \"https://example.com/f.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = []\n";
        let refinement = BindRefinement {
            tag: Some("v1".to_owned()),
            ..BindRefinement::default()
        };
        let err = bind(base, "t", &names(&["fonts"]), &refinement)
            .expect_err("pinning a ref on a url source must be rejected at validate");
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn set_source_roots_writes_root_onto_the_source_table_not_the_binding() {
        let base = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"dotfiles\"]\n";
        let out = set_source_roots(base, &names(&["dotfiles"]), "nvim")
            .expect("writing a source root preserves valid toml");

        let cfg = Config::parse(&out).expect("the rooted source re-parses");
        assert_eq!(
            cfg.sources["dotfiles"].root.as_deref(),
            Some(Path::new("nvim")),
            "`--root` must land on `[sources.dotfiles].root`, never on the binding, got:\n{out}"
        );
        let binding = &cfg.targets["t"].sources.as_ref().unwrap()["dotfiles"];
        assert!(
            binding.take.is_none() && binding.source.is_none(),
            "the binding stays bare; root is source-owned, got:\n{out}"
        );
    }

    #[test]
    fn set_source_roots_errors_when_source_absent_from_this_file_not_silently_dropped() {
        let local_only = "version = 1\n\n[targets.t]\npath = \"~/x\"\nsources = [\"dotfiles\"]\n";
        let err = set_source_roots(local_only, &names(&["dotfiles"]), "nvim")
            .expect_err("a source missing from this file must error, never silently drop `root`");
        match err {
            Error::Config(msg) => assert!(
                msg.contains("dotfiles") && msg.contains("root"),
                "the error must name the source and `root`, got:\n{msg}"
            ),
            other => panic!("expected Error::Config, got {other:?}"),
        }
    }

    #[test]
    fn set_source_roots_on_url_source_makes_config_reject() {
        let base = "version = 1\n\n[sources.fonts]\nurl = \"https://example.com/f.tar.gz\"\n\n\
             [targets.t]\npath = \"~/x\"\nsources = [\"fonts\"]\n";
        let out = set_source_roots(base, &names(&["fonts"]), "sub")
            .expect("set_source_roots emits valid toml even for a url source");
        let err = Config::parse(&out)
            .expect_err("a url source carrying a root must be rejected by the config layer");
        assert!(matches!(err, Error::Config(_)));
    }

    // 8. validate_source_references.

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

    fn resolved_pairs(cfg: &Config, target: &str) -> Vec<(String, String)> {
        let t = &cfg.targets[target];
        t.resolve_sources(&cfg.sources)
            .into_iter()
            .map(|b| (b.identity.to_owned(), b.source.to_owned()))
            .collect()
    }

    fn target_sources_item(text: &str, target: &str) -> Item {
        let doc = text
            .parse::<DocumentMut>()
            .expect("emitted config must be syntactically valid toml");
        doc["targets"][target]["sources"].clone()
    }

    const TWO_BARE_SOURCES: &str = "version = 1\n\n[sources.a]\ngit = \"g\"\n\n\
         [sources.b]\ngit = \"h\"\n\n[targets.t]\npath = \"~/x\"\n";

    #[test]
    fn two_bare_binds_emit_flat_list_not_a_table() {
        let first = bind(TWO_BARE_SOURCES, "t", &names(&["a"]), &bare())
            .expect("first bare bind")
            .text;
        let result = bind(&first, "t", &names(&["b"]), &bare())
            .expect("second bare bind appends")
            .text;

        let sources = target_sources_item(&result, "t");
        let array = sources
            .as_array()
            .unwrap_or_else(|| panic!("two bare binds must stay a flat array, got:\n{result}"));
        let elems: Vec<&str> = array.iter().filter_map(Value::as_str).collect();
        assert_eq!(
            elems,
            vec!["a", "b"],
            "two bare binds must produce a flat list of string elements, got:\n{result}"
        );
        let cfg = Config::parse(&result).expect("flat-list output parses");
        assert_eq!(
            resolved_pairs(&cfg, "t"),
            vec![
                ("a".to_owned(), "a".to_owned()),
                ("b".to_owned(), "b".to_owned())
            ]
        );
    }

    #[test]
    fn aliased_bind_promotes_flat_list_to_keyed_table() {
        let flat = bind(TWO_BARE_SOURCES, "t", &names(&["a"]), &bare())
            .expect("seed a flat list with one bare element")
            .text;
        assert!(
            target_sources_item(&flat, "t").is_array(),
            "precondition: target holds a flat list, got:\n{flat}"
        );

        let refinement = BindRefinement {
            r#as: Some("alias".to_owned()),
            ..BindRefinement::default()
        };
        let result = bind(&flat, "t", &names(&["b"]), &refinement)
            .expect("an aliased bind over a flat list must succeed")
            .text;

        let sources = target_sources_item(&result, "t");
        assert!(
            sources.is_table_like(),
            "the first alias must promote the flat list to a keyed table, got:\n{result}"
        );
        assert!(
            !sources.is_array(),
            "promotion must NOT emit an inline-table array (001 rejects it on parse), got:\n{result}"
        );

        let cfg = Config::parse(&result)
            .expect("the promoted keyed table must re-parse as valid phora.toml");
        let mut pairs = resolved_pairs(&cfg, "t");
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("a".to_owned(), "a".to_owned()),
                ("alias".to_owned(), "b".to_owned()),
            ],
            "after promotion `a` stays bound bare and `alias` resolves to source `b`"
        );
    }

    #[test]
    fn samename_take_refinement_promotes_and_omits_redundant_source_key() {
        let flat = bind(TWO_BARE_SOURCES, "t", &names(&["a"]), &bare())
            .expect("seed a flat list")
            .text;
        let refinement = BindRefinement {
            take: vec![TakeArg::Pattern("skills/".to_owned())],
            ..BindRefinement::default()
        };
        let result = bind(&flat, "t", &names(&["b"]), &refinement)
            .expect("a same-name take refinement over a flat list must succeed")
            .text;

        let sources = target_sources_item(&result, "t");
        assert!(
            sources.is_table_like() && !sources.is_array(),
            "a take refinement must promote the flat list to a keyed table, got:\n{result}"
        );
        let b_entry = sources
            .get("b")
            .unwrap_or_else(|| panic!("refined entry must be keyed under `b`, got:\n{result}"));
        assert!(
            b_entry.get("source").is_none(),
            "a same-name refinement keyed by `b` must omit the redundant `source` field, \
             got:\n{result}"
        );
        assert!(
            b_entry.get("root").is_none(),
            "`root` is source-owned and must NEVER be written onto the binding, got:\n{result}"
        );

        let cfg = Config::parse(&result).expect("promoted same-name table re-parses");
        let binding = &cfg.targets["t"].sources.as_ref().unwrap()["b"];
        let take = binding
            .take
            .as_deref()
            .expect("the binding carries its emitted take");
        assert_eq!(
            take.len(),
            1,
            "the refinement's `--take skills/` must survive the promotion, got:\n{result}"
        );

        let mut pairs = resolved_pairs(&cfg, "t");
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("a".to_owned(), "a".to_owned()),
                ("b".to_owned(), "b".to_owned()),
            ],
            "the refined `b` entry's effective source is its own key `b`"
        );
    }

    #[test]
    fn keyed_table_never_auto_demotes_to_list_on_unbind() {
        let flat = bind(TWO_BARE_SOURCES, "t", &names(&["a"]), &bare())
            .expect("seed flat list")
            .text;
        let refinement = BindRefinement {
            r#as: Some("alias".to_owned()),
            ..BindRefinement::default()
        };
        let keyed = bind(&flat, "t", &names(&["b"]), &refinement)
            .expect("promote to keyed table")
            .text;
        assert!(
            target_sources_item(&keyed, "t").is_table_like(),
            "precondition: target is now a keyed table, got:\n{keyed}"
        );

        let result = unbind(&keyed, "t", &names(&["alias"]))
            .expect("unbinding the refined entry leaves only a bare-eligible `a`")
            .text;

        let sources = target_sources_item(&result, "t");
        assert!(
            sources.is_table_like() && !sources.is_array(),
            "removing the refined entry must KEEP the keyed-table form, never demote to a list, \
             got:\n{result}"
        );
        let cfg = Config::parse(&result).expect("post-unbind keyed table re-parses");
        assert_eq!(
            resolved_pairs(&cfg, "t"),
            vec![("a".to_owned(), "a".to_owned())]
        );
    }

    #[test]
    fn remove_source_scrubs_divergent_keyed_entry_by_source_field() {
        let main = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [sources.other]\ngit = \"h\"\n\n\
             [targets.A]\npath = \"~/a\"\n\n[targets.A.sources]\n\
             settings = { source = \"dotfiles\" }\nkeep = { source = \"other\" }\n";
        let local = "version = 1\n";

        let result = remove_source(main, local, "dotfiles").expect("remove dotfiles");

        let sources = target_sources_item(&result.main, "A");
        assert!(
            sources.get("settings").is_none(),
            "the keyed entry `settings = {{ source = dotfiles }}` must be scrubbed when \
             `dotfiles` is deleted (matched by effective source, not key), got:\n{}",
            result.main
        );
        assert!(
            sources.get("keep").is_some(),
            "the sibling `keep = {{ source = other }}` points at a surviving source and must \
             NOT be scrubbed, got:\n{}",
            result.main
        );
    }

    #[test]
    fn remove_source_scrubs_samename_keyed_entry_by_key() {
        let main = "version = 1\n\n[sources.loqui]\ngit = \"g\"\n\n\
             [sources.other]\ngit = \"h\"\n\n\
             [targets.A]\npath = \"~/a\"\n\n[targets.A.sources]\n\
             loqui = { root = \"x\" }\nkeep = { source = \"other\" }\n";
        let local = "version = 1\n";

        let result = remove_source(main, local, "loqui").expect("remove loqui");

        let sources = target_sources_item(&result.main, "A");
        assert!(
            sources.get("loqui").is_none(),
            "a same-name keyed entry whose key == the deleted source must be scrubbed, \
             got:\n{}",
            result.main
        );
        assert!(
            sources.get("keep").is_some(),
            "the sibling `keep = {{ source = other }}` must survive the targeted scrub, got:\n{}",
            result.main
        );
    }

    #[test]
    fn remove_source_scrubs_keyed_entry_in_local_doc() {
        let main =
            "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n[sources.other]\ngit = \"h\"\n";
        let local = "version = 1\n\n[targets.B]\npath = \"~/b\"\n\n\
             [targets.B.sources]\nsettings = { source = \"dotfiles\" }\n\
             keep = { source = \"other\" }\n";

        let result = remove_source(main, local, "dotfiles").expect("remove dotfiles");

        let sources = target_sources_item(&result.local, "B");
        assert!(
            sources.get("settings").is_none(),
            "a keyed binding in phora.local.toml must be scrubbed by effective source too, \
             got:\n{}",
            result.local
        );
        assert!(
            sources.get("keep").is_some(),
            "the sibling `keep = {{ source = other }}` in phora.local.toml must survive, got:\n{}",
            result.local
        );
    }

    #[test]
    fn bind_then_unbind_to_empty_round_trips_through_parse() {
        let bound = bind(TWO_BARE_SOURCES, "t", &names(&["a"]), &bare())
            .expect("bind a fresh bare source")
            .text;
        let result = unbind(&bound, "t", &names(&["a"])).expect("unbind the only source");

        assert!(
            result.tombstoned,
            "unbinding the last source must report a tombstone"
        );
        let sources = target_sources_item(&result.text, "t");
        let array = sources.as_array().unwrap_or_else(|| {
            panic!(
                "a bare-list target stays a list at empty, got:\n{}",
                result.text
            )
        });
        assert!(
            array.is_empty(),
            "the tombstone must leave an empty `sources = []`, got:\n{}",
            result.text
        );
        let cfg = Config::parse(&result.text).expect("the tombstone state must re-parse Ok");
        assert!(
            cfg.targets["t"].resolve_sources(&cfg.sources).is_empty(),
            "a re-parsed tombstone resolves to zero bindings, got:\n{}",
            result.text
        );
    }

    #[test]
    fn remove_source_prefers_source_field_over_colliding_key() {
        let main = "version = 1\n\n[sources.loqui]\ngit = \"g\"\n\n\
             [sources.dotfiles]\ngit = \"h\"\n\n\
             [targets.A]\npath = \"~/a\"\n\n[targets.A.sources]\n\
             dotfiles = { source = \"loqui\" }\n";
        let local = "version = 1\n";

        let retained = remove_source(main, local, "dotfiles").expect("remove source dotfiles");
        let sources = target_sources_item(&retained.main, "A");
        assert!(
            sources.get("dotfiles").is_some(),
            "the entry keyed `dotfiles` has effective source `loqui`; deleting source `dotfiles` \
             must RETAIN it — the scrub predicate matches by the `source` field, not the key, \
             got:\n{}",
            retained.main
        );

        let scrubbed = remove_source(main, local, "loqui").expect("remove source loqui");
        let sources = target_sources_item(&scrubbed.main, "A");
        assert!(
            sources.get("dotfiles").is_none(),
            "deleting the entry's effective source `loqui` must SCRUB the `dotfiles`-keyed entry, \
             got:\n{}",
            scrubbed.main
        );
    }

    #[test]
    fn refined_bind_over_legacy_array_of_tables_errors_not_lossy() {
        let base = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"dotfiles\", root = \".claude\" }]\n";
        let refinement = BindRefinement {
            r#as: Some("dots".to_owned()),
            ..BindRefinement::default()
        };
        let err = bind(base, "t", &names(&["dotfiles"]), &refinement)
            .expect_err("promoting a legacy array-of-tables must error, never drop the entry");
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("[targets.t.sources]")),
            "the rejection must name the keyed-table migration hint, not silently lose the binding"
        );
    }

    #[test]
    fn remove_source_on_legacy_array_of_tables_errors_with_hint() {
        let main = "version = 1\n\n[sources.dotfiles]\ngit = \"g\"\n\n\
             [targets.t]\npath = \"~/x\"\n\
             sources = [{ source = \"dotfiles\" }]\n";
        let err = remove_source(main, "", "dotfiles").expect_err(
            "removing a source bound via legacy array-of-tables must error, not dangle",
        );
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("[targets.t.sources]")),
            "the rejection must name the keyed-table migration hint, not leave a dangling binding"
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
