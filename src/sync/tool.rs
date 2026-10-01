//! Resolves a granted tool to the executable a build runs; the only place that calls mise.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::config::{ToolGrant, ToolSpec};
use crate::error::{Error, Result};

pub(super) struct ResolvedTool {
    pub(super) path: PathBuf,
    pub(super) digest: String,
}

const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "csh",
    "tcsh",
    "pwsh",
    "node",
    "deno",
    "bun",
    "perl",
    "ruby",
    "php",
    "lua",
    "osascript",
    "env",
    "xargs",
];

pub(super) fn resolve(grant: &ToolGrant, spec: &ToolSpec, program: &str) -> Result<ResolvedTool> {
    let path = match grant {
        ToolGrant::Path(path) => expand_home(path),
        ToolGrant::Mise => mise_executable(spec, program)?,
    };
    let fail = |detail: String| Error::Config(format!("tool `{spec}`: {detail}"));
    let canonical = std::fs::canonicalize(&path)
        .map_err(|e| fail(format!("{} is not usable: {e}", path.display())))?;
    if let Some(name) = [&path, &canonical]
        .into_iter()
        .filter_map(|p| p.file_name()?.to_str())
        .find(|name| is_interpreter(name))
    {
        return Err(fail(format!(
            "{} is the interpreter `{name}`, which runs whatever its arguments say; grant the \
             tool itself",
            path.display()
        )));
    }
    let meta =
        std::fs::metadata(&canonical).map_err(|e| fail(format!("{}: {e}", canonical.display())))?;
    if !meta.is_file() || !is_executable(&meta) {
        return Err(fail(format!(
            "{} is not an executable file",
            path.display()
        )));
    }
    let bytes = std::fs::read(&canonical)?;
    Ok(ResolvedTool {
        path: canonical,
        digest: format!("blake3:{}", blake3::hash(&bytes).to_hex()),
    })
}

fn is_interpreter(name: &str) -> bool {
    INTERPRETERS.contains(&name) || name.starts_with("python")
}

fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

#[derive(Deserialize)]
struct Executable {
    name: String,
    path: PathBuf,
}

fn mise_executable(spec: &ToolSpec, program: &str) -> Result<PathBuf> {
    let mut listed = bin_paths(spec)?;
    if listed.is_empty() {
        install(spec)?;
        listed = bin_paths(spec)?;
    }
    if listed.is_empty() {
        return Err(Error::Config(format!(
            "tool `{spec}`: mise installed nothing for it"
        )));
    }
    let names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    listed
        .iter()
        .find(|e| e.name == program)
        .map(|e| e.path.clone())
        .ok_or_else(|| {
            Error::Config(format!(
                "tool `{spec}` provides no executable `{program}`; it provides [{}]",
                names.join(", ")
            ))
        })
}

fn bin_paths(spec: &ToolSpec) -> Result<Vec<Executable>> {
    let output = mise()?
        .env("MISE_OFFLINE", "1")
        .args(["bin-paths", "--json", &spec.to_string()])
        .output()
        .map_err(|e| mise_missing(spec, &e))?;
    if !output.status.success() {
        return Err(Error::Config(format!(
            "tool `{spec}`: `mise bin-paths` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| {
        Error::Config(format!(
            "tool `{spec}`: unexpected `mise bin-paths --json` output: {e}"
        ))
    })
}

fn install(spec: &ToolSpec) -> Result<()> {
    let output = mise()?
        .args(["install", &spec.to_string()])
        .output()
        .map_err(|e| mise_missing(spec, &e))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::Config(format!(
        "tool `{spec}`: `mise install` failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// Run outside any project so no project config or trust prompt applies.
fn mise() -> Result<Command> {
    let cwd = std::env::temp_dir().join("phora-mise");
    std::fs::create_dir_all(&cwd)?;
    let mut command = Command::new("mise");
    command
        .current_dir(cwd)
        .env("MISE_YES", "1")
        .env("NO_COLOR", "1");
    Ok(command)
}

fn mise_missing(spec: &ToolSpec, error: &std::io::Error) -> Error {
    Error::Config(format!(
        "tool `{spec}` is granted through mise, but mise cannot run: {error}"
    ))
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}
