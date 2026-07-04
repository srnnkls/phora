//! `phora bind` / `phora unbind` verbs: validate names, check target existence,
//! and mutate a target's explicit `sources` list.

use std::path::Path;
use std::str::FromStr;

use toml_edit::DocumentMut;

use super::config_edit::{self, BindRefinement};
use super::{load_config_from, load_local_config, read_config_text, render, target_config_file};
use crate::config::merge_configs;
use crate::error::{Error, Result};
use crate::kernel::{SourceName, TargetName};

/// Error text for a target that is not defined: names the target and the
/// `phora target add <name> --path <path>` create hint, suffixed with `--local`
/// when the bind writes to the local config.
#[must_use]
pub fn missing_target_message(target: &str, local: bool) -> String {
    let local_flag = if local { " --local" } else { "" };
    format!(
        "target '{target}' does not exist\n  \
         create it with: phora target add {target} --path <path>{local_flag}"
    )
}

/// Error text for a source that is not defined: names the source and a create
/// hint (`phora add <url>` / `phora source add <url>`).
#[must_use]
pub fn missing_source_message(source: &str) -> String {
    format!(
        "source '{source}' does not exist\n  \
         create it with: phora add <url> --name {source}"
    )
}

/// Warning text when an unbind empties a target's `sources`: the target now
/// deploys nothing.
#[must_use]
pub fn unbind_tombstone_warning(target: &str) -> String {
    format!("target '{target}' now binds no sources and deploys nothing")
}

/// Reject a write whose merged view (the pending file plus its on-disk sibling)
/// would leave a dangling `[targets.*].sources` reference.
pub(super) fn guard_no_dangling_references(new_text: &str, local: bool) -> Result<()> {
    let (main_text, local_text) = if local {
        (read_config_text("phora.toml")?, new_text.to_owned())
    } else {
        (new_text.to_owned(), read_config_text("phora.local.toml")?)
    };
    validate_merged_references(&main_text, &local_text)
}

fn validate_merged_references(main_text: &str, local_text: &str) -> Result<()> {
    let main = crate::config::Config::parse(main_text)?;
    let local = crate::config::Config::parse(local_text)?;
    let merged = merge_configs(main, Some(local));
    config_edit::validate_source_references(&merged)
}

fn merged_config(cwd: &Path) -> Result<crate::config::Config> {
    let base = load_config_from(cwd)?;
    let local = load_local_config(cwd)?;
    Ok(merge_configs(base, local))
}

fn target_exists(text: &str, target: &str) -> Result<bool> {
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| Error::Config(format!("parse toml: {e}")))?;
    Ok(doc
        .get("targets")
        .and_then(toml_edit::Item::as_table_like)
        .is_some_and(|targets| targets.contains_key(target)))
}

pub(super) fn run_bind(
    sources: &[String],
    to: &str,
    local: bool,
    refinement: &BindRefinement,
) -> Result<()> {
    for source in sources {
        SourceName::from_str(source)?;
    }
    TargetName::from_str(to)?;

    let cwd = Path::new(".");
    let merged = merged_config(cwd)?;
    for source in sources {
        if !merged.sources.contains_key(source) {
            return Err(Error::Config(missing_source_message(source)));
        }
    }

    let Some(target) = merged.targets.get(to) else {
        return Err(Error::Config(missing_target_message(to, local)));
    };

    let file = target_config_file(local);
    let original = read_config_text(file)?;

    let mut text = original.clone();
    if !target_exists(&text, to)? {
        text = config_edit::upsert_target(&text, to, &target.path.to_string_lossy(), None)?;
    }
    if let Some(root) = refinement.root.as_deref() {
        text = config_edit::set_source_roots(&text, sources, root)?;
    }

    let result = config_edit::bind(&text, to, sources, refinement)?;
    if !result.changed && result.text == original {
        render::print_bind_unchanged(sources, to);
        return Ok(());
    }
    guard_no_dangling_references(&result.text, local)?;
    std::fs::write(file, &result.text)?;
    render::print_bound(sources, to);
    Ok(())
}

pub(super) fn run_unbind(sources: &[String], from: &str, local: bool) -> Result<()> {
    for source in sources {
        SourceName::from_str(source)?;
    }
    TargetName::from_str(from)?;

    let file = target_config_file(local);
    let text = read_config_text(file)?;

    if !target_exists(&text, from)? {
        return Err(Error::Config(missing_target_message(from, local)));
    }

    let result = config_edit::unbind(&text, from, sources)?;
    guard_no_dangling_references(&result.text, local)?;
    std::fs::write(file, &result.text)?;
    if result.tombstoned {
        render::warn_unbind_tombstone(from);
    } else {
        render::print_unbound(sources, from);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_rejects_post_mutation_text_with_dangling_reference() {
        let main = "version = 1\n\n[sources.real]\ngit = \"g\"\n\n\
             [targets.T]\npath = \"~/t\"\nsources = [\"ghost\"]\n";
        let err = validate_merged_references(main, "version = 1\n")
            .expect_err("a post-mutation text binding an undefined source must be rejected");
        assert!(
            matches!(err, Error::Config(msg) if msg.contains("`T`") && msg.contains("ghost")),
            "the pre-write guard must name the dangling (target, source) pair"
        );
    }

    #[test]
    fn guard_resolves_reference_defined_only_in_sibling_file() {
        let main = "version = 1\n\n[targets.T]\npath = \"~/t\"\nsources = [\"overlay\"]\n";
        let local = "version = 1\n\n[sources.overlay]\npath = \"/tmp/o\"\n";
        validate_merged_references(main, local)
            .expect("a source defined only in the sibling file still resolves in the merged view");
    }

    #[test]
    fn missing_target_message_names_target_and_create_hint() {
        let msg = missing_target_message("staging", false);
        assert!(msg.contains("staging"), "must name the target");
        assert!(
            msg.contains("does not exist"),
            "must state the target does not exist"
        );
        assert!(
            msg.contains("phora target add staging --path"),
            "must give the `phora target add <name> --path` create hint"
        );
        assert!(
            !msg.contains("--local"),
            "a non-local remedy must not suggest --local"
        );
        assert!(
            missing_target_message("staging", true).contains("--path <path> --local"),
            "a local remedy must suffix the create hint with --local"
        );
    }

    #[test]
    fn missing_source_message_names_source_and_create_hint() {
        let msg = missing_source_message("tools");
        assert!(msg.contains("tools"), "must name the source");
        assert!(
            msg.contains("phora add") || msg.contains("phora source add"),
            "must give a create hint pointing at `phora add`/`phora source add`"
        );
    }

    #[test]
    fn unbind_tombstone_warning_says_target_deploys_nothing() {
        let warn = unbind_tombstone_warning("claude");
        assert!(warn.contains("claude"), "must name the target");
        assert!(
            warn.to_lowercase().contains("nothing"),
            "must warn the target now deploys nothing"
        );
    }
}
