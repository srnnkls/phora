//! ARCH-004 zero-churn pin: `compute_digest` must stay byte-identical across the cleanup refactor.

use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use phora::config::Refspec;
use phora::kernel::{Selection, SourceName};
use phora::source::{GitBackend, SourceBackend};
use tempfile::TempDir;

fn sn(name: &str) -> SourceName {
    SourceName::from_str(name).expect("valid source name")
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
        .output()
        .expect("git runs");
    assert!(
        status.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

struct DigestFixture {
    _src: TempDir,
    _git_dir: TempDir,
    backend: GitBackend,
    url: String,
    commit: String,
}

fn build_fixture() -> DigestFixture {
    let src = TempDir::new().expect("src tempdir");
    let root = src.path();

    git(root, &["init", "-b", "main", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "core.autocrlf", "false"]);

    write(&root.join("editor/init.lua"), b"-- init\n");
    write(&root.join("editor/lua/opts.lua"), b"return {}\n");
    write(&root.join("README.md"), b"loose root file\n");
    write(&root.join(".config/settings.json"), b"{\"k\":1}\n");
    write(&root.join(".config/nested/app.toml"), b"a = 1\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "fixture"]);
    let out = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse runs");
    let commit = String::from_utf8(out.stdout)
        .expect("utf8 sha")
        .trim()
        .to_owned();

    let git_dir = TempDir::new().expect("git dir tempdir");
    let backend = GitBackend::new(git_dir.path().to_path_buf());
    let url = root.to_string_lossy().into_owned();
    backend
        .fetch(&sn("fixture"), &url)
        .expect("fetch builds mirror");

    DigestFixture {
        _src: src,
        _git_dir: git_dir,
        backend,
        url,
        commit,
    }
}

fn selection(include: &[&str], exclude: &[&str]) -> Selection {
    let to_vec = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    Selection::new(&to_vec(include), &to_vec(exclude)).expect("selection builds")
}

fn digest(
    fixture: &DigestFixture,
    root: Option<&str>,
    include: &[&str],
    exclude: &[&str],
) -> String {
    fixture
        .backend
        .compute_digest(
            &sn("fixture"),
            &fixture.url,
            &fixture.commit,
            root.map(Path::new),
            &selection(include, exclude),
        )
        .expect("compute_digest succeeds")
}

#[test]
fn resolve_matches_committed_head() {
    let fixture = build_fixture();
    let resolved = fixture
        .backend
        .resolve(
            &sn("fixture"),
            &fixture.url,
            &Refspec::Branch("main".to_owned()),
        )
        .expect("branch resolves");
    assert_eq!(
        resolved, fixture.commit,
        "main must resolve to the committed HEAD"
    );
}

#[test]
fn digest_pins_full_tree_with_root_loose_and_dotfile_dir() {
    let fixture = build_fixture();
    let value = digest(&fixture, None, &[], &[]);
    assert_eq!(
        value,
        "blake3:920b4081e48d0500d65bb36753595c9987d6c0e7f4d3acb7b33d4e8b083ed22c"
    );
}

#[test]
fn digest_pins_tree_without_root_loose_file() {
    let fixture = build_fixture();
    let value = digest(&fixture, None, &[], &["/README.md"]);
    assert_eq!(
        value,
        "blake3:f5f20404d2ea56d711ec3a47f1167803300055b14e1fbb7fd84a09d360eb300f"
    );
}

#[test]
fn digest_pins_tree_without_top_level_dotfile_dir() {
    let fixture = build_fixture();
    let value = digest(&fixture, None, &[], &["/.config"]);
    assert_eq!(
        value,
        "blake3:619b3b51342eb66551a9a8b5057d26b5762dadfd3a190ed9cedfb3ac1de03033"
    );
}

#[test]
fn digest_pins_editor_subtree_only() {
    let fixture = build_fixture();
    let value = digest(&fixture, None, &["editor/**"], &[]);
    assert_eq!(
        value,
        "blake3:a4bf87d47664c474e7f850b59219ea1c918a63ca3547150365e17d54c5d1a142"
    );
}

#[test]
fn digest_pins_dotfile_subtree_via_root() {
    let fixture = build_fixture();
    let value = digest(&fixture, Some(".config"), &[], &[]);
    assert_eq!(
        value,
        "blake3:27db1c96b7134c9bd8aeb6f1d8782a45c115faac31067eed4ac21bffd6ede7e5"
    );
}
