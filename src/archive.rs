//! Archive extraction for url-mode sources.

use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use gix::object::tree::EntryKind;

use crate::error::{Error, Result};
use crate::source::safe_component;

#[derive(Debug)]
pub struct ExtractedEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub data: Vec<u8>,
}

/// Validates an archive entry name as a relative, traversal-free path.
pub fn safe_archive_path(raw: &str) -> Result<PathBuf> {
    let reject = |why: &str| Err(Error::Source(format!("unsafe archive path {raw:?}: {why}")));
    if raw.is_empty() {
        return reject("empty");
    }
    if raw.starts_with('/') {
        return reject("absolute");
    }
    if raw.contains('\\') {
        return reject("backslash");
    }
    if raw.contains('\0') {
        return reject("nul byte");
    }
    if looks_like_windows_drive(raw) {
        return reject("windows drive root");
    }
    let mut path = PathBuf::new();
    for segment in raw.split('/') {
        path.push(safe_component(segment)?);
    }
    Ok(path)
}

fn looks_like_windows_drive(raw: &str) -> bool {
    let mut chars = raw.chars();
    matches!((chars.next(), chars.next()), (Some(c), Some(':')) if c.is_ascii_alphabetic())
}

/// Decompression-bomb ceiling: total extracted bytes across all entries. 1 GiB is a generous cap for release assets.
const MAX_EXTRACTED_BYTES: u64 = 1 << 30;

/// Extracts an archive (or raw file) into normalized entries.
pub fn extract(archive_path: &Path, url: &str) -> Result<Vec<ExtractedEntry>> {
    let bytes = std::fs::read(archive_path)?;
    extract_archive(&bytes, url, MAX_EXTRACTED_BYTES)
}

fn extract_archive(bytes: &[u8], url: &str, max_total: u64) -> Result<Vec<ExtractedEntry>> {
    let entries = match detect_format(bytes) {
        Format::Gzip => extract_tar(GzDecoder::new(bytes), max_total)?,
        Format::Tar => extract_tar(bytes, max_total)?,
        Format::Zip => extract_zip(bytes, max_total)?,
        Format::Raw => return Ok(vec![raw_entry(bytes, url)?]),
    };
    Ok(strip_single_top_level(entries))
}

enum Format {
    Gzip,
    Zip,
    Tar,
    Raw,
}

fn detect_format(bytes: &[u8]) -> Format {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        Format::Gzip
    } else if bytes.starts_with(b"PK\x03\x04") {
        Format::Zip
    } else if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        Format::Tar
    } else {
        Format::Raw
    }
}

fn raw_entry(bytes: &[u8], url: &str) -> Result<ExtractedEntry> {
    let basename = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or(url);
    Ok(ExtractedEntry {
        path: safe_archive_path(basename)?,
        kind: EntryKind::Blob,
        data: bytes.to_vec(),
    })
}

fn extract_tar<R: Read>(reader: R, max_total: u64) -> Result<Vec<ExtractedEntry>> {
    use tar::EntryType;
    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let header = entry.header();
        let entry_type = header.entry_type();
        if entry_type.is_dir() {
            continue;
        }
        let mode = header.mode()?;
        let name = entry
            .path()?
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Source("non-utf8 archive entry name".to_owned()))?;
        let path = safe_archive_path(&name)?;

        let (kind, data) = match entry_type {
            EntryType::Symlink => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| Error::Source(format!("symlink {name:?} has no target")))?;
                let target = target.to_str().ok_or_else(|| {
                    Error::Source(format!("non-utf8 symlink target for {name:?}"))
                })?;
                (EntryKind::Link, target.as_bytes().to_vec())
            }
            EntryType::Regular | EntryType::Continuous => {
                let data = read_capped(&mut entry, &mut total, max_total)?;
                (mode_to_blob_kind(mode), data)
            }
            other => {
                return Err(Error::Source(format!(
                    "unsupported archive entry type {other:?} for {name:?}"
                )));
            }
        };
        entries.push(ExtractedEntry { path, kind, data });
    }
    Ok(entries)
}

fn extract_zip(bytes: &[u8], max_total: u64) -> Result<Vec<ExtractedEntry>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| Error::Source(format!("invalid zip archive: {e}")))?;
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| Error::Source(format!("reading zip entry {index}: {e}")))?;
        if file.is_dir() {
            continue;
        }
        let name = std::str::from_utf8(file.name_raw())
            .map_err(|_| Error::Source(format!("non-utf8 zip entry name at index {index}")))?
            .to_owned();
        if name.ends_with('/') {
            continue;
        }
        let path = safe_archive_path(&name)?;

        let data = read_capped(&mut file, &mut total, max_total)?;
        let kind = match file.unix_mode() {
            Some(mode) if mode & 0o170_000 == 0o120_000 => EntryKind::Link,
            Some(mode) => mode_to_blob_kind(mode),
            None => EntryKind::Blob,
        };
        entries.push(ExtractedEntry { path, kind, data });
    }
    Ok(entries)
}

/// Caps the actual decompressed bytes read, not the attacker-controlled header size.
fn read_capped<R: Read>(reader: &mut R, total: &mut u64, max_total: u64) -> Result<Vec<u8>> {
    let remaining = max_total.saturating_sub(*total);
    let mut data = Vec::new();
    reader.take(remaining + 1).read_to_end(&mut data)?;
    *total += data.len() as u64;
    if *total > max_total {
        return Err(Error::Source(format!(
            "archive exceeds maximum extracted size ({MAX_EXTRACTED_BYTES} bytes)"
        )));
    }
    Ok(data)
}

fn mode_to_blob_kind(mode: u32) -> EntryKind {
    if mode & 0o111 != 0 {
        EntryKind::BlobExecutable
    } else {
        EntryKind::Blob
    }
}

fn strip_single_top_level(mut entries: Vec<ExtractedEntry>) -> Vec<ExtractedEntry> {
    let top_component = |path: &Path| {
        let mut iter = path.components();
        let first = iter.next();
        let has_more = iter.next().is_some();
        first.filter(|_| has_more).map(|c| c.as_os_str().to_owned())
    };
    let Some(prefix) = entries.first().and_then(|e| top_component(&e.path)) else {
        return entries;
    };
    let strippable = entries
        .iter()
        .all(|e| top_component(&e.path).as_deref() == Some(prefix.as_os_str()));
    if !strippable {
        return entries;
    }
    for entry in &mut entries {
        if let Ok(stripped) = entry.path.strip_prefix(&prefix) {
            entry.path = stripped.to_path_buf();
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use crate::archive::{ExtractedEntry, extract, extract_archive, safe_archive_path};
    use crate::error::Error;
    use gix::object::tree::EntryKind;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn write_tar(
        dir: &Path,
        name: &str,
        build: impl FnOnce(&mut tar::Builder<Vec<u8>>),
    ) -> PathBuf {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        let bytes = builder.into_inner().expect("finish tar");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write tar file");
        path
    }

    fn append_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8], mode: u32) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, path, data)
            .expect("append regular file");
    }

    fn append_raw_named(builder: &mut tar::Builder<Vec<u8>>, name: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        let name_bytes = name.as_bytes();
        // raw byte field write bypasses set_path's `..` rejection
        let gnu = header.as_gnu_mut().expect("gnu header");
        gnu.name[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        builder
            .append(&header, data)
            .expect("append raw-named entry");
    }

    fn by_path(entries: Vec<ExtractedEntry>) -> BTreeMap<PathBuf, ExtractedEntry> {
        entries.into_iter().map(|e| (e.path.clone(), e)).collect()
    }

    // ---- safe_archive_path (pure) ----

    #[test]
    fn safe_archive_path_accepts_nested_relative() {
        let cleaned = safe_archive_path("a/b/c").expect("a nested relative path must be accepted");
        assert_eq!(
            cleaned,
            PathBuf::from("a/b/c"),
            "a/b/c must clean to the same relative path"
        );
    }

    #[test]
    fn safe_archive_path_rejects_traversal_and_unsafe_segments() {
        for raw in [
            "a/../b",
            "../b",
            "/abs/path",
            "a\\b",
            "a/b\0c",
            "",
            "a//b",
            ".",
            "a/./b",
        ] {
            let result = safe_archive_path(raw);
            assert!(
                matches!(result, Err(Error::Source(_))),
                "{raw:?} must be rejected as Error::Source, got: {result:?}"
            );
        }
    }

    // ---- extract: formats ----

    #[test]
    fn extract_plain_tar_yields_blobs_with_exact_paths_and_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "src.tar", |b| {
            append_file(b, "foo.txt", b"hello", 0o644);
            append_file(b, "dir/bar.txt", b"world", 0o644);
        });

        let entries = by_path(
            extract(&archive, "https://example.com/src.tar").expect("plain tar must extract"),
        );

        assert_eq!(entries.len(), 2, "two files, directories not emitted");

        let foo = entries.get(Path::new("foo.txt")).expect("foo.txt entry");
        assert_eq!(foo.kind, EntryKind::Blob, "foo.txt is a regular blob");
        assert_eq!(foo.data, b"hello", "foo.txt content");

        let bar = entries
            .get(Path::new("dir/bar.txt"))
            .expect("dir/bar.txt entry");
        assert_eq!(bar.kind, EntryKind::Blob, "dir/bar.txt is a regular blob");
        assert_eq!(bar.data, b"world", "dir/bar.txt content");
    }

    #[test]
    fn extract_tar_gz_matches_plain_tar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = tar::Builder::new(Vec::new());
        append_file(&mut builder, "foo.txt", b"hello", 0o644);
        append_file(&mut builder, "dir/bar.txt", b"world", 0o644);
        let tar_bytes = builder.into_inner().expect("finish tar");

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip tar bytes");
        let gz_bytes = encoder.finish().expect("finish gzip");
        let archive = dir.path().join("src.tar.gz");
        std::fs::write(&archive, gz_bytes).expect("write tar.gz");

        let entries = by_path(
            extract(&archive, "https://example.com/src.tar.gz").expect("tar.gz must extract"),
        );

        assert_eq!(entries.len(), 2, "two files from the gzipped tar");
        assert_eq!(
            entries.get(Path::new("foo.txt")).expect("foo.txt").data,
            b"hello"
        );
        assert_eq!(
            entries
                .get(Path::new("dir/bar.txt"))
                .expect("dir/bar.txt")
                .data,
            b"world"
        );
    }

    #[test]
    fn extract_zip_matches_plain_tar() {
        use zip::write::SimpleFileOptions;
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("src.zip");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("foo.txt", SimpleFileOptions::default())
            .expect("start foo.txt");
        zip.write_all(b"hello").expect("write foo.txt");
        zip.start_file("dir/bar.txt", SimpleFileOptions::default())
            .expect("start dir/bar.txt");
        zip.write_all(b"world").expect("write dir/bar.txt");
        let cursor = zip.finish().expect("finish zip");
        std::fs::write(&archive, cursor.into_inner()).expect("write zip file");

        let entries =
            by_path(extract(&archive, "https://example.com/src.zip").expect("zip must extract"));

        assert_eq!(entries.len(), 2, "two files from the zip");
        let foo = entries.get(Path::new("foo.txt")).expect("foo.txt");
        assert_eq!(foo.kind, EntryKind::Blob, "foo.txt is a blob");
        assert_eq!(foo.data, b"hello", "foo.txt content");
        let bar = entries.get(Path::new("dir/bar.txt")).expect("dir/bar.txt");
        assert_eq!(bar.data, b"world", "dir/bar.txt content");
    }

    #[test]
    fn extract_raw_single_file_uses_url_basename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("downloaded.bin");
        std::fs::write(&archive, b"plain-bytes").expect("write raw file");

        let entries = extract(&archive, "https://example.com/path/README.md")
            .expect("a non-archive file must extract as one raw entry");

        assert_eq!(entries.len(), 1, "raw file yields exactly one entry");
        let entry = &entries[0];
        assert_eq!(
            entry.path,
            PathBuf::from("README.md"),
            "raw entry path is the url basename"
        );
        assert_eq!(entry.kind, EntryKind::Blob, "raw entry is a blob");
        assert_eq!(
            entry.data, b"plain-bytes",
            "raw entry data is the file bytes"
        );
    }

    // ---- extract: auto-strip ----

    #[test]
    fn extract_strips_single_top_level_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "pkg.tar", |b| {
            append_file(b, "pkg-1.2.3/src/main.rs", b"fn main() {}", 0o644);
            append_file(b, "pkg-1.2.3/README", b"readme", 0o644);
        });

        let entries =
            by_path(extract(&archive, "https://example.com/pkg.tar").expect("must extract"));

        assert_eq!(entries.len(), 2, "two files after strip");
        assert!(
            entries.contains_key(Path::new("src/main.rs")),
            "single top dir pkg-1.2.3/ must be stripped from src/main.rs, got keys: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
        assert!(
            entries.contains_key(Path::new("README")),
            "single top dir pkg-1.2.3/ must be stripped from README, got keys: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            entries.get(Path::new("src/main.rs")).expect("main.rs").data,
            b"fn main() {}"
        );
    }

    #[test]
    fn extract_does_not_strip_multiple_top_level_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "multi.tar", |b| {
            append_file(b, "a/x", b"ax", 0o644);
            append_file(b, "b/y", b"by", 0o644);
        });

        let entries =
            by_path(extract(&archive, "https://example.com/multi.tar").expect("must extract"));

        assert_eq!(entries.len(), 2, "two files, no strip");
        assert!(
            entries.contains_key(Path::new("a/x")),
            "two top dirs must not be stripped: a/x preserved, got: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
        assert!(
            entries.contains_key(Path::new("b/y")),
            "two top dirs must not be stripped: b/y preserved, got: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
    }

    // ---- extract: modes ----

    #[cfg(unix)]
    #[test]
    fn extract_maps_exec_bit_to_blob_executable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "modes.tar", |b| {
            append_file(b, "run.sh", b"#!/bin/sh\n", 0o755);
            append_file(b, "plain.txt", b"data", 0o644);
        });

        let entries =
            by_path(extract(&archive, "https://example.com/modes.tar").expect("must extract"));

        assert_eq!(
            entries.get(Path::new("run.sh")).expect("run.sh").kind,
            EntryKind::BlobExecutable,
            "0o755 entry must map to BlobExecutable"
        );
        assert_eq!(
            entries.get(Path::new("plain.txt")).expect("plain.txt").kind,
            EntryKind::Blob,
            "0o644 entry must map to Blob"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_symlink_yields_link_entry_with_target_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "link.tar", |b| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            b.append_link(&mut header, "link", "target/path")
                .expect("append symlink");
        });

        let entries =
            by_path(extract(&archive, "https://example.com/link.tar").expect("must extract"));

        assert_eq!(entries.len(), 1, "single symlink entry");
        let link = entries.get(Path::new("link")).expect("link entry");
        assert_eq!(link.kind, EntryKind::Link, "symlink must map to Link kind");
        assert_eq!(
            link.data, b"target/path",
            "symlink data must be the target path bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_zip_maps_exec_bit_to_blob_executable() {
        use zip::write::SimpleFileOptions;
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("modes.zip");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(
            "run.sh",
            SimpleFileOptions::default().unix_permissions(0o755),
        )
        .expect("start run.sh");
        zip.write_all(b"#!/bin/sh\n").expect("write run.sh");
        zip.start_file(
            "plain.txt",
            SimpleFileOptions::default().unix_permissions(0o644),
        )
        .expect("start plain.txt");
        zip.write_all(b"data").expect("write plain.txt");
        let cursor = zip.finish().expect("finish zip");
        std::fs::write(&archive, cursor.into_inner()).expect("write zip file");

        let entries =
            by_path(extract(&archive, "https://example.com/modes.zip").expect("must extract"));

        assert_eq!(
            entries.get(Path::new("run.sh")).expect("run.sh").kind,
            EntryKind::BlobExecutable,
            "zip 0o755 entry must map to BlobExecutable (external-attrs code path)"
        );
        assert_eq!(
            entries.get(Path::new("plain.txt")).expect("plain.txt").kind,
            EntryKind::Blob,
            "zip 0o644 entry must map to Blob"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_zip_symlink_yields_link_entry() {
        use zip::write::SimpleFileOptions;
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("link.zip");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("regular.txt", SimpleFileOptions::default())
            .expect("start regular.txt");
        zip.write_all(b"plain").expect("write regular.txt");
        zip.add_symlink("link", "target/path", SimpleFileOptions::default())
            .expect("add symlink");
        let cursor = zip.finish().expect("finish zip");
        std::fs::write(&archive, cursor.into_inner()).expect("write zip file");

        let entries =
            by_path(extract(&archive, "https://example.com/link.zip").expect("must extract"));

        let link = entries.get(Path::new("link")).expect("link entry");
        assert_eq!(
            link.kind,
            EntryKind::Link,
            "zip symlink must map to Link kind"
        );
        assert_eq!(
            link.data, b"target/path",
            "zip symlink data must be the target path bytes"
        );
        assert_eq!(
            entries
                .get(Path::new("regular.txt"))
                .expect("regular.txt")
                .kind,
            EntryKind::Blob,
            "only the symlink entry is a Link; the regular file stays a Blob"
        );
    }

    #[test]
    fn extract_does_not_strip_single_top_level_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "flat.tar", |b| {
            append_file(b, "README.md", b"readme", 0o644);
            append_file(b, "dir/inner", b"inner", 0o644);
        });

        let entries =
            by_path(extract(&archive, "https://example.com/flat.tar").expect("must extract"));

        assert_eq!(entries.len(), 2, "two files, nothing stripped");
        assert!(
            entries.contains_key(Path::new("README.md")),
            "a top-level FILE means no common top DIRECTORY to strip: README.md must stay intact, got: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
        assert!(
            entries.contains_key(Path::new("dir/inner")),
            "nested path must stay unchanged when there is no single top dir, got: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn extract_keeps_lone_top_level_file_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "lone.tar", |b| {
            append_file(b, "README.md", b"readme", 0o644);
        });

        let entries =
            by_path(extract(&archive, "https://example.com/lone.tar").expect("must extract"));

        assert_eq!(entries.len(), 1, "one file");
        assert!(
            entries.contains_key(Path::new("README.md")),
            "a single root-level file must keep its path, not be stripped to empty, got: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn extract_rejects_mid_path_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "mid.tar", |b| {
            append_raw_named(b, "a/../../etc/passwd", b"pwned");
        });

        let result = extract(&archive, "https://example.com/mid.tar");
        assert!(
            matches!(result, Err(Error::Source(_))),
            "traversal after a legitimate component must be rejected as Error::Source, got: {result:?}"
        );
    }

    #[test]
    fn extract_rejects_zip_traversal_entry() {
        use zip::write::SimpleFileOptions;
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("evil.zip");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("../escape", SimpleFileOptions::default())
            .expect("start ../escape");
        zip.write_all(b"pwned").expect("write ../escape");
        let cursor = zip.finish().expect("finish zip");
        std::fs::write(&archive, cursor.into_inner()).expect("write zip file");

        let result = extract(&archive, "https://example.com/evil.zip");
        assert!(
            matches!(result, Err(Error::Source(_))),
            "a zip entry with a traversal path must be rejected as Error::Source, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_rejects_non_utf8_entry_name() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "badname.tar", |b| {
            let raw = std::ffi::OsStr::from_bytes(&[0xff, 0xfe]);
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header
                .set_path(std::path::Path::new(raw))
                .expect("set non-utf8 path");
            header.set_cksum();
            b.append(&header, &b"abc"[..])
                .expect("append non-utf8 entry");
        });

        let result = extract(&archive, "https://example.com/badname.tar");
        assert!(
            matches!(result, Err(Error::Source(_))),
            "a non-utf8 entry name must be rejected as Error::Source, got: {result:?}"
        );
    }

    #[test]
    fn extract_rejects_traversal_entry_inside_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = write_tar(dir.path(), "evil.tar", |b| {
            append_raw_named(b, "../escape", b"pwned");
        });

        let result = extract(&archive, "https://example.com/evil.tar");
        assert!(
            matches!(result, Err(Error::Source(_))),
            "a traversal entry path inside the archive must be rejected, got: {result:?}"
        );
    }

    #[test]
    fn extract_rejects_archive_exceeding_size_cap() {
        let mut builder = tar::Builder::new(Vec::new());
        append_file(&mut builder, "big.txt", &[b'x'; 100], 0o644);
        let bytes = builder.into_inner().expect("finish tar");

        let over = extract_archive(&bytes, "https://example.com/big.tar", 10);
        assert!(
            matches!(over, Err(Error::Source(_))),
            "100 bytes of content must exceed a 10-byte cap as Error::Source, got: {over:?}"
        );

        let under = extract_archive(&bytes, "https://example.com/big.tar", 1 << 20)
            .expect("a cap far above the content must extract");
        assert_eq!(under.len(), 1, "one file under a generous cap");
        assert_eq!(under[0].data.len(), 100, "full content read under the cap");
    }

    #[test]
    fn extract_rejects_raw_url_with_unsafe_basename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("downloaded.bin");
        std::fs::write(&archive, b"plain-bytes").expect("write raw file");

        let result = extract(&archive, "https://example.com/foo/..");
        assert!(
            matches!(result, Err(Error::Source(_))),
            "a raw url whose basename is `..` must be rejected as Error::Source, got: {result:?}"
        );
    }
}
