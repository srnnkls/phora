use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn spawn_unauthorized() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"phora\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.flush();
        }
    });
    port
}

fn write_helper(dir: &Path, record: &Path) -> std::path::PathBuf {
    let helper = dir.join("record-helper");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s %s\\n' \"${{GCM_INTERACTIVE-unset}}\" \
             \"${{GIT_TERMINAL_PROMPT-unset}}\" >> '{}'\n",
            record.display()
        ),
    )
    .expect("write helper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
            .expect("chmod helper");
    }
    helper
}

fn sync_without_terminal(preset: &[(&str, &str)]) -> String {
    let root = tempfile::tempdir().expect("tempdir");
    let port = spawn_unauthorized();
    let record = root.path().join("record");
    let helper = write_helper(root.path(), &record);
    let gitconfig = root.path().join("gitconfig");
    std::fs::write(
        &gitconfig,
        format!(
            "[credential]\n\thelper =\n\thelper = {}\n",
            helper.display()
        ),
    )
    .expect("write gitconfig");
    let project = root.path().join("project");
    std::fs::create_dir(&project).expect("project dir");
    std::fs::write(
        project.join("phora.toml"),
        format!(
            "[sources.private]\ngit = \"http://127.0.0.1:{port}/private.git\"\n\n\
             [targets.out]\npath = \"out\"\nsources = [\"private\"]\n"
        ),
    )
    .expect("write phora.toml");

    let mut command = Command::new(env!("CARGO_BIN_EXE_phora"));
    command
        .args(["sync", "--no-progress"])
        .current_dir(&project)
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GCM_INTERACTIVE")
        .env_remove("GIT_TERMINAL_PROMPT")
        .envs(preset.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn phora");
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        if child.try_wait().expect("wait phora").is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("phora sync did not finish without a terminal");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(&record).unwrap_or_default()
}

#[test]
fn credential_helpers_may_not_prompt_without_a_terminal() {
    let seen = sync_without_terminal(&[]);
    assert!(!seen.is_empty(), "the credential helper was never asked");
    for line in seen.lines() {
        assert_eq!(line, "never 0");
    }
}

#[test]
fn explicit_prompt_settings_survive() {
    let seen = sync_without_terminal(&[("GCM_INTERACTIVE", "auto"), ("GIT_TERMINAL_PROMPT", "1")]);
    assert!(!seen.is_empty(), "the credential helper was never asked");
    for line in seen.lines() {
        assert_eq!(line, "auto 1");
    }
}
