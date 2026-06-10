//! RED pins for the ARCH-002 `Selection` value object.

use std::path::Path;

use phora::kernel::Selection;

fn selection(include: &[&str], exclude: &[&str]) -> Selection {
    let inc: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
    let exc: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
    Selection::new(&inc, &exc).expect("patterns build into a selection")
}

fn file(sel: &Selection, rel: &str) -> bool {
    sel.selects_path(Path::new(rel), false)
}

fn dir(sel: &Selection, rel: &str) -> bool {
    sel.selects_path(Path::new(rel), true)
}

// ---- selects_path parity with PathMatcher::allows_path (no dotfile rule) ----

#[test]
fn double_star_bak_exclude_matches_root_level_file() {
    let sel = selection(&[], &["**/*.bak"]);
    assert!(
        !file(&sel, "foo.bak"),
        "root-level foo.bak must be excluded"
    );
}

#[test]
fn double_star_bak_exclude_matches_nested_file() {
    let sel = selection(&[], &["**/*.bak"]);
    assert!(
        !file(&sel, "sub/foo.bak"),
        "nested sub/foo.bak must be excluded"
    );
}

#[test]
fn double_star_bak_exclude_allows_non_bak_files() {
    let sel = selection(&[], &["**/*.bak"]);
    assert!(file(&sel, "foo.json"));
    assert!(file(&sel, "sub/foo.json"));
}

#[test]
fn anchored_exclude_matches_root_only() {
    let sel = selection(&[], &["/secret.txt"]);
    assert!(!file(&sel, "secret.txt"), "root secret.txt excluded");
    assert!(
        file(&sel, "sub/secret.txt"),
        "anchored pattern must NOT reach nested secret.txt"
    );
}

#[test]
fn unanchored_path_exclude_matches_at_root_and_any_depth() {
    let sel = selection(&[], &["editor/x.json"]);
    assert!(
        !file(&sel, "editor/x.json"),
        "root-level editor/x.json excluded"
    );
    assert!(
        !file(&sel, "nested/editor/x.json"),
        "nested editor/x.json excluded via `**/` variant"
    );
    assert!(file(&sel, "editor/y.json"), "non-matching file allowed");
}

#[test]
fn path_include_admits_only_matching_files() {
    let sel = selection(&["**/*.json"], &[]);
    assert!(file(&sel, "config.json"), "root json passes include");
    assert!(file(&sel, "sub/config.json"), "nested json passes include");
    assert!(!file(&sel, "config.yaml"), "non-json rejected by include");
}

#[test]
fn path_include_never_prunes_directories() {
    let sel = selection(&["**/*.json"], &[]);
    assert!(dir(&sel, "sub"), "include must not prune directories");
}

#[test]
fn directory_matching_anchored_exclude_is_pruned() {
    let sel = selection(&[], &["/build"]);
    assert!(
        !dir(&sel, "build"),
        "anchored exclude prunes root build dir"
    );
    assert!(dir(&sel, "src"), "unmatched directory is traversable");
    assert!(
        dir(&sel, "sub/build"),
        "anchored exclude does not reach nested build"
    );
}

#[test]
fn path_exclude_overrides_path_include() {
    let sel = selection(&["**/*.json"], &["**/secret.json"]);
    assert!(file(&sel, "ok.json"));
    assert!(
        !file(&sel, "secret.json"),
        "exclude wins over include at root"
    );
    assert!(
        !file(&sel, "sub/secret.json"),
        "exclude wins over include nested"
    );
}

// ---- selects_artifact parity (non-hidden names: PathMatcher::allows_artifact) ----

#[test]
fn empty_include_selects_all_non_hidden_artifacts() {
    let sel = selection(&[], &[]);
    assert!(sel.selects_artifact("editor"));
    assert!(sel.selects_artifact("anything-at-all"));
}

#[test]
fn artifact_include_selects_only_listed_names() {
    let sel = selection(&["editor", "lint"], &[]);
    assert!(sel.selects_artifact("editor"));
    assert!(sel.selects_artifact("lint"));
    assert!(!sel.selects_artifact("vim"));
}

#[test]
fn artifact_exclude_overrides_include() {
    let sel = selection(&["editor", "code-review"], &["code-*"]);
    assert!(sel.selects_artifact("editor"));
    assert!(!sel.selects_artifact("code-review"));
}

#[test]
fn artifact_level_exclude_filters_by_name() {
    let sel = selection(&[], &["code-*"]);
    assert!(!sel.selects_artifact("code-review"));
    assert!(sel.selects_artifact("editor"));
}

// ---- the dotfile gate (NEW — lives ONLY in Selection) ----

#[test]
fn literal_dot_include_selects_matching_hidden_dir() {
    let sel = selection(&[".config"], &[]);
    assert!(
        sel.selects_artifact(".config"),
        "include `.config` (literal leading dot) opts the hidden dir in"
    );
}

#[test]
fn dot_star_include_selects_all_hidden_dirs() {
    let sel = selection(&[".*"], &[]);
    assert!(
        sel.selects_artifact(".config"),
        "`.*` begins with `.` so it opts hidden names in"
    );
    assert!(
        sel.selects_artifact(".local"),
        "`.*` opts every top-level dotdir in"
    );
}

#[test]
fn star_include_does_not_select_hidden_dir() {
    let sel = selection(&["*"], &[]);
    assert!(
        !sel.selects_artifact(".config"),
        "globset `*` matches `.config`, but the gate requires a leading-dot include pattern (no dotglob)"
    );
}

#[test]
fn bare_glob_include_does_not_select_hidden_dir() {
    let sel = selection(&["code-*"], &[]);
    assert!(
        !sel.selects_artifact(".config"),
        "a bare-name glob without a leading dot must not opt a hidden name in"
    );
}

#[test]
fn empty_include_does_not_select_hidden_dir() {
    let sel = selection(&[], &[]);
    assert!(
        !sel.selects_artifact(".config"),
        "with no opt-in pattern present, hidden names stay out (today's default)"
    );
}

#[test]
fn unrelated_include_does_not_select_hidden_dir() {
    let sel = selection(&["editor"], &[]);
    assert!(
        !sel.selects_artifact(".config"),
        "an include that names only a non-hidden artifact must not opt a hidden name in"
    );
}

#[test]
fn exclude_still_wins_over_dotfile_opt_in() {
    let sel = selection(&[".*"], &[".config"]);
    assert!(
        !sel.selects_artifact(".config"),
        "exclude overrides the dotfile opt-in"
    );
    assert!(
        sel.selects_artifact(".local"),
        "a sibling hidden dir not excluded is still opted in by `.*`"
    );
}

#[test]
fn dotfile_gate_does_not_alter_non_hidden_membership() {
    let sel = selection(&[".config"], &[]);
    assert!(
        !sel.selects_artifact("editor"),
        "a dotfile-only include must not admit unrelated non-hidden names"
    );
}
