#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod common;

fn git(directory: &Path, args: &[&str]) {
    common::assert_sandboxed(directory);
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn phora_output(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .current_dir(directory)
        .args(args)
        .env("HOME", directory.join("home"))
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .output()
        .expect("run phora")
}

fn phora(directory: &Path, args: &[&str]) {
    let output = phora_output(directory, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_prunes_dangling_link(remove_target: bool) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let root = fixture.path();
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).expect("source directory");
    fs::create_dir_all(&destination).expect("destination directory");
    fs::write(source.join("tool"), b"managed\n").expect("source artifact");
    fs::write(source.join("keep"), b"source sentinel\n").expect("source sentinel");
    fs::write(destination.join("unmanaged"), b"user data\n").expect("unmanaged file");
    git(&source, &["init", "-b", "main", "--template="]);
    git(&source, &["add", "tool", "keep"]);
    git(&source, &["commit", "-m", "fixture"]);

    let base = format!(
        "version = 1\n[paths]\ncache = \"cache\"\nstate = \"state\"\n\
         [sources.live]\npath = {:?}\nbranch = \"main\"\ndeploy = \"link\"\ninclude = [\"tool\"]\n",
        source.to_str().expect("source path")
    );
    let target = format!(
        "[targets.dest]\npath = {:?}\nsources = [\"live\"]\n",
        destination.to_str().expect("destination path")
    );
    fs::write(root.join("phora.toml"), format!("{base}{target}")).expect("initial config");
    phora(root, &["sync", "--no-hooks"]);
    let link = destination.join("tool");
    assert!(
        fs::symlink_metadata(&link)
            .expect("deployed link")
            .is_symlink()
    );

    fs::remove_file(source.join("tool")).expect("remove link target");
    let remaining = if remove_target {
        base
    } else {
        format!("{base}{}", target.replace("[\"live\"]", "[]"))
    };
    fs::write(root.join("phora.toml"), remaining).expect("remove binding or target");
    phora(root, &["sync", "--no-hooks", "--prune"]);

    let error = fs::symlink_metadata(&link).expect_err("pruning must unlink the dangling entry");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        fs::read(source.join("keep")).expect("source sentinel survives"),
        b"source sentinel\n"
    );
    assert_eq!(
        fs::read(destination.join("unmanaged")).expect("unmanaged file survives"),
        b"user data\n"
    );
}

#[test]
fn prune_unbound_source_removes_dangling_link() {
    assert_prunes_dangling_link(false);
}

#[test]
fn prune_removed_target_removes_dangling_link() {
    assert_prunes_dangling_link(true);
}

fn shared_link_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let source = fixture.path().join("source");
    fs::create_dir_all(source.join("skills/adversarial")).expect("skill directory");
    fs::create_dir_all(source.join("replacement")).expect("replacement directory");
    fs::write(source.join("skills/adversarial/GUIDE.md"), b"guide\n").expect("skill");
    fs::write(source.join("replacement/GUIDE.md"), b"do not overwrite\n").expect("replacement");
    git(&source, &["init", "-b", "main", "--template="]);
    git(&source, &["add", "skills", "replacement"]);
    git(&source, &["commit", "-m", "shared source fixture"]);
    fixture
}

fn shared_link_config(root: &Path) -> String {
    let source = root.join("source");
    format!(
        "version = 1\n[paths]\ncache = \"cache\"\nstate = \"state\"\n\
         [sources.whole]\npath = {0:?}\nbranch = \"main\"\ndeploy = \"link\"\ninclude = [\"skills\"]\n\
         [sources.child]\npath = {0:?}\nbranch = \"main\"\ndeploy = \"link\"\nroot = \"skills\"\ninclude = [\"adversarial\"]\n\
         [sources.replacement]\npath = {0:?}\nbranch = \"main\"\nroot = \"replacement\"\n\
         [targets.all]\npath = {1:?}\nsources = [\"whole\"]\n",
        source.to_str().expect("source path"),
        root.join("all").to_str().expect("target path"),
    )
}

#[test]
fn repeated_sync_allows_distinct_links_to_shared_and_nested_sources() {
    let fixture = shared_link_fixture();
    let root = fixture.path();
    let config = format!(
        "{}\n[targets.same]\npath = {:?}\nsources = [\"whole\"]\n\
         [targets.child]\npath = {:?}\nsources = [\"child\"]\n",
        shared_link_config(root),
        root.join("same").to_str().expect("same target"),
        root.join("child").to_str().expect("child target"),
    );
    fs::write(root.join("phora.toml"), config).expect("config");
    phora(root, &["sync", "--no-hooks"]);
    phora(root, &["sync", "--no-hooks", "--frozen"]);
    for (destination, source) in [
        ("all/skills", "source/skills"),
        ("same/skills", "source/skills"),
        ("child/adversarial", "source/skills/adversarial"),
    ] {
        let destination = root.join(destination);
        assert!(
            fs::symlink_metadata(&destination)
                .expect("deployed entry")
                .is_symlink()
        );
        assert_eq!(
            fs::canonicalize(destination).expect("link target"),
            fs::canonicalize(root.join(source)).expect("source")
        );
    }
    assert_eq!(
        fs::read(root.join("source/skills/adversarial/GUIDE.md")).expect("source unchanged"),
        b"guide\n"
    );
}

#[test]
fn sync_rejects_descendant_target_through_a_parent_alias_of_a_deployed_link() {
    let fixture = shared_link_fixture();
    let root = fixture.path();
    let base = shared_link_config(root);
    fs::write(root.join("phora.toml"), &base).expect("initial config");
    phora(root, &["sync", "--no-hooks"]);
    std::os::unix::fs::symlink(root.join("all"), root.join("alias")).expect("parent alias");
    fs::write(
        root.join("phora.toml"),
        format!(
            "{base}\n[targets.nested]\npath = {:?}\nsources = [\"replacement\"]\n",
            root.join("alias/skills/adversarial")
                .to_str()
                .expect("nested target"),
        ),
    )
    .expect("overlapping config");

    let output = phora_output(root, &["sync", "--no-hooks"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("two targets project artifacts onto overlapping physical paths")
    );
    assert_eq!(
        fs::read(root.join("source/skills/adversarial/GUIDE.md")).expect("source unchanged"),
        b"guide\n"
    );
}

fn generated_file_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("generated fixture");
    let root = fixture.path();
    fs::create_dir_all(root.join("build/claude/skills/code")).expect("generated directory");
    for file in ["SKILL.md", "obsolete.txt"] {
        fs::write(root.join("build/claude/skills/code").join(file), file).expect("generated file");
    }
    fs::write(
        root.join("phora.toml"),
        r#"
[paths]
cache = "cache"
state = "state"
[sources.generated]
path = "build"
root = "claude"
deploy = "link"
[targets.claude]
path = "deployed"
sources.generated = { collapse = false }
"#,
    )
    .expect("configuration");
    phora(root, &["sync"]);
    fs::write(root.join("deployed/foreign.txt"), "keep me").expect("unmanaged file");
    fs::remove_file(root.join("build/claude/skills/code/obsolete.txt"))
        .expect("remove generated file");
    fixture
}

#[test]
fn prune_removes_a_dangling_generated_file_without_changing_the_binding() {
    let fixture = generated_file_fixture();
    let root = fixture.path();
    assert!(
        !phora_output(root, &["sync"]).status.success(),
        "pruning requires the flag"
    );
    phora(root, &["sync", "--prune"]);
    assert!(fs::symlink_metadata(root.join("deployed/skills/code/obsolete.txt")).is_err());
    assert_eq!(
        fs::read_to_string(root.join("deployed/foreign.txt")).expect("foreign file"),
        "keep me"
    );
    assert_eq!(
        fs::read_to_string(root.join("deployed/skills/code/SKILL.md")).expect("skill"),
        "SKILL.md"
    );
    phora(root, &["sync", "--frozen", "--no-hooks"]);
}

#[test]
fn prune_preserves_a_generated_link_replaced_by_a_file() {
    let fixture = generated_file_fixture();
    let root = fixture.path();
    let path = root.join("deployed/skills/code/obsolete.txt");
    fs::remove_file(&path).expect("remove old link");
    fs::write(&path, "user data").expect("replacement");
    assert!(!phora_output(root, &["sync", "--prune"]).status.success());
    assert_eq!(
        fs::read_to_string(&path).expect("preserved replacement"),
        "user data"
    );
}

#[test]
fn prune_preserves_a_file_installed_by_pre_deploy_over_a_dangling_link() {
    let fixture = generated_file_fixture();
    let root = fixture.path();
    let config = root.join("phora.toml");
    let body = fs::read_to_string(&config).expect("config");
    fs::write(config, format!("{body}\n[targets.claude.hooks]\npre_deploy = 'rm deployed/skills/code/obsolete.txt && printf user-data > deployed/skills/code/obsolete.txt'\n")).expect("hook");
    phora(root, &["sync", "--prune"]);
    assert_eq!(
        fs::read_to_string(root.join("deployed/skills/code/obsolete.txt"))
            .expect("preserved replacement"),
        "user-data"
    );
}

#[test]
fn prune_does_not_infer_a_missing_link_after_the_source_root_changes() {
    let fixture = generated_file_fixture();
    let root = fixture.path();
    fs::create_dir_all(root.join("build/codex/skills/code")).expect("new source root");
    fs::write(root.join("build/codex/skills/code/SKILL.md"), "Codex").expect("new skill");
    let config = root.join("phora.toml");
    let body = fs::read_to_string(&config)
        .expect("config")
        .replace("root = \"claude\"", "root = \"codex\"");
    fs::write(config, body).expect("change root");
    assert!(!phora_output(root, &["sync", "--prune"]).status.success());
    assert!(
        fs::symlink_metadata(root.join("deployed/skills/code/obsolete.txt"))
            .expect("old link remains")
            .is_symlink()
    );
}
