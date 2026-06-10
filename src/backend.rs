//! Mode-aware source routing: dispatches each `SourceBackend` call to the git or
//! http adapter by the source's declared mode.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Refspec, SourceMode};
use crate::error::Result;
use crate::kernel::Selection;
use crate::source::{ExportRequest, ExportResult, SourceBackend};

/// Routes each `SourceBackend` call to `git` or `http` by the source's declared
/// mode, looked up by name. An unmapped name defaults to the git backend.
pub struct RouterBackend<G, H> {
    git: G,
    http: H,
    modes: BTreeMap<String, SourceMode>,
}

impl<G, H> RouterBackend<G, H> {
    pub fn new(git: G, http: H, modes: BTreeMap<String, SourceMode>) -> Self {
        Self { git, http, modes }
    }

    pub fn git_backend(&self) -> &G {
        &self.git
    }

    pub fn http_backend(&self) -> &H {
        &self.http
    }

    fn is_url(&self, source: &str) -> bool {
        matches!(self.modes.get(source), Some(SourceMode::Url))
    }
}

impl<G: SourceBackend, H: SourceBackend> SourceBackend for RouterBackend<G, H> {
    fn fetch(&self, source: &str, url: &str) -> Result<()> {
        if self.is_url(source) {
            self.http.fetch(source, url)
        } else {
            self.git.fetch(source, url)
        }
    }

    fn resolve(&self, source: &str, url: &str, refspec: &Refspec) -> Result<String> {
        if self.is_url(source) {
            self.http.resolve(source, url, refspec)
        } else {
            self.git.resolve(source, url, refspec)
        }
    }

    fn commit_time(&self, source: &str, url: &str, commit: &str) -> Result<u64> {
        if self.is_url(source) {
            self.http.commit_time(source, url, commit)
        } else {
            self.git.commit_time(source, url, commit)
        }
    }

    fn discover_artifacts(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &Selection,
    ) -> Result<Vec<String>> {
        if self.is_url(source) {
            self.http
                .discover_artifacts(source, url, commit, root, selection)
        } else {
            self.git
                .discover_artifacts(source, url, commit, root, selection)
        }
    }

    fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult> {
        if self.is_url(req.source) {
            self.http.export_artifact(req)
        } else {
            self.git.export_artifact(req)
        }
    }

    fn compute_digest(
        &self,
        source: &str,
        url: &str,
        commit: &str,
        root: Option<&Path>,
        selection: &Selection,
    ) -> Result<String> {
        if self.is_url(source) {
            self.http
                .compute_digest(source, url, commit, root, selection)
        } else {
            self.git
                .compute_digest(source, url, commit, root, selection)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use tempfile::TempDir;

    use crate::config::{Refspec, SourceMode};
    use crate::error::{Error, Result};
    use crate::kernel::Selection;
    use crate::source::{
        ExportRequest, ExportResult, GitBackend, HttpBackend, RouterBackend, SourceBackend,
    };

    // ── local http server serving a real .tar.gz ───────────────────

    const HELLO_BODY: &[u8] = b"hi";

    struct TarServer {
        port: u16,
    }

    impl TarServer {
        fn spawn(body: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            let port = listener.local_addr().expect("local addr").port();
            std::thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    Self::serve(stream, &body);
                }
            });
            Self { port }
        }

        fn serve(mut stream: TcpStream, body: &[u8]) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/pkg-1.0.tar.gz", self.port)
        }
    }

    fn build_pkg_tar_gz() -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_size(HELLO_BODY.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();

        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_data(&mut header, "pkg-1.0/hello.txt", HELLO_BODY)
            .expect("append tar entry");
        let tar_bytes = builder.into_inner().expect("finish tar");

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip tar bytes");
        encoder.finish().expect("finish gzip")
    }

    // ── local git mirror (the GitBackend-test way) ─────────────────

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn run_git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_git_repo() -> (TempDir, String, String) {
        let src = TempDir::new().unwrap();
        let p = src.path();
        run_git(p, &["init", "-b", "main", "."]);
        run_git(p, &["config", "user.email", "test@example.com"]);
        run_git(p, &["config", "user.name", "Test"]);
        std::fs::write(p.join("README.md"), b"hello\n").unwrap();
        run_git(p, &["add", "-A"]);
        run_git(p, &["commit", "-m", "initial"]);
        let head = {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(p)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let url = p.to_string_lossy().into_owned();
        (src, url, head)
    }

    fn empty_selection() -> Selection {
        Selection::new(&[], &[]).expect("empty selection builds")
    }

    // ── behavioral dispatch: real git + real http through the router ──

    #[test]
    fn router_resolves_url_source_via_http_and_git_source_via_git() {
        let (_git_src, g_url, g_head) = build_git_repo();
        let server = TarServer::spawn(build_pkg_tar_gz());
        let u_url = server.url();

        let git_dir = TempDir::new().expect("git_dir tempdir");
        let git = GitBackend::new(git_dir.path().to_path_buf());
        let http = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

        let mut modes = BTreeMap::new();
        modes.insert("g".to_string(), SourceMode::Git);
        modes.insert("u".to_string(), SourceMode::Url);
        let router = RouterBackend::new(git, http, modes);

        router
            .fetch("g", &g_url)
            .expect("router fetches git source");
        router
            .fetch("u", &u_url)
            .expect("router fetches url source");

        // url source resolved with a bogus git ref: only succeeds if routed to Http
        // (which ignores the refspec and reads refs/heads/phora).
        let u_commit = router
            .resolve("u", &u_url, &Refspec::Branch("main".into()))
            .expect("url source must route to Http and resolve despite a git-style refspec");
        assert_eq!(u_commit.len(), 40, "synthetic phora commit is 40-hex");

        // git source resolved with Refspec::None would fail on Http; routing to Git
        // with Branch(main) is what makes it succeed and match the real head.
        let g_commit = router
            .resolve("g", &g_url, &Refspec::Branch("main".into()))
            .expect("git source must route to Git and resolve branch main");
        assert_eq!(
            g_commit, g_head,
            "the git source must resolve to its real HEAD commit, proving it was routed to Git"
        );
        assert_ne!(
            u_commit, g_commit,
            "the two sources resolve to distinct commits, ruling out cross-routing"
        );
    }

    #[test]
    fn router_misrouting_would_fail_url_source_through_git() {
        // A url source sent to the Git backend with a Branch refspec must error:
        // the synthetic mirror has no refs/heads/main, only refs/heads/phora.
        let server = TarServer::spawn(build_pkg_tar_gz());
        let u_url = server.url();
        let git_dir = TempDir::new().expect("git_dir tempdir");

        let http = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());
        http.fetch("u", &u_url).expect("import synthetic mirror");

        let git = GitBackend::new(git_dir.path().to_path_buf());
        assert!(
            git.resolve("u", &u_url, &Refspec::Branch("main".into()))
                .is_err(),
            "a url source mis-sent to Git with Branch(main) must fail: there is no such branch, \
             only refs/heads/phora. This is why dispatch-on-mode (not url-scheme) is load-bearing."
        );
    }

    // ── spy dispatch: pins routing on name/mode, not url scheme ──────

    #[derive(Default)]
    struct Spy {
        fetches: RefCell<Vec<String>>,
        resolves: RefCell<Vec<String>>,
        discovers: RefCell<Vec<String>>,
        digests: RefCell<Vec<String>>,
    }

    impl SourceBackend for Spy {
        fn fetch(&self, source: &str, _url: &str) -> Result<()> {
            self.fetches.borrow_mut().push(source.to_string());
            Ok(())
        }

        fn resolve(&self, source: &str, _url: &str, _refspec: &Refspec) -> Result<String> {
            self.resolves.borrow_mut().push(source.to_string());
            Ok(format!("resolved-{source}"))
        }

        fn commit_time(&self, _source: &str, _url: &str, _commit: &str) -> Result<u64> {
            Ok(0)
        }

        fn discover_artifacts(
            &self,
            source: &str,
            _url: &str,
            _commit: &str,
            _root: Option<&Path>,
            _selection: &Selection,
        ) -> Result<Vec<String>> {
            self.discovers.borrow_mut().push(source.to_string());
            Ok(vec![])
        }

        fn export_artifact(&self, _req: &ExportRequest<'_>) -> Result<ExportResult> {
            Err(Error::Source("spy export".into()))
        }

        fn compute_digest(
            &self,
            source: &str,
            _url: &str,
            _commit: &str,
            _root: Option<&Path>,
            _selection: &Selection,
        ) -> Result<String> {
            self.digests.borrow_mut().push(source.to_string());
            Ok("blake3:spy".into())
        }
    }

    /// A `RouterBackend` generic over its two adapters, so spies can stand in for
    /// the real backends. If `RouterBackend` is concrete over GitBackend/HttpBackend
    /// only, the implementer can instead expose a spy-friendly constructor; this
    /// test pins the dispatch contract either way.
    fn spy_router(modes: BTreeMap<String, SourceMode>) -> RouterBackend<Spy, Spy> {
        RouterBackend::new(Spy::default(), Spy::default(), modes)
    }

    #[test]
    fn dispatch_sends_url_mode_to_http_and_git_mode_to_git_by_name() {
        let mut modes = BTreeMap::new();
        modes.insert("g".to_string(), SourceMode::Git);
        modes.insert("u".to_string(), SourceMode::Url);
        let router = spy_router(modes);

        router
            .fetch("g", "https://example.com/o/r.git")
            .expect("git fetch");
        router
            .fetch("u", "https://example.com/pkg.tar.gz")
            .expect("url fetch");
        router
            .resolve("u", "https://example.com/pkg.tar.gz", &Refspec::None)
            .expect("url resolve");
        router
            .resolve(
                "g",
                "https://example.com/o/r.git",
                &Refspec::Branch("main".into()),
            )
            .expect("git resolve");

        assert_eq!(
            router.git_backend().fetches.borrow().as_slice(),
            ["g"],
            "only the git-mode source `g` may reach the git backend's fetch"
        );
        assert_eq!(
            router.http_backend().fetches.borrow().as_slice(),
            ["u"],
            "only the url-mode source `u` may reach the http backend's fetch"
        );
        assert_eq!(
            router.git_backend().resolves.borrow().as_slice(),
            ["g"],
            "git-mode resolve routes to git"
        );
        assert_eq!(
            router.http_backend().resolves.borrow().as_slice(),
            ["u"],
            "url-mode resolve routes to http"
        );
    }

    #[test]
    fn git_url_ending_in_dot_git_routes_to_git_not_http() {
        // Routing must be by declared mode, never by url scheme/suffix. A `.git`
        // url under SourceMode::Git must hit the git backend, never http.
        let mut modes = BTreeMap::new();
        modes.insert("g".to_string(), SourceMode::Git);
        let router = spy_router(modes);

        router
            .fetch("g", "https://example.com/o/r.git")
            .expect("git fetch");

        assert_eq!(
            router.git_backend().fetches.borrow().as_slice(),
            ["g"],
            "a `.git` url declared Git-mode must route to the git backend"
        );
        assert!(
            router.http_backend().fetches.borrow().is_empty(),
            "a `.git` url must NOT reach the http backend regardless of its scheme/suffix"
        );
    }

    #[test]
    fn discover_and_digest_dispatch_by_mode() {
        let mut modes = BTreeMap::new();
        modes.insert("u".to_string(), SourceMode::Url);
        modes.insert("g".to_string(), SourceMode::Git);
        let router = spy_router(modes);
        let m = empty_selection();

        router
            .discover_artifacts("u", "http://x/pkg.tgz", "c", None, &m)
            .expect("url discover");
        router
            .compute_digest("g", "https://x/y.git", "c", None, &m)
            .expect("git digest");

        assert!(
            router.git_backend().fetches.borrow().is_empty()
                && router.http_backend().fetches.borrow().is_empty(),
            "discover/digest must not trigger fetch"
        );

        assert_eq!(
            router.http_backend().discovers.borrow().as_slice(),
            ["u"],
            "the url-mode source `u` must reach the http backend's discover_artifacts"
        );
        assert!(
            router.git_backend().discovers.borrow().is_empty(),
            "no source discovered here is git-mode, so git's discover_artifacts must be untouched"
        );

        assert_eq!(
            router.git_backend().digests.borrow().as_slice(),
            ["g"],
            "the git-mode source `g` must reach the git backend's compute_digest"
        );
        assert!(
            router.http_backend().digests.borrow().is_empty(),
            "no source digested here is url-mode, so http's compute_digest must be untouched"
        );
    }

    #[test]
    fn unknown_source_name_has_a_defined_route() {
        // A name absent from the modes map must resolve to the git backend (the
        // default), never panic and never silently hit http.
        let router = spy_router(BTreeMap::new());
        let _ = router.fetch("mystery", "https://example.com/o/r.git");
        assert_eq!(
            router.git_backend().fetches.borrow().as_slice(),
            ["mystery"],
            "a source with no recorded mode must default to the git backend"
        );
        assert!(
            router.http_backend().fetches.borrow().is_empty(),
            "an unmapped source must never be routed to http"
        );
    }

    #[test]
    fn router_is_usable_as_dyn_source_backend() {
        let mut modes = BTreeMap::new();
        modes.insert("g".to_string(), SourceMode::Git);
        let router = spy_router(modes);
        let as_dyn: &dyn SourceBackend = &router;
        let result = as_dyn
            .resolve("g", "https://x/y.git", &Refspec::Branch("main".into()))
            .expect("router is a SourceBackend so sync(&dyn SourceBackend) keeps working");
        assert_eq!(
            result, "resolved-g",
            "the value must come back through the vtable from the git-routed Spy, \
             pinning runtime dispatch through &dyn SourceBackend rather than mere compilation"
        );
    }
}
