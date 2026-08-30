//! Behavioral half of the T008 contract (paired with the live
//! `tests/projection_contract_gate.rs`). The public API these tests exercise IS
//! the contract T008 implements — the `use` paths and constructor/accessor names
//! below are the intended surface.

use std::path::Path;

use phora::config::{Config, DeployMode, LayoutConfig, TakeEntry, TemplateOptIn};
use phora::projection::build::{project_binding, project_target, projected_artifact_keys};
use phora::projection::diagnostic::ProjectionError;
use phora::projection::model::Materialization;
use phora::projection::model::{
    ArtifactRelativePath, BindingProjection, BindingProjectionInput, CollapsePreference,
    ContentTransform, LayoutSpec, MaterializationPolicy, OfferSpec, ProjectedArtifact,
    ResolvedSourceRef, TakeSpec, TargetPath, TargetProjection, TemplatePolicy,
};
use phora::source::{SourceEntryKind, SourceEntryMeta, SourceInventory, SourcePath};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn resolved_ref(name: &str) -> ResolvedSourceRef {
    ResolvedSourceRef::new(name, COMMIT)
}

fn flat_layout() -> LayoutSpec {
    LayoutSpec::from(&LayoutConfig::default())
}

fn copy_policy() -> MaterializationPolicy {
    MaterializationPolicy::from(&DeployMode::Copy)
}

fn link_policy() -> MaterializationPolicy {
    MaterializationPolicy::from(&DeployMode::Link)
}

fn suffix_templates() -> TemplatePolicy {
    TemplatePolicy::from(&TemplateOptIn::SuffixOnly)
}

fn destinations(binding: &BindingProjection) -> Vec<String> {
    let mut out: Vec<String> = binding
        .artifacts
        .iter()
        .map(|a: &ProjectedArtifact| a.destination.as_str().to_owned())
        .collect();
    out.sort();
    out
}

fn target_destinations(target: &TargetProjection) -> Vec<String> {
    let mut out: Vec<String> = target
        .artifacts
        .iter()
        .map(|a| a.destination.as_str().to_owned())
        .collect();
    out.sort();
    out
}

// ---- source-owned pure value types ----

#[test]
fn source_inventory_from_paths_orders_entries_independent_of_input_order() {
    let inv =
        SourceInventory::from_paths(["dir/b.md", "a.md"]).expect("valid paths seed an inventory");
    assert_eq!(inv.entries.len(), 2, "one entry per input path");
    assert_eq!(
        inv.entries[0].path.as_str(),
        "a.md",
        "entries are ORDERED, not input-order: `a.md` sorts before `dir/b.md` even though it was \
         passed second"
    );
    assert_eq!(
        inv.entries[0].kind,
        SourceEntryKind::File,
        "from_paths seeds plain File entries (executable/symlink kinds come from PR5 discovery)"
    );
    assert_eq!(inv.entries[1].path.as_str(), "dir/b.md");
}

#[test]
fn source_inventory_from_paths_hard_errors_on_a_lexically_invalid_leaf() {
    for bad in ["a\0b", "a:b", "../escape"] {
        assert!(
            SourceInventory::from_paths([bad]).is_err(),
            "a discovered leaf `{bad:?}` that fails the lexical `safe_relpath` rule must be a HARD \
             ERROR at inventory construction, never a silent drop that slips past to fail later at \
             deploy-time safe_relpath"
        );
    }
    assert!(
        SourceInventory::from_paths(["ok.md", "a:b"]).is_err(),
        "one invalid path among valid siblings still fails the whole seed — no partial inventory"
    );
}

fn inventory_of<'a>(paths: impl IntoIterator<Item = &'a str>) -> SourceInventory {
    SourceInventory::from_paths(paths).expect("contract inventory paths are lexically valid")
}

#[test]
fn source_entry_kind_distinguishes_file_executable_and_symlink() {
    let exec = SourceEntryMeta {
        path: SourcePath::new("bin/tool").expect("valid"),
        kind: SourceEntryKind::Executable,
    };
    let link = SourceEntryMeta {
        path: SourcePath::new("link").expect("valid"),
        kind: SourceEntryKind::Symlink,
    };
    assert_eq!(
        exec.kind,
        SourceEntryKind::Executable,
        "an executable entry carries the Executable kind"
    );
    assert_ne!(
        exec.kind, link.kind,
        "an executable entry and a symlink entry carry distinct kinds"
    );
}

// ---- lexical relative path newtypes: SourcePath / TargetPath / ArtifactRelativePath (R2-7) ----

macro_rules! path_newtype_contract {
    ($ty:ty, $accept:ident, $reject:ident, $lexical:ident) => {
        #[test]
        fn $accept() {
            for ok in [
                "a",
                "a/b",
                "a/b/c",
                "a.b/c-d_e",
                ".config/nvim/init.lua",
                "a/.hidden",
                "café/init",
            ] {
                let p = <$ty>::new(ok)
                    .unwrap_or_else(|e| panic!("`{ok}` must be a valid relative path, got {e:?}"));
                assert_eq!(
                    p.as_str(),
                    ok,
                    "construction must preserve the input path verbatim (no case fold, no \
                     NFC normalization, no reshaping)"
                );
            }
        }

        #[test]
        fn $reject() {
            for bad in [
                // absolute / parent-escape / empty
                "",
                "/",
                "/etc/passwd",
                "..",
                "../escape",
                "a/../b",
                "a/..",
                // bare and interior single-dot (current-dir) components
                ".",
                "a/./b",
                // drive letters and backslashes
                "C:/win",
                "C:\\win",
                "a\\b",
                // empty interior / trailing components
                "a//b",
                "a/",
                "a/b/",
                // NUL byte
                "a\0b",
                // colon-bearing component (NTFS alternate-data-stream foot-gun)
                "a:b",
                "a/b:c",
                // reserved DOS device names (inert on Unix, escape on Windows)
                "CON",
                "nul",
                "aux.txt",
                "com1",
                "lpt9",
                "a/CON/b",
            ] {
                assert!(
                    <$ty>::new(bad).is_err(),
                    "`{bad:?}` must be rejected by the lexical constructor (matches production \
                     `safe_relpath`: absolute / parent-escape / single-dot / empty / \
                     empty-or-trailing component / backslash / NUL / colon / reserved device name)"
                );
            }
        }

        #[test]
        fn $lexical() {
            let absent = "definitely/not/on/disk/xyzzy/plugh";
            assert!(
                <$ty>::new(absent).is_ok(),
                "a lexically valid relative path must construct with no filesystem probe — \
                 physical symlink/existence checks are sync-side, never in the pure constructor"
            );
        }
    };
}

path_newtype_contract!(
    SourcePath,
    source_path_accepts_nested_relative,
    source_path_rejects_unsafe_forms,
    source_path_construction_is_lexical_only
);
path_newtype_contract!(
    TargetPath,
    target_path_accepts_nested_relative,
    target_path_rejects_unsafe_forms,
    target_path_construction_is_lexical_only
);
path_newtype_contract!(
    ArtifactRelativePath,
    artifact_relative_path_accepts_nested_relative,
    artifact_relative_path_rejects_unsafe_forms,
    artifact_relative_path_construction_is_lexical_only
);

macro_rules! path_ordering_contract {
    ($ty:ty, $ord:ident) => {
        #[test]
        fn $ord() {
            let mk = |s: &str| <$ty>::new(s).expect("valid");

            let mut v = vec![mk("b/y"), mk("a/z"), mk("a/b")];
            v.sort();
            let sorted: Vec<&str> = v.iter().map(<$ty>::as_str).collect();
            assert_eq!(
                sorted,
                ["a/b", "a/z", "b/y"],
                "Ord is a lexicographic order over the path string"
            );

            let upper = mk("A/b");
            let lower = mk("a/b");
            assert_ne!(
                upper, lower,
                "case differences yield distinct paths — folding is a collision-time concern, \
                 not identity"
            );
            assert!(
                upper < lower,
                "ASCII uppercase sorts before lowercase under the documented case-sensitive order"
            );

            let mut u = vec![mk("z"), mk("café"), mk("a")];
            u.sort();
            let unicode: Vec<&str> = u.iter().map(<$ty>::as_str).collect();
            assert_eq!(
                unicode,
                ["a", "café", "z"],
                "ordering is by UTF-8 byte / code-point order: the ASCII prefix `caf` places \
                 `café` between `a` and `z`, never case-folded"
            );
            assert_ne!(
                mk("café"),
                mk("cafe\u{0301}"),
                "composed `é` (U+00E9) and decomposed `e`+`U+0301` are DISTINCT paths — NFC is a \
                 collision-time fold, not part of identity or ordering"
            );

            let (a, b, c) = (mk("a"), mk("a/b"), mk("b"));
            assert!(a < b && b < c && a < c, "a total order must be transitive");
            assert!(!(b < a), "antisymmetry: a < b implies not b < a");
        }
    };
}

path_ordering_contract!(
    TargetPath,
    target_path_ordering_is_case_sensitive_lexicographic_and_codepoint
);
path_ordering_contract!(
    ArtifactRelativePath,
    artifact_relative_path_ordering_is_case_sensitive_lexicographic_and_codepoint
);

// ---- config -> spec, one-way ----

fn parsed_offer_spec(source_body: &str) -> OfferSpec {
    let cfg =
        Config::parse(&format!("version = 1\n[sources.s]\n{source_body}")).expect("config parses");
    let parsed = cfg.parsed_sources().expect("sources parse");
    OfferSpec::from(parsed["s"].offer())
}

#[test]
fn offer_spec_carries_include_exclude_and_root_from_config() {
    let spec = parsed_offer_spec(
        "git = \"https://e.test/r.git\"\n\
         include = [\"skills/**\"]\n\
         exclude = [\"skills/private/**\"]\n\
         root = \"nested\"\n",
    );
    assert_eq!(
        spec.includes(),
        ["skills/**"],
        "include maps through verbatim"
    );
    assert_eq!(
        spec.excludes(),
        ["skills/private/**"],
        "exclude maps through for `include − exclude` composition"
    );
    assert_eq!(
        spec.root(),
        Some(Path::new("nested")),
        "a declared `root` must survive into the spec by EXACT value so matching re-anchors there"
    );
    assert!(
        !spec.is_implicit_full(),
        "a declared include makes the offer explicit, not the implicit full offer"
    );
}

#[test]
fn offer_spec_absent_include_is_the_implicit_full_offer() {
    let spec = parsed_offer_spec("git = \"https://e.test/r.git\"\n");
    assert!(
        spec.is_implicit_full(),
        "no `include` must convert to the implicit full offer (OCaml `no .mli = public`)"
    );
    assert!(
        spec.includes().is_empty(),
        "the implicit full offer declares no include patterns"
    );
    assert_eq!(
        spec.root(),
        None,
        "an offer with no declared root re-anchors at the source root"
    );
}

#[test]
fn offer_spec_is_owned_and_outlives_its_config() {
    let spec = {
        let cfg = Config::parse(
            "version = 1\n[sources.s]\ngit = \"https://e.test/r.git\"\ninclude = [\"x/**\"]\n",
        )
        .expect("config parses");
        let parsed = cfg.parsed_sources().expect("sources parse");
        OfferSpec::from(parsed["s"].offer())
    };
    assert_eq!(
        spec.includes(),
        ["x/**"],
        "the spec must own its data (one-way conversion), remaining usable after the config drops"
    );
}

#[test]
fn take_spec_distinguishes_project_all_none_and_an_explicit_set() {
    assert!(
        TakeSpec::from_entries(None).is_project_all(),
        "an omitted `take` converts to project-everything"
    );
    assert!(
        TakeSpec::from_entries(Some(&[])).is_project_none(),
        "an explicit empty `take = []` converts to project-nothing (distinct from omitted)"
    );

    let entries = vec![
        TakeEntry::Leaf("a".to_owned()),
        TakeEntry::Rename {
            src: "b/X.md".to_owned(),
            dest: "b/x.md".to_owned(),
        },
    ];
    let spec = TakeSpec::from_entries(Some(&entries));
    assert_eq!(
        spec.literals(),
        ["a"],
        "literal leaves map through verbatim"
    );
    assert_eq!(
        spec.renames(),
        [("b/X.md".to_owned(), "b/x.md".to_owned())],
        "rename pairs map through as (src, dest)"
    );
}

#[test]
fn take_spec_classifies_glob_entries_apart_from_literals() {
    let entries = vec![
        TakeEntry::Leaf("skills/**".to_owned()),
        TakeEntry::Leaf("nested/".to_owned()),
        TakeEntry::Leaf("init.lua".to_owned()),
    ];
    let spec = TakeSpec::from_entries(Some(&entries));
    assert_eq!(
        spec.literals(),
        ["init.lua"],
        "a plain leaf name with no metacharacter is a literal"
    );
    assert_eq!(
        spec.globs(),
        ["nested/", "skills/**"],
        "an entry ending in `/` or bearing `*?[]` classifies as a glob (is_take_glob), never a \
         literal that must be offered exactly"
    );
}

#[test]
fn template_policy_preserves_the_config_render_decision() {
    assert!(
        !TemplatePolicy::from(&TemplateOptIn::Disabled).renders("x.tmpl"),
        "`template = false` converts to a policy that renders nothing"
    );
    let suffix = TemplatePolicy::from(&TemplateOptIn::SuffixOnly);
    assert!(
        suffix.renders("x.tmpl"),
        "the default suffix policy renders `.tmpl` files"
    );
    assert_eq!(
        suffix.deployed_name("x.tmpl"),
        "x",
        "a rendered `.tmpl` file deploys under its stripped name"
    );
}

#[test]
fn template_policy_from_a_glob_list_renders_matching_files() {
    let cfg = Config::parse(
        "version = 1\n\
         [sources.vendor]\n\
         git = \"https://e.test/r.git\"\n\
         [targets.t]\n\
         path = \"~/x\"\n\
         [targets.t.sources]\n\
         vendor = { template = [\"*.yaml\"] }\n",
    )
    .expect("a binding template glob list must parse");
    let opt_in = cfg.targets["t"]
        .sources
        .as_ref()
        .expect("target declares a binding")
        .values()
        .next()
        .expect("one binding")
        .template_opt_in();
    let policy = TemplatePolicy::from(&opt_in);

    assert!(
        policy.renders("config.yaml"),
        "a `template = [\"*.yaml\"]` glob list renders a matching file even without a `.tmpl` suffix"
    );
    assert!(
        policy.renders("x.tmpl"),
        "a glob list still renders `.tmpl` files (the suffix convention composes with the globs)"
    );
    assert_eq!(
        policy.deployed_name("config.yaml"),
        "config.yaml",
        "a glob-rendered file without a `.tmpl` suffix keeps its name (nothing to strip)"
    );
}

#[test]
fn materialization_policy_maps_deploy_mode_one_way() {
    let link = MaterializationPolicy::from(&DeployMode::Link);
    let copy = MaterializationPolicy::from(&DeployMode::Copy);
    assert_ne!(
        link, copy,
        "link and copy materialization are distinct policies — they drive different collapse, \
         template-render, and suffix-strip behavior"
    );
    assert_eq!(
        copy,
        MaterializationPolicy::from(&DeployMode::Copy),
        "the config->spec conversion is a pure function of the deploy mode"
    );
}

#[test]
fn collapse_preference_maps_the_config_override_one_way() {
    assert_eq!(
        CollapsePreference::from(None),
        CollapsePreference::default(),
        "an omitted `collapse` converts to the default (collapse-where-sound) preference"
    );
    assert_ne!(
        CollapsePreference::from(Some(true)),
        CollapsePreference::from(Some(false)),
        "`collapse = true` (force) and `collapse = false` (per-leaf) are distinct preferences"
    );
    assert_ne!(
        CollapsePreference::from(Some(true)),
        CollapsePreference::default(),
        "a demanded collapse differs from the default"
    );
}

// ---- project_binding over a pure in-memory inventory ----

fn binding_input<'a>(
    identity: &'a str,
    source: &'a ResolvedSourceRef,
    inventory: &'a SourceInventory,
    offer: &'a OfferSpec,
    take: &'a TakeSpec,
    templates: &'a TemplatePolicy,
    layout: &'a LayoutSpec,
) -> BindingProjectionInput<'a> {
    BindingProjectionInput {
        identity,
        source,
        offer,
        inventory,
        take,
        collapse: CollapsePreference::default(),
        history: false,
        materialization: copy_policy(),
        layout,
        templates,
    }
}

fn project_mode(
    identity: &str,
    inventory: &SourceInventory,
    offer: &OfferSpec,
    take: &TakeSpec,
    templates: &TemplatePolicy,
    layout: &LayoutSpec,
    materialization: MaterializationPolicy,
) -> Result<BindingProjection, ProjectionError> {
    let source = resolved_ref(identity);
    let input = BindingProjectionInput {
        identity,
        source: &source,
        offer,
        inventory,
        take,
        collapse: CollapsePreference::default(),
        history: false,
        materialization,
        layout,
        templates,
    };
    project_binding(&input)
}

fn project_flat(
    identity: &str,
    inventory: &SourceInventory,
    offer: &OfferSpec,
    take: &TakeSpec,
    templates: &TemplatePolicy,
    layout: &LayoutSpec,
) -> Result<BindingProjection, ProjectionError> {
    project_mode(
        identity,
        inventory,
        offer,
        take,
        templates,
        layout,
        copy_policy(),
    )
}

#[test]
fn project_binding_emits_native_published_key_order_with_full_artifact_fields() {
    // Input order (z, m/x, a, m/y) is deliberately NOT the published-key order, and
    // `m/` is wholly taken so under link it collapses to one CollapsedDir. A helper
    // that pre-sorts destinations would hide a projection that returned artifacts
    // unordered; every assertion below reads the NATIVE, unsorted sequence.
    let inventory = inventory_of(["z.md", "m/x.md", "a.md", "m/y.md"]);
    let source = resolved_ref("dotfiles");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();
    let input = BindingProjectionInput {
        identity: "dotfiles",
        source: &source,
        offer: &offer,
        inventory: &inventory,
        take: &take,
        collapse: CollapsePreference::default(),
        history: false,
        materialization: link_policy(),
        layout: &layout,
        templates: &templates,
    };
    let binding = project_binding(&input).expect("a well-formed link binding projects");

    let native: Vec<&str> = binding
        .artifacts
        .iter()
        .map(|a| a.destination.as_str())
        .collect();
    assert_eq!(
        native,
        ["a.md", "m", "z.md"],
        "artifacts emit in deterministic ascending published-key order, independent of the \
         inventory input order (z, m/x, a, m/y)"
    );
    assert_eq!(
        projected_artifact_keys(&binding),
        ["a.md", "m", "z.md"],
        "the published keys track the artifacts one-for-one in that same native order"
    );

    for artifact in &binding.artifacts {
        assert!(
            !artifact.destination.as_str().starts_with('/'),
            "destinations stay TARGET-RELATIVE — sync joins the root; projection never absolutizes \
             (got `{}`)",
            artifact.destination.as_str()
        );
        assert_eq!(
            artifact.source,
            resolved_ref("dotfiles"),
            "every artifact carries the binding's resolved source ref (identity + commit)"
        );
    }

    let leaf_a = &binding.artifacts[0];
    assert!(
        matches!(leaf_a.materialization, Materialization::Leaf(_)),
        "`a.md` is a standalone Leaf materialization, not a collapsed dir"
    );
    let a_leaves: Vec<(&str, &ContentTransform)> = leaf_a
        .leaves
        .iter()
        .map(|l| (l.source.as_str(), &l.transform))
        .collect();
    assert_eq!(
        a_leaves,
        [("a.md", &ContentTransform::Identity)],
        "the leaf artifact carries its single source leaf at the Identity transform"
    );

    let dir_m = &binding.artifacts[1];
    assert!(
        matches!(dir_m.materialization, Materialization::CollapsedDir { .. }),
        "the wholly-taken `m/` collapses to one CollapsedDir materialization under link"
    );
    let m_leaves: Vec<&str> = dir_m.leaves.iter().map(|l| l.source.as_str()).collect();
    assert_eq!(
        m_leaves,
        ["m/x.md", "m/y.md"],
        "the collapsed dir retains every kept leaf under it, in source order"
    );
    for leaf in &dir_m.leaves {
        assert_eq!(
            leaf.transform,
            ContentTransform::Identity,
            "a link-mode leaf never renders — Identity transform, never Template"
        );
    }

    let leaf_z = &binding.artifacts[2];
    assert!(
        matches!(leaf_z.materialization, Materialization::Leaf(_)),
        "`z.md` is a standalone Leaf materialization"
    );
    assert_eq!(
        leaf_z
            .leaves
            .iter()
            .map(|l| l.source.as_str())
            .collect::<Vec<_>>(),
        ["z.md"],
        "the trailing leaf artifact carries `z.md` at its source path"
    );
}

#[test]
fn project_binding_rooted_offer_leaf_source_is_the_full_inventory_path() {
    let inventory = inventory_of(["editor/init.lua", "editor/lua/opts.lua"]);
    let offer = parsed_offer_spec("git = \"https://e.test/r.git\"\nroot = \"editor\"\n");
    let binding = project_flat(
        "ed",
        &inventory,
        &offer,
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
    )
    .expect("a rooted offer projects");

    let mut sources: Vec<&str> = binding
        .artifacts
        .iter()
        .flat_map(|a| a.leaves.iter().map(|l| l.source.as_str()))
        .collect();
    sources.sort_unstable();
    assert_eq!(
        sources,
        ["editor/init.lua", "editor/lua/opts.lua"],
        "each leaf.source is the root-joined inventory path (both the standalone leaf and the \
         collapsed-dir branch), not the re-anchored root-relative `init.lua`/`lua/opts.lua`"
    );
}

#[test]
fn target_relative_destination_joins_the_target_root_to_the_absolute_deploy_path() {
    let inventory = inventory_of(["a.md"]);
    let binding = project_flat(
        "dotfiles",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
    )
    .expect("projects");

    let dest = binding.artifacts[0].destination.as_str();
    assert!(
        !dest.starts_with('/'),
        "the projection keeps the destination target-relative so the root-join is the consumer's \
         job (deploy.rs/target.rs), not projection's"
    );
    let joined = Path::new("/home/u/deploy").join(dest);
    assert_eq!(
        joined,
        Path::new("/home/u/deploy/a.md"),
        "a consumer reconstructs the absolute deploy path by joining the target root onto the \
         target-relative destination; a projection that re-absolutized (leading `/`) would make \
         `join(root, dest)` discard the root and escape the target"
    );
}

#[test]
fn project_binding_copy_mode_marks_template_leaves_and_strips_the_suffix() {
    let inventory = inventory_of(["c.tmpl"]);
    let binding = project_flat(
        "dotfiles",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
    )
    .expect("a template leaf projects");

    assert_eq!(
        destinations(&binding),
        ["c"],
        "under copy a rendered `.tmpl` leaf deploys under its stripped name"
    );
    let transforms: Vec<&ContentTransform> = binding
        .artifacts
        .iter()
        .flat_map(|a| a.leaves.iter().map(|l| &l.transform))
        .collect();
    assert_eq!(
        transforms,
        [&ContentTransform::Template],
        "under copy a `.tmpl` leaf carries the Template transform"
    );
}

#[test]
fn project_binding_link_mode_neither_strips_nor_renders_a_tmpl_leaf() {
    let inventory = inventory_of(["c.tmpl"]);
    let binding = project_mode(
        "dotfiles",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
        link_policy(),
    )
    .expect("a link-mode leaf projects");

    assert_eq!(
        destinations(&binding),
        ["c.tmpl"],
        "link materialization NEVER renders, so a `.tmpl` leaf keeps its suffix — no suffix strip \
         (the strip is copy-only)"
    );
    let transforms: Vec<&ContentTransform> = binding
        .artifacts
        .iter()
        .flat_map(|a| a.leaves.iter().map(|l| &l.transform))
        .collect();
    assert_eq!(
        transforms,
        [&ContentTransform::Identity],
        "a link-mode `.tmpl` leaf carries Identity, not Template: link symlinks the live file and \
         never renders it"
    );
}

#[test]
fn project_binding_collapse_diverges_by_materialization() {
    let inventory = inventory_of(["d/a.md", "d/secret.md"]);
    let offer = parsed_offer_spec("git = \"https://e.test/r.git\"\ninclude = [\"d/a.md\"]\n");

    let under_copy = project_mode(
        "s",
        &inventory,
        &offer,
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
        copy_policy(),
    )
    .expect("copy projects");
    assert_eq!(
        destinations(&under_copy),
        ["d"],
        "under COPY a within-dir exclude does NOT block collapse: `d/` collapses to one subtree \
         artifact `d`"
    );

    let under_link = project_mode(
        "s",
        &inventory,
        &offer,
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
        link_policy(),
    )
    .expect("link projects");
    assert_eq!(
        destinations(&under_link),
        ["d/a.md"],
        "under LINK the offered-out `d/secret.md` makes `d/` un-clean, so collapse is blocked and \
         the kept leaf falls back per-leaf — the CollapseMode::Link divergence"
    );
}

#[test]
fn project_binding_composes_by_source_layout_under_the_binding_identity() {
    let cfg = Config::parse("version = 1\n[targets.t]\npath = \"~/d\"\nlayout = \"by-source\"\n")
        .expect("config parses");
    let layout = LayoutSpec::from(&cfg.targets["t"].layout());

    let inventory = inventory_of(["a.md"]);
    let binding = project_flat(
        "editor",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &layout,
    )
    .expect("projects under by-source layout");

    assert_eq!(
        destinations(&binding),
        ["editor/a.md"],
        "by-source layout prefixes each destination with the binding identity, target-relative"
    );
}

#[test]
fn project_binding_prefixed_layout_joins_identity_with_the_separator() {
    let cfg = Config::parse("version = 1\n[targets.t]\npath = \"~/d\"\nlayout = \"prefixed\"\n")
        .expect("config parses");
    let layout = LayoutSpec::from(&cfg.targets["t"].layout());

    let inventory = inventory_of(["a/deep.md"]);
    let binding = project_flat(
        "mysrc",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &layout,
    )
    .expect("projects under prefixed layout");

    assert_eq!(
        destinations(&binding),
        ["mysrc-a"],
        "prefixed layout joins identity `mysrc` and the collapsed published key `a` with the \
         default `-` separator (a wholly-taken `a/` collapses to `a`)"
    );
}

#[test]
fn project_binding_prefixed_layout_uses_the_configured_custom_separator() {
    let cfg = Config::parse(
        "version = 1\n[targets.t]\npath = \"~/d\"\nlayout = { type = \"prefixed\", separator = \"_\" }\n",
    )
    .expect("a prefixed layout with a custom separator parses");
    let layout = LayoutSpec::from(&cfg.targets["t"].layout());

    let inventory = inventory_of(["a/deep.md"]);
    let binding = project_flat(
        "mysrc",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &layout,
    )
    .expect("projects under a custom-separator prefixed layout");

    assert_eq!(
        destinations(&binding),
        ["mysrc_a"],
        "prefixed layout joins identity and the collapsed key with the CONFIGURED `_` separator \
         exactly, never the default `-`"
    );
}

#[test]
fn projected_artifact_keys_are_the_target_relative_destinations() {
    let inventory = inventory_of(["a.md", "b.md"]);
    let binding = project_flat(
        "dotfiles",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &flat_layout(),
    )
    .expect("projects");

    let mut keys = projected_artifact_keys(&binding);
    keys.sort();
    assert_eq!(
        keys,
        ["a.md", "b.md"],
        "under FLAT layout the published key and the destination coincide, one per artifact"
    );
}

#[test]
fn projected_artifact_keys_are_published_keys_distinct_from_layout_destinations() {
    let cfg = Config::parse("version = 1\n[targets.t]\npath = \"~/d\"\nlayout = \"by-source\"\n")
        .expect("config parses");
    let layout = LayoutSpec::from(&cfg.targets["t"].layout());

    let inventory = inventory_of(["a.md"]);
    let binding = project_flat(
        "editor",
        &inventory,
        &OfferSpec::implicit_full(),
        &TakeSpec::from_entries(None),
        &suffix_templates(),
        &layout,
    )
    .expect("projects under by-source layout");

    assert_eq!(
        destinations(&binding),
        ["editor/a.md"],
        "by-source layout composes the destination by prefixing the binding identity `editor`"
    );
    assert_eq!(
        projected_artifact_keys(&binding),
        ["a.md"],
        "the published key is the pre-layout artifact key `a.md`, NOT the layout-composed \
         destination `editor/a.md` — prune republishes keys, and a projection that returned the \
         composed destination as the key would double-apply the layout on the next sync"
    );
}

// ---- project_target aggregation and success shape ----

#[test]
fn project_target_returns_a_target_projection_with_artifacts_and_a_warnings_vector() {
    let inv_one = inventory_of(["a.md"]);
    let inv_two = inventory_of(["b.md"]);
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();

    let one_source = resolved_ref("one");
    let two_source = resolved_ref("two");
    let inputs = [
        binding_input(
            "one",
            &one_source,
            &inv_one,
            &offer,
            &take,
            &templates,
            &layout,
        ),
        binding_input(
            "two",
            &two_source,
            &inv_two,
            &offer,
            &take,
            &templates,
            &layout,
        ),
    ];

    let projection = project_target("home", &inputs).expect("two clean bindings project");
    assert_eq!(
        projection.target, "home",
        "the TargetProjection names the target it was built for"
    );
    assert_eq!(
        target_destinations(&projection),
        ["a.md", "b.md"],
        "the per-target artifacts are the union of every binding's projected destinations"
    );
    assert!(
        projection.warnings.is_empty(),
        "a target of clean bindings carries an empty warnings vector; got {:?}",
        projection.warnings
    );
}

// ---- ProjectionError variants (§9) ----

#[test]
fn project_binding_rejects_a_take_literal_outside_the_offer() {
    let inventory = inventory_of(["a.md"]);
    let take = TakeSpec::from_entries(Some(&[TakeEntry::Leaf("missing.md".to_owned())]));

    let err = project_flat(
        "dotfiles",
        &inventory,
        &OfferSpec::implicit_full(),
        &take,
        &suffix_templates(),
        &flat_layout(),
    )
    .expect_err("a take literal naming an unoffered leaf must be a ProjectionError");

    assert!(
        matches!(err, ProjectionError::LeafNotOffered { .. }),
        "an unoffered take literal is a structured LeafNotOffered, not a display-string match; \
         got {err:?}"
    );
}

#[test]
fn project_binding_force_collapse_blocked_under_link_is_a_collapse_error() {
    let inventory = inventory_of(["d/a.md", "d/secret.md"]);
    let offer = parsed_offer_spec("git = \"https://e.test/r.git\"\ninclude = [\"d/a.md\"]\n");
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();
    let source = resolved_ref("s");
    let input = BindingProjectionInput {
        identity: "s",
        source: &source,
        offer: &offer,
        inventory: &inventory,
        take: &take,
        collapse: CollapsePreference::from(Some(true)),
        history: false,
        materialization: link_policy(),
        layout: &layout,
        templates: &templates,
    };

    let err = project_binding(&input)
        .expect_err("a demanded collapse blocked by a within-dir exclude under link must error");

    assert!(
        matches!(err, ProjectionError::CollapseBlocked { .. }),
        "a blocked forced collapse is a structured CollapseBlocked; got {err:?}"
    );
}

#[test]
fn project_target_rejects_two_bindings_colliding_on_one_destination() {
    let inv = inventory_of(["x.md"]);
    let one = resolved_ref("one");
    let two = resolved_ref("two");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();

    let inputs = [
        binding_input("one", &one, &inv, &offer, &take, &templates, &layout),
        binding_input("two", &two, &inv, &offer, &take, &templates, &layout),
    ];
    let err = project_target("t", &inputs)
        .expect_err("two bindings landing on the same flat destination must be a hard error");
    assert!(
        matches!(err, ProjectionError::DuplicateDestination { .. }),
        "a cross-binding destination clash is a structured DuplicateDestination; got {err:?}"
    );
}

#[test]
fn project_target_rejects_a_case_differing_destination_pair_under_the_fold() {
    let upper = inventory_of(["X.md"]);
    let lower = inventory_of(["x.md"]);
    let one = resolved_ref("one");
    let two = resolved_ref("two");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();

    let inputs = [
        binding_input("one", &one, &upper, &offer, &take, &templates, &layout),
        binding_input("two", &two, &lower, &offer, &take, &templates, &layout),
    ];
    let err = project_target("t", &inputs).expect_err(
        "`X.md` and `x.md` collide under the simple-lowercase fold used across bindings",
    );
    assert!(
        matches!(err, ProjectionError::DuplicateDestination { .. }),
        "a fold-equal case-differing pair is a DuplicateDestination; got {err:?}"
    );
}

#[test]
fn project_target_rejects_an_nfc_equivalent_destination_pair() {
    let composed = inventory_of(["caf\u{00e9}.md"]);
    let decomposed = inventory_of(["cafe\u{0301}.md"]);
    let one = resolved_ref("one");
    let two = resolved_ref("two");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();

    let inputs = [
        binding_input("one", &one, &composed, &offer, &take, &templates, &layout),
        binding_input("two", &two, &decomposed, &offer, &take, &templates, &layout),
    ];
    let err = project_target("t", &inputs).expect_err(
        "composed `café` and decomposed `cafe`+combining-acute normalize to the same NFC form and \
         collide across bindings",
    );
    assert!(
        matches!(err, ProjectionError::DuplicateDestination { .. }),
        "an NFC-equivalent pair is a DuplicateDestination under the NFC+lowercase fold; got {err:?}"
    );
}

#[test]
fn project_target_admits_an_eszett_vs_ss_pair_the_simple_fold_keeps_distinct() {
    let eszett = inventory_of(["stra\u{00df}e.md"]);
    let ss = inventory_of(["strasse.md"]);
    let one = resolved_ref("one");
    let two = resolved_ref("two");
    let offer = OfferSpec::implicit_full();
    let take = TakeSpec::from_entries(None);
    let templates = suffix_templates();
    let layout = flat_layout();

    let inputs = [
        binding_input("one", &one, &eszett, &offer, &take, &templates, &layout),
        binding_input("two", &two, &ss, &offer, &take, &templates, &layout),
    ];
    let projection = project_target("t", &inputs).expect(
        "`stra\u{00df}e.md` and `strasse.md` stay DISTINCT under the projection's NFC + \
         simple-lowercase fold — only a FULL Unicode case-fold would merge \u{00df} into `ss`, \
         which would over-reject files that coexist on APFS/NTFS; the projection must ADMIT them",
    );
    assert_eq!(
        target_destinations(&projection),
        ["strasse.md", "stra\u{00df}e.md"],
        "both destinations survive as separate artifacts — no DuplicateDestination collapse"
    );
}

#[test]
fn project_target_rejects_a_leaf_nested_under_another_bindings_collapsed_dir() {
    let dir_inv = inventory_of(["d/a.md", "d/b.md"]);
    let leaf_inv = inventory_of(["x.md"]);
    let dir_source = resolved_ref("dir");
    let leaf_source = resolved_ref("leaf");
    let offer = OfferSpec::implicit_full();
    let dir_take = TakeSpec::from_entries(None);
    let leaf_take = TakeSpec::from_entries(Some(&[TakeEntry::Rename {
        src: "x.md".to_owned(),
        dest: "d/x.md".to_owned(),
    }]));
    let templates = suffix_templates();
    let layout = flat_layout();

    let inputs = [
        binding_input(
            "dir",
            &dir_source,
            &dir_inv,
            &offer,
            &dir_take,
            &templates,
            &layout,
        ),
        binding_input(
            "leaf",
            &leaf_source,
            &leaf_inv,
            &offer,
            &leaf_take,
            &templates,
            &layout,
        ),
    ];

    let err = project_target("t", &inputs).expect_err(
        "a collapsed dir at `d` and a leaf renamed to `d/x.md` overlap as ancestor/descendant — \
         `d` deploys as one directory artifact the leaf would escape into",
    );
    assert!(
        matches!(err, ProjectionError::DuplicateDestination { .. }),
        "a file-vs-directory-ancestor overlap is rejected as a DuplicateDestination; got {err:?}"
    );
}
