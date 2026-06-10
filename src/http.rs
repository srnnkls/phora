//! HTTP download and digest verification for url-mode sources.

use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};
use crate::kernel::{Algo, Digest};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_TIMEOUT: Duration = Duration::from_mins(5);

/// Streams the body at `url` into `dest`, following redirects.
///
/// # Errors
///
/// Returns [`Error::Source`] on a non-2xx status (message names the status and
/// url), on transport/connection failure, or on a filesystem error writing `dest`.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .build()
        .new_agent();

    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) => {
            return Err(Error::Source(format!(
                "GET {url} failed with status {code}"
            )));
        }
        Err(err) => return Err(Error::Source(format!("GET {url} failed: {err}"))),
    };

    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(dest).map_err(|err| {
        Error::Source(format!(
            "creating {} for GET {url} failed: {err}",
            dest.display()
        ))
    })?;
    if let Err(err) = std::io::copy(&mut reader, &mut file) {
        let _ = std::fs::remove_file(dest);
        return Err(Error::Source(format!(
            "writing GET {url} to {} failed: {err}",
            dest.display()
        )));
    }
    Ok(())
}

/// Verifies `bytes` against `expected`, hashing with the matching algorithm.
///
/// # Errors
///
/// Returns [`Error::Source`] on mismatch; the message names both the expected
/// and the actual digest as lowercase 64-char hex.
pub fn verify_digest(bytes: &[u8], expected: &Digest) -> Result<()> {
    let (algo, actual) = match expected.algo() {
        Algo::Sha256 => ("sha256", Sha256::digest(bytes).to_vec()),
        Algo::Blake3 => ("blake3", blake3::hash(bytes).as_bytes().to_vec()),
    };

    if actual == expected.bytes() {
        return Ok(());
    }

    Err(Error::Source(format!(
        "{algo} digest mismatch: expected {}, got {}",
        hex_lower(expected.bytes()),
        hex_lower(&actual),
    )))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

#[cfg(test)]
mod tests {
    use crate::error::Error;
    use crate::http::{download, verify_digest};
    use crate::kernel::Digest;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::str::FromStr as _;
    use std::time::Duration;

    const PAYLOAD: &[u8] = b"phora-fixture-payload\n";
    const PAYLOAD_SHA256: &str = "284a0c0f808e5d7e62d7576fa6bcd4d55eb0160d26a7d8eb0333eb3972b27e13";
    const PAYLOAD_BLAKE3: &str = "0dfb206cd5f062609525e25cc19489d2ae35f7f94d8172a9f4285f124f8e9092";

    /// One-shot `127.0.0.1` HTTP stub; accept thread detached so a non-connecting download never hangs the test on join.
    struct CannedServer {
        port: u16,
    }

    impl CannedServer {
        fn spawn(status_line: &'static str, body: &'static [u8]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            let port = listener.local_addr().expect("local addr").port();
            std::thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    Self::serve(stream, status_line, body);
                }
            });
            Self { port }
        }

        fn serve(mut stream: TcpStream, status_line: &str, body: &[u8]) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }
    }

    #[test]
    fn download_writes_body_to_dest() {
        let server = CannedServer::spawn("200 OK", PAYLOAD);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("payload.bin");

        download(&server.url("/payload.bin"), &dest).expect("download should succeed on 200");

        let written = std::fs::read(&dest).expect("read downloaded file");
        assert_eq!(
            written, PAYLOAD,
            "downloaded bytes must equal the served body exactly"
        );
    }

    #[test]
    fn download_to_unwritable_dest_errors_as_source() {
        let server = CannedServer::spawn("200 OK", PAYLOAD);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("missing_subdir").join("file.bin");

        let err = download(&server.url("/x"), &dest)
            .expect_err("creating a file under a missing parent must error");

        assert!(
            matches!(err, Error::Source(_)),
            "file-creation failure must map to Error::Source, got: {err:?}"
        );
    }

    #[test]
    fn download_non_2xx_errors_with_status() {
        let server = CannedServer::spawn("404 Not Found", b"nope");
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("missing.bin");

        let err =
            download(&server.url("/missing.bin"), &dest).expect_err("404 must be an error, not Ok");

        match err {
            Error::Source(msg) => assert!(
                msg.contains("404"),
                "non-2xx error message must include the status code, got: {msg}"
            ),
            other => panic!("expected Error::Source for non-2xx, got: {other:?}"),
        }
    }

    #[test]
    fn download_connection_failure_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("unreachable.bin");

        let err = download("http://127.0.0.1:1/x", &dest)
            .expect_err("connecting to a refused port must error");

        assert!(
            matches!(err, Error::Source(_)),
            "connection failure must map to Error::Source, got: {err:?}"
        );
    }

    #[test]
    fn verify_digest_accepts_matching_sha256() {
        let digest = Digest::from_str(&format!("sha256:{PAYLOAD_SHA256}")).expect("valid sha256");
        verify_digest(PAYLOAD, &digest).expect("matching sha256 must verify");
    }

    #[test]
    fn blake3_fixture_matches_payload() {
        let computed = blake3::hash(PAYLOAD).to_hex();
        assert_eq!(
            computed.as_str(),
            PAYLOAD_BLAKE3,
            "blake3 fixture must match the payload"
        );
    }

    #[test]
    fn verify_digest_accepts_matching_blake3() {
        let digest = Digest::from_str(&format!("blake3:{PAYLOAD_BLAKE3}")).expect("valid blake3");
        verify_digest(PAYLOAD, &digest).expect("matching blake3 must verify");
    }

    #[test]
    fn verify_digest_rejects_mismatch_naming_expected_and_actual() {
        let wrong_hex = "0".repeat(64);
        let digest = Digest::from_str(&format!("sha256:{wrong_hex}")).expect("valid 64-hex sha256");

        let err = verify_digest(PAYLOAD, &digest).expect_err("wrong sha256 must reject");

        match err {
            Error::Source(msg) => {
                assert!(
                    msg.contains(&wrong_hex),
                    "mismatch error must name the expected hex `{wrong_hex}`, got: {msg}"
                );
                assert!(
                    msg.contains(PAYLOAD_SHA256),
                    "mismatch error must name the actual hex `{PAYLOAD_SHA256}`, got: {msg}"
                );
            }
            other => panic!("expected Error::Source on mismatch, got: {other:?}"),
        }
    }

    #[test]
    fn verify_digest_rejects_blake3_mismatch_naming_expected_and_actual() {
        let wrong_hex = "0".repeat(64);
        let digest = Digest::from_str(&format!("blake3:{wrong_hex}")).expect("valid 64-hex blake3");

        let err = verify_digest(PAYLOAD, &digest).expect_err("wrong blake3 must reject");

        match err {
            Error::Source(msg) => {
                assert!(
                    msg.contains(&wrong_hex),
                    "mismatch error must name the expected hex `{wrong_hex}`, got: {msg}"
                );
                assert!(
                    msg.contains(PAYLOAD_BLAKE3),
                    "mismatch error must name the actual hex `{PAYLOAD_BLAKE3}`, got: {msg}"
                );
            }
            other => panic!("expected Error::Source on mismatch, got: {other:?}"),
        }
    }
}
