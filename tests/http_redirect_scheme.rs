use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use phora::http::download;
use phora::source::SourceError;

const PAYLOAD: &[u8] = b"phora-redirect-fixture\n";

/// One-shot `127.0.0.1` stub answering the first request with a 200 and `body`.
fn spawn_ok(body: &'static [u8]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            serve_ok(stream, body);
        }
    });
    port
}

fn serve_ok(mut stream: TcpStream, body: &[u8]) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 1024];
    let _ = stream.read(&mut buf);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// One-shot `127.0.0.1` stub answering the first request with a `302` to `location`.
fn spawn_redirect(location: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    port
}

fn spawn_relative_redirect(location: String, expected_path: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_request_line(&mut stream);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
        if let Ok((mut stream, _)) = listener.accept() {
            let path = read_request_line(&mut stream);
            if path.as_deref() == Some(expected_path.as_str()) {
                serve_ok(stream, PAYLOAD);
            } else {
                let response =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        }
    });
    port
}

fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    head.lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
}

#[test]
fn download_follows_relative_path_redirect() {
    let port = spawn_relative_redirect("payload.bin".to_string(), "/dir/payload.bin".to_string());
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("payload.bin");

    download(&format!("http://127.0.0.1:{port}/dir/start"), &dest)
        .expect("a relative-path Location must resolve against the current url");

    let written = std::fs::read(&dest).expect("read downloaded file");
    assert_eq!(
        written, PAYLOAD,
        "resolving `payload.bin` from `/dir/start` must fetch `/dir/payload.bin`"
    );
}

#[test]
fn download_follows_dot_segment_redirect() {
    let port =
        spawn_relative_redirect("../payload.bin".to_string(), "/dir/payload.bin".to_string());
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("payload.bin");

    download(&format!("http://127.0.0.1:{port}/dir/sub/start"), &dest)
        .expect("a dot-segment Location must be normalized against the current url");

    let written = std::fs::read(&dest).expect("read downloaded file");
    assert_eq!(
        written, PAYLOAD,
        "resolving `../payload.bin` from `/dir/sub/start` must fetch `/dir/payload.bin`"
    );
}

#[test]
fn download_refuses_redirect_to_disallowed_scheme() {
    let port = spawn_redirect("file:///etc/hostname".to_string());
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("leaked.bin");

    let err = download(&format!("http://127.0.0.1:{port}/start"), &dest)
        .expect_err("a redirect to file:// must be refused, not followed");

    match err {
        SourceError::Source(msg) => {
            let lower = msg.to_lowercase();
            assert!(
                lower.contains("file"),
                "refusal error must name the offending scheme (file://), got: {msg}"
            );
            assert!(
                lower.contains("scheme"),
                "refusal error must explain the failure is a disallowed redirect scheme, got: {msg}"
            );
        }
        other => {
            panic!("expected SourceError::Source for a disallowed redirect scheme, got: {other:?}")
        }
    }

    assert!(
        !dest.exists(),
        "a refused cross-scheme redirect must not write any file to dest"
    );
}

#[test]
fn download_follows_same_scheme_redirect() {
    let target = spawn_ok(PAYLOAD);
    let port = spawn_redirect(format!("http://127.0.0.1:{target}/payload.bin"));
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("payload.bin");

    download(&format!("http://127.0.0.1:{port}/start"), &dest)
        .expect("an http->http (same-scheme) redirect must still be followed");

    let written = std::fs::read(&dest).expect("read downloaded file");
    assert_eq!(
        written, PAYLOAD,
        "following a same-scheme redirect must deliver the target's body"
    );
}
