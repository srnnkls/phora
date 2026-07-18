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

pub struct StagedArtifact {
    pub files: Vec<StagedFile>,
    pub digest: String,
    pub vars_digest: Option<String>,
}

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
        std::fs::write(&out_path, &data)?;
        set_deterministic_mtime(&out_path, self.commit_time)?;

        if executable && self.policy.preserve_executable {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&out_path)?.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(&out_path, perms)?;
            }
        }

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

fn set_deterministic_mtime(path: &Path, commit_time: u64) -> Result<()> {
    let seconds = i64::try_from(commit_time)
        .map_err(|e| SourceError::Source(format!("commit_time out of range: {e}")))?;
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(seconds, 0))?;
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
