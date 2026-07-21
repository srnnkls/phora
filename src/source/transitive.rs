use std::path::Path;

use crate::config::transitive::TransitiveManifest;
use crate::config::{ParsedSource, Remote};

use super::{Result, SourceBackend, SourceError, SourceName, is_local_path};

/// Acquires and decodes a dependency's `phora.toml` at either its pinned commit
/// or a freshly resolved commit.
pub(crate) fn acquire_dependency_manifest(
    backend: &(dyn SourceBackend + Sync),
    source_name: &SourceName,
    parsed_source: &ParsedSource,
    remote: &str,
    pinned_commit: Option<&str>,
) -> Result<(String, TransitiveManifest)> {
    let commit = if let Some(commit) = pinned_commit {
        commit.to_owned()
    } else {
        backend.fetch(source_name, remote)?;
        backend.resolve(source_name, remote, &parsed_source.refspec())?
    };
    let bytes = backend
        .read_file_at(source_name, remote, &commit, Path::new("phora.toml"))
        .map_err(|error| match error {
            absent @ SourceError::FileAbsent { .. } => SourceError::DependencyManifestMissing {
                remote: remote.to_owned(),
                source: Box::new(absent),
            },
            other => other,
        })?;
    let manifest_text =
        String::from_utf8(bytes).map_err(|source| SourceError::DependencyManifestUtf8 {
            remote: remote.to_owned(),
            source,
        })?;
    let manifest = TransitiveManifest::parse_toml(&manifest_text)
        .map_err(|source| SourceError::DependencyManifestParse { source })?;
    Ok((commit, manifest))
}

/// Rejects a transitive remote that could escape into the consumer's local filesystem.
pub(crate) fn validate_dependency_remote(
    name: &str,
    parsed_source: &ParsedSource,
    remote: &str,
    depth: usize,
) -> Result<()> {
    let escapes = matches!(parsed_source.remote, Remote::Path(_))
        || remote.starts_with("file://")
        || is_relative_fs_remote(remote)
        || (depth > 1 && is_local_path(remote));
    if escapes {
        return Err(SourceError::TransitiveRemoteRejected {
            name: name.to_owned(),
            remote: remote.to_owned(),
        });
    }
    Ok(())
}

/// True for a relative filesystem path; false for URL/scp remotes and absolute paths.
fn is_relative_fs_remote(remote: &str) -> bool {
    if remote.contains("://") {
        return false;
    }
    if let Some(colon) = remote.find(':') {
        let first_slash = remote.find('/');
        if first_slash.is_none_or(|slash| colon < slash) {
            return false;
        }
    }
    !Path::new(remote).is_absolute()
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::config::{Refspec, Source};
    use crate::source::{GitBackend, mirror_path};

    enum ManifestRead {
        Bytes(Vec<u8>),
        Absent,
        BackendFailure,
    }

    struct ManifestBackend {
        read: ManifestRead,
        fail_fetch: bool,
    }

    impl SourceBackend for ManifestBackend {
        fn fetch(&self, _source: &SourceName, _url: &str) -> super::super::Result<()> {
            if self.fail_fetch {
                Err(SourceError::Source("backend sentinel".to_owned()))
            } else {
                Ok(())
            }
        }

        fn read_file_at(
            &self,
            source: &SourceName,
            _url: &str,
            commit: &str,
            path: &Path,
        ) -> super::super::Result<Vec<u8>> {
            match &self.read {
                ManifestRead::Bytes(bytes) => Ok(bytes.clone()),
                ManifestRead::Absent => Err(SourceError::FileAbsent {
                    source_name: source.as_str().to_owned(),
                    commit: commit.to_owned(),
                    path: path.to_path_buf(),
                }),
                ManifestRead::BackendFailure => {
                    Err(SourceError::Source("backend sentinel".to_owned()))
                }
            }
        }

        fn resolve(
            &self,
            _source: &SourceName,
            _url: &str,
            _refspec: &Refspec,
        ) -> super::super::Result<String> {
            Ok("a".repeat(40))
        }

        fn commit_time(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
        ) -> super::super::Result<u64> {
            Ok(0)
        }

        fn compute_digest(
            &self,
            _source: &SourceName,
            _url: &str,
            _commit: &str,
            _root: Option<&Path>,
            _include: &[String],
            _exclude: &[String],
        ) -> super::super::Result<String> {
            Ok("blake3:test".to_owned())
        }
    }

    fn parsed_git_source(remote: &str) -> ParsedSource {
        let raw: Source = toml::from_str(&format!("git = {remote:?}\ntransitive = true\n"))
            .expect("source DTO parses");
        ParsedSource::parse("dep", &raw).expect("source parses")
    }

    fn assert_source_error_type<E>(error: &E)
    where
        E: StdError + 'static,
    {
        assert_eq!(
            std::any::type_name::<E>(),
            std::any::type_name::<SourceError>(),
            "source::transitive fallible boundaries must return the source-local SourceError"
        );
        assert!(
            (error as &(dyn StdError + 'static)).is::<SourceError>(),
            "the concrete boundary error must remain downcastable to SourceError"
        );
    }

    fn concrete_source_error<E>(error: &E) -> &SourceError
    where
        E: StdError + 'static,
    {
        (error as &(dyn StdError + 'static))
            .downcast_ref::<SourceError>()
            .expect("the boundary error must be the concrete source-local SourceError")
    }

    fn chain_has_source_error<E, F>(error: &E, predicate: F) -> bool
    where
        E: StdError + 'static,
        F: Fn(&SourceError) -> bool,
    {
        let mut current: Option<&(dyn StdError + 'static)> = Some(error);
        while let Some(item) = current {
            if item.downcast_ref::<SourceError>().is_some_and(&predicate) {
                return true;
            }
            current = item.source();
        }
        false
    }

    fn chain_has_error<E, T>(error: &E) -> bool
    where
        E: StdError + 'static,
        T: StdError + 'static,
    {
        let mut current: Option<&(dyn StdError + 'static)> = Some(error);
        while let Some(item) = current {
            if item.is::<T>() {
                return true;
            }
            current = item.source();
        }
        false
    }

    fn pinned_manifest(
        read: ManifestRead,
    ) -> std::result::Result<(String, TransitiveManifest), impl StdError> {
        let backend = ManifestBackend {
            read,
            fail_fetch: false,
        };
        let remote = "https://example.test/dep.git";
        acquire_dependency_manifest(
            &backend,
            &SourceName::trusted("dep"),
            &parsed_git_source(remote),
            remote,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )
    }

    #[test]
    fn manifest_boundary_returns_structured_missing_error() {
        let error = pinned_manifest(ManifestRead::Absent)
            .expect_err("a dependency without phora.toml must fail");
        assert_source_error_type(&error);
        assert!(
            error
                .to_string()
                .contains("dependency at `https://example.test/dep.git` has no phora.toml"),
            "missing-manifest diagnostic changed: {error}"
        );
        assert!(
            chain_has_source_error(
                &error,
                |source| matches!(source, SourceError::FileAbsent { path, .. } if path == Path::new("phora.toml"))
            ),
            "mapping a missing manifest must retain the structured FileAbsent source in its chain: {error}"
        );
    }

    #[test]
    fn manifest_boundary_returns_structured_utf8_error() {
        let error = pinned_manifest(ManifestRead::Bytes(vec![0xff]))
            .expect_err("a non-UTF-8 dependency manifest must fail");
        assert_source_error_type(&error);
        assert!(
            error
                .to_string()
                .contains("phora.toml at `https://example.test/dep.git` is not utf-8"),
            "invalid-UTF-8 diagnostic changed: {error}"
        );
        assert!(
            chain_has_error::<_, std::string::FromUtf8Error>(&error),
            "invalid UTF-8 must remain available as a structured source: {error}"
        );
    }

    #[test]
    fn manifest_boundary_returns_structured_parse_error() {
        let error = pinned_manifest(ManifestRead::Bytes(b"version = [\n".to_vec()))
            .expect_err("an invalid dependency manifest must fail");
        assert_source_error_type(&error);
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("config error") && diagnostic.contains("unclosed array"),
            "manifest-parse diagnostic changed: {diagnostic}"
        );
        assert!(
            chain_has_error::<_, toml::de::Error>(&error),
            "manifest parse failures must retain the structured TOML source error: {error}"
        );
    }

    #[test]
    fn manifest_boundary_preserves_backend_source_error_variant() {
        let error = pinned_manifest(ManifestRead::BackendFailure)
            .expect_err("a backend manifest-read failure must fail");
        assert_source_error_type(&error);
        assert!(
            chain_has_source_error(
                &error,
                |source| matches!(source, SourceError::Source(message) if message == "backend sentinel")
            ),
            "backend SourceError::Source must remain in the structured chain instead of being stringified: {error}"
        );

        let backend = ManifestBackend {
            read: ManifestRead::Bytes(b"version = 1\n".to_vec()),
            fail_fetch: true,
        };
        let remote = "https://example.test/dep.git";
        let fetch_error = acquire_dependency_manifest(
            &backend,
            &SourceName::trusted("dep"),
            &parsed_git_source(remote),
            remote,
            None,
        )
        .expect_err("a backend fetch failure must fail");
        assert_source_error_type(&fetch_error);
        assert!(
            chain_has_source_error(
                &fetch_error,
                |source| matches!(source, SourceError::Source(message) if message == "backend sentinel")
            ),
            "backend fetch variants must propagate without stringification: {fetch_error}"
        );
    }

    #[test]
    fn remote_boundary_returns_structured_source_error() {
        let raw: Source =
            toml::from_str("path = \"/etc\"\ntransitive = true\n").expect("path source DTO parses");
        let parsed = ParsedSource::parse("escape", &raw).expect("path source parses");
        let error = validate_dependency_remote("escape", &parsed, "/etc", 1)
            .expect_err("a transitive local path must fail");
        assert_source_error_type(&error);
        assert!(
            error
                .to_string()
                .contains("source `escape`: transitive remote not allowed"),
            "remote-confinement diagnostic changed: {error}"
        );
    }

    #[test]
    fn source_failure_categories_use_distinct_error_variants() {
        let missing = pinned_manifest(ManifestRead::Absent)
            .expect_err("a missing dependency manifest must fail");
        let utf8 = pinned_manifest(ManifestRead::Bytes(vec![0xff]))
            .expect_err("a non-UTF-8 dependency manifest must fail");
        let parse = pinned_manifest(ManifestRead::Bytes(b"version = [\n".to_vec()))
            .expect_err("an invalid dependency manifest must fail");
        let raw: Source =
            toml::from_str("path = \"/etc\"\ntransitive = true\n").expect("path DTO parses");
        let parsed = ParsedSource::parse("escape", &raw).expect("path source parses");
        let remote = validate_dependency_remote("escape", &parsed, "/etc", 1)
            .expect_err("a transitive local path must fail");

        let categories = [
            (
                "missing manifest",
                std::mem::discriminant(concrete_source_error(&missing)),
            ),
            (
                "invalid UTF-8",
                std::mem::discriminant(concrete_source_error(&utf8)),
            ),
            (
                "manifest parse",
                std::mem::discriminant(concrete_source_error(&parse)),
            ),
            (
                "remote confinement",
                std::mem::discriminant(concrete_source_error(&remote)),
            ),
        ];
        for (index, (left_name, left)) in categories.iter().enumerate() {
            for (right_name, right) in &categories[index + 1..] {
                assert_ne!(
                    left, right,
                    "{left_name} and {right_name} must have distinct SourceError variant classes; a generic string variant is not structured ownership"
                );
            }
        }
    }

    fn git(cwd: &Path, args: &[&str]) {
        crate::store::assert_git_sandboxed(cwd);
        let _serial = crate::store::guard_git_fork();
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn manifest_boundary_reads_only_phora_toml_and_ignores_dep_lock() {
        let src = tempfile::TempDir::new().expect("source repo");
        git(src.path(), &["init", "-b", "main", "."]);
        git(src.path(), &["config", "user.email", "t@example.com"]);
        git(src.path(), &["config", "user.name", "T"]);
        std::fs::write(
            src.path().join("phora.toml"),
            b"version = 1\n\n[sources.nvim]\ngit = \"https://github.com/dep/nvim.git\"\n",
        )
        .expect("write manifest");
        std::fs::write(
            src.path().join("phora.lock"),
            b"version = 2\n\n[[trusted_hooks]]\npreimage = \"blake3:evil\"\n",
        )
        .expect("write dep lock");
        git(src.path(), &["add", "-A"]);
        git(src.path(), &["commit", "-m", "dep with self-trusting lock"]);

        let mirror_root = tempfile::TempDir::new().expect("mirror root");
        let remote = src.path().to_string_lossy().into_owned();
        let mirror = mirror_path(mirror_root.path(), &remote);
        std::fs::create_dir_all(mirror.parent().expect("mirror parent"))
            .expect("create mirror parent");
        git(
            mirror_root.path(),
            &[
                "clone",
                "--mirror",
                &remote,
                mirror.to_str().expect("mirror path"),
            ],
        );
        let commit_output = {
            let _serial = crate::store::guard_git_fork();
            Command::new("git")
                .args([
                    "-C",
                    mirror.to_str().expect("mirror path"),
                    "rev-parse",
                    "HEAD",
                ])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("resolve mirror head")
        };
        assert!(commit_output.status.success(), "resolve mirror head");
        let commit = String::from_utf8(commit_output.stdout)
            .expect("commit is UTF-8")
            .trim()
            .to_owned();
        let backend = GitBackend::new(mirror_root.path().to_path_buf());
        let (_, manifest) = acquire_dependency_manifest(
            &backend,
            &SourceName::trusted("dep"),
            &parsed_git_source(&remote),
            &remote,
            Some(&commit),
        )
        .expect("source boundary reads and parses only phora.toml");
        assert!(manifest.sources.contains_key("nvim"));
        assert!(
            !format!("{manifest:?}").contains("blake3:evil"),
            "a dep-shipped lock must be unreachable from manifest acquisition"
        );
    }
}
