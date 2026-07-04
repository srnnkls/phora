//! HTTP download and digest verification for url-mode sources.

use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use ureq::http::{self, Uri, header};

use crate::kernel::{Algo, Digest};
use crate::source::SourceError;

type Result<T> = std::result::Result<T, SourceError>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_REDIRECTS: u32 = 10;

/// Streams the body at `url` into `dest`, following redirects whose scheme is `https`,
/// or `http` when the original `url` was itself `http`, up to [`MAX_REDIRECTS`] hops.
///
/// # Errors
///
/// Returns [`SourceError::Source`] on a non-2xx status (message names the status and
/// url), on a redirect to a disallowed scheme (message names the scheme), on exceeding
/// the redirect limit, on transport/connection failure, or on a filesystem error
/// writing `dest`.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .build()
        .new_agent();

    let origin: Uri = url
        .parse()
        .map_err(|err| SourceError::Source(format!("GET {url} has an invalid url: {err}")))?;
    let origin_is_http = origin
        .scheme_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("http"));

    let mut current = origin;
    let mut hops = 0u32;
    let response = loop {
        let response = match agent.get(current.clone()).call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(code)) => {
                return Err(SourceError::Source(format!(
                    "GET {url} failed with status {code}"
                )));
            }
            Err(err) => return Err(SourceError::Source(format!("GET {url} failed: {err}"))),
        };

        let location = response
            .status()
            .is_redirection()
            .then(|| response.headers().get(header::LOCATION))
            .flatten();
        let Some(location) = location else {
            break response;
        };

        let location = location.to_str().map_err(|err| {
            SourceError::Source(format!(
                "GET {url} redirect has a non-ascii Location: {err}"
            ))
        })?;

        let scheme = scheme_of(location).map_or_else(
            || if origin_is_http { "http" } else { "https" }.to_string(),
            str::to_ascii_lowercase,
        );
        let allowed = scheme == "https" || (scheme == "http" && origin_is_http);
        if !allowed {
            return Err(SourceError::Source(format!(
                "GET {url} refused redirect to disallowed scheme `{scheme}`"
            )));
        }

        let next = resolve_redirect(&current, location).map_err(|err| {
            SourceError::Source(format!(
                "GET {url} redirect to `{location}` is not a valid url: {err}"
            ))
        })?;

        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(SourceError::Source(format!(
                "GET {url} exceeded {MAX_REDIRECTS} redirects"
            )));
        }
        current = next;
    };

    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(dest).map_err(|err| {
        SourceError::Source(format!(
            "creating {} for GET {url} failed: {err}",
            dest.display()
        ))
    })?;
    if let Err(err) = std::io::copy(&mut reader, &mut file) {
        let _ = std::fs::remove_file(dest);
        return Err(SourceError::Source(format!(
            "writing GET {url} to {} failed: {err}",
            dest.display()
        )));
    }
    Ok(())
}

fn scheme_of(location: &str) -> Option<&str> {
    let colon = location.find(':')?;
    let scheme = &location[..colon];
    let mut chars = scheme.chars();
    let starts_alpha = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (starts_alpha && rest_ok).then_some(scheme)
}

fn resolve_redirect(base: &Uri, location: &str) -> std::result::Result<Uri, http::uri::InvalidUri> {
    if scheme_of(location).is_some() {
        return location.parse();
    }

    let base_scheme = base.scheme_str().unwrap_or("https");
    let reference = location.split('#').next().unwrap_or_default();

    if reference.starts_with("//") {
        return format!("{base_scheme}:{reference}").parse();
    }

    let (ref_path, ref_query) = match reference.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (reference, None),
    };

    let authority = base
        .authority()
        .map(ToString::to_string)
        .unwrap_or_default();
    let (path, query) = if ref_path.is_empty() {
        (base.path().to_string(), ref_query.or_else(|| base.query()))
    } else if ref_path.starts_with('/') {
        (remove_dot_segments(ref_path), ref_query)
    } else {
        (
            remove_dot_segments(&merge_paths(base.path(), ref_path)),
            ref_query,
        )
    };

    let mut resolved = format!("{base_scheme}://{authority}{path}");
    if let Some(query) = query {
        resolved.push('?');
        resolved.push_str(query);
    }
    resolved.parse()
}

/// RFC 3986 §5.2.3 path merge; `base` always has an authority (it is an absolute url).
fn merge_paths(base_path: &str, ref_path: &str) -> String {
    match base_path.rfind('/') {
        Some(slash) => format!("{}{ref_path}", &base_path[..=slash]),
        None => format!("/{ref_path}"),
    }
}

/// RFC 3986 §5.2.4 dot-segment removal.
fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let trailing_slash = path.ends_with('/');
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let mut resolved = String::with_capacity(path.len());
    if path.starts_with('/') {
        resolved.push('/');
    }
    resolved.push_str(&out.join("/"));
    if trailing_slash && !resolved.ends_with('/') {
        resolved.push('/');
    }
    resolved
}

/// Verifies `bytes` against `expected`, hashing with the matching algorithm.
///
/// # Errors
///
/// Returns [`SourceError::Source`] on mismatch; the message names both the expected
/// and the actual digest as lowercase 64-char hex.
pub fn verify_digest(bytes: &[u8], expected: &Digest) -> Result<()> {
    let (algo, actual) = match expected.algo() {
        Algo::Sha256 => ("sha256", Sha256::digest(bytes).to_vec()),
        Algo::Blake3 => ("blake3", blake3::hash(bytes).as_bytes().to_vec()),
    };

    if actual == expected.bytes() {
        return Ok(());
    }

    Err(SourceError::Source(format!(
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
    use crate::http::{download, verify_digest};
    use crate::kernel::Digest;
    use crate::source::SourceError;
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
            matches!(err, SourceError::Source(_)),
            "file-creation failure must map to SourceError::Source, got: {err:?}"
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
            SourceError::Source(msg) => assert!(
                msg.contains("404"),
                "non-2xx error message must include the status code, got: {msg}"
            ),
            other => panic!("expected SourceError::Source for non-2xx, got: {other:?}"),
        }
    }

    #[test]
    fn download_connection_failure_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("unreachable.bin");

        let err = download("http://127.0.0.1:1/x", &dest)
            .expect_err("connecting to a refused port must error");

        assert!(
            matches!(err, SourceError::Source(_)),
            "connection failure must map to SourceError::Source, got: {err:?}"
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
            SourceError::Source(msg) => {
                assert!(
                    msg.contains(&wrong_hex),
                    "mismatch error must name the expected hex `{wrong_hex}`, got: {msg}"
                );
                assert!(
                    msg.contains(PAYLOAD_SHA256),
                    "mismatch error must name the actual hex `{PAYLOAD_SHA256}`, got: {msg}"
                );
            }
            other => panic!("expected SourceError::Source on mismatch, got: {other:?}"),
        }
    }

    #[test]
    fn verify_digest_rejects_blake3_mismatch_naming_expected_and_actual() {
        let wrong_hex = "0".repeat(64);
        let digest = Digest::from_str(&format!("blake3:{wrong_hex}")).expect("valid 64-hex blake3");

        let err = verify_digest(PAYLOAD, &digest).expect_err("wrong blake3 must reject");

        match err {
            SourceError::Source(msg) => {
                assert!(
                    msg.contains(&wrong_hex),
                    "mismatch error must name the expected hex `{wrong_hex}`, got: {msg}"
                );
                assert!(
                    msg.contains(PAYLOAD_BLAKE3),
                    "mismatch error must name the actual hex `{PAYLOAD_BLAKE3}`, got: {msg}"
                );
            }
            other => panic!("expected SourceError::Source on mismatch, got: {other:?}"),
        }
    }
}
