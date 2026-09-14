//! Typed request routing for Git, URL, and worktree source capabilities.

use crate::source::{
    MirrorKey, ResolvePolicy, ResolveRequest, ResolvedSource, SnapshotId, SourceDirectoryEntry,
    SourceEntry, SourceError, SourceInventory, SourceLocation, SourceName, SourcePath, SourceStore,
    WorktreeMirrorAddress, WorktreeMirrorGuard, WorktreeObservationRequest,
    WorktreeObservationResult,
};

type Result<T> = std::result::Result<T, SourceError>;

/// Routes each source request by its typed location.
pub struct RouterBackend<G, H> {
    git: G,
    http: H,
}

impl<G, H> RouterBackend<G, H> {
    pub fn new(git: G, http: H) -> Self {
        Self { git, http }
    }
}

impl<G: SourceStore, H: SourceStore> SourceStore for RouterBackend<G, H> {
    fn resolve(&self, request: &ResolveRequest, policy: ResolvePolicy) -> Result<ResolvedSource> {
        match &request.location {
            SourceLocation::Url { .. } => SourceStore::resolve(&self.http, request, policy),
            SourceLocation::Git { .. } | SourceLocation::Worktree { .. } => {
                SourceStore::resolve(&self.git, request, policy)
            }
        }
    }

    fn inventory(
        &self,
        snapshot: &SnapshotId,
        root: Option<&SourcePath>,
    ) -> Result<SourceInventory> {
        SourceStore::inventory(&self.git, snapshot, root)
    }

    fn read(&self, snapshot: &SnapshotId, path: &SourcePath) -> Result<SourceEntry> {
        SourceStore::read(&self.git, snapshot, path)
    }

    fn list_directory(
        &self,
        snapshot: &SnapshotId,
        path: Option<&SourcePath>,
    ) -> Result<Vec<SourceDirectoryEntry>> {
        SourceStore::list_directory(&self.git, snapshot, path)
    }

    fn lock_worktree_mirror(
        &self,
        source: &SourceName,
        key: &MirrorKey,
    ) -> Result<WorktreeMirrorGuard> {
        SourceStore::lock_worktree_mirror(&self.git, source, key)
    }

    fn lock_worktree_mirror_at(
        &self,
        source: &SourceName,
        address: &WorktreeMirrorAddress,
    ) -> Result<WorktreeMirrorGuard> {
        SourceStore::lock_worktree_mirror_at(&self.git, source, address)
    }

    fn observe_worktree(
        &self,
        request: &WorktreeObservationRequest,
    ) -> Result<WorktreeObservationResult> {
        SourceStore::observe_worktree(&self.git, request)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tempfile::TempDir;

    use crate::source::model::SourceName;
    use crate::source::{
        Commit, GitBackend, HttpBackend, MirrorKey, NormalizedUrl, ResolvePolicy, ResolveRequest,
        ResolvedRevision, ResolvedSource, RevisionSpec, RouterBackend, SnapshotId,
        SourceDirectoryEntry, SourceEntry, SourceEntryKind, SourceEntryMeta, SourceError,
        SourceIdentity, SourceInventory, SourceLocation, SourcePath, SourceStore, SourceTimestamp,
        digest_snapshot,
    };

    type Result<T> = std::result::Result<T, SourceError>;

    fn sn(name: &str) -> SourceName {
        SourceName::trusted(name)
    }

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
        crate::sync::state::locking::assert_git_sandboxed(cwd);
        let _serial = crate::sync::state::locking::guard_git_fork();
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
            let _serial = crate::sync::state::locking::guard_git_fork();
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

    // ── behavioral dispatch: real git + real http through the router ──

    #[test]
    fn router_resolves_url_source_via_http_and_git_source_via_git() {
        let (_git_src, g_url, g_head) = build_git_repo();
        let server = TarServer::spawn(build_pkg_tar_gz());
        let u_url = server.url();

        let git_dir = TempDir::new().expect("git_dir tempdir");
        let git = GitBackend::new(git_dir.path().to_path_buf());
        let http = HttpBackend::new(git_dir.path().to_path_buf(), BTreeMap::new());

        let router = RouterBackend::new(git, http);

        let u_source = SourceStore::resolve(
            &router,
            &ResolveRequest {
                name: sn("u"),
                location: SourceLocation::Url { url: u_url },
                revision: RevisionSpec::None,
            },
            ResolvePolicy::Refresh,
        )
        .expect("url source must route to Http and import a synthetic snapshot");
        let u_commit = u_source.snapshot.commit().to_string();
        assert_eq!(u_commit.len(), 40, "synthetic phora commit is 40-hex");

        let g_source = SourceStore::resolve(
            &router,
            &ResolveRequest {
                name: sn("g"),
                location: SourceLocation::Git { url: g_url },
                revision: RevisionSpec::Branch("main".into()),
            },
            ResolvePolicy::Refresh,
        )
        .expect("git source must route to Git and resolve branch main");
        let g_commit = g_source.snapshot.commit().to_string();
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
        SourceStore::resolve(
            &http,
            &ResolveRequest {
                name: sn("u"),
                location: SourceLocation::Url { url: u_url.clone() },
                revision: RevisionSpec::None,
            },
            ResolvePolicy::Refresh,
        )
        .expect("import synthetic mirror");

        let git = GitBackend::new(git_dir.path().to_path_buf());
        assert!(
            SourceStore::resolve(
                &git,
                &ResolveRequest {
                    name: sn("u"),
                    location: SourceLocation::Git { url: u_url },
                    revision: RevisionSpec::Branch("main".into()),
                },
                ResolvePolicy::CachedOnly,
            )
            .is_err(),
            "a url source mis-sent to Git with Branch(main) must fail: there is no such branch, \
             only refs/heads/phora. This is why dispatch-on-mode (not url-scheme) is load-bearing."
        );
    }

    // ── spy dispatch: pins routing on name/mode, not url scheme ──────

    #[derive(Default, Clone)]
    struct Spy {
        resolves: Arc<Mutex<Vec<String>>>,
        inventories: Arc<Mutex<usize>>,
        reads: Arc<Mutex<usize>>,
        directory_lists: Arc<Mutex<usize>>,
    }

    impl SourceStore for Spy {
        fn resolve(
            &self,
            request: &ResolveRequest,
            _policy: ResolvePolicy,
        ) -> Result<ResolvedSource> {
            self.resolves
                .lock()
                .expect("resolve recorder lock")
                .push(request.name.to_string());
            let url = match &request.location {
                SourceLocation::Git { url } | SourceLocation::Url { url } => url,
                SourceLocation::Worktree { .. } => {
                    unreachable!("router spy tests use Git and URL sources")
                }
            };
            let normalized = NormalizedUrl::parse(url);
            let commit: Commit = "0".repeat(40).parse().expect("fixture commit is valid");
            let identity = match request.location {
                SourceLocation::Url { .. } => SourceIdentity::Url(normalized.clone()),
                SourceLocation::Git { .. } => SourceIdentity::Git(normalized.clone()),
                SourceLocation::Worktree { .. } => unreachable!(),
            };
            Ok(ResolvedSource {
                name: request.name.clone(),
                snapshot: SnapshotId::Git {
                    mirror: MirrorKey::from_url(&normalized),
                    commit: commit.clone(),
                },
                revision: ResolvedRevision::Commit(commit),
                authored_at: SourceTimestamp::from_unix_seconds(0),
                normalized_location: identity,
            })
        }

        fn inventory(
            &self,
            _snapshot: &SnapshotId,
            _root: Option<&SourcePath>,
        ) -> Result<SourceInventory> {
            *self.inventories.lock().expect("inventory recorder lock") += 1;
            Ok(SourceInventory::default())
        }

        fn read(&self, _snapshot: &SnapshotId, path: &SourcePath) -> Result<SourceEntry> {
            *self.reads.lock().expect("read recorder lock") += 1;
            Ok(SourceEntry {
                meta: SourceEntryMeta {
                    path: path.clone(),
                    kind: SourceEntryKind::File,
                },
                bytes: b"spy".to_vec(),
            })
        }

        fn list_directory(
            &self,
            _snapshot: &SnapshotId,
            _path: Option<&SourcePath>,
        ) -> Result<Vec<SourceDirectoryEntry>> {
            *self
                .directory_lists
                .lock()
                .expect("directory recorder lock") += 1;
            Ok(Vec::new())
        }
    }

    // Git and HTTP share recorded state with their in-router twins via Arc.
    fn spy_router() -> (RouterBackend<Spy, Spy>, Spy, Spy) {
        let git = Spy::default();
        let http = Spy::default();
        let router = RouterBackend::new(git.clone(), http.clone());
        (router, git, http)
    }

    fn request(name: &str, location: SourceLocation, revision: RevisionSpec) -> ResolveRequest {
        ResolveRequest {
            name: sn(name),
            location,
            revision,
        }
    }

    fn snapshot() -> SnapshotId {
        let normalized = NormalizedUrl::parse("https://example.com/o/r.git");
        SnapshotId::Git {
            mirror: MirrorKey::from_url(&normalized),
            commit: "0".repeat(40).parse().expect("fixture commit is valid"),
        }
    }

    #[test]
    fn dispatch_sends_url_mode_to_http_and_git_mode_to_git_by_name() {
        let (router, git, http) = spy_router();

        SourceStore::resolve(
            &router,
            &request(
                "u",
                SourceLocation::Url {
                    url: "https://example.com/pkg.tar.gz".to_owned(),
                },
                RevisionSpec::None,
            ),
            ResolvePolicy::Refresh,
        )
        .expect("url resolve");
        SourceStore::resolve(
            &router,
            &request(
                "g",
                SourceLocation::Git {
                    url: "https://example.com/o/r.git".to_owned(),
                },
                RevisionSpec::Branch("main".into()),
            ),
            ResolvePolicy::Refresh,
        )
        .expect("git resolve");

        assert_eq!(
            git.resolves
                .lock()
                .expect("git resolve recorder")
                .as_slice(),
            ["g"],
            "only the git-mode source `g` may reach the Git store's resolve"
        );
        assert_eq!(
            http.resolves
                .lock()
                .expect("HTTP resolve recorder")
                .as_slice(),
            ["u"],
            "only the URL-mode source `u` may reach the HTTP store's resolve"
        );
    }

    #[test]
    fn immutable_snapshot_inventory_uses_the_shared_git_representation() {
        let (router, git, http) = spy_router();

        SourceStore::inventory(&router, &snapshot(), None).expect("first snapshot inventory");
        SourceStore::inventory(&router, &snapshot(), None).expect("second snapshot inventory");

        assert_eq!(
            *git.inventories.lock().expect("git inventory recorder"),
            2,
            "Git and URL resolutions converge on the Git snapshot representation"
        );
        assert_eq!(
            *http.inventories.lock().expect("HTTP inventory recorder"),
            0,
            "the HTTP adapter imports snapshots but does not own immutable tree walks"
        );
    }

    #[test]
    fn list_directory_uses_the_shared_git_snapshot_representation() {
        let (router, git, http) = spy_router();

        SourceStore::list_directory(&router, &snapshot(), Some(&SourcePath::new("d").unwrap()))
            .expect("first directory listing");
        SourceStore::list_directory(&router, &snapshot(), Some(&SourcePath::new("d").unwrap()))
            .expect("second directory listing");

        assert_eq!(
            *git.directory_lists.lock().expect("git list recorder"),
            2,
            "directory listing is an immutable Git snapshot operation for both source kinds"
        );
        assert_eq!(
            *http.directory_lists.lock().expect("HTTP list recorder"),
            0,
            "the HTTP adapter must not receive immutable snapshot directory listings"
        );
    }

    #[test]
    fn git_url_ending_in_dot_git_routes_to_git_not_http() {
        // Routing must be by declared mode, never by url scheme/suffix. A `.git`
        // url under SourceMode::Git must hit the git backend, never http.
        let (router, git, http) = spy_router();

        SourceStore::resolve(
            &router,
            &request(
                "g",
                SourceLocation::Git {
                    url: "https://example.com/o/r.git".to_owned(),
                },
                RevisionSpec::Branch("main".into()),
            ),
            ResolvePolicy::Refresh,
        )
        .expect("Git resolve");

        assert_eq!(
            git.resolves
                .lock()
                .expect("git resolve recorder")
                .as_slice(),
            ["g"],
            "a `.git` URL declared Git-mode must route to the Git store"
        );
        assert!(
            http.resolves
                .lock()
                .expect("HTTP resolve recorder")
                .is_empty(),
            "a `.git` url must NOT reach the http backend regardless of its scheme/suffix"
        );
    }

    #[test]
    fn digest_reads_immutable_snapshots_through_the_git_store() {
        let (router, git, http) = spy_router();

        digest_snapshot(&router, &snapshot(), &[SourcePath::new("a").unwrap()])
            .expect("first digest");
        digest_snapshot(&router, &snapshot(), &[SourcePath::new("b").unwrap()])
            .expect("second digest");

        assert_eq!(
            *git.reads.lock().expect("git read recorder"),
            2,
            "digest_snapshot reads both Git and imported URL snapshots from the Git store"
        );
        assert_eq!(
            *http.reads.lock().expect("HTTP read recorder"),
            0,
            "digest_snapshot must not send immutable reads to the HTTP importer"
        );
    }

    #[test]
    fn unknown_source_name_has_a_defined_route() {
        // A name absent from the modes map must resolve to the git backend (the
        // default), never panic and never silently hit http.
        let (router, git, http) = spy_router();
        SourceStore::resolve(
            &router,
            &request(
                "mystery",
                SourceLocation::Git {
                    url: "https://example.com/o/r.git".to_owned(),
                },
                RevisionSpec::Branch("main".into()),
            ),
            ResolvePolicy::CachedOnly,
        )
        .expect("unmapped source resolves through the default route");
        assert_eq!(
            git.resolves
                .lock()
                .expect("git resolve recorder")
                .as_slice(),
            ["mystery"],
            "a source with no recorded mode must default to the git backend"
        );
        assert!(
            http.resolves
                .lock()
                .expect("HTTP resolve recorder")
                .is_empty(),
            "an unmapped source must never be routed to http"
        );
    }

    #[test]
    fn router_is_usable_as_dyn_source_store() {
        let (router, _git, _http) = spy_router();
        let as_dyn: &dyn SourceStore = &router;
        let result = as_dyn
            .resolve(
                &request(
                    "g",
                    SourceLocation::Git {
                        url: "https://x/y.git".to_owned(),
                    },
                    RevisionSpec::Branch("main".into()),
                ),
                ResolvePolicy::CachedOnly,
            )
            .expect("router is a SourceStore so sync(&dyn SourceStore) keeps working");
        assert_eq!(
            result.name,
            sn("g"),
            "the typed value must come back through the vtable from the Git-routed Spy, \
             pinning runtime dispatch through &dyn SourceStore rather than mere compilation"
        );
    }
}
