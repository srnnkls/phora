use super::*;
use crate::config::admit_transitive_hooks;
use crate::config::transitive::{FetchNode, Instance, TransitiveManifest};

fn composed_instance() -> Instance {
    let fetch = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:dead");
    Instance::new("root", "dep", "anchor", fetch)
}

fn raw_source(body: &str) -> Source {
    let toml = format!("version = 1\n\n[sources.s]\n{body}");
    toml::from_str::<Config>(&toml)
        .expect("raw source DTO deserializes")
        .sources
        .remove("s")
        .expect("source `s` present")
}

#[test]
fn source_without_transitive_defaults_to_flat() {
    let source = raw_source("git = \"https://github.com/me/x.git\"\n");
    assert!(
        !source.is_transitive(),
        "a source with no `transitive` key must keep today's flat behavior (false)"
    );
    assert_eq!(
        source.transitive, None,
        "the wire DTO must carry `transitive` absent (None), not a defaulted value"
    );
}

#[test]
fn source_with_transitive_true_activates() {
    let source = raw_source("git = \"https://github.com/me/x.git\"\ntransitive = true\n");
    assert!(
        source.is_transitive(),
        "`transitive = true` on a source must activate transitive resolution"
    );
}

#[test]
fn source_with_transitive_false_stays_flat() {
    let source = raw_source("git = \"https://github.com/me/x.git\"\ntransitive = false\n");
    assert!(
        !source.is_transitive(),
        "`transitive = false` must be flat, identical to the default"
    );
}

const DEP_MANIFEST: &str = r#"
version = 1
protocol = "https"

[sources.nvim]
git = "https://github.com/dep/nvim.git"

[targets.editor]
path = "nvim"
sources = ["nvim"]

[targets.editor.hooks]
on_change = "./install.sh"

[hooks]
post_sync = "echo consumer-owned-only"
"#;

#[test]
fn manifest_types_the_declarative_graph_fields() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST).expect("a dep phora.toml parses once");
    assert!(
        manifest.sources.contains_key("nvim"),
        "the manifest must type the dep's `[sources]` graph field"
    );
    assert!(
        manifest.targets.contains_key("editor"),
        "the manifest must type the dep's `[targets]` graph field"
    );
}

#[test]
fn manifest_retains_per_target_hooks_as_opaque_value_not_rejected() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST).expect(
        "a manifest carrying `[targets.X.hooks]` must NOT be rejected by deny_unknown_fields",
    );
    let hooks = manifest
        .hooks()
        .expect("per-target hooks must be retained out-of-band as an opaque value");
    let rendered = format!("{hooks:?}");
    assert!(
        rendered.contains("install.sh"),
        "hooks must be held as an uninterpreted toml::Value (still inspectable), got: {rendered}"
    );
}

#[test]
fn manifest_hooks_value_is_uninterpreted_toml() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST).expect("manifest parses");
    let hooks: &toml::Value = manifest
        .hooks()
        .expect("hooks retained for the later admission phase");
    let reserialized = toml::to_string(hooks)
        .expect("the opaque hooks payload must round-trip back to TOML, proving it is raw data");
    assert!(
        reserialized.contains("on_change") && reserialized.contains("./install.sh"),
        "the input hook command (`on_change = \"./install.sh\"`) must survive verbatim and \
         uninterpreted in the opaque payload — not parsed into HookCommand, not dropped; \
         re-serialized payload was: {reserialized}"
    );
}

#[test]
fn manifest_strips_global_post_sync_hook() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST).expect("manifest parses");
    let rendered = format!("{:?}", manifest.hooks());
    assert!(
        !rendered.contains("consumer-owned-only"),
        "a transitive global `[hooks] post_sync` must be stripped (consumer-owned only), got: {rendered}"
    );
}

#[test]
fn manifest_drops_trust_control_fields() {
    let with_trust = r#"
version = 1
trust = "all"
trusted_hooks = ["dep#editor"]
allow_hooks = true

[sources.nvim]
git = "https://github.com/dep/nvim.git"
"#;
    let without_trust = r#"
version = 1

[sources.nvim]
git = "https://github.com/dep/nvim.git"
"#;
    let with = TransitiveManifest::parse(with_trust)
        .expect("trust-control fields must be DROPPED (ignored/tolerated), never rejected by deny_unknown_fields");
    let without = TransitiveManifest::parse(without_trust).expect("baseline manifest parses");
    assert_eq!(
        format!("{with:?}"),
        format!("{without:?}"),
        "the parser must tolerate AND drop trust-control fields: a manifest carrying \
         trust/trusted_hooks/allow_hooks must type-out IDENTICALLY to the same manifest \
         without them, so no trust state can ride into admission"
    );
}

#[test]
fn fetch_node_dedups_a_diamond_to_one_fetch() {
    let left = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:deadbeef");
    let right = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:deadbeef");
    assert_eq!(
        left, right,
        "a diamond reaching the same (url, ref, digest) must dedup to ONE FetchNode"
    );
    let set: std::collections::HashSet<_> = [left, right].into_iter().collect();
    assert_eq!(
        set.len(),
        1,
        "FetchNode must hash-dedup the diamond to one fetch"
    );
}

#[test]
fn fetch_node_normalizes_equivalent_urls() {
    let scp = FetchNode::new("git@github.com:dep/x.git", "main", "blake3:dead");
    let https = FetchNode::new("https://github.com/dep/x", "main", "blake3:dead");
    assert_eq!(
        scp, https,
        "FetchNode identity must use the NORMALIZED url so equivalent forms dedup"
    );
}

#[test]
fn fetch_node_differs_on_digest() {
    let a = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:aaaa");
    let b = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:bbbb");
    assert_ne!(
        a, b,
        "FetchNode is (url, ref, DIGEST); a different digest is a different node"
    );
}

#[test]
fn instance_namespaces_distinctly_from_fetch_node() {
    let fetch = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:dead");
    let under_a = Instance::new("root", "deps", "anchor_a", fetch.clone());
    let under_b = Instance::new("root", "deps", "anchor_b", fetch.clone());
    assert_ne!(
        under_a, under_b,
        "the SAME fetched node mounted at two anchors must be two distinct Instances"
    );
    assert_eq!(
        under_a.fetch_node(),
        &fetch,
        "an Instance keys namespacing/hooks/paths but still references its shared FetchNode"
    );
}

#[test]
fn instance_distinguishes_parent_and_source_name() {
    let fetch = FetchNode::new("https://github.com/dep/x.git", "main", "blake3:dead");
    let from_root = Instance::new("root", "deps", "anchor", fetch.clone());
    let from_other = Instance::new("other-parent", "deps", "anchor", fetch.clone());
    assert_ne!(
        from_root, from_other,
        "Instance = (parent, source_name, anchor_target, fetch_node); a different parent is a different instance"
    );
}

const TRANSITIVE_DEP: &str = "version = 1\n\n[sources.tropos]\ngit = \"https://github.com/srnnkls/tropos.git\"\ntransitive = true\n\n";

fn merged(toml: &str) -> Config {
    merge_configs(Config::parse(toml).expect("config parses"), None)
}

#[test]
fn binding_a_transitive_source_lowers_into_an_import_with_the_default_offer() {
    let config = merged(&format!(
        "{TRANSITIVE_DEP}[targets.claude]\npath = \"~/.claude\"\nsources = [\"tropos\"]\n"
    ));
    let target = &config.targets["claude"];
    assert!(
        target.sources.iter().flatten().next().is_none(),
        "a transitive binding must leave `sources`, got: {:?}",
        target.sources
    );
    let imports = target
        .offer_bindings
        .as_deref()
        .expect("the binding became an import");
    assert_eq!(
        imports
            .iter()
            .map(|i| (i.identity.as_str(), i.source(), i.offer()))
            .collect::<Vec<_>>(),
        vec![("tropos", "tropos", "default")]
    );
    config
        .validate()
        .expect("a bare transitive binding validates");
}

#[test]
fn a_binding_offer_selects_a_named_offer_and_aliases_keep_identities_apart() {
    let config = merged(&format!(
        "{TRANSITIVE_DEP}[targets.claude]\npath = \"~/.claude\"\n\
         sources.tropos = {{ offer = \"fas\" }}\n\
         sources.skills = {{ source = \"tropos\" }}\n"
    ));
    let mut imports: Vec<(String, String, String)> = config.targets["claude"]
        .offer_bindings
        .iter()
        .flatten()
        .map(|i| {
            (
                i.identity.clone(),
                i.source().to_owned(),
                i.offer().to_owned(),
            )
        })
        .collect();
    imports.sort();
    assert_eq!(
        imports,
        vec![
            ("skills".into(), "tropos".into(), "default".into()),
            ("tropos".into(), "tropos".into(), "fas".into()),
        ]
    );
}

#[test]
fn lowering_is_idempotent() {
    let mut config = merged(&format!(
        "{TRANSITIVE_DEP}[targets.claude]\npath = \"~/.claude\"\nsources = [\"tropos\"]\n"
    ));
    config.lower_transitive();
    assert_eq!(
        config.targets["claude"]
            .offer_bindings
            .as_ref()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn a_transitive_flag_set_only_in_the_overlay_classifies_the_base_binding() {
    let base = Config::parse(
        "version = 1\n\n[sources.tropos]\ngit = \"https://github.com/srnnkls/tropos.git\"\n\n\
         [targets.claude]\npath = \"~/.claude\"\nsources.tropos = { offer = \"fas\" }\n",
    )
    .expect("base parses");
    let local = Config::parse("version = 1\n\n[sources.tropos]\ntransitive = true\n")
        .expect("local parses");
    let config = merge_configs(base, Some(local));
    config
        .validate()
        .expect("`offer` is legal once the overlay makes the source transitive");
    assert_eq!(
        config.targets["claude"]
            .offer_bindings
            .as_ref()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn offer_on_a_flat_binding_is_rejected() {
    let config = merged(
        "version = 1\n\n[sources.dotfiles]\ngit = \"https://github.com/srnnkls/tropos.git\"\n\n\
         [targets.claude]\npath = \"~/.claude\"\nsources.dotfiles = { offer = \"fas\" }\n",
    );
    let msg = config
        .validate()
        .expect_err("offer needs a transitive source")
        .to_string();
    assert!(
        msg.contains("dotfiles") && msg.contains("offer"),
        "got: {msg}"
    );
}

#[test]
fn template_on_a_transitive_binding_is_rejected() {
    let config = merged(&format!(
        "{TRANSITIVE_DEP}[targets.claude]\npath = \"~/.claude\"\nsources.tropos = {{ template = false }}\n"
    ));
    let msg = config
        .validate()
        .expect_err("template is flat-only")
        .to_string();
    assert!(
        msg.contains("tropos") && msg.contains("template"),
        "got: {msg}"
    );
}

#[test]
fn a_transitive_source_cannot_shape_its_own_offer() {
    for key in ["root = \"x\"", "include = [\"a\"]", "exclude = [\"b\"]"] {
        let config = merged(&format!(
            "version = 1\n\n[sources.tropos]\ngit = \"https://github.com/srnnkls/tropos.git\"\ntransitive = true\n{key}\n\n\
             [targets.claude]\npath = \"~/.claude\"\nsources = [\"tropos\"]\n"
        ));
        let msg = config
            .validate()
            .expect_err("the manifest owns the offers")
            .to_string();
        assert!(
            msg.contains("tropos") && msg.contains("offer"),
            "{key}: {msg}"
        );
    }
}

#[test]
fn a_transitive_source_no_target_binds_is_rejected() {
    let config = merged(TRANSITIVE_DEP);
    let msg = config
        .validate()
        .expect_err("an unbound transitive source is never resolved")
        .to_string();
    assert!(
        msg.contains("tropos") && msg.contains("transitive"),
        "got: {msg}"
    );
}

#[test]
fn the_imports_key_is_gone() {
    let err = Config::parse(&format!(
        "{TRANSITIVE_DEP}[targets.claude]\npath = \"~/.claude\"\nimports = [\"dep\"]\n"
    ))
    .expect_err("targets bind transitive sources through `sources`");
    assert!(err.to_string().contains("imports"), "got: {err}");
}

const OFFERS_MANIFEST: &str = r#"
[sources.loqui]
git = "https://github.com/me/loqui.git"

[sources.moira]
git = "https://github.com/me/moira.git"

[targets.claude]
path = "~/.claude"
sources = ["loqui"]

[targets.loqui]
path = "skills/loqui/reference/loqui"
sources = ["loqui"]

[targets.moira]
path = "rules/fas/moira"
sources = ["moira"]

[offers.default]
include = ["skills/**", "rules/fas/**"]

[offers.fas]
root = "rules/fas"
targets = ["moira"]
"#;

#[test]
fn the_default_offer_selects_every_offerable_target() {
    let manifest = TransitiveManifest::parse(OFFERS_MANIFEST).expect("manifest parses");
    let default = manifest.offer("default").expect("default offer");
    assert_eq!(
        default.targets.keys().collect::<Vec<_>>(),
        vec!["loqui", "moira"],
        "a target outside the repo is local config, never offered"
    );
    assert_eq!(
        default.targets["moira"].path,
        std::path::PathBuf::from("rules/fas/moira")
    );
}

#[test]
fn an_offer_root_re_roots_a_shared_target() {
    let manifest = TransitiveManifest::parse(OFFERS_MANIFEST).expect("manifest parses");
    let fas = manifest.offer("fas").expect("named offer");
    assert_eq!(fas.targets.keys().collect::<Vec<_>>(), vec!["moira"]);
    assert_eq!(fas.targets["moira"].path, std::path::PathBuf::from("moira"));
}

#[test]
fn an_undeclared_offer_names_the_declared_ones() {
    let manifest = TransitiveManifest::parse(OFFERS_MANIFEST).expect("manifest parses");
    let msg = manifest
        .offer("agents")
        .expect_err("undeclared offer")
        .to_string();
    assert!(
        msg.contains("agents") && msg.contains("default") && msg.contains("fas"),
        "got: {msg}"
    );
}

#[test]
fn the_default_offer_exists_undeclared() {
    let manifest = TransitiveManifest::parse(
        "[sources.gestalt]\ngit = \"https://github.com/srnnkls/gestalt.git\"\n\n[targets.gestalt]\npath = \"skills/gestalt\"\nsources = [\"gestalt\"]\n",
    )
    .expect("manifest parses");
    let default = manifest.offer("default").expect("implicit default");
    assert_eq!(default.targets.keys().collect::<Vec<_>>(), vec!["gestalt"]);
    assert_eq!(manifest.offer_names().collect::<Vec<_>>(), vec!["default"]);
}

#[test]
fn an_offer_selecting_an_undeclared_target_names_the_declared_ones() {
    let manifest = TransitiveManifest::parse(&format!(
        "{OFFERS_MANIFEST}\n[offers.agents]\ntargets = [\"agents\"]\n"
    ))
    .expect("manifest parses");
    let msg = manifest
        .offer("agents")
        .expect_err("undeclared target")
        .to_string();
    assert!(
        msg.contains("`agents`") && msg.contains("loqui") && msg.contains("moira"),
        "got: {msg}"
    );
}

#[test]
fn an_offer_naming_a_target_outside_its_root_is_rejected() {
    let manifest = TransitiveManifest::parse(&format!(
        "{OFFERS_MANIFEST}\n[offers.rules]\nroot = \"rules\"\ntargets = [\"loqui\"]\n"
    ))
    .expect("manifest parses");
    let msg = manifest
        .offer("rules")
        .expect_err("loqui lies outside rules/")
        .to_string();
    assert!(msg.contains("outside the offer root"), "got: {msg}");
}

const LOCAL_ONLY_TARGETS: &str = r#"
[sources.notes]
path = "~/notes"

[sources.henia]
build = { inputs = ["notes"], run = "henia build" }

[targets.notes]
path = "notes"
sources = ["notes"]

[targets.built]
path = "built"
sources = ["henia"]
"#;

#[test]
fn the_default_offer_skips_targets_binding_unpublishable_sources() {
    let manifest = TransitiveManifest::parse(LOCAL_ONLY_TARGETS).expect("manifest parses");
    let default = manifest.offer("default").expect("default offer");
    assert!(default.targets.is_empty(), "got: {:?}", default.targets);
}

#[test]
fn naming_a_target_that_binds_an_unpublishable_source_is_rejected() {
    for (target, kind) in [("notes", "a local path"), ("built", "a build")] {
        let manifest = TransitiveManifest::parse(&format!(
            "{LOCAL_ONLY_TARGETS}\n[offers.x]\ntargets = [\"{target}\"]\n"
        ))
        .expect("manifest parses");
        let msg = manifest.offer("x").expect_err(target).to_string();
        assert!(
            msg.contains("cannot be offered") && msg.contains(kind),
            "{target}: {msg}"
        );
    }
}

#[test]
fn an_offer_without_include_publishes_own_files_minus_phora_files() {
    let manifest = TransitiveManifest::parse("[offers.rules]\n").expect("manifest parses");
    let own = manifest
        .offer("rules")
        .expect("rules")
        .files
        .expect("own files");
    assert_eq!(own.path.as_deref(), Some("."));
    assert_eq!(
        own.exclude.as_deref(),
        Some(&["/phora.toml".to_owned(), "/phora.lock".to_owned()][..])
    );
}

#[test]
fn own_files_give_way_to_every_target_path() {
    let manifest = TransitiveManifest::parse(OFFERS_MANIFEST).expect("manifest parses");
    let own = manifest
        .offer("default")
        .expect("default")
        .files
        .expect("own files");
    assert_eq!(
        own.exclude.as_deref(),
        Some(
            &[
                "/skills/loqui/reference/loqui/".to_owned(),
                "/rules/fas/moira/".to_owned()
            ][..]
        ),
        "an offer with `include` publishes phora files only if selected, and every target path yields"
    );
    let fas = manifest
        .offer("fas")
        .expect("fas")
        .files
        .expect("own files");
    assert_eq!(fas.root.as_deref(), Some(std::path::Path::new("rules/fas")));
    assert_eq!(
        fas.exclude.as_deref(),
        Some(
            &[
                "/phora.toml".to_owned(),
                "/phora.lock".to_owned(),
                "/moira/".to_owned()
            ][..]
        )
    );
}

#[test]
fn own_files_give_way_to_a_target_the_offer_does_not_select() {
    let manifest = TransitiveManifest::parse(&format!(
        "{OFFERS_MANIFEST}\n[offers.skills]\ntargets = []\n"
    ))
    .expect("manifest parses");
    let skills = manifest.offer("skills").expect("skills");
    assert!(skills.targets.is_empty());
    let own = skills.files.expect("own files");
    assert!(
        own.exclude
            .iter()
            .flatten()
            .any(|e| e == "/skills/loqui/reference/loqui/"),
        "a deselected target's committed copy must not ride along, got: {:?}",
        own.exclude
    );
}

#[test]
fn a_target_at_the_repo_root_subtracts_nothing() {
    let manifest = TransitiveManifest::parse(
        "[sources.dotfiles]\ngit = \"https://github.com/srnnkls/dotfiles.git\"\n\n\
         [targets.home]\npath = \".\"\nsources = [\"dotfiles\"]\n",
    )
    .expect("manifest parses");
    let own = manifest
        .offer("default")
        .expect("default")
        .files
        .expect("own files");
    assert_eq!(
        own.exclude.as_deref(),
        Some(&["/phora.toml".to_owned(), "/phora.lock".to_owned()][..])
    );
}

#[test]
fn a_target_covering_the_offer_root_leaves_no_own_files() {
    let manifest = TransitiveManifest::parse(&format!(
        "{OFFERS_MANIFEST}\n[offers.moira]\nroot = \"rules/fas/moira\"\n"
    ))
    .expect("manifest parses");
    let moira = manifest.offer("moira").expect("moira");
    assert!(moira.files.is_none());
    assert_eq!(moira.targets["moira"].path, std::path::PathBuf::new());
}

#[test]
fn manifest_rejects_keys_with_the_member_separator() {
    for text in [
        "[sources.\"a%b\"]\ngit = \"https://github.com/srnnkls/tropos.git\"\n",
        "[targets.\"a%b\"]\npath = \"x\"\n",
    ] {
        let err = TransitiveManifest::parse(text).expect_err(text);
        assert!(err.to_string().contains('%'), "{text}: {err}");
    }
}

#[test]
fn escaping_offer_roots_and_named_target_paths_are_rejected() {
    for text in [
        "[offers.fas]\nroot = \"../rules\"\n",
        "[offers.fas]\nroot = \"/etc/fas\"\n",
        "[sources.gestalt]\ngit = \"https://github.com/srnnkls/gestalt.git\"\n[targets.gestalt]\npath = \"../gestalt\"\nsources = [\"gestalt\"]\n[offers.fas]\ntargets = [\"gestalt\"]\n",
        "[sources.gestalt]\ngit = \"https://github.com/srnnkls/gestalt.git\"\n[targets.gestalt]\npath = \"~/gestalt\"\nsources = [\"gestalt\"]\n[offers.fas]\ntargets = [\"gestalt\"]\n",
    ] {
        let manifest = TransitiveManifest::parse(text).expect(text);
        let err = manifest.offer("fas").expect_err(text);
        assert!(
            err.to_string().contains("relative subpath"),
            "{text}: {err}"
        );
    }
}

#[test]
fn hooks_are_retained_per_target() {
    let text = r#"
[sources.gestalt]
git = "https://github.com/srnnkls/gestalt.git"

[targets.gestalt]
path = "skills/gestalt"
sources = ["gestalt"]
hooks.on_change = "./install-skill.sh"

[targets.rules]
path = "rules"
sources = ["gestalt"]
"#;
    let manifest = TransitiveManifest::parse(text).expect("manifest parses");
    let hooks = manifest
        .hooks()
        .and_then(toml::Value::as_table)
        .expect("hooks");
    assert_eq!(hooks.keys().collect::<Vec<_>>(), vec!["gestalt"]);
}

#[test]
fn a_transitive_binding_inside_an_offered_target_lowers_into_an_import() {
    let manifest = TransitiveManifest::parse(
        "[sources.moira]\ngit = \"https://github.com/srnnkls/moira.git\"\ntransitive = true\n\n\
         [targets.rules]\npath = \"rules/fas/moira\"\nsources.moira = { offer = \"fas\" }\n",
    )
    .expect("manifest parses");
    let offer = manifest.offer("default").expect("default");
    let target = &offer.targets["rules"];
    assert!(target.sources.iter().flatten().next().is_none());
    let imports = target.offer_bindings.as_deref().expect("nested import");
    assert_eq!((imports[0].source(), imports[0].offer()), ("moira", "fas"));
}

// TDEP-HOOK-GATE-001

const DEP_MANIFEST_WITH_TRUST_AND_HOOKS: &str = r#"
version = 1
trust = "all"
trusted_hooks = ["editor#./install.sh"]
allow_hooks = true

[sources.nvim]
git = "https://github.com/dep/nvim.git"

[targets.editor]
path = "nvim"
sources = ["nvim"]

[targets.editor.hooks]
on_change = "./install.sh"
"#;

#[test]
fn manifest_drops_trust_control_yet_retains_per_target_hooks() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST_WITH_TRUST_AND_HOOKS).expect(
        "a manifest carrying BOTH trust-control fields AND a [targets.X.hooks] block must parse — \
         trust-control tolerated-and-dropped, hooks retained opaque",
    );
    let rendered = format!("{manifest:?}");
    for dropped in ["trust", "trusted_hooks", "allow_hooks", "\"all\""] {
        assert!(
            !rendered.contains(dropped),
            "no API may expose the dropped trust-control field `{dropped}`, got: {rendered}"
        );
    }
    let hooks = manifest
        .hooks()
        .expect("the per-target hooks must still be retained as an opaque value");
    let reserialized = toml::to_string(hooks).expect("opaque payload round-trips to TOML");
    assert!(
        reserialized.contains("./install.sh"),
        "the per-target hook command must survive verbatim in the opaque payload even when the \
         manifest also carried trust-control, got: {reserialized}"
    );
}

#[test]
fn admission_interprets_opaque_into_structured_candidate_keyed_by_instance() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST).expect("dep manifest parses");
    let opaque = manifest
        .hooks()
        .expect("manifest retains opaque per-target hooks");
    let instance = composed_instance();

    let (candidates, _diagnostics) =
        admit_transitive_hooks(opaque, "editor", "ns%1%editor", &instance);

    assert_eq!(
        candidates.len(),
        1,
        "the `editor` target's single on_change command must yield exactly one candidate, got: {candidates:?}"
    );
    let candidate = &candidates[0];
    assert_eq!(
        candidate.dep_instance,
        instance.stable_key(),
        "a candidate must own the CONFINED instance's stable_key() as dep_instance, not a bare name"
    );
    assert_eq!(
        candidate.command,
        crate::config::HookCommand::Shell {
            run: "./install.sh".to_owned(),
            shell: None,
        },
        "the opaque toml::Value must be INTERPRETED into the structured HookCommand DTO (run/shell)"
    );
    assert!(
        candidate.hook_id.starts_with("ns%1%editor#"),
        "the hook_id must namespace the COMPOSED target name then the scope, got: {}",
        candidate.hook_id
    );
    assert!(
        candidate.hook_id.contains("on_change") || candidate.hook_id.contains("on-change"),
        "the hook_id must name the on_change scope, got: {}",
        candidate.hook_id
    );
}

#[test]
fn admission_yields_no_candidates_for_a_target_with_no_hooks() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST).expect("dep manifest parses");
    let opaque = manifest
        .hooks()
        .expect("manifest retains opaque per-target hooks");
    let instance = composed_instance();

    let (candidates, _diagnostics) =
        admit_transitive_hooks(opaque, "nonexistent", "ns%1%nonexistent", &instance);

    assert!(
        candidates.is_empty(),
        "a composed target the dep declared no hooks for must yield zero candidates, got: {candidates:?}"
    );
}

#[test]
fn admission_produces_candidates_but_never_a_trusted_marker() {
    let manifest = TransitiveManifest::parse(DEP_MANIFEST_WITH_TRUST_AND_HOOKS)
        .expect("dep manifest with trust-control + hooks parses");
    let opaque = manifest
        .hooks()
        .expect("manifest retains opaque per-target hooks");
    let instance = composed_instance();

    let (candidates, _diagnostics) =
        admit_transitive_hooks(opaque, "editor", "ns%1%editor", &instance);

    assert_eq!(
        candidates.len(),
        1,
        "even a manifest declaring `trust = \"all\"` produces a CANDIDATE, never a pre-approved hook"
    );
    let rendered = format!("{:?}", candidates[0]);
    assert!(
        !rendered.to_lowercase().contains("trust") && !rendered.to_lowercase().contains("approv"),
        "a CandidateHook must carry no trust/approval state — GATE strips by default; the \
         dep's own trust-control can never self-approve, got: {rendered}"
    );
}

fn hook_id_for(run: &str, shell: Option<&str>) -> String {
    let toml = match shell {
        None => format!(
            "version = 1\n\n[sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
             [targets.editor]\npath = \"nvim\"\nsources = [\"nvim\"]\n\n\
             [targets.editor.hooks]\non_change = {{ run = \"{run}\" }}\n"
        ),
        Some(shell) => format!(
            "version = 1\n\n[sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n\n\
             [targets.editor]\npath = \"nvim\"\nsources = [\"nvim\"]\n\n\
             [targets.editor.hooks]\non_change = {{ run = \"{run}\", shell = \"{shell}\" }}\n"
        ),
    };
    let manifest = TransitiveManifest::parse(&toml).expect("manifest parses");
    let opaque = manifest.hooks().expect("manifest retains opaque hooks");
    let instance = composed_instance();
    let (candidates, _diagnostics) =
        admit_transitive_hooks(opaque, "editor", "ns%1%editor", &instance);
    candidates
        .into_iter()
        .next()
        .expect("one candidate")
        .hook_id
}

#[test]
fn hook_id_resists_hash_delimiter_injection() {
    let injected = hook_id_for("x", Some("y#z"));
    let benign = hook_id_for("x#y", Some("z"));
    assert_ne!(
        injected, benign,
        "two DISTINCT (run, shell) commands that collide under unescaped `#` concatenation must \
         NOT share a hook_id; an injected `#` cannot forge a trust key, \
         got injected={injected} benign={benign}"
    );
}

#[test]
fn hook_id_canonicalizes_absent_and_explicit_default_shell() {
    let absent = hook_id_for("./install.sh", None);
    let explicit = hook_id_for("./install.sh", Some("sh -c"));
    assert_eq!(
        absent, explicit,
        "shell = None and shell = Some(\"sh -c\") are the same effective command and MUST share \
         one trust key, got absent={absent} explicit={explicit}"
    );
}

#[test]
fn hook_id_preserves_greppable_prefix_and_namespacing() {
    let id = hook_id_for("./install.sh", None);
    assert!(
        id.starts_with("ns%1%editor#on_change#"),
        "the human-readable `composed_target#on_change#` prefix must survive for auditing, got: {id}"
    );
}

#[test]
fn a_root_offer_naming_a_local_target_fails_validation() {
    let config = merged(&format!(
        "{TRANSITIVE_DEP}[targets.claude]\npath = \"~/.claude\"\nsources = [\"tropos\"]\n\n\
         [offers.agents]\ntargets = [\"claude\"]\n"
    ));
    let msg = config
        .validate()
        .expect_err("an absolute target cannot be offered")
        .to_string();
    assert!(
        msg.contains("agents") && msg.contains("claude") && msg.contains("relative subpath"),
        "got: {msg}"
    );
}

#[test]
fn a_target_key_with_the_member_separator_fails_validation() {
    let config = merged("version = 1\n\n[targets.\"a%b\"]\npath = \"x\"\n");
    let msg = config.validate().expect_err("`%` is reserved").to_string();
    assert!(msg.contains("a%b"), "got: {msg}");
}

#[test]
fn a_local_overlay_cannot_declare_offers() {
    let err = Config::parse_local("version = 1\n\n[offers.default]\n")
        .expect_err("offers publish committed content");
    assert!(err.to_string().contains("offers"), "got: {err}");
}

#[test]
fn local_only_sources_do_not_block_an_import() {
    let manifest = TransitiveManifest::parse(
        "[sources.tropos]\npath = \".\"\n\n\
         [sources.henia]\nbuild = { inputs = [\"tropos\"], run = \"henia build\" }\n\n\
         [targets.claude]\npath = \"~/.claude\"\nsources = [\"henia\"]\n\n\
         [offers.default]\ninclude = [\"skills/**\"]\n",
    )
    .expect("sources only the local config binds are not part of any offer");
    assert!(manifest.offer("default").is_ok());
}
