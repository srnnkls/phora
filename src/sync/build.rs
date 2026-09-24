//! Build sources: materialize pinned inputs, run the generator, import its output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::config::{
    BuildSpec, Config, DEFAULT_SHELL_PREFIX, HookCommand, ParsedSource, Refspec, Target,
};
use crate::error::{Error, Result};
use crate::lock::{BUILD_RESOLVED, LockedSource, encode_ref};
use crate::source::{
    BUILD_MIRROR, Commit, ResolvePolicy, ResolveRequest, ResolvedSource, RevisionSpec,
    SourceLocation, SourceName, SourceStore,
};

use super::progress::{Phase, SILENT};
use super::request::{ConflictPolicy, HookPolicy, PrunePolicy, SyncEvents, SyncOptions};
use super::resolve::{RoutedSources, selected_source_digest};
use super::state::FileStateStore;
use super::{
    RunOptions, SyncRunInput, SyncStatus, SyncWarning, SyncWorkspace, effective_lock, hooks,
    resolved_remotes, sync_workspace, transitive,
};

pub(super) struct Built {
    name: String,
    locked: LockedSource,
    resolved: ResolvedSource,
}

#[derive(Default)]
pub(super) struct Builds {
    built: Vec<Built>,
    input_locks: Vec<LockedSource>,
    pub failed: bool,
}

impl Builds {
    pub fn route(self, routed: &mut RoutedSources) {
        for built in self.built {
            let commit = built.locked.commit.clone();
            routed.commits.insert(
                (built.name.clone(), encode_ref(&Refspec::None)),
                commit.clone(),
            );
            routed
                .resolved
                .insert((built.name.clone(), commit), built.resolved);
            routed.locks.push((built.name, built.locked));
        }
        for locked in self.input_locks {
            let pinned = routed.locks.iter().any(|(_, l)| {
                l.name == locked.name && l.r#ref == locked.r#ref && l.instance == locked.instance
            });
            if !pinned {
                routed.locks.push((locked.name.clone(), locked));
            }
        }
    }
}

/// # Errors
///
/// Returns an error when inputs cannot be materialized, `--frozen` meets an unbuilt
/// source, or a build fails with no previous output to fall back to.
pub(super) fn run(
    input: &SyncRunInput<'_>,
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    backend: &dyn SourceStore,
    events: &mut SyncEvents<'_>,
) -> Result<Builds> {
    let mut builds = Builds::default();
    let specs: Vec<_> = parsed
        .iter()
        .filter_map(|(name, source)| source.build().map(|spec| (name, source, spec)))
        .collect();
    if specs.is_empty() {
        return Ok(builds);
    }
    input.sink().phase_started(Phase::Build);
    for (name, source, spec) in specs {
        let scratch = Scratch::create()?;
        let inputs = scratch.inputs();
        let input_locks = materialize(input, config, parsed, backend, name, spec, &scratch)?;
        builds.input_locks.extend(input_locks);
        let key = build_key(spec, &inputs)?;
        let locked = effective_lock(input).and_then(|lock| {
            lock.find_entry(name, None)
                .filter(|l| l.resolved == BUILD_RESOLVED)
                .cloned()
        });
        let previous = locked
            .as_ref()
            .and_then(|l| pinned_output(backend, name, &l.commit).ok().map(|r| (l, r)));

        let (resolved, key) = match previous {
            Some((locked, resolved)) if locked.build.as_deref() == Some(key.as_str()) => {
                (resolved, key)
            }
            _ if input.frozen() => {
                return Err(Error::Lock(format!(
                    "build source `{name}` has no locked output for its inputs; \
                     --frozen refuses to run a build"
                )));
            }
            previous => {
                let output = scratch.path.join("output");
                match execute(name, spec, &inputs, &output, backend) {
                    Ok(resolved) => (resolved, key),
                    Err(error) => {
                        let Some((locked, resolved)) = previous else {
                            return Err(Error::Sync(format!("build `{name}` failed: {error}")));
                        };
                        events.push_warning(SyncWarning::BuildFailed {
                            source: name.clone(),
                            detail: error.to_string(),
                        });
                        builds.failed = true;
                        (resolved, locked.build.clone().unwrap_or_default())
                    }
                }
            }
        };
        let commit = resolved.snapshot.commit().to_string();
        builds.built.push(Built {
            name: name.clone(),
            locked: LockedSource {
                name: name.clone(),
                git: BUILD_MIRROR.to_owned(),
                resolved: BUILD_RESOLVED.to_owned(),
                commit,
                digest: selected_source_digest(backend, &resolved, source)?,
                config_digest: source.config_digest(),
                r#ref: None,
                instance: None,
                build: Some(key),
            },
            resolved,
        });
    }
    input.sink().phase_finished(Phase::Build);
    Ok(builds)
}

/// Deploys through a private registry, so inputs land exactly as a target would receive them.
fn materialize(
    input: &SyncRunInput<'_>,
    config: &Config,
    parsed: &BTreeMap<String, ParsedSource>,
    backend: &dyn SourceStore,
    name: &str,
    spec: &BuildSpec,
    scratch: &Scratch,
) -> Result<Vec<LockedSource>> {
    let inputs = scratch.inputs();
    let mut config = config.clone();
    config.hooks = None;
    config.targets = spec
        .inputs
        .iter()
        .map(|source| {
            let transitive = parsed.get(source).is_some_and(ParsedSource::is_transitive);
            Ok((
                source.clone(),
                anchor(&inputs.join(source), source, transitive)?,
            ))
        })
        .collect::<Result<_>>()?;
    let mut parsed: BTreeMap<_, _> = parsed
        .iter()
        .filter(|(source, _)| spec.inputs.contains(source))
        .map(|(source, parsed)| (source.clone(), parsed.clone()))
        .collect();
    let mut remotes = resolved_remotes(&config, &parsed)?;
    let lock = effective_lock(input);
    let mut graph = transitive::resolve_transitive_graph(
        &config,
        &parsed,
        backend,
        input.frozen(),
        if input.refresh_sources() {
            None
        } else {
            lock.as_ref()
        },
    )?;
    let import_refs = std::mem::take(&mut graph.import_refs);
    let instances = graph.inject(&mut config, &mut parsed, &mut remotes);
    let workspace = SyncWorkspace {
        config,
        parsed,
        remotes,
        instances,
        import_refs,
        hook_candidates: Vec::new(),
        builds: Builds::default(),
    };
    let nested = SyncRunInput {
        options: SyncOptions {
            conflict_policy: ConflictPolicy::Overwrite,
            prune_policy: PrunePolicy::KeepOrphans,
            hook_policy: HookPolicy::None,
            ..input.options
        },
        locks: input.locks.clone(),
        resolver: None,
        sink: SILENT,
        trust_prompt: None,
        lockless: false,
        ..*input
    };
    let registry = FileStateStore::open(scratch.path.join("state"))?;
    let execution = sync_workspace(
        &nested,
        workspace,
        backend,
        &registry,
        Vec::new(),
        SyncEvents::new(SILENT),
        std::time::Instant::now(),
    )?;
    if execution.report.status == SyncStatus::Failed || execution.deploy_failures {
        return Err(Error::Sync(format!(
            "build `{name}`: materializing its inputs failed"
        )));
    }
    let locks = execution.report.locks;
    Ok(locks
        .base
        .into_iter()
        .chain(locks.local)
        .flat_map(|lock| lock.sources)
        .collect())
}

fn anchor(path: &Path, source: &str, transitive: bool) -> Result<Target> {
    let mut table = toml::value::Table::new();
    table.insert(
        "path".to_owned(),
        toml::Value::String(path.to_string_lossy().into_owned()),
    );
    if transitive {
        table.insert(
            "imports".to_owned(),
            toml::Value::Array(vec![toml::Value::String(source.to_owned())]),
        );
    } else {
        let mut binding = toml::value::Table::new();
        binding.insert("collapse".to_owned(), toml::Value::Boolean(false));
        let mut sources = toml::value::Table::new();
        sources.insert(source.to_owned(), toml::Value::Table(binding));
        table.insert("sources".to_owned(), toml::Value::Table(sources));
    }
    toml::Value::Table(table)
        .try_into()
        .map_err(|e| Error::Config(format!("build input `{source}`: {e}")))
}

/// Output symlinks are captured as the files they point to: a linked input is live
/// worktree content, and the scratch directory is gone after the build.
fn execute(
    name: &str,
    spec: &BuildSpec,
    inputs: &Path,
    output: &Path,
    backend: &dyn SourceStore,
) -> Result<ResolvedSource> {
    std::fs::create_dir_all(output)
        .map_err(|e| Error::Sync(format!("create build output {}: {e}", output.display())))?;
    let status = hooks::command(&spec.command)?
        .env("PHORA_INPUT", inputs)
        .env("PHORA_OUTPUT", output)
        .env("PHORA_SOURCE", name)
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| Error::Sync(format!("run `{}`: {e}", spec.command.display())))?;
    if !status.success() {
        return Err(Error::Sync(format!(
            "`{}` exited with {status}",
            spec.command.display()
        )));
    }
    backend
        .resolve(
            &ResolveRequest {
                name: SourceName::trusted(name.to_owned()),
                location: SourceLocation::Build {
                    output: Some(output.to_path_buf()),
                    follow_symlinks: true,
                },
                revision: RevisionSpec::None,
            },
            ResolvePolicy::Refresh,
        )
        .map_err(Into::into)
}

fn pinned_output(backend: &dyn SourceStore, name: &str, commit: &str) -> Result<ResolvedSource> {
    backend
        .resolve(
            &ResolveRequest {
                name: SourceName::trusted(name.to_owned()),
                location: SourceLocation::Build {
                    output: None,
                    follow_symlinks: false,
                },
                revision: RevisionSpec::Commit(commit.parse::<Commit>()?),
            },
            ResolvePolicy::CachedOnly,
        )
        .map_err(Into::into)
}

fn build_key(spec: &BuildSpec, inputs: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut field = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(b"phora build key v1");
    command_fields(&spec.command, &mut field);
    if let Some(key) = &spec.key {
        let output = hooks::command(key)?
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Error::Sync(format!("run build key `{}`: {e}", key.display())))?;
        if !output.status.success() {
            return Err(Error::Sync(format!(
                "build key `{}` exited with {}",
                key.display(),
                output.status
            )));
        }
        field(&output.stdout);
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(inputs)
        .follow_links(true)
        .sort_by_file_name()
    {
        let entry = entry.map_err(|e| Error::Sync(format!("walk build inputs: {e}")))?;
        if entry.file_type().is_dir() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(inputs)
            .map_err(|e| Error::Sync(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, entry.into_path()));
    }
    for (relative, path) in files {
        field(relative.as_bytes());
        let meta = std::fs::metadata(&path)?;
        field(if is_executable(&meta) {
            b"exec"
        } else {
            b"file"
        });
        field(&std::fs::read(&path)?);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn command_fields(command: &HookCommand, field: &mut impl FnMut(&[u8])) {
    match command {
        HookCommand::Shell { run, shell } => {
            field(b"shell");
            field(shell.as_deref().unwrap_or(DEFAULT_SHELL_PREFIX).as_bytes());
            field(run.as_bytes());
        }
        HookCommand::Exec { cmd } => {
            field(b"exec");
            for arg in cmd {
                field(arg.as_bytes());
            }
        }
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn create() -> Result<Self> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("phora-build-{}-{nonce}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self {
            path: path.canonicalize()?,
        })
    }
}

impl Scratch {
    fn inputs(&self) -> PathBuf {
        self.path.join("input")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
