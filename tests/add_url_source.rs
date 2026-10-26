use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

use phora::source::http::serves_git;
use tempfile::TempDir;

const ADVERTISEMENT: &str = "application/x-git-upload-pack-advertisement";

/// One-shot `127.0.0.1` stub answering the first request with `status` and
/// `content_type`, reporting the request target it received.
fn spawn_stub(status: &'static str, content_type: &'static str) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_owned();
            let _ = sender.send(target);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (port, receiver)
}

fn received(receiver: &mpsc::Receiver<String>) -> String {
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("stub received a request")
}

#[test]
fn probe_requests_smart_http_discovery_under_the_url() {
    let (port, requests) = spawn_stub("200 OK", ADVERTISEMENT);
    serves_git(&format!("http://127.0.0.1:{port}/team/proj/")).expect("probe answers");
    assert_eq!(
        received(&requests),
        "/team/proj/info/refs?service=git-upload-pack"
    );
}

#[test]
fn probe_reads_an_upload_pack_advertisement_as_git() {
    let (port, _requests) = spawn_stub("200 OK", ADVERTISEMENT);
    assert!(serves_git(&format!("http://127.0.0.1:{port}/team/proj")).expect("probe answers"));
}

#[test]
fn probe_reads_a_credentials_challenge_as_git() {
    let (port, _requests) = spawn_stub("401 Unauthorized", "text/plain");
    assert!(serves_git(&format!("http://127.0.0.1:{port}/team/private")).expect("probe answers"));
}

#[test]
fn probe_reads_a_missing_discovery_endpoint_as_no_git() {
    let (port, _requests) = spawn_stub("404 Not Found", "text/html");
    assert!(!serves_git(&format!("http://127.0.0.1:{port}/pdf/paper")).expect("probe answers"));
}

#[test]
fn probe_reads_a_page_without_an_advertisement_as_no_git() {
    let (port, _requests) = spawn_stub("200 OK", "text/html; charset=utf-8");
    assert!(!serves_git(&format!("http://127.0.0.1:{port}/docs/page")).expect("probe answers"));
}

#[test]
fn probe_errs_when_no_server_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    serves_git(&format!("http://127.0.0.1:{port}/team/proj"))
        .expect_err("a refused connection is no answer");
}

fn run_add(cwd: &Path, url: &str) -> Output {
    let home = TempDir::new().expect("home tempdir");
    Command::new(env!("CARGO_BIN_EXE_phora"))
        .args(["add", url])
        .current_dir(cwd)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("xdg/cache"))
        .env("XDG_STATE_HOME", home.path().join("xdg/state"))
        .output()
        .expect("phora binary runs")
}

#[test]
fn add_declares_a_plain_http_download_as_a_url_source() {
    let (port, _requests) = spawn_stub("404 Not Found", "text/html");
    let cwd = TempDir::new().expect("cwd tempdir");
    let url = format!("http://127.0.0.1:{port}/papers/code-act.pdf");

    let out = run_add(cwd.path(), &url);
    assert!(
        out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let config = std::fs::read_to_string(cwd.path().join("phora.toml")).expect("read phora.toml");
    assert!(
        config.contains(&format!("url = \"{url}\"")),
        "the download must be declared as `url`, got:\n{config}"
    );
    assert!(
        !config.contains("git = "),
        "the download must not be declared as a git remote, got:\n{config}"
    );
}
