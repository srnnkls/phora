use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::TemplateOptIn;
use crate::projection::model::{
    ArtifactRelativePath, ProjectedArtifact, ProjectedLeaf, TargetProjection,
};
use crate::source::{ExportPolicy, SourceEntryKind, SourceError, hash_framed_entry, vars_digest};

type Result<T> = std::result::Result<T, SourceError>;

pub struct StageRequest<'a> {
    pub artifact: &'a ProjectedArtifact,
    pub target: &'a TargetProjection,
    pub variables: &'a BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct StagedArtifact {
    pub files: Vec<StagedFile>,
    pub digest: String,
    pub vars_digest: Option<String>,
}

#[derive(Debug)]
pub struct StagedFile {
    pub destination: ArtifactRelativePath,
    pub size: u64,
    pub mtime: u64,
    pub blake3: String,
}

/// # Errors
/// - render failures, forbidden or escaping symlinks, deployed-name collisions,
///   leaf resolution failures, and staging I/O.
pub fn stage_artifact(
    request: &StageRequest<'_>,
    root: Option<&Path>,
    policy: &ExportPolicy,
    staging_dir: &Path,
    commit_time: u64,
    template_opt_in: &TemplateOptIn,
    mut resolve: impl FnMut(&Path) -> Result<(Vec<u8>, SourceEntryKind)>,
) -> Result<StagedArtifact> {
    std::fs::create_dir_all(staging_dir)?;

    let leaves: Vec<PlannedLeaf<'_>> = request
        .artifact
        .leaves
        .iter()
        .map(|leaf| PlannedLeaf::new(leaf, root))
        .collect();
    let mut repo_relative_sources: BTreeMap<&Path, &Path> = BTreeMap::new();
    for leaf in &leaves {
        if let Some(prior) = repo_relative_sources.insert(leaf.root_relative, leaf.repo_relative)
            && prior != leaf.repo_relative
        {
            return Err(SourceError::Source(format!(
                "root-relative source {} is ambiguous: {} and {} both key it",
                leaf.root_relative.display(),
                prior.display(),
                leaf.repo_relative.display()
            )));
        }
    }

    let renderer = Renderer::new(template_opt_in, request.variables);
    let mut walk = ExportWalk {
        out_base: staging_dir,
        policy,
        commit_time,
        files: Vec::new(),
        hasher: blake3::Hasher::new(),
        renderer: &renderer,
        deployed_names: BTreeMap::new(),
        rendered_any: false,
    };
    walk.run(&leaves, |rel| {
        resolve(repo_relative_sources.get(rel).copied().unwrap_or(rel))
    })?;

    let digest = format!("blake3:{}", walk.hasher.finalize().to_hex());
    let vars_digest = walk.rendered_any.then(|| renderer.vars_digest());
    Ok(StagedArtifact {
        files: walk.files,
        digest,
        vars_digest,
    })
}

struct PlannedLeaf<'a> {
    repo_relative: &'a Path,
    root_relative: &'a Path,
    dest: &'a Path,
}

impl<'a> PlannedLeaf<'a> {
    fn new(leaf: &'a ProjectedLeaf, root: Option<&Path>) -> Self {
        let repo_relative = Path::new(leaf.source.as_str());
        let root_relative = root
            .and_then(|r| repo_relative.strip_prefix(r).ok())
            .unwrap_or(repo_relative);
        Self {
            repo_relative,
            root_relative,
            dest: Path::new(leaf.destination.as_str()),
        }
    }
}

impl StageLeaf for PlannedLeaf<'_> {
    fn source(&self) -> &Path {
        self.root_relative
    }

    fn dest(&self) -> &Path {
        self.dest
    }
}

impl StagedRecord for StagedFile {
    fn staged(destination: &Path, size: u64, mtime: u64, blake3: String) -> Self {
        let destination = destination.to_string_lossy().replace('\\', "/");
        let destination = ArtifactRelativePath::new(&destination)
            .expect("staged destinations originate from validated artifact-relative paths");
        Self {
            destination,
            size,
            mtime,
            blake3,
        }
    }
}

pub(crate) trait StageLeaf {
    fn source(&self) -> &Path;
    fn dest(&self) -> &Path;
}

pub(crate) trait StagedRecord {
    fn staged(destination: &Path, size: u64, mtime: u64, blake3: String) -> Self;
}

pub(crate) struct Renderer<'a> {
    opt_in: &'a TemplateOptIn,
    env: minijinja::Environment<'static>,
    vars: &'a BTreeMap<String, String>,
}

impl<'a> Renderer<'a> {
    pub(crate) fn new(opt_in: &'a TemplateOptIn, vars: &'a BTreeMap<String, String>) -> Self {
        let mut env = minijinja::Environment::new();
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
        let keep_trailing_newline = true;
        env.set_keep_trailing_newline(keep_trailing_newline);
        // Bounds runaway templates from untrusted sources (e.g. unbounded loops).
        env.set_fuel(Some(1_000_000));
        Self { opt_in, env, vars }
    }

    fn rel_key(entry_rel: &Path) -> String {
        entry_rel.to_string_lossy().replace('\\', "/")
    }

    fn render(&self, entry_rel: &Path, source_bytes: &[u8]) -> Result<Rendered> {
        let key = Self::rel_key(entry_rel);
        if !self.opt_in.renders(&key) {
            return Ok(Rendered {
                bytes: source_bytes.to_vec(),
                templated: false,
            });
        }
        let template = std::str::from_utf8(source_bytes).map_err(|e| SourceError::Render {
            path: entry_rel.to_path_buf(),
            message: format!("template is not valid UTF-8: {e}"),
        })?;
        self.env
            .render_str(template, self.vars)
            .map(|bytes| Rendered {
                bytes: bytes.into_bytes(),
                templated: true,
            })
            .map_err(|e| SourceError::Render {
                path: entry_rel.to_path_buf(),
                message: e.to_string(),
            })
    }

    pub(crate) fn vars_digest(&self) -> String {
        vars_digest(self.vars)
    }
}

struct Rendered {
    bytes: Vec<u8>,
    templated: bool,
}

pub(crate) struct ExportWalk<'a, 'r, F> {
    pub(crate) out_base: &'a Path,
    pub(crate) policy: &'a ExportPolicy,
    pub(crate) commit_time: u64,
    pub(crate) files: Vec<F>,
    pub(crate) hasher: blake3::Hasher,
    pub(crate) renderer: &'a Renderer<'r>,
    pub(crate) deployed_names: BTreeMap<PathBuf, PathBuf>,
    pub(crate) rendered_any: bool,
}

fn dest_has_vcs_component(dest: &Path) -> bool {
    dest.components().any(|c| c.as_os_str() == ".git")
}

impl<F: StagedRecord> ExportWalk<'_, '_, F> {
    pub(crate) fn run<L: StageLeaf>(
        &mut self,
        leaves: &[L],
        mut resolve: impl FnMut(&Path) -> Result<(Vec<u8>, SourceEntryKind)>,
    ) -> Result<()> {
        for leaf in leaves {
            if !self.policy.vcs_opt_in && dest_has_vcs_component(leaf.dest()) {
                continue;
            }
            let (bytes, kind) = resolve(leaf.source())?;
            match kind {
                SourceEntryKind::File => {
                    self.stage_leaf(leaf.dest(), leaf.source(), &bytes, false)?;
                }
                SourceEntryKind::Executable => {
                    self.stage_leaf(leaf.dest(), leaf.source(), &bytes, true)?;
                }
                SourceEntryKind::Symlink => self.stage_link(leaf.dest(), &bytes)?,
            }
        }
        Ok(())
    }

    fn register_deployed_name(&mut self, deployed_rel: &Path, source_rel: &Path) -> Result<()> {
        let folded = crate::sync::confine::fold_path(deployed_rel);
        let name = folded.to_string_lossy().into_owned();
        if let Some(prior) = self.deployed_names.insert(folded, source_rel.to_path_buf()) {
            return Err(SourceError::DeployedNameCollision {
                name,
                first: prior,
                second: source_rel.to_path_buf(),
            });
        }
        Ok(())
    }

    fn stage_leaf(
        &mut self,
        deployed_rel: &Path,
        source_rel: &Path,
        source_bytes: &[u8],
        executable: bool,
    ) -> Result<()> {
        self.register_deployed_name(deployed_rel, source_rel)?;

        let rendered = self.renderer.render(source_rel, source_bytes)?;
        self.rendered_any |= rendered.templated;
        let data = rendered.bytes;

        let out_path = self.out_base.join(deployed_rel);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_leaf(
            &out_path,
            &data,
            self.commit_time,
            executable && self.policy.preserve_executable,
        )?;

        let tag: &[u8] = if executable {
            b"\x00exec\x00"
        } else {
            b"\x00file\x00"
        };
        hash_framed_entry(
            &mut self.hasher,
            deployed_rel.to_string_lossy().as_bytes(),
            tag,
            &data,
        );

        self.files.push(F::staged(
            deployed_rel,
            data.len() as u64,
            self.commit_time,
            blake3::hash(&data).to_hex().to_string(),
        ));
        Ok(())
    }

    fn stage_link(&mut self, deployed_rel: &Path, target: &[u8]) -> Result<()> {
        if !self.policy.allow_symlinks {
            return Err(SourceError::SymlinkNotAllowed {
                path: deployed_rel.to_path_buf(),
            });
        }
        if symlink_target_escapes(deployed_rel, target) {
            return Err(SourceError::SymlinkEscape {
                path: deployed_rel.to_path_buf(),
                target: String::from_utf8_lossy(target).into_owned(),
            });
        }
        self.register_deployed_name(deployed_rel, deployed_rel)?;

        let out_path = self.out_base.join(deployed_rel);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        materialize_symlink(&out_path, target)?;

        hash_framed_entry(
            &mut self.hasher,
            deployed_rel.to_string_lossy().as_bytes(),
            b"\x00link\x00",
            target,
        );
        Ok(())
    }
}

fn write_leaf(path: &Path, data: &[u8], commit_time: u64, executable: bool) -> Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::File::create(path)?;
    file.write_all(data)?;
    if executable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file.metadata()?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            file.set_permissions(perms)?;
        }
    }
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(commit_time))?;
    Ok(())
}

pub(crate) fn symlink_target_escapes(deployed_rel: &Path, target: &[u8]) -> bool {
    if matches!(target.first(), Some(b'/' | b'\\')) {
        return true;
    }
    if matches!(target, [drive, b':', ..] if drive.is_ascii_alphabetic()) {
        return true;
    }
    let parent_depth = deployed_rel.parent().map_or(0, |p| p.components().count());
    let mut depth = i64::try_from(parent_depth).unwrap_or(i64::MAX);
    for step in target.split(|&b| b == b'/' || b == b'\\') {
        match step {
            b"" | b"." => {}
            b".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth = depth.saturating_add(1),
        }
    }
    false
}

#[cfg(unix)]
fn materialize_symlink(out_path: &Path, target: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::OsStr::from_bytes(target);
    std::os::unix::fs::symlink(target, out_path)?;
    Ok(())
}

#[cfg(windows)]
fn materialize_symlink(out_path: &Path, target: &[u8]) -> Result<()> {
    let target = String::from_utf8_lossy(target);
    std::os::windows::fs::symlink_file(target.as_ref(), out_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;
    use crate::projection::model::Materialization;
    use crate::projection::model::{
        ContentTransform, ProjectedArtifact, ProjectedLeaf, ResolvedSourceRef, TargetPath,
        TargetProjection,
    };
    use crate::source::{
        GitBackend, ResolvePolicy, ResolveRequest, RevisionSpec, SourceLocation, SourcePath,
        SourceStore,
    };

    use crate::source::SourceName;

    fn sn(name: &str) -> SourceName {
        SourceName::trusted(name)
    }

    /// Author timestamp of the single commit in [`build_export_fixture`]; every
    /// staged file's mtime must equal this.
    const EXPORT_COMMIT_TIME: u64 = 1_700_000_000;

    const EDITOR_INIT_CONTENT: &[u8] = b"-- editor init\nvim.opt.number = true\n";
    const EDITOR_OPTS_CONTENT: &[u8] = b"-- nested opts\nreturn {}\n";
    const EDITOR_RUN_CONTENT: &[u8] = b"#!/bin/sh\necho run\n";
    const EDITOR_NOTES_CONTENT: &[u8] = b"scratch notes, excluded by **/*.bak\n";
    const LINK_NAME: &str = "link";
    const LINK_TARGET: &str = "init.lua";

    struct ExportFixture {
        _src: TempDir,
        _git_dir: TempDir,
        backend: GitBackend,
        url: String,
        /// Sole commit; its author time equals [`EXPORT_COMMIT_TIME`].
        commit: String,
    }

    impl ExportFixture {
        fn refresh(&self) {
            SourceStore::resolve(
                &self.backend,
                &ResolveRequest {
                    name: sn("src"),
                    location: SourceLocation::Git {
                        url: self.url.clone(),
                    },
                    revision: RevisionSpec::Branch("main".to_owned()),
                },
                ResolvePolicy::Refresh,
            )
            .expect("refresh fixture snapshot");
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn run_git_dated(
        cwd: &Path,
        args: &[&str],
        author_date: &str,
        committer_date: &str,
    ) -> std::process::Output {
        crate::sync::state::locking::assert_git_sandboxed(cwd);
        let _serial = crate::sync::state::locking::guard_git_fork();
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", author_date)
            .env("GIT_COMMITTER_DATE", committer_date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
        run_git_dated(cwd, args, "@1700000000 +0000", "@1700000000 +0000")
    }

    fn init_export_repo(src_path: &Path) {
        run_git(src_path, &["init", "-b", "main", "."]);
        run_git(src_path, &["config", "user.email", "test@example.com"]);
        run_git(src_path, &["config", "user.name", "Test"]);
        run_git(src_path, &["config", "core.autocrlf", "false"]);
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn commit_export_repo(src_path: &Path) -> String {
        run_git_dated(
            src_path,
            &["commit", "-m", "artifacts"],
            "@1700000000 +0000",
            "@1800000000 +0000",
        );
        String::from_utf8(run_git(src_path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn export_fixture_from(src: TempDir, commit: String) -> ExportFixture {
        let git_dir = TempDir::new().expect("git dir tempdir");
        let backend = GitBackend::new(git_dir.path().to_path_buf());
        let url = src.path().to_string_lossy().into_owned();

        ExportFixture {
            _src: src,
            _git_dir: git_dir,
            backend,
            url,
            commit,
        }
    }

    /// Clean base with no root-level symlink: `editor/` is symlink-free; the
    /// only symlink lives in a dedicated `linky/` artifact.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_export_fixture() -> ExportFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();

        init_export_repo(src_path);

        let editor = src_path.join("editor");
        std::fs::create_dir_all(editor.join("lua")).unwrap();
        std::fs::create_dir_all(editor.join("bin")).unwrap();
        std::fs::write(editor.join("init.lua"), EDITOR_INIT_CONTENT).unwrap();
        std::fs::write(editor.join("lua/opts.lua"), EDITOR_OPTS_CONTENT).unwrap();
        std::fs::write(editor.join("bin/run.sh"), EDITOR_RUN_CONTENT).unwrap();
        std::fs::write(editor.join("notes.bak"), EDITOR_NOTES_CONTENT).unwrap();

        std::fs::create_dir_all(src_path.join("lint")).unwrap();
        std::fs::write(src_path.join("lint/rules.toml"), b"[rules]\n").unwrap();

        let linky = src_path.join("linky");
        std::fs::create_dir_all(&linky).unwrap();
        std::fs::write(linky.join("init.lua"), EDITOR_INIT_CONTENT).unwrap();

        std::fs::create_dir_all(src_path.join(".hidden")).unwrap();
        std::fs::write(src_path.join(".hidden/secret"), b"nope\n").unwrap();

        run_git(src_path, &["add", "-A"]);
        run_git(
            src_path,
            &["update-index", "--chmod=+x", "editor/bin/run.sh"],
        );

        std::os::unix::fs::symlink(LINK_TARGET, linky.join(LINK_NAME)).unwrap();
        run_git(src_path, &["add", "linky/link"]);

        let commit = commit_export_repo(src_path);

        let link_mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", "linky/link"]).stdout).unwrap();
        assert!(
            link_mode.starts_with("120000"),
            "linky/link must be committed as a git symlink (120000), got: {link_mode}"
        );
        let run_mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", "editor/bin/run.sh"]).stdout)
                .unwrap();
        assert!(
            run_mode.starts_with("100755"),
            "editor/bin/run.sh must be committed executable (100755), got: {run_mode}"
        );

        export_fixture_from(src, commit)
    }

    fn build_collision_fixture(files: &[(&str, &[u8])]) -> ExportFixture {
        let src = TempDir::new().expect("collision src tempdir");
        let src_path = src.path();
        init_export_repo(src_path);

        let art = src_path.join("art");
        std::fs::create_dir_all(&art).expect("create art dir");
        for (name, content) in files {
            std::fs::write(art.join(name), content).expect("write collision file");
        }
        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        export_fixture_from(src, commit)
    }

    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_symlink_template_collision_fixture() -> ExportFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();
        init_export_repo(src_path);

        let art = src_path.join("art");
        std::fs::create_dir_all(&art).unwrap();
        std::fs::write(art.join("link.tmpl"), b"rendered\n").unwrap();
        std::os::unix::fs::symlink(LINK_TARGET, art.join("link")).unwrap();
        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        export_fixture_from(src, commit)
    }

    fn build_dir_template_collision_fixture() -> ExportFixture {
        let src = TempDir::new().expect("collision src tempdir");
        let src_path = src.path();
        init_export_repo(src_path);

        let art = src_path.join("art");
        std::fs::create_dir_all(art.join("config")).expect("create config dir");
        std::fs::write(art.join("config").join("inner.txt"), b"inner\n").expect("write inner");
        std::fs::write(art.join("config.tmpl"), b"rendered\n").expect("write config.tmpl");
        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        export_fixture_from(src, commit)
    }

    fn mtime_secs(path: &Path) -> u64 {
        let ft = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(path).expect("metadata of staged file"),
        );
        u64::try_from(ft.unix_seconds()).expect("non-negative mtime")
    }

    fn leaf(source: &str, dest: &str) -> ProjectedLeaf {
        ProjectedLeaf {
            source: SourcePath::new(source).expect("valid source path"),
            destination: ArtifactRelativePath::new(dest).expect("valid dest path"),
            transform: ContentTransform::Identity,
        }
    }

    fn stage_with(
        fixture: &ExportFixture,
        staging: &Path,
        leaves: &[ProjectedLeaf],
        policy: &ExportPolicy,
        vars: &BTreeMap<String, String>,
    ) -> Result<StagedArtifact> {
        let artifact = ProjectedArtifact {
            destination: TargetPath::new("artifact").expect("valid dest"),
            source: ResolvedSourceRef::new("src", &fixture.commit),
            materialization: Materialization::CollapsedDir {
                dir: "artifact".to_owned(),
            },
            kept_leaves: Vec::new(),
            leaves: leaves.to_vec(),
        };
        let target = TargetProjection {
            target: "dest".to_owned(),
            bindings: Vec::new(),
            artifacts: Vec::new(),
            warnings: Vec::new(),
        };
        let resolved = SourceStore::resolve(
            &fixture.backend,
            &ResolveRequest {
                name: sn("src"),
                location: SourceLocation::Git {
                    url: fixture.url.clone(),
                },
                revision: RevisionSpec::Commit(
                    fixture.commit.parse().expect("fixture commit is valid hex"),
                ),
            },
            ResolvePolicy::CachedOnly,
        )
        .expect("resolve staged fixture snapshot");
        stage_artifact(
            &StageRequest {
                artifact: &artifact,
                target: &target,
                variables: vars,
            },
            None,
            policy,
            staging,
            EXPORT_COMMIT_TIME,
            &TemplateOptIn::SuffixOnly,
            |repo_relative| {
                let path = SourcePath::new(&repo_relative.to_string_lossy().replace('\\', "/"))?;
                let entry = SourceStore::read(&fixture.backend, &resolved.snapshot, &path)?;
                Ok((entry.bytes, entry.meta.kind))
            },
        )
    }

    fn export_named(
        fixture: &ExportFixture,
        staging: &Path,
        leaves: &[ProjectedLeaf],
        policy: &ExportPolicy,
    ) -> Result<StagedArtifact> {
        stage_with(fixture, staging, leaves, policy, &BTreeMap::new())
    }

    /// The `editor` artifact's three non-`.bak` leaves, each mapped to its
    /// dir-relative deployed path — the leaf-granular expression of `exclude **/*.bak`.
    fn editor_leaves() -> Vec<ProjectedLeaf> {
        vec![
            leaf("editor/init.lua", "init.lua"),
            leaf("editor/lua/opts.lua", "lua/opts.lua"),
            leaf("editor/bin/run.sh", "bin/run.sh"),
        ]
    }

    fn export_editor(
        fixture: &ExportFixture,
        staging: &Path,
        policy: &ExportPolicy,
    ) -> Result<StagedArtifact> {
        export_named(fixture, staging, &editor_leaves(), policy)
    }

    /// The `linky` artifact's blob and symlink leaves, mapped to dir-relative dests.
    fn linky_leaves() -> Vec<ProjectedLeaf> {
        vec![
            leaf("linky/init.lua", "init.lua"),
            leaf(&format!("linky/{LINK_NAME}"), LINK_NAME),
        ]
    }

    #[test]
    fn export_materializes_files_with_exact_content() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        export_editor(&fixture, staging.path(), &ExportPolicy::default()).expect("export succeeds");

        assert_eq!(
            std::fs::read(staging.path().join("init.lua")).expect("init.lua exists"),
            EDITOR_INIT_CONTENT
        );
        assert_eq!(
            std::fs::read(staging.path().join("lua/opts.lua")).expect("nested opts.lua exists"),
            EDITOR_OPTS_CONTENT
        );
    }

    #[test]
    fn export_excludes_bak_files_by_path_matcher() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let result = export_editor(&fixture, staging.path(), &ExportPolicy::default())
            .expect("export succeeds");

        assert!(
            !staging.path().join("notes.bak").exists(),
            "**/*.bak must exclude notes.bak from staging"
        );
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.destination.as_str() == "notes.bak"),
            "excluded file must not appear in StagedArtifact.files"
        );
    }

    /// A real source blob renamed onto a dest that buries it under a `.git`
    /// path component — the write-time vector a `take` rename produces. The
    /// `keep.txt` leaf is the control: it must survive.
    fn dot_git_dest_leaves(dest: &str) -> Vec<ProjectedLeaf> {
        vec![
            leaf("editor/init.lua", dest),
            leaf("editor/lua/opts.lua", "keep.txt"),
        ]
    }

    #[test]
    fn export_prunes_a_nested_dot_git_dest_at_write() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let result = export_named(
            &fixture,
            staging.path(),
            &dot_git_dest_leaves("nested/.git/config"),
            &ExportPolicy::default(),
        )
        .expect("export succeeds; the .git-dest leaf is pruned, not an error");

        assert!(
            !staging.path().join("nested/.git/config").exists(),
            "a leaf whose DEST buries it under a `.git` component must be pruned at write, never \
             staged under the deploy target where confine cannot guard it"
        );
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.destination.as_str() == "nested/.git/config"),
            "the pruned `.git`-dest leaf must be absent from the manifest; got: {:?}",
            result.files
        );
        assert!(
            staging.path().join("keep.txt").exists()
                && result
                    .files
                    .iter()
                    .any(|f| f.destination.as_str() == "keep.txt"),
            "the control leaf with a clean dest must still be staged and recorded; got: {:?}",
            result.files
        );
    }

    #[test]
    fn export_prunes_a_top_level_dot_git_dest_at_write() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let result = export_named(
            &fixture,
            staging.path(),
            &dot_git_dest_leaves(".git/config"),
            &ExportPolicy::default(),
        )
        .expect("export succeeds; the top-level .git-dest leaf is pruned");

        assert!(
            !staging.path().join(".git/config").exists(),
            "a `.git` component at the TOP of the dest must be pruned at write"
        );
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.destination.as_str() == ".git/config"),
            "the pruned top-level `.git`-dest leaf must be absent from the manifest; got: {:?}",
            result.files
        );
    }

    #[test]
    fn export_writes_a_dot_git_dest_when_policy_opts_in() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            vcs_opt_in: true,
            ..ExportPolicy::default()
        };
        let result = export_named(
            &fixture,
            staging.path(),
            &dot_git_dest_leaves("nested/.git/config"),
            &policy,
        )
        .expect("export succeeds with the .git opt-in");

        assert!(
            staging.path().join("nested/.git/config").exists(),
            "with the `.git` opt-in the write-time prune must be suppressed and the leaf staged"
        );
        assert!(
            result
                .files
                .iter()
                .any(|f| f.destination.as_str() == "nested/.git/config"),
            "an opted-in `.git`-dest leaf must appear in the manifest; got: {:?}",
            result.files
        );
    }

    #[test]
    fn export_writes_a_dot_gitignore_basename_dest_without_opt_in() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let result = export_named(
            &fixture,
            staging.path(),
            &dot_git_dest_leaves("nested/.gitignore"),
            &ExportPolicy::default(),
        )
        .expect("export succeeds; a `.gitignore` basename is not a `.git` component");

        assert!(
            staging.path().join("nested/.gitignore").exists(),
            "a `.gitignore` basename is NOT a `.git` path component and must be staged even without \
             the opt-in"
        );
        assert!(
            result
                .files
                .iter()
                .any(|f| f.destination.as_str() == "nested/.gitignore"),
            "the `.gitignore` leaf must be recorded in the manifest; got: {:?}",
            result.files
        );
    }

    #[test]
    fn export_result_lists_exported_files() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let result = export_editor(&fixture, staging.path(), &ExportPolicy::default())
            .expect("export succeeds");

        let mut listed: Vec<String> = result
            .files
            .iter()
            .map(|f| f.destination.to_string())
            .collect();
        listed.sort();

        assert!(
            listed.contains(&"init.lua".to_string()),
            "files must list init.lua, got {listed:?}"
        );
        assert!(
            listed.contains(&"lua/opts.lua".to_string()),
            "files must list nested lua/opts.lua, got {listed:?}"
        );
        assert!(
            listed.contains(&"bin/run.sh".to_string()),
            "files must list bin/run.sh, got {listed:?}"
        );
    }

    #[test]
    fn export_sets_mtime_to_commit_time() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let result = export_editor(&fixture, staging.path(), &ExportPolicy::default())
            .expect("export succeeds");

        assert!(!result.files.is_empty(), "expected staged files");
        for file in &result.files {
            let on_disk = staging.path().join(file.destination.as_str());
            assert_eq!(
                mtime_secs(&on_disk),
                EXPORT_COMMIT_TIME,
                "staged {} mtime must equal commit_time",
                file.destination
            );
            assert_eq!(
                file.mtime, EXPORT_COMMIT_TIME,
                "StagedFile.mtime must equal commit_time"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn export_preserves_executable_bit_by_default() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        export_editor(&fixture, staging.path(), &ExportPolicy::default()).expect("export succeeds");

        let mode = std::fs::metadata(staging.path().join("bin/run.sh"))
            .expect("run.sh exists")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "preserve_executable default true: run.sh must have an exec bit, mode {mode:o}"
        );
    }

    #[test]
    fn export_rejects_symlink_when_policy_disallows() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            allow_symlinks: false,
            ..ExportPolicy::default()
        };
        let err = export_named(&fixture, staging.path(), &linky_leaves(), &policy)
            .expect_err("linky/link is a symlink; allow_symlinks=false must error");

        assert!(
            matches!(err, SourceError::SymlinkNotAllowed { .. }),
            "must be a real symlink-policy rejection, not the unimplemented stub: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(LINK_NAME),
            "rejection must name the offending symlink {LINK_NAME}, got: {msg}"
        );
        assert!(
            msg.contains("symlink"),
            "rejection must identify a symlink-policy rejection, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_materializes_symlink_when_allowed() {
        let fixture = build_export_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            allow_symlinks: true,
            ..ExportPolicy::default()
        };
        export_named(&fixture, staging.path(), &linky_leaves(), &policy)
            .expect("export with symlinks allowed");

        let link = staging.path().join(LINK_NAME);
        let meta = std::fs::symlink_metadata(&link).expect("link entry exists");
        assert!(
            meta.file_type().is_symlink(),
            "allowed symlink must be materialized as a symlink, not dereferenced"
        );
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            Path::new(LINK_TARGET),
            "symlink target must be preserved verbatim"
        );
    }

    #[cfg(unix)]
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_symlink_target_fixture(rel: &str, target: &str) -> ExportFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();
        init_export_repo(src_path);

        let art = src_path.join("art");
        let link_path = art.join(rel);
        std::fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        std::fs::write(art.join("sibling"), b"sib\n").unwrap();
        std::os::unix::fs::symlink(target, &link_path).unwrap();

        run_git(src_path, &["add", "-A"]);
        let commit = commit_export_repo(src_path);

        let mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", &format!("art/{rel}")]).stdout)
                .unwrap();
        assert!(
            mode.starts_with("120000"),
            "art/{rel} must be committed as a git symlink (120000), got: {mode}"
        );

        export_fixture_from(src, commit)
    }

    #[cfg(unix)]
    fn symlink_leaf(rel: &str) -> Vec<ProjectedLeaf> {
        vec![leaf(&format!("art/{rel}"), rel)]
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_symlink_with_absolute_target() {
        let fixture = build_symlink_target_fixture("link", "/etc/passwd");
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            allow_symlinks: true,
            ..ExportPolicy::default()
        };
        let err = export_named(&fixture, staging.path(), &symlink_leaf("link"), &policy)
            .expect_err(
                "an absolute symlink target (/etc/passwd) escapes the artifact tree and must be \
                 rejected at stage time even with allow_symlinks=true",
            );

        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("link"),
            "escape diagnostic must name the offending symlink path `link`, got: {err}"
        );
        assert!(
            msg.contains("/etc/passwd"),
            "escape diagnostic must name the escaping target `/etc/passwd`, got: {err}"
        );
        assert!(
            msg.contains("symlink") && msg.contains("escape"),
            "diagnostic must read as a symlink-escape rejection, not an unrelated export failure, \
             got: {err}"
        );
        assert!(
            std::fs::symlink_metadata(staging.path().join("link")).is_err(),
            "nothing must be materialized when an escaping symlink is rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_symlink_target_escaping_root_via_dotdot() {
        let fixture = build_symlink_target_fixture("dir/link", "../../outside");
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            allow_symlinks: true,
            ..ExportPolicy::default()
        };
        let err = export_named(&fixture, staging.path(), &symlink_leaf("dir/link"), &policy)
            .expect_err(
                "dir/link -> ../../outside escapes the artifact root (parent `dir`, then two `..` \
                 steps climb above root) and must be rejected at stage time",
            );

        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("dir/link"),
            "escape diagnostic must name the offending symlink path `dir/link`, got: {err}"
        );
        assert!(
            msg.contains("../../outside"),
            "escape diagnostic must name the escaping target `../../outside`, got: {err}"
        );
        assert!(
            msg.contains("symlink") && msg.contains("escape"),
            "diagnostic must read as a symlink-escape rejection, not an unrelated export failure, \
             got: {err}"
        );
        assert!(
            std::fs::symlink_metadata(staging.path().join("dir/link")).is_err(),
            "nothing must be materialized when an escaping symlink is rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_materializes_symlink_with_in_root_dotdot_target() {
        let fixture = build_symlink_target_fixture("dir/link", "../sibling");
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            allow_symlinks: true,
            ..ExportPolicy::default()
        };
        export_named(&fixture, staging.path(), &symlink_leaf("dir/link"), &policy)
            .expect("dir/link -> ../sibling stays inside the artifact root and must materialize");

        let link = staging.path().join("dir/link");
        let meta = std::fs::symlink_metadata(&link).expect("in-root symlink must be materialized");
        assert!(
            meta.file_type().is_symlink(),
            "an in-root `..` target must be materialized as a symlink, not rejected or dereferenced"
        );
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            Path::new("../sibling"),
            "an accepted in-root symlink must preserve its target bytes verbatim"
        );
    }

    #[test]
    fn export_rejects_symlink_colliding_with_rendered_deployed_name() {
        let fixture = build_symlink_template_collision_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let policy = ExportPolicy {
            allow_symlinks: true,
            ..ExportPolicy::default()
        };
        let leaves = vec![
            leaf(
                "art/link.tmpl",
                &TemplateOptIn::SuffixOnly.deployed_name("link.tmpl"),
            ),
            leaf("art/link", "link"),
        ];
        let err = export_named(&fixture, staging.path(), &leaves, &policy)
            .expect_err("symlink `link` and rendered `link.tmpl` both deploy to `link`");
        assert!(
            matches!(err, SourceError::DeployedNameCollision { .. }),
            "a symlink and a blob mapping to the same deployed name must collide, not last-writer-wins, got: {err:?}"
        );
    }

    #[test]
    fn export_rejects_directory_colliding_with_rendered_deployed_name() {
        let fixture = build_dir_template_collision_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let leaves = vec![
            leaf(
                "art/config.tmpl",
                &TemplateOptIn::SuffixOnly.deployed_name("config.tmpl"),
            ),
            leaf("art/config/inner.txt", "config"),
        ];
        let err = export_named(&fixture, staging.path(), &leaves, &ExportPolicy::default())
            .expect_err("directory `config` and rendered `config.tmpl` both deploy to `config`");
        assert!(
            matches!(err, SourceError::DeployedNameCollision { .. }),
            "a directory and a blob mapping to the same deployed name must collide, not surface a raw fs error, got: {err:?}"
        );
    }

    #[test]
    fn export_aborts_a_runaway_template_via_fuel_instead_of_hanging() {
        let runaway = b"{% for i in range(100000000) %}x{% endfor %}\n";
        let fixture = build_collision_fixture(&[("loop.txt.tmpl", runaway)]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let leaves = vec![leaf(
            "art/loop.txt.tmpl",
            &TemplateOptIn::SuffixOnly.deployed_name("loop.txt.tmpl"),
        )];
        let err = export_named(&fixture, staging.path(), &leaves, &ExportPolicy::default())
            .expect_err("a template that exhausts the fuel budget must surface an error");
        assert!(
            matches!(err, SourceError::Render { .. }),
            "exceeding the minijinja fuel cap must abort the artifact as a Render error, not panic or hang, got: {err:?}"
        );
    }

    // ---- StagedArtifact.vars_digest (TPH-010) ----

    /// Every leaf under the `art` subtree, mapped to its suffix-stripped dir-relative dest.
    fn art_leaves(fixture: &ExportFixture) -> Vec<ProjectedLeaf> {
        let resolved = SourceStore::resolve(
            &fixture.backend,
            &ResolveRequest {
                name: sn("src"),
                location: SourceLocation::Git {
                    url: fixture.url.clone(),
                },
                revision: RevisionSpec::Commit(
                    fixture.commit.parse().expect("fixture commit is valid"),
                ),
            },
            ResolvePolicy::CachedOnly,
        )
        .expect("resolve cached fixture snapshot");
        SourceStore::inventory(
            &fixture.backend,
            &resolved.snapshot,
            Some(&SourcePath::new("art").expect("valid source root")),
        )
        .expect("inventory art leaves")
        .entries
        .into_iter()
        .map(|entry| {
            let rel = entry.path.as_str();
            leaf(
                &format!("art/{rel}"),
                &TemplateOptIn::SuffixOnly.deployed_name(rel),
            )
        })
        .collect()
    }

    fn export_art_with_vars(
        fixture: &ExportFixture,
        staging: &Path,
        vars: &BTreeMap<String, String>,
    ) -> StagedArtifact {
        let leaves = art_leaves(fixture);
        stage_with(fixture, staging, &leaves, &ExportPolicy::default(), vars).expect("export art")
    }

    #[test]
    fn export_vars_digest_is_none_when_no_template_rendered() {
        let fixture = build_collision_fixture(&[("plain.txt", b"static body\n")]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let mut vars = BTreeMap::new();
        vars.insert("name".to_owned(), "ada".to_owned());

        let result = export_art_with_vars(&fixture, staging.path(), &vars);

        assert_eq!(
            result.vars_digest, None,
            "a feature-free artifact (no `.tmpl`, no template rendered) must report vars_digest = \
             None even when vars are present, so a vars change leaves it untouched (INV-8), got {:?}",
            result.vars_digest
        );
    }

    #[test]
    fn export_vars_digest_is_some_when_a_template_rendered() {
        let fixture = build_collision_fixture(&[("greeting.txt.tmpl", b"hello {{ name }}\n")]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let mut vars = BTreeMap::new();
        vars.insert("name".to_owned(), "ada".to_owned());

        let result = export_art_with_vars(&fixture, staging.path(), &vars);

        assert!(
            result.vars_digest.is_some(),
            "an artifact that rendered at least one template must report vars_digest = Some(..), \
             got {:?}",
            result.vars_digest
        );
    }

    #[test]
    fn export_vars_digest_changes_when_a_var_value_changes() {
        let fixture = build_collision_fixture(&[("greeting.txt.tmpl", b"hello {{ name }}\n")]);
        fixture.refresh();

        let staging_a = TempDir::new().expect("staging a");
        let mut vars_a = BTreeMap::new();
        vars_a.insert("name".to_owned(), "ada".to_owned());
        let digest_a = export_art_with_vars(&fixture, staging_a.path(), &vars_a).vars_digest;

        let staging_b = TempDir::new().expect("staging b");
        let mut vars_b = BTreeMap::new();
        vars_b.insert("name".to_owned(), "grace".to_owned());
        let digest_b = export_art_with_vars(&fixture, staging_b.path(), &vars_b).vars_digest;

        assert!(
            digest_a.is_some() && digest_b.is_some(),
            "both renders must produce a vars_digest"
        );
        assert_ne!(
            digest_a, digest_b,
            "changing a vars value must change the vars_digest so every templated artifact is \
             marked Outdated and re-renders, got {digest_a:?} vs {digest_b:?}"
        );
    }

    #[test]
    fn export_vars_digest_hashes_full_vars_not_only_consumed_keys() {
        let fixture = build_collision_fixture(&[("greeting.txt.tmpl", b"hello {{ name }}\n")]);
        fixture.refresh();

        let staging_a = TempDir::new().expect("staging a");
        let mut vars_a = BTreeMap::new();
        vars_a.insert("name".to_owned(), "ada".to_owned());
        let digest_a = export_art_with_vars(&fixture, staging_a.path(), &vars_a).vars_digest;

        let staging_b = TempDir::new().expect("staging b");
        let mut vars_b = BTreeMap::new();
        vars_b.insert("name".to_owned(), "ada".to_owned());
        vars_b.insert("unused".to_owned(), "x".to_owned());
        let digest_b = export_art_with_vars(&fixture, staging_b.path(), &vars_b).vars_digest;

        assert_ne!(
            digest_a, digest_b,
            "the digest scope is the FULL effective vars map, not consumed-keys-only: adding a var \
             the template never references must still change vars_digest, got {digest_a:?} vs {digest_b:?}"
        );
    }

    // ---- mapped export (leaf aliasing, T2b) ----

    const MAP_TOP_CONTENT: &[u8] = b"# top-level agents\n";
    const MAP_NESTED_CONTENT: &[u8] = b"# nested agents\n";
    const MAP_TOOL_CONTENT: &[u8] = b"#!/bin/sh\necho tool\n";

    /// Top-level and nested leaves plus an executable, all committed at root.
    #[expect(
        clippy::unwrap_used,
        reason = "fixture setup fails loudly; git CLI is assumed present"
    )]
    fn build_map_fixture() -> ExportFixture {
        let src = TempDir::new().unwrap();
        let src_path = src.path();

        init_export_repo(src_path);

        std::fs::write(src_path.join("AGENTS.md"), MAP_TOP_CONTENT).unwrap();
        std::fs::create_dir_all(src_path.join("nested")).unwrap();
        std::fs::write(src_path.join("nested/AGENTS.md"), MAP_NESTED_CONTENT).unwrap();
        std::fs::write(src_path.join("tool.sh"), MAP_TOOL_CONTENT).unwrap();

        run_git(src_path, &["add", "-A"]);
        run_git(src_path, &["update-index", "--chmod=+x", "tool.sh"]);

        let commit = commit_export_repo(src_path);

        let tool_mode =
            String::from_utf8(run_git(src_path, &["ls-files", "-s", "tool.sh"]).stdout).unwrap();
        assert!(
            tool_mode.starts_with("100755"),
            "tool.sh must be committed executable (100755), got: {tool_mode}"
        );

        export_fixture_from(src, commit)
    }

    fn export_mapped(
        fixture: &ExportFixture,
        staging: &Path,
        map: &[(&str, &str)],
    ) -> Result<StagedArtifact> {
        let leaves: Vec<ProjectedLeaf> = map.iter().map(|(key, dest)| leaf(key, dest)).collect();
        export_named(fixture, staging, &leaves, &ExportPolicy::default())
    }

    #[test]
    fn mapped_export_renames_top_level_blob() {
        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("AGENTS.md", "CLAUDE.md")];
        let result = export_mapped(&fixture, staging.path(), map).expect("mapped export succeeds");

        assert_eq!(
            std::fs::read(staging.path().join("CLAUDE.md")).expect("CLAUDE.md staged"),
            MAP_TOP_CONTENT,
            "the source bytes of AGENTS.md must land at the renamed dest CLAUDE.md"
        );
        assert!(
            !staging.path().join("AGENTS.md").exists(),
            "the source name must not be staged; only the dest name"
        );
        let listed: Vec<String> = result
            .files
            .iter()
            .map(|f| f.destination.to_string())
            .collect();
        assert_eq!(
            listed,
            vec!["CLAUDE.md".to_string()],
            "files must contain exactly the dest path, not the source key, got {listed:?}"
        );
    }

    #[test]
    fn mapped_export_flattens_nested_key_to_dest() {
        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("nested/AGENTS.md", "codex.md")];
        let result = export_mapped(&fixture, staging.path(), map).expect("mapped export succeeds");

        assert_eq!(
            std::fs::read(staging.path().join("codex.md")).expect("codex.md staged"),
            MAP_NESTED_CONTENT,
            "a nested source key must stage flat at the single-component dest"
        );
        assert!(
            !staging.path().join("nested").exists(),
            "the nested source path must not be reproduced under staging"
        );
        let listed: Vec<String> = result
            .files
            .iter()
            .map(|f| f.destination.to_string())
            .collect();
        assert_eq!(
            listed,
            vec!["codex.md".to_string()],
            "files must list the flat dest, not the nested key, got {listed:?}"
        );
    }

    #[test]
    fn mapped_export_stages_all_entries_of_a_multi_key_map() {
        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("AGENTS.md", "CLAUDE.md"), ("nested/AGENTS.md", "codex.md")];
        let result = export_mapped(&fixture, staging.path(), map).expect("mapped export succeeds");

        assert_eq!(
            std::fs::read(staging.path().join("CLAUDE.md")).expect("CLAUDE.md staged"),
            MAP_TOP_CONTENT,
            "the top-level key's bytes must land at its dest"
        );
        assert_eq!(
            std::fs::read(staging.path().join("codex.md")).expect("codex.md staged"),
            MAP_NESTED_CONTENT,
            "the nested key's bytes must land at its dest"
        );

        let mut listed: Vec<String> = result
            .files
            .iter()
            .map(|f| f.destination.to_string())
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            vec!["CLAUDE.md".to_string(), "codex.md".to_string()],
            "files must contain exactly both dests, no source keys and no extras, got {listed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mapped_export_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("tool.sh", "run")];
        export_mapped(&fixture, staging.path(), map).expect("mapped export succeeds");

        let mode = std::fs::metadata(staging.path().join("run"))
            .expect("run staged")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "executable source leaf must keep an exec bit through the rename, mode {mode:o}"
        );
    }

    #[test]
    fn mapped_export_errors_when_key_is_a_directory() {
        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("nested", "x.md")];
        let err = export_mapped(&fixture, staging.path(), map)
            .expect_err("a key resolving to a directory must error; only regular files map");
        assert!(
            !staging.path().join("x.md").exists(),
            "no dest must be staged when a key resolves to a directory"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("nested"),
            "the error must name the offending key so an unrelated failure can't pass, got {msg:?}"
        );
    }

    #[test]
    fn mapped_export_errors_when_key_is_missing() {
        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("does/not/exist.md", "x.md")];
        let err = export_mapped(&fixture, staging.path(), map)
            .expect_err("a key resolving to nothing must error");
        assert!(
            !staging.path().join("x.md").exists(),
            "no dest must be staged when a key resolves to nothing"
        );
        assert!(
            matches!(err, SourceError::FileAbsent { .. }),
            "a missing key must be the absent-at-commit variant the snapshot read reports, not \
             the not-a-leaf variant, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("exist.md"),
            "the error must name the missing key so an unrelated failure can't pass, got {msg:?}"
        );
    }

    #[test]
    fn mapped_export_errors_when_two_keys_share_a_dest() {
        let fixture = build_map_fixture();
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let map = &[("AGENTS.md", "x.md"), ("nested/AGENTS.md", "x.md")];
        let err = export_mapped(&fixture, staging.path(), map)
            .expect_err("two keys mapping to one dest must collide");
        assert!(
            matches!(err, SourceError::DeployedNameCollision { .. }),
            "mapped collisions must route through register_deployed_name, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("x.md"),
            "the collision must name the shared dest, got {msg:?}"
        );
    }

    #[test]
    fn export_rejects_deployed_names_colliding_only_by_ascii_case() {
        let fixture = build_collision_fixture(&[
            ("first.md", b"upper deploys to README.md\n"),
            ("second.md", b"lower deploys to readme.md\n"),
        ]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let leaves = vec![
            leaf("art/first.md", "README.md"),
            leaf("art/second.md", "readme.md"),
        ];
        let err = export_named(&fixture, staging.path(), &leaves, &ExportPolicy::default())
            .expect_err("`README.md` and `readme.md` fold to one deployed name and must collide");
        assert!(
            matches!(err, SourceError::DeployedNameCollision { .. }),
            "a case-only deployed-name clash must be a hard DeployedNameCollision, not \
             last-writer-wins, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("first.md") && msg.contains("second.md"),
            "the collision must name both colliding source paths, got {msg:?}"
        );
        assert!(
            msg.to_lowercase().contains("readme.md"),
            "the collision must name the case-folded deployed name, got {msg:?}"
        );
    }

    // ---- CLIFF-UNIFOLD-005: non-ASCII deployed-name case-collision + casefold divergence ----

    #[test]
    fn export_rejects_deployed_names_colliding_only_by_latin_accented_case() {
        let fixture = build_collision_fixture(&[
            ("first.md", b"upper accented deploy\n"),
            ("second.md", b"lower accented deploy\n"),
        ]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let leaves = vec![
            leaf("art/first.md", "\u{00c9}.md"),  // É.md
            leaf("art/second.md", "\u{00e9}.md"), // é.md
        ];
        let err = export_named(&fixture, staging.path(), &leaves, &ExportPolicy::default())
            .expect_err(
                "`\u{00c9}.md` and `\u{00e9}.md` fold to one deployed name and must collide",
            );
        assert!(
            matches!(err, SourceError::DeployedNameCollision { .. }),
            "a non-ASCII case-only deployed-name clash must be a hard DeployedNameCollision (the \
             registration fold is not ASCII-only), not last-writer-wins, got {err:?}"
        );
    }

    #[test]
    fn export_rejects_deployed_names_colliding_only_by_cyrillic_case() {
        let fixture = build_collision_fixture(&[
            ("first.md", b"upper cyrillic deploy\n"),
            ("second.md", b"lower cyrillic deploy\n"),
        ]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let leaves = vec![
            leaf("art/first.md", "\u{0401}.md"),  // Ё.md
            leaf("art/second.md", "\u{0451}.md"), // ё.md
        ];
        let err = export_named(&fixture, staging.path(), &leaves, &ExportPolicy::default())
            .expect_err("Cyrillic `\u{0401}.md` and `\u{0451}.md` fold to one deployed name");
        assert!(
            matches!(err, SourceError::DeployedNameCollision { .. }),
            "a Cyrillic case-only deployed-name clash must be a hard DeployedNameCollision, got \
             {err:?}"
        );
    }

    #[test]
    fn export_keeps_eszett_and_ss_deployed_names_distinct_documented_limitation() {
        let fixture = build_collision_fixture(&[
            ("sharp.md", b"deploys to strasse.md\n"),
            (
                "plain.md",
                b"deploys to strasse.md too but folded distinctly\n",
            ),
        ]);
        fixture.refresh();
        let staging = TempDir::new().expect("staging dir");

        let leaves = vec![
            leaf("art/sharp.md", "stra\u{00df}e.md"), // straße.md
            leaf("art/plain.md", "strasse.md"),
        ];
        export_named(&fixture, staging.path(), &leaves, &ExportPolicy::default()).expect(
            "documented UNIFOLD limitation: the registration fold is simple lowercase, so \
             `stra\u{00df}e.md` and `strasse.md` are DISTINCT deployed names and must NOT collide \
             — full case-folding would over-reject this",
        );
        assert!(
            staging.path().join("stra\u{00df}e.md").exists(),
            "the `stra\u{00df}e.md` leaf must materialize as its own deployed name"
        );
        assert!(
            staging.path().join("strasse.md").exists(),
            "the `strasse.md` leaf must materialize distinctly alongside `stra\u{00df}e.md`"
        );
    }
}
