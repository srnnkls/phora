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

#[test]
fn imports_accepts_a_bare_source_name_list() {
    let toml = "version = 1\n\n[sources.dep]\ngit = \"https://github.com/me/d.git\"\ntransitive = true\n\n\
                [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\n";
    let config = Config::parse(toml).expect("a bare-name imports list must parse");
    let target = config.targets.get("home").expect("target `home` present");
    assert_eq!(
        target.imports,
        Some(vec!["dep".to_string()]),
        "`imports` must type as a flat Vec<String> and carry the exact bare source names"
    );
}

#[test]
fn imports_rejects_a_refined_table_import() {
    let toml = "version = 1\n\n[sources.dep]\ngit = \"https://github.com/me/d.git\"\ntransitive = true\n\n\
                [targets.home]\npath = \"~/deploy\"\nimports = [{ source = \"dep\", root = \"../escape\" }]\n";
    // Security contract: `Vec<String>` makes escape-capable refinements (root/map/as) unrepresentable.
    assert!(
        Config::parse(toml).is_err(),
        "a refined-table import must NOT parse into the bare-name `Vec<String>` field"
    );
}

#[test]
fn imports_rejects_a_map_form_refinement() {
    let toml = "version = 1\n\n[sources.dep]\ngit = \"https://github.com/me/d.git\"\ntransitive = true\n\n\
                [targets.home.imports.dep]\nas = \"renamed\"\n";
    // Security contract: `Vec<String>` makes per-import keyed-table refinement unrepresentable.
    assert!(
        Config::parse(toml).is_err(),
        "a map-form (keyed-table) imports refinement must NOT parse into `Vec<String>`"
    );
}

#[test]
fn source_in_both_imports_and_sources_is_rejected() {
    let toml = "version = 1\n\n[sources.dep]\ngit = \"https://github.com/me/d.git\"\ntransitive = true\n\n\
                [targets.home]\npath = \"~/deploy\"\nimports = [\"dep\"]\nsources = [\"dep\"]\n";
    let config = Config::parse(toml).expect("the document itself is structurally valid TOML");
    let err = config
        .validate()
        .expect_err("a source referenced by BOTH imports (mount) and sources (flat) is an error");
    let msg = err.to_string();
    assert!(
        msg.contains("dep") && (msg.contains("imports") || msg.contains("mount")),
        "the conflict must name the doubly-referenced source `dep` and the imports/mount conflict, got: {msg}"
    );
}

#[test]
fn imports_reference_to_a_non_transitive_source_is_rejected() {
    let toml = "version = 1\n\n[sources.flat]\ngit = \"https://github.com/me/d.git\"\n\n\
                [targets.home]\npath = \"~/deploy\"\nimports = [\"flat\"]\n";
    let config = Config::parse(toml).expect("structurally valid TOML");
    let err = config
        .validate()
        .expect_err("mounting a NON-transitive source via imports must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("flat") && msg.contains("transitive"),
        "the rejection must name `flat` and explain a mount requires a transitive source, got: {msg}"
    );
}

#[test]
fn transitive_source_flat_bound_via_sources_is_rejected() {
    let toml = "version = 1\n\n[sources.dep]\ngit = \"https://github.com/me/d.git\"\ntransitive = true\n\n\
                [targets.home]\npath = \"~/deploy\"\nsources = [\"dep\"]\n";
    let config = Config::parse(toml).expect("structurally valid TOML");
    let err = config.validate().expect_err(
        "a transitive source flat-bound via `sources` (never imported) must be rejected: \
                     it would be silently flat-downgraded, bypassing escape-remote rejection",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("dep")
            && msg.contains("transitive")
            && (msg.contains("sources") || msg.contains("flat")),
        "the rejection must name `dep`, mark it transitive, and explain it cannot be flat-bound via `sources`, got: {msg}"
    );
}

#[test]
fn transitive_source_declared_but_never_imported_is_rejected() {
    let toml = "version = 1\n\n[sources.dep]\ngit = \"https://github.com/me/d.git\"\ntransitive = true\n\n\
                [targets.home]\npath = \"~/deploy\"\n";
    let config = Config::parse(toml).expect("structurally valid TOML");
    let err = config.validate().expect_err(
        "a transitive source that no target imports must be rejected: it would never \
                     be mounted and its sub-graph never resolved",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("dep") && msg.contains("transitive") && msg.contains("import"),
        "the rejection must name `dep`, mark it transitive, and explain no target imports it, got: {msg}"
    );
}

#[test]
fn imports_reference_to_an_undefined_source_is_rejected() {
    let toml = "version = 1\n\n\
                [targets.home]\npath = \"~/deploy\"\nimports = [\"ghost\"]\n";
    let config = Config::parse(toml).expect("structurally valid TOML");
    let err = config
        .validate()
        .expect_err("an imports reference to an undefined source must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("ghost"),
        "the rejection must name the undefined source `ghost`, got: {msg}"
    );
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
