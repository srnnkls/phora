#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write fixture file");
}

struct Fixture {
    _home: TempDir,
    _src: TempDir,
    cwd: TempDir,
    home_path: PathBuf,
    xdg_cache: PathBuf,
    xdg_state: PathBuf,
}

fn build_fixture() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let src = TempDir::new().expect("src tempdir");
    let cwd = TempDir::new().expect("cwd tempdir");

    let src_path = src.path();
    git(src_path, &["init", "-b", "main", "."]);
    git(src_path, &["config", "user.email", "test@example.com"]);
    git(src_path, &["config", "user.name", "Test"]);
    git(src_path, &["config", "core.autocrlf", "false"]);
    write(&src_path.join("alpha/conf.txt"), b"alpha upstream\n");
    write(&src_path.join("zulu/conf.txt"), b"zulu upstream\n");
    git(src_path, &["add", "-A"]);
    git(src_path, &["commit", "-m", "fixture"]);

    let home_path = home.path().to_path_buf();
    let target_path = home_path.join("deploy");
    let xdg_cache = home_path.join("xdg/cache");
    let xdg_state = home_path.join("xdg/state");

    let config = format!(
        "version = 1\n\n[sources.dotfiles]\ngit = \"{src}\"\nbranch = \"main\"\n\
         include = [\"alpha\", \"zulu\"]\n\n[targets.home]\npath = \"{target}\"\n\
         sources = [\"dotfiles\"]\nlayout = \"flat\"\n",
        src = src_path.display(),
        target = target_path.display(),
    );
    write(&cwd.path().join("phora.toml"), config.as_bytes());

    Fixture {
        _home: home,
        _src: src,
        cwd,
        home_path,
        xdg_cache,
        xdg_state,
    }
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phora"))
            .args(args)
            .current_dir(self.cwd.path())
            .env("HOME", &self.home_path)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE")
            .output()
            .expect("phora binary runs")
    }

    fn deployed(&self, artifact: &str) -> PathBuf {
        self.home_path
            .join("deploy")
            .join(artifact)
            .join("conf.txt")
    }
}

#[test]
fn non_tty_skip_lines_keep_their_at_entry_apply_order_across_conflicts() {
    let fx = build_fixture();

    let first = fx.run(&["sync"]);
    assert!(
        first.status.success(),
        "premise: the initial deploy must succeed; stderr:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );

    std::fs::write(fx.deployed("alpha"), b"hand edit alpha\n").expect("edit alpha");
    std::fs::write(fx.deployed("zulu"), b"hand edit zulu\n").expect("edit zulu");

    let out = fx.run(&["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        out.status.success(),
        "a non-interactive Modified conflict is a warn+skip, NOT a failure; exit {:?}\nstderr:\n{stderr}",
        out.status.code()
    );

    let alpha_line = "phora: skipping locally modified dotfiles:alpha";
    let zulu_line = "phora: skipping locally modified dotfiles:zulu";
    let alpha_at = stderr
        .find(alpha_line)
        .unwrap_or_else(|| panic!("the alpha skip line must be emitted; stderr:\n{stderr}"));
    let zulu_at = stderr
        .find(zulu_line)
        .unwrap_or_else(|| panic!("the zulu skip line must be emitted; stderr:\n{stderr}"));
    assert!(
        alpha_at < zulu_at,
        "the two non-TTY warn+skip lines must stay in apply-pass (alpha-before-zulu) order — the \
         parity T033 must preserve when only the DECISION moves to preflight; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("use --force to overwrite"),
        "each skip must still advise --force; stderr:\n{stderr}"
    );

    assert_eq!(
        std::fs::read(fx.deployed("alpha")).expect("read alpha"),
        b"hand edit alpha\n",
        "a skipped Modified artifact must keep its local edit"
    );
    assert_eq!(
        std::fs::read(fx.deployed("zulu")).expect("read zulu"),
        b"hand edit zulu\n",
        "the second skipped artifact keeps its edit too"
    );
}
