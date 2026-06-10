//! RED pins for the ARCH-001 kernel value objects (`Digest`, `RelPath`, `Commit`).

use std::path::Path;
use std::str::FromStr;

use phora::kernel::{Commit, Digest, RelPath};

const SHA256_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const BLAKE3_HEX: &str = "2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0";

fn decode_hex32(hex: &str) -> [u8; 32] {
    let bytes = hex.as_bytes();
    assert_eq!(bytes.len(), 64, "fixture hex must be 64 chars");
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        let hi = (pair[0] as char).to_digit(16).expect("hex digit");
        let lo = (pair[1] as char).to_digit(16).expect("hex digit");
        *slot = u8::try_from(hi * 16 + lo).expect("hex pair fits in u8");
    }
    out
}

// ---- Digest: parse both algorithms ----

#[test]
fn digest_parses_sha256_into_thirty_two_bytes() {
    let digest = Digest::from_str(&format!("sha256:{SHA256_HEX}")).expect("valid sha256 digest");

    assert_eq!(
        digest.bytes(),
        &decode_hex32(SHA256_HEX)[..],
        "sha256 body must decode to the exact 32 raw bytes"
    );
}

#[test]
fn digest_parses_blake3_into_thirty_two_bytes() {
    let digest = Digest::from_str(&format!("blake3:{BLAKE3_HEX}")).expect("valid blake3 digest");

    assert_eq!(
        digest.bytes(),
        &decode_hex32(BLAKE3_HEX)[..],
        "blake3 body must decode to the exact 32 raw bytes"
    );
}

// ---- Digest: Display round-trips both old types' string forms ----

#[test]
fn digest_display_round_trips_sha256() {
    let s = format!("sha256:{SHA256_HEX}");
    assert_eq!(
        Digest::from_str(&s).expect("parses").to_string(),
        s,
        "sha256 Display must reproduce the exact `<algo>:<hex>` input"
    );
}

#[test]
fn digest_display_round_trips_blake3() {
    let s = format!("blake3:{BLAKE3_HEX}");
    assert_eq!(
        Digest::from_str(&s).expect("parses").to_string(),
        s,
        "blake3 Display must reproduce the form registry Digest renders (e.g. `digest blake3:<64hex>`)"
    );
}

// ---- Digest: rejections (strict 64-hex contract for BOTH algos) ----

#[test]
fn digest_rejects_unknown_algorithm() {
    let bad = format!("md5:{SHA256_HEX}");
    assert!(
        Digest::from_str(&bad).is_err(),
        "unknown algorithm prefix must be rejected"
    );
}

#[test]
fn digest_rejects_missing_prefix() {
    assert!(
        Digest::from_str(SHA256_HEX).is_err(),
        "a bare hex body with no `<algo>:` prefix must be rejected"
    );
}

#[test]
fn digest_rejects_short_body() {
    assert!(
        Digest::from_str("blake3:abc").is_err(),
        "a body shorter than 64 hex chars must be rejected (tightens registry Digest, which accepted any non-empty body)"
    );
}

#[test]
fn digest_rejects_wrong_length_body() {
    let too_long = "0".repeat(65);
    assert!(
        Digest::from_str(&format!("sha256:{too_long}")).is_err(),
        "a body that is not exactly 64 hex chars must be rejected"
    );
}

#[test]
fn digest_rejects_non_hex_body() {
    let non_hex = format!("z{}", &SHA256_HEX[1..]);
    assert!(
        Digest::from_str(&format!("sha256:{non_hex}")).is_err(),
        "non-hex characters in the body must be rejected"
    );
}

// ---- Commit: hex validation at the resolve boundary (gix ObjectId::from_hex parity) ----

#[test]
fn commit_accepts_forty_hex_sha1() {
    let sha1 = "0123456789abcdef0123456789abcdef01234567";
    let commit = Commit::from_str(sha1).expect("40-hex sha1 commit id parses");
    assert_eq!(
        commit.to_string(),
        sha1,
        "Commit must round-trip the 40-hex id it accepted"
    );
}

#[test]
fn commit_accepts_sixty_four_hex_sha256() {
    let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert!(
        Commit::from_str(sha256).is_ok(),
        "a 64-hex sha256 commit id must parse (gix supports sha256 repos)"
    );
}

#[test]
fn commit_rejects_wrong_length() {
    assert!(
        Commit::from_str("abc123").is_err(),
        "a hex string that is neither 40 nor 64 chars must be rejected"
    );
}

#[test]
fn commit_rejects_non_hex() {
    let non_hex = "zzz3456789abcdef0123456789abcdef01234567";
    assert!(
        Commit::from_str(non_hex).is_err(),
        "non-hex characters must be rejected at the resolve boundary"
    );
}

#[test]
fn commit_accepts_uppercase_and_canonicalizes_to_lowercase() {
    let upper = "0123456789ABCDEF0123456789abcdef01234567";
    let commit = Commit::from_str(upper).expect("uppercase hex accepted (gix parity)");
    assert_eq!(
        commit.to_string(),
        upper.to_lowercase(),
        "uppercase hex must canonicalize to lowercase (gix ObjectId::from_hex parity)"
    );
}

// ---- RelPath: normalized at construction, parity with current behavior ----

#[test]
fn relpath_strips_leading_dot_segment() {
    let rel = RelPath::from_str("./a/b").expect("a leading `./` relative path normalizes");
    assert_eq!(
        rel.as_path(),
        Path::new("a/b"),
        "a leading `./` must normalize away to `a/b`"
    );
}

#[test]
fn relpath_drops_internal_dot_segments() {
    let rel = RelPath::from_str("a/./b").expect("an internal `.` relative path normalizes");
    assert_eq!(
        rel.as_path(),
        Path::new("a/b"),
        "internal `.` segments must be removed"
    );
}

#[test]
fn relpath_normalizes_a_plain_relative_path_to_itself() {
    let rel = RelPath::from_str("a/b/c").expect("a plain relative path normalizes");
    assert_eq!(rel.as_path(), Path::new("a/b/c"));
}

#[test]
fn relpath_rejects_absolute_path() {
    assert!(
        RelPath::from_str("/etc/passwd").is_err(),
        "an absolute path must be rejected by a relative-path value object"
    );
}

#[test]
fn relpath_rejects_parent_escape() {
    assert!(
        RelPath::from_str("../escape").is_err(),
        "a path that escapes its root via leading `..` must be rejected (parity with safe_component traversal guard)"
    );
}

#[test]
fn relpath_display_renders_normalized_form() {
    let rel = RelPath::from_str("./x/y").expect("normalizes");
    assert_eq!(
        rel.to_string(),
        "x/y",
        "Display must render the normalized form, not the raw input"
    );
}
