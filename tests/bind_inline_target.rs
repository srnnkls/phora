//! `bind` / `unbind` edit a target written as an inline table with dotted `sources` keys.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const MAIN: &str = "version = 1\n\n[sources.fas]\npath = \"/tmp/fas\"\n\n\
                    [sources.fas-rules]\npath = \"/tmp/fas\"\n\n\
                    [sources.claude]\npath = \"/tmp/claude\"\n";

const LOCAL_HEAD: &str = "# local overrides\n[targets]\n# the fas target\n";

fn phora(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .output()
        .expect("phora runs");
    assert!(
        out.status.success(),
        "phora {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(cwd.join("phora.local.toml")).expect("local config")
}

#[test]
fn bind_and_unbind_edit_dotted_sources_of_an_inline_local_target() {
    let cwd = TempDir::new().expect("cwd");
    std::fs::write(cwd.path().join("phora.toml"), MAIN).expect("main");
    std::fs::write(
        cwd.path().join("phora.local.toml"),
        format!(
            "{LOCAL_HEAD}fas = {{ path = \"/tmp/x/.config/fas\", sources.fas = {{ collapse = false }}, \
             sources.fas-rules = {{ take = [{{ \"fas/\" = \"rules\" }}], collapse = false }} }}\n"
        ),
    )
    .expect("local");

    assert_eq!(
        phora(cwd.path(), &["unbind", "--local", "--from", "fas", "fas"]),
        format!(
            "{LOCAL_HEAD}fas = {{ path = \"/tmp/x/.config/fas\", \
             sources.fas-rules = {{ take = [{{ \"fas/\" = \"rules\" }}], collapse = false }} }}\n"
        )
    );
    assert_eq!(
        phora(cwd.path(), &["bind", "--local", "--to", "fas", "claude"]),
        format!(
            "{LOCAL_HEAD}fas = {{ path = \"/tmp/x/.config/fas\", \
             sources.fas-rules = {{ take = [{{ \"fas/\" = \"rules\" }}], collapse = false }}, \
             sources.claude = {{}} }}\n"
        )
    );
    assert_eq!(
        phora(
            cwd.path(),
            &["unbind", "--local", "--from", "fas", "fas-rules", "claude"]
        ),
        format!("{LOCAL_HEAD}fas = {{ path = \"/tmp/x/.config/fas\", sources = {{}} }}\n"),
        "the emptied target keeps an explicit empty binding set"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("phora.toml")).expect("main"),
        MAIN,
        "--local leaves phora.toml untouched"
    );
}
