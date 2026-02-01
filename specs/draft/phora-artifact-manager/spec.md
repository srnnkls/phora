---
issue_type: Feature
created: 2026-01-31
status: Draft
stage: design
---

# Phora: Git-based Artifact Package Manager

A package manager for fetching, caching, and projecting artifacts from git sources to local filesystem targets. Think `cargo` for any file-based artifacts.

## Philosophy

- **Invisible infrastructure** — Git fetches, caches, versions. User never sees it.
- **Projection, not reference** — Real files via reflinks, not symlinks.
- **Edit-safe** — COW semantics mean local edits never corrupt shared cache.
- **Vendoring built-in** — Eject to take ownership, keep file.
- **Boring UX** — Four commands. Lock file. Done.
- **No target metadata** — Phora does not persist any `.phora-*` files/directories in targets.
  (Temporary staging directories may be created during sync and removed afterward.)

## Use Cases

- Dotfiles modules from multiple repos
- Shared configuration (linters, editor configs, CI templates)
- Design systems, component libraries
- Policy libraries (OPA/Rego, Sentinel)
- Any file artifacts versioned in git

## Commands

|Command                                                     |Purpose                                                 |
|------------------------------------------------------------|--------------------------------------------------------|
|`phora add <url>`                                           |Parse URL, add source to config                         |
|`phora sync`                                                |Fetch sources + project to targets                      |
|`phora update [source]`                                     |Bump lock to latest, then sync                          |
|`phora list [--plan]`                                       |Show sources and deployment state; --plan shows pending |
|`phora verify`                                              |Verify deployed files by hashing contents (cold path)   |
|`phora where ...`                                           |Query global registry (where-used / deployments)        |
|`phora eject <artifact> --source <source> --target <target>`|Stop managing, keep file (vendor)                       |
|`phora clean`                                               |Garbage-collect unused cached snapshots                 |
|`phora rebuild-registry`                                    |Rebuild global registry from lock + on-disk targets     |
|`phora check-match --source <source> <path>`                |Debug include/exclude matching (like `git check-ignore`)|

## Files

```
project/
├── phora.toml          # what you want (committed)
├── phora.local.toml    # local overrides (NOT committed; optional)
├── phora.lock          # resolved sources from phora.toml (committed)
└── phora.local.lock    # resolved sources from phora.local.toml (NOT committed)
~/.phora/
├── git/                # bare mirrors (invisible)
│   ├── company.git
│   └── personal.git
├── cache/              # exported snapshots (plain files, no .git)
│   └── <source>/
│       └── <commit>/
│           └── <root>/
│               └── <artifact>/
├── state/              # deployment registry (authoritative; no writes into targets)
│   ├── targets/
│   │   └── <target>/
│   │       ├── meta.toml
│   │       └── artifacts/
│   │           └── <source>/
│   │               └── <artifact>.toml
│   └── locks/
│       └── state.lock
└── meta.json           # strategy cache, filesystem info
```

### phora.local.toml (local overrides)

`phora.local.toml` allows machine/user-specific overrides without modifying the shared project config.
It is intended for:
  * Local filesystem paths (e.g., local git checkouts)
  * Local auth selection (e.g., tokens, ssh key locations)
  * Local target paths (different OS/usernames)
  * Development-mode deployment choices (e.g., link-mode for local sources)

Hard rules:
  * `phora.local.toml` MUST NOT be required for the project to function.
  * `phora.local.toml` SHOULD be ignored by VCS (add to `.gitignore`).
  * If present, `phora.local.toml` MUST be loaded after `phora.toml` and merged as an overlay.

Config loading order (effective config):
  1. Read `phora.toml`
  2. If present, read `phora.local.toml`
  3. Compute EffectiveConfig = merge(base, local_overlay)

Merge semantics:
  * Objects/tables merge recursively by key.
  * Scalars in local overlay replace base values.
  * Arrays in local overlay replace base arrays (no concatenation) unless explicitly documented otherwise.

Operational note:
  * `phora.lock` is generated ONLY from `phora.toml` — always safe to commit.
  * `phora.local.lock` is generated ONLY for sources overridden in `phora.local.toml`.
  * Effective lock = merge(phora.lock, phora.local.lock) where local entries override base.
  * For debuggability, `phora list` SHOULD indicate when local overrides are active.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        phora sync                           │
└─────────────────────────────────────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        ▼                   ▼                   ▼
┌───────────────────┐ ┌───────────────┐ ┌───────────────────┐
│   Git Backend     │ │    Cache      │ │    Projection     │
│   (gitoxide)      │ │   (export)    │ │  (reflink/copy)   │
├───────────────────┤ ├───────────────┤ ├───────────────────┤
│ • bare mirrors    │ │ • tree export │ │ • multi-source    │
│ • fetch/auth      │ │ • atomic      │ │ • layouts         │
│ • ref resolution  │ │ • no .git     │ │ • atomic swap     │
│ • integrity       │ │ • pathspecs   │ │ • eject/restore   │
└───────────────────┘ └───────────────┘ └───────────────────┘
        │                   │                   │
        └─────────┬─────────┘                   │
                  ▼                             ▼
          ~/.phora/git/                   target paths
          ~/.phora/cache/                (plain directories)
          ~/.phora/state/                (global authoritative state)
```

**Hard constraints:** No `.git` in cache or targets. No Phora metadata written into targets. Everything deployed is plain directories.

**No target metadata:** No persistent `.phora-*` files or directories in targets.

-----

## Config Schema

### phora.toml

```toml
version = 1

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"
root = "modules"

[sources.company-configs]
git = "https://github.com/company/shared-configs.git"
tag = "v2.1"
root = "configs"
include = ["editor", "lint"]        # artifact-level: only these artifacts
exclude = ["**/test/**", "*.bak"]   # path-level: exclude within artifacts

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
tag = "v1.0"
root = "languages"
allow_symlinks = false              # default
preserve_executable = true          # default

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]

[targets.vscode]
path = "~/.config/Code/User"
sources = ["dotfiles", "company-configs"]
layout = "flat"

[targets.cupcake-policies]
path = "~/.cupcake/policies/claude"
sources = ["loqui"]
layout = { type = "prefixed", separator = "/" }
```

### phora.local.toml (examples)

Use cases:

**1) Override a source to use a local checkout and live-link it (development workflow):**

```toml
# phora.local.toml (NOT committed)
version = 1

[sources.loqui]
git = "/home/soeren/dev/loqui"   # local path override
branch = "main"
deploy = "link"                  # optional: link-mode for local dev
```

**2) Override target paths on a different OS/user:**

```toml
version = 1

[targets.vscode]
path = "C:/Users/Soeren/AppData/Roaming/Code/User"
```

**3) Override auth/token source:**

```toml
version = 1

[hosts.github]
auth = { type = "token", env = "GITHUB_TOKEN_WORK" }
```

Compatibility:
  * Any key valid in `phora.toml` is valid in `phora.local.toml`.
  * Unknown keys SHOULD error (to avoid silent misconfiguration).

Optional CLI ergonomics (non-normative):
  * `phora config --effective` prints EffectiveConfig after merges (for debugging).
  * `phora sync --no-local` ignores `phora.local.toml` (for CI/repro runs).

### phora.lock

Lock files contain **only source resolution** — no projections. Deployment state lives in the registry.

```toml
# Generated by phora from phora.toml - do not edit
# Safe to commit: contains only sources from phora.toml
version = 1

[[sources]]
name = "dotfiles"
git = "https://github.com/me/dotfiles.git"
resolved = "main"
commit = "abc123def456789"
digest = "sha256:a1b2c3..."

[[sources]]
name = "company-configs"
git = "https://github.com/company/shared-configs.git"
resolved = "v2.1"
commit = "def456789abc123"
digest = "sha256:d4e5f6..."

[[sources]]
name = "loqui"
git = "https://github.com/srnnkls/loqui.git"
resolved = "v1.0"
commit = "789xyz123456abc"
digest = "sha256:g7h8i9..."
```

### phora.local.lock

Generated for sources overridden in `phora.local.toml`. NOT committed.

```toml
# Generated by phora from phora.local.toml - do not edit
# DO NOT COMMIT: contains local-only source overrides
version = 1

[[sources]]
name = "loqui"                              # overrides base lock entry
git = "/home/soeren/dev/loqui"              # local checkout
resolved = "main"
commit = "local-abc123def456789"
digest = "sha256:local..."
```

**Write logic during sync:**

```rust
for (name, source) in effective_config.sources {
    let locked_source = resolve(source);

    if local_config.overrides(name) {
        local_lock.sources.push(locked_source);
    } else {
        base_lock.sources.push(locked_source);
    }
}

write("phora.lock", base_lock);
if !local_lock.sources.is_empty() {
    write("phora.local.lock", local_lock);
} else {
    remove_if_exists("phora.local.lock");
}
```

**Benefits:**
  * `phora update` on a non-overridden source updates `phora.lock` — safe to commit immediately
  * Local dev with a checkout never touches `phora.lock`
  * CI sees only `phora.lock` (no local files), always reproducible
  * Clear mental model: "local files are for local state"

## Data Model

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ─────────────────────────────────────────────────────────────
// Config (phora.toml)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
}

#[derive(Debug, Deserialize)]
pub struct Host {
    /// URL template for git operations. Supports: {owner}, {repo}, {ref}, {path}
    pub git_url: Option<String>,
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum AuthConfig {
    #[serde(rename = "ssh")]
    Ssh { key: Option<PathBuf> },
    #[serde(rename = "token")]
    Token { env: String },
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub git: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub allow_symlinks: bool,
    #[serde(default)]
    pub allow_submodules: bool,
    #[serde(default = "default_true")]
    pub preserve_executable: bool,
}

fn default_true() -> bool { true }

impl Source {
    pub fn refspec(&self) -> Refspec {
        if let Some(rev) = &self.rev {
            Refspec::Rev(rev.clone())
        } else if let Some(tag) = &self.tag {
            Refspec::Tag(tag.clone())
        } else if let Some(branch) = &self.branch {
            Refspec::Branch(branch.clone())
        } else {
            Refspec::Branch("main".into())
        }
    }

    pub fn export_policy(&self) -> ExportPolicy {
        ExportPolicy {
            allow_symlinks: self.allow_symlinks,
            allow_submodules: self.allow_submodules,
            preserve_executable: self.preserve_executable,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Refspec {
    Branch(String),
    Tag(String),
    Rev(String),
}

impl std::fmt::Display for Refspec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Branch(s) | Self::Tag(s) | Self::Rev(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Target {
    pub path: PathBuf,
    pub sources: Option<Vec<String>>,
    #[serde(default)]
    pub layout: LayoutConfig,
}

impl Target {
    pub fn resolve_sources<'a>(&'a self, all: &'a BTreeMap<String, Source>) -> Vec<&'a str> {
        match &self.sources {
            Some(names) => names.iter().map(|s| s.as_str()).collect(),
            None => all.keys().map(|s| s.as_str()).collect(),
        }
    }

    pub fn expanded_path(&self) -> PathBuf {
        let path_str = self.path.to_string_lossy();
        if path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path_str[2..]);
            }
        }
        self.path.clone()
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(from = "LayoutConfigRaw")]
pub struct LayoutConfig {
    pub kind: LayoutKind,
    pub separator: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    #[default]
    Flat,
    BySource,
    Prefixed,
}

impl LayoutConfig {
    pub fn artifact_path(&self, source: &str, artifact: &str) -> PathBuf {
        match self.kind {
            LayoutKind::Flat => PathBuf::from(artifact),
            LayoutKind::BySource => PathBuf::from(source).join(artifact),
            LayoutKind::Prefixed => {
                PathBuf::from(format!("{}{}{}", source, self.separator, artifact))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LayoutConfigRaw {
    Simple(String),
    Full {
        r#type: String,
        separator: Option<String>,
    },
}

impl From<LayoutConfigRaw> for LayoutConfig {
    fn from(raw: LayoutConfigRaw) -> Self {
        match raw {
            LayoutConfigRaw::Simple(s) => LayoutConfig {
                kind: LayoutKind::parse(&s),
                separator: if s == "prefixed" { "-".into() } else { String::new() },
            },
            LayoutConfigRaw::Full { r#type, separator } => LayoutConfig {
                kind: LayoutKind::parse(&r#type),
                separator: separator.unwrap_or_else(|| {
                    if r#type == "prefixed" { "-".into() } else { String::new() }
                }),
            },
        }
    }
}

impl LayoutKind {
    fn parse(s: &str) -> Self {
        match s {
            "by-source" => Self::BySource,
            "prefixed" => Self::Prefixed,
            _ => Self::Flat,
        }
    }
}
```

**Note on `layout.separator`:**

* `separator` is ignored unless `layout.kind == Prefixed`.
* For `Flat` and `BySource`, the effective separator is an empty string and MUST NOT affect computed paths.

```rust
// ─────────────────────────────────────────────────────────────
// Lock (phora.lock)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lock {
    pub version: u32,
    pub sources: Vec<LockedSource>,
    // NOTE: No projections — deployment state lives in registry
}

impl Lock {
    pub fn find_source(&self, name: &str) -> Option<&LockedSource> {
        self.sources.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSource {
    pub name: String,
    pub git: String,
    pub resolved: String,
    pub commit: String,
    pub digest: String,
}

/// Effective lock merges base and local locks
pub fn merge_locks(base: &Lock, local: Option<&Lock>) -> Lock {
    let mut merged = base.clone();
    if let Some(local) = local {
        for local_source in &local.sources {
            // Local overrides base by name
            merged.sources.retain(|s| s.name != local_source.name);
            merged.sources.push(local_source.clone());
        }
    }
    merged
}

// ─────────────────────────────────────────────────────────────
// Export Policy
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExportPolicy {
    pub allow_symlinks: bool,
    pub allow_submodules: bool,
    pub preserve_executable: bool,
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self {
            allow_symlinks: false,
            allow_submodules: false,
            preserve_executable: true,
        }
    }
}
```

## Path Matching

Patterns control which artifacts are discovered and which files are exported/projected.

### Pattern Classification

A pattern is classified by its structure:

| Pattern Type       | Rule                                          | Matches Against    |
| ------------------ | --------------------------------------------- | ------------------ |
| **Artifact-level** | No `/` and no `**` and doesn't start with `/` | Artifact name only |
| **Path-level**     | Contains `/` or `**` or starts with `/`       | Full relative path |

Examples:

| Pattern         | Type            | Matches                         |
| --------------- | --------------- | ------------------------------- |
| `editor`        | Artifact        | Artifact named "editor"         |
| `code-*`        | Artifact        | Artifacts starting with "code-" |
| `**/test/**`    | Path            | Any path containing `/test/`    |
| `/editor`       | Path (anchored) | Only `editor` at root           |
| `editor/*.json` | Path            | JSON files in editor artifact   |

### Pattern Syntax

Glob syntax (compatible with globset crate):

| Syntax   | Meaning                         |
| -------- | ------------------------------- |
| `*`      | Any sequence except `/`         |
| `**`     | Any sequence including `/`      |
| `?`      | Any single character except `/` |
| `[abc]`  | Any character in set            |
| `[!abc]` | Any character not in set        |

### Anchoring

* **Unanchored (default)**: matches anywhere in path
* **Anchored (starts with /)**: matches from root only

Phora convention: For unanchored path-level patterns, Phora normalizes by prepending `**/` to enable "match anywhere" semantics. This differs from standard glob behavior.

### Evaluation Order

1. If include is empty → all items are candidates
2. If include is non-empty → only items matching at least one pattern are candidates
3. exclude patterns filter out from candidates

### Implementation

```rust
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;

pub struct PathMatcher {
    artifact_include: Option<GlobSet>,
    artifact_exclude: GlobSet,
    path_include: Option<GlobSet>,
    path_exclude: GlobSet,
}

impl PathMatcher {
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, Error> {
        let (art_inc, path_inc) = Self::partition_patterns(include)?;
        let (art_exc, path_exc) = Self::partition_patterns(exclude)?;

        Ok(Self {
            artifact_include: Self::build_globset_opt(&art_inc)?,
            artifact_exclude: Self::build_globset(&art_exc)?,
            path_include: Self::build_globset_opt(&path_inc)?,
            path_exclude: Self::build_globset(&path_exc)?,
        })
    }

    /// Classify and normalize patterns
    fn partition_patterns(patterns: &[String]) -> Result<(Vec<String>, Vec<String>), Error> {
        let mut artifact = Vec::new();
        let mut path = Vec::new();

        for p in patterns {
            if Self::is_path_level(p) {
                path.push(Self::normalize_path_pattern(p));
            } else {
                artifact.push(p.clone());
            }
        }

        Ok((artifact, path))
    }

    /// Path-level if starts with `/`, contains `/`, or contains `**`
    fn is_path_level(pattern: &str) -> bool {
        pattern.starts_with('/') || pattern.contains('/') || pattern.contains("**")
    }

    /// Normalize: strip leading `/` for anchored; prepend `**/` for unanchored path patterns
    fn normalize_path_pattern(pattern: &str) -> String {
        if pattern.starts_with('/') {
            // Anchored: strip leading slash, match from start
            pattern[1..].to_string()
        } else if pattern.starts_with("**/") {
            // Already unanchored
            pattern.to_string()
        } else {
            // Unanchored: prepend **/ to match anywhere
            format!("**/{}", pattern)
        }
    }

    fn build_globset(patterns: &[String]) -> Result<GlobSet, Error> {
        let mut builder = GlobSetBuilder::new();
        for p in patterns {
            builder.add(Glob::new(p)?);
        }
        Ok(builder.build()?)
    }

    fn build_globset_opt(patterns: &[String]) -> Result<Option<GlobSet>, Error> {
        if patterns.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Self::build_globset(patterns)?))
        }
    }

    /// Check if artifact name passes artifact-level filters
    pub fn allows_artifact(&self, name: &str) -> bool {
        if let Some(inc) = &self.artifact_include {
            if !inc.is_match(name) {
                return false;
            }
        }
        !self.artifact_exclude.is_match(name)
    }

    /// Check if relative path passes path-level filters
    pub fn allows_path(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        if let Some(inc) = &self.path_include {
            if !inc.is_match(&*path_str) {
                return false;
            }
        }
        !self.path_exclude.is_match(&*path_str)
    }
}
```

## Artifact Discovery

### Definition

An artifact is a directory that is a direct child of the configured root.

```
<root>/
├── code-review/     ← artifact "code-review"
│   ├── SKILL.md
│   └── examples/
├── debugging/       ← artifact "debugging"
│   └── SKILL.md
├── README.md        ← NOT an artifact (file)
└── .hidden/         ← NOT an artifact (hidden)
```

### Rules

1. **Directories only**: Files at root level are not artifacts
2. **Direct children only**: Nested directories are content, not separate artifacts
3. **Name = directory name**: No transformation
4. **Hidden excluded**: Names starting with `.` are skipped
5. **v1 symlink rule**: Symlink-as-artifact at root level is disallowed, even when `allow_symlinks=true`. Symlinks are allowed within artifact contents.

### Discovery Algorithm

```rust
use std::path::Path;

pub fn discover_artifacts(
    cache_root: &Path,
    matcher: &PathMatcher,
) -> Result<Vec<String>, Error> {
    let mut artifacts = Vec::new();

    for entry in std::fs::read_dir(cache_root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(Error::InvalidArtifactName)?;

        // Skip hidden
        if name.starts_with('.') {
            continue;
        }

        // v1 rule: symlink-as-artifact is disallowed
        if file_type.is_symlink() {
            return Err(Error::SymlinkArtifactNotSupportedV1 {
                path: name.to_string(),
            });
        }

        // Artifacts must be directories
        if !file_type.is_dir() {
            continue;
        }

        // Apply artifact-level include/exclude
        if matcher.allows_artifact(name) {
            artifacts.push(name.to_string());
        }
    }

    artifacts.sort(); // deterministic order
    Ok(artifacts)
}
```

### Edge Cases

| Scenario                       | Behavior                         |
| ------------------------------ | -------------------------------- |
| Empty root (no subdirs)        | No artifacts; warning emitted    |
| Root contains only files       | No artifacts; warning emitted    |
| Name starts with `.`           | Skipped                          |
| Name contains spaces           | Allowed (discouraged)            |
| Symlink at root level          | Error (v1: not supported)        |
| Symlink within artifact        | Allowed if `allow_symlinks=true` |
| Broken symlink within artifact | Error during export/projection   |

## Cache

### Model: Exported Snapshots

Phora's cache is pure filesystem state derived from a Git commit tree. It contains no Git metadata and makes no assumptions about being a checkout.

```
~/.phora/
├── git/                  # bare mirrors (gitoxide-managed)
│   ├── company.git
│   └── dotfiles.git
└── cache/                # exported snapshots (plain files, no .git)
    └── <source>/
        └── <commit>/
            └── <root>/
                └── <artifact>/
```

### Properties

* **No shell-out**: All Git operations via gix
* **No worktrees**: Cache directories are exports, not checkouts
* **Content-addressable**: Key is `<source>/<commit>`; content is immutable
* **Deterministic**: Export result defined by (repo, commit, root, include/exclude, policy)

### Export Process

When `~/.phora/cache/<source>/<commit>/...` doesn't exist:

1. Open mirror repo `~/.phora/git/<source>.git`
2. Resolve commit (from lock or freshly resolved)
3. Load commit tree
4. Select subtree at root (if configured)
5. Walk entries, apply path-level include/exclude
6. Materialize files into staging directory
7. Atomically rename staging → final cache path

### Atomic Export

Export writes to staging first:

```
~/.phora/cache/<source>/<commit>.staging-<nonce>/
```

Then renames to final path:

```
~/.phora/cache/<source>/<commit>/
```

If export fails, staging is removed; any existing cache remains untouched.

### Symlink, Executable, and Submodule Policy

| Entry Type          | Default  | Behavior                                                                          |
| ------------------- | -------- | --------------------------------------------------------------------------------- |
| Symlink (120000)    | Reject   | Error with clear message; opt-in via `allow_symlinks=true`                        |
| Submodule (160000)  | Reject   | Error; opt-in via `allow_submodules=true` (v1: still errors, reserved for future) |
| Executable (100755) | Preserve | Set +x on Unix; recorded in registry on Windows                                   |

When allowed, symlinks are materialized as symlinks (not dereferenced). Phora never follows symlinks during export.

**v1 limitation (Windows):** Directory symlinks may not be reproduced correctly. Git symlink targets are commonly relative to the link location, so type inference via `metadata(target)` is unreliable. Phora always creates file symlinks on Windows.

### Digest

Lock file digest is computed from the exported snapshot, not Git objects. This represents exactly what will be deployed.

**Definition:** stable hash over (relative_path, mode_class, content_or_target) for all entries, excluding `.phora-*` files.

### Implementation

```rust
use gix::ObjectId;
use std::path::{Path, PathBuf};

pub struct Cache {
    git_dir: PathBuf,   // ~/.phora/git
    cache_dir: PathBuf, // ~/.phora/cache
}

impl Cache {
    pub fn new(git_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self { git_dir, cache_dir }
    }

    pub fn snapshot_path(&self, source: &str, commit: &str, root: Option<&Path>) -> PathBuf {
        let mut path = self.cache_dir.join(source).join(commit);
        if let Some(r) = root {
            path = path.join(r);
        }
        path
    }

    pub fn ensure_snapshot(
        &self,
        source: &str,
        commit: &str,
        root: Option<&Path>,
        matcher: &PathMatcher,
        policy: &ExportPolicy,
    ) -> Result<PathBuf, Error> {
        let final_path = self.cache_dir.join(source).join(commit);

        if final_path.exists() {
            return Ok(self.snapshot_path(source, commit, root));
        }

        let stage = final_path.with_extension(format!("staging-{}", nonce()));
        if stage.exists() {
            std::fs::remove_dir_all(&stage)?;
        }

        self.export_snapshot(source, commit, root, matcher, policy, &stage)?;

        std::fs::rename(&stage, &final_path)?;

        Ok(self.snapshot_path(source, commit, root))
    }

    fn export_snapshot(
        &self,
        source: &str,
        commit: &str,
        root: Option<&Path>,
        matcher: &PathMatcher,
        policy: &ExportPolicy,
        out_dir: &Path,
    ) -> Result<(), Error> {
        let mirror = self.git_dir.join(format!("{}.git", source));
        let repo = gix::open(&mirror)?;

        let oid = ObjectId::from_hex(commit.as_bytes())?;
        let commit_obj = repo.find_commit(oid)?;
        let tree = commit_obj.tree()?;

        // Navigate to root subtree if specified
        let subtree = match root {
            Some(r) => {
                let entry = tree.lookup_entry_by_path(r)?
                    .ok_or(Error::RootNotFound { root: r.to_path_buf() })?;
                repo.find_tree(entry.object_id())?
            }
            None => tree,
        };

        std::fs::create_dir_all(out_dir)?;

        self.export_tree_recursive(&repo, &subtree, out_dir, Path::new(""), matcher, policy)?;

        Ok(())
    }

    fn export_tree_recursive(
        &self,
        repo: &gix::Repository,
        tree: &gix::Tree,
        out_base: &Path,
        rel_path: &Path,
        matcher: &PathMatcher,
        policy: &ExportPolicy,
    ) -> Result<(), Error> {
        for entry in tree.iter() {
            let entry = entry?;
            let name = entry.filename();
            let entry_rel = rel_path.join(name);
            let out_path = out_base.join(&entry_rel);

            // Apply path-level filtering
            if !matcher.allows_path(&entry_rel) {
                continue;
            }

            match entry.mode().kind() {
                gix::object::tree::EntryKind::Blob => {
                    let blob = repo.find_blob(entry.object_id())?;
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&out_path, blob.data)?;
                }

                gix::object::tree::EntryKind::BlobExecutable => {
                    let blob = repo.find_blob(entry.object_id())?;
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&out_path, blob.data)?;

                    if policy.preserve_executable {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mut perms = std::fs::metadata(&out_path)?.permissions();
                            perms.set_mode(perms.mode() | 0o111);
                            std::fs::set_permissions(&out_path, perms)?;
                        }
                    }
                }

                gix::object::tree::EntryKind::Link => {
                    if !policy.allow_symlinks {
                        return Err(Error::SymlinkNotAllowed { path: entry_rel });
                    }

                    let blob = repo.find_blob(entry.object_id())?;
                    let target = std::str::from_utf8(blob.data)?;

                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    create_symlink(&out_path, Path::new(target))?;
                }

                gix::object::tree::EntryKind::Tree => {
                    std::fs::create_dir_all(&out_path)?;
                    let subtree = repo.find_tree(entry.object_id())?;
                    self.export_tree_recursive(repo, &subtree, out_base, &entry_rel, matcher, policy)?;
                }

                gix::object::tree::EntryKind::Commit => {
                    // Submodule
                    if !policy.allow_submodules {
                        return Err(Error::SubmoduleNotAllowed { path: entry_rel });
                    }
                    // v1: even if allowed, we don't recursively fetch submodules
                }
            }
        }

        Ok(())
    }
}

fn nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", n)
}

fn create_symlink(dst: &Path, target: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dst)?;
        Ok(())
    }

    #[cfg(windows)]
    {
        // v1 limitation (intentional, predictable):
        // Always create file symlinks on Windows.
        // Reason: the symlink target may be relative and `metadata(target)` is unreliable
        // from within the cache/projection context. Directory symlinks may be added in v2
        // by resolving target type via Git tree semantics during export.
        std::os::windows::fs::symlink_file(target, dst)?;
        Ok(())
    }
}
```

## Projection

### Model: Stage + Atomic Directory Swap

Phora deploys artifacts into targets as plain directories, using reflink when possible, with atomic swap so partial installs are never visible.

**Hard constraint:** Targets have no `.git`. Everything deployed is just files.

**No target metadata:** Phora does not persist manifests inside targets. Deployment state is stored in a global registry.

### Per-target Reflink Detection

Reflink support depends on both cache and target filesystems. Detection is performed per (cache_dev, target_dev) pair.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionStrategy {
    Reflink,
    Copy,
}

pub fn detect_projector(cache_path: &Path, target_path: &Path) -> Box<dyn Projector> {
    let _ = std::fs::create_dir_all(target_path);

    if probe_reflink(cache_path, target_path) {
        return Box::new(ReflinkProjector);
    }

    Box::new(CopyProjector)
}

fn probe_reflink(cache_path: &Path, target_path: &Path) -> bool {
    let probe_src = cache_path.join(".phora-reflink-probe");
    let probe_dst = target_path.join(".phora-reflink-probe");

    // Create probe file
    if std::fs::write(&probe_src, b"reflink-probe").is_err() {
        return false;
    }

    let result = reflink_copy::reflink(&probe_src, &probe_dst).is_ok();

    // Cleanup
    let _ = std::fs::remove_file(&probe_src);
    let _ = std::fs::remove_file(&probe_dst);

    result
}
```

The `reflink-copy` crate handles platform differences:

```toml
[dependencies]
reflink-copy = "0.1"  # Wraps clonefile/FICLONE/FSCTL_DUPLICATE_EXTENTS
```

Results are cached in `~/.phora/meta.json` keyed by device ID or mount point.

### Platform-Specific Projection Strategies

#### Strategy Matrix (v1)

| OS        | Tier 1 (v1)                     | Tier 2 (future)           |
|-----------|---------------------------------|---------------------------|
| **macOS** | Reflink (APFS) → Copy           | macFUSE overlay           |
| **Linux** | Reflink (Btrfs/XFS) → Copy      | FUSE overlay, bind mounts |
| **Windows** | Reflink (ReFS/Dev Drive) → Copy | ProjFS                    |

#### macOS

| Strategy     | Support             | Notes                                        |
|--------------|---------------------|----------------------------------------------|
| **Reflink**  | APFS only           | `clonefile(2)` — native since 10.12          |
| **Copy**     | Always              | Universal fallback                           |
| **Hardlink** | HFS+, APFS          | Breaks edit-safety (shared inode)            |
| **Symlink**  | Always              | Phora avoids (tool confusion, permissions)   |
| **FUSE**     | Requires macFUSE    | Kernel extension pain on Apple Silicon       |

**APFS reflink behavior:**
  * Instant, zero-copy clone via copy-on-write
  * Safe: writes to clone don't affect original
  * Works cross-volume only within same APFS container
  * HFS+ volumes → fallback to copy

#### Linux

| Strategy      | Support                     | Notes                                  |
|---------------|-----------------------------|----------------------------------------|
| **Reflink**   | Btrfs, XFS, bcachefs, OCFS2 | `FICLONE` ioctl / `copy_file_range`    |
| **Copy**      | Always                      | Universal fallback                     |
| **Hardlink**  | Same filesystem             | Same edit-safety problem as macOS      |
| **Symlink**   | Always                      | Phora avoids                           |
| **FUSE**      | Native kernel support       | No kext pain, overlayfs also an option |
| **Bind mount**| Native                      | Requires root or user namespaces       |

**Filesystem prevalence:**

| Filesystem | Reflink             | Common on                   |
|------------|---------------------|-----------------------------|
| ext4       | ✗                   | Most servers, older distros |
| Btrfs      | ✓                   | Fedora, openSUSE, NixOS     |
| XFS        | ✓ (since 4.16)      | RHEL 8+, enterprise         |
| ZFS        | ✗ (different model) | Proxmox, TrueNAS            |
| bcachefs   | ✓                   | Bleeding edge               |

**Linux reflink detection:**

```rust
fn probe_reflink_linux(src: &Path, dst: &Path) -> bool {
    use std::os::unix::io::AsRawFd;

    let src_file = match std::fs::File::open(src) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let dst_file = match std::fs::File::create(dst) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // FICLONE ioctl - atomic reflink
    const FICLONE: libc::c_ulong = 0x40049409;

    let result = unsafe {
        libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd())
    };

    let _ = std::fs::remove_file(dst);
    result == 0
}
```

**Practical note:** Many Linux users will hit the copy fallback (ext4 is ubiquitous). That's acceptable.

#### Windows

| Strategy     | Support                              | Notes                                      |
|--------------|--------------------------------------|--------------------------------------------|
| **Reflink**  | ReFS only, Dev Drive                 | `FSCTL_DUPLICATE_EXTENTS_TO_FILE`          |
| **Copy**     | Always                               | `CopyFileW`                                |
| **Hardlink** | NTFS, same volume                    | `CreateHardLinkW` — same edit-safety issue |
| **Symlink**  | NTFS, requires elevation or dev mode | Phora avoids                               |
| **ProjFS**   | Native since 1809                    | Windows Projected File System              |

**Reflink on Windows:**

Very limited — only works on ReFS:

```rust
#[cfg(windows)]
fn probe_reflink_windows(src: &Path, dst: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Ioctl::FSCTL_DUPLICATE_EXTENTS_TO_FILE;

    // Only works on ReFS
    // NTFS does not support block cloning

    // ... ioctl setup ...
    // Returns false on NTFS (most users)
}
```

**Practical reality:** Almost no one runs ReFS on workstations. NTFS is ubiquitous.

**Dev Drive:** Windows 11 introduced Dev Drive (ReFS-based) specifically for developer workloads. This does support reflink. Worth detecting, but still minority.

**Windows Projected File System (ProjFS) — future:**

```
┌─────────────────┐
│   User sees     │  ← Virtual files at target path
│   plain files   │
├─────────────────┤
│     ProjFS      │  ← Windows kernel component
├─────────────────┤
│  Phora provider │  ← Serves content from cache on demand
└─────────────────┘
```

Pros:
  * Zero upfront copy
  * Files appear instantly
  * Reads pull from cache lazily
  * Native Windows API (no third-party driver)

Cons:
  * Provider must stay running (or files disappear/become placeholders)
  * More complex than copy
  * v2+ feature

### Symlink Handling During Projection

Separate from strategy, need to handle symlinks within artifacts:

```rust
impl Projector for ReflinkProjector {
    fn project_tree(&self, src: &Path, dst: &Path, policy: &ExportPolicy) -> Result<ProjectionResult, Error> {
        for entry in walkdir::WalkDir::new(src) {
            let entry = entry?;
            let rel = entry.path().strip_prefix(src)?;
            let dst_path = dst.join(rel);

            let ft = entry.file_type();

            if ft.is_symlink() {
                if !policy.allow_symlinks {
                    return Err(Error::SymlinkNotAllowed { path: rel.into() });
                }
                let target = std::fs::read_link(entry.path())?;
                symlink(&target, &dst_path)?;
            } else if ft.is_dir() {
                std::fs::create_dir_all(&dst_path)?;
            } else if ft.is_file() {
                self.project_file(entry.path(), &dst_path)?;
            }
        }
        // ...
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    // v1: always file symlink (per spec)
    std::os::windows::fs::symlink_file(target, link)
}
```

### v1 Summary

Keep it boring:

1. **Probe reflink at runtime** — single code path, platform-agnostic
2. **Works great on:** APFS, Btrfs, XFS, ReFS/Dev Drive
3. **Falls back to copy on:** ext4, NTFS, network mounts
4. **Trait-based design** allows ProjFS/FUSE later without changing sync logic

### Global Registry Record (no target metadata)

Phora stores deployment metadata in a global registry under `~/.phora/state/…` (authoritative).
Targets remain free of `.phora-*` files to avoid "manifest pollution" in tool-scanned directories.

Registry record location (file backend v1):
`~/.phora/state/targets/<target>/artifacts/<source>/<artifact>.toml`

**Global Awareness (origin linkage)**

Phora's registry is authoritative not only for "what is deployed", but also for "what cache snapshot was used".
This enables:
  * reverse lookup ("where is this used?")
  * safe garbage collection of unused cache snapshots
  * future content-addressed dedupe (CAS)

Each registry record MUST contain an `[origin]` block that links the deployment to its cache snapshot.

Canonical `cache_key` format (v1):
```
cache_key = "<source>/<commit>/<root>"
```

Where:
  * `<source>` is the source name from config
  * `<commit>` is the resolved commit hash from the lock
  * `<root>` is the configured root path within the repo, normalized as:
      - empty string if no root is configured
      - slash-separated relative path with no leading slash otherwise

NOTE (v1): matcher/policy are per-source configuration and are therefore implicit in `<source>`.
If matcher/policy become deployment-varying in the future, `cache_key` MUST be extended to include them (e.g., config hash).

Purpose:

* Track provenance (source, commit, digest)
* Enable fast modification detection
* Support `phora list`, future `phora diff`, and `phora clean` / GC

Example record:

```toml
version = 1
target = "vscode"
source = "company-configs"
artifact = "snippets"
commit = "def456789abc123"
digest = "sha256:d4e5f6..."
projected_at = "2026-01-31T12:34:56Z"
strategy = "copy"
layout = "flat"
allow_symlinks = false
preserve_executable = true

[origin]
cache_key = "company-configs/def456789abc123/configs"
# Optional redundancy (reserved for future CAS). In v1 this MUST equal record.digest.
digest = "sha256:d4e5f6..."

[[files]]
path = "python.json"
size = 12345
mtime = 1738329296
sha256 = "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d..."  # for phora verify
```

**Ejected Artifacts (per-target metadata)**

Ejected artifacts are tracked in `~/.phora/state/targets/<target>/meta.toml`:

```toml
version = 1

# Artifacts the user has ejected (vendored) — Phora won't overwrite
[[ejected]]
source = "company-configs"
artifact = "snippets"
ejected_at = "2026-01-31T14:00:00Z"

[[ejected]]
source = "dotfiles"
artifact = "old-config"
ejected_at = "2026-01-30T10:00:00Z"
```

When an artifact is ejected:
  * Registry record is deleted (no longer "managed")
  * Entry added to target's `meta.toml` ejected list
  * Files remain on disk untouched

When an ejected artifact's files are deleted and `phora sync` runs:
  * Artifact is re-projected (ejected entry removed)

### Registry Interface (pluggable backend)

Phora defines a small registry interface so future backends (e.g., redb) can be added without changing sync logic.

```rust
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactKey {
    pub target: String,
    pub source: String,
    pub artifact: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Origin {
    /// Canonical v1 cache key: "<source>/<commit>/<root>"
    pub cache_key: String,
    /// Optional (reserved for CAS); in v1 MUST equal RegistryRecord.digest if present.
    pub digest: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegistryRecord {
    pub version: u32,
    pub key: ArtifactKey,
    pub commit: String,
    pub digest: String,
    pub origin: Origin,
    pub projected_at: String,
    pub strategy: String,
    pub layout: String,
    pub allow_symlinks: bool,
    pub preserve_executable: bool,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ManifestFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
    /// Content hash for `phora verify`. Computed at projection time.
    pub sha256: String,
}

pub trait Registry {
    fn get(&self, key: &ArtifactKey) -> Result<Option<RegistryRecord>, Error>;
    fn put(&self, record: &RegistryRecord) -> Result<(), Error>;
    fn remove(&self, key: &ArtifactKey) -> Result<(), Error>;
    fn list_target(&self, target: &str) -> Result<Vec<RegistryRecord>, Error>;
    fn list_all(&self) -> Result<Vec<RegistryRecord>, Error>;

    // Ejected artifact management (per-target meta.toml)
    fn load_ejected(&self, target: &str) -> Result<Vec<EjectedEntry>, Error>;
    fn save_ejected(&self, target: &str, ejected: &[EjectedEntry]) -> Result<(), Error>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EjectedEntry {
    pub source: String,
    pub artifact: String,
    pub ejected_at: String,
}
```

**Reverse lookup (v1):**
  * FileRegistry MAY implement reverse lookups by scanning `list_all()` and filtering in-memory.
  * This is acceptable in v1 because state records are small and count is expected to be manageable.
  * Future backends (e.g., redb) MAY maintain secondary indexes for faster queries.

### FileRegistry (v1 default)

File-based registry uses one TOML per artifact and atomic writes (temp + fsync + rename).

Writers MUST hold `~/.phora/state/locks/state.lock` exclusively during sync/update/eject/clean.
Readers (`status`) MAY take a shared lock; writes are atomic per record.

### ScanResult and scanning helpers

Phora still needs directory scanning to detect drift. Scanning is used in two modes:

* Strict (write path): error on disallowed symlinks
* Soft (read path): never error; report symlinks so callers can mark Modified (better UX)

```rust
#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<ManifestFile>,
    /// Relative paths of symlinks encountered (excluded from files list).
    /// NOTE: Soft scans never error; they only report symlinks for "treat as Modified".
    pub symlinks: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
enum ScanMode {
    Strict,
    Soft,
}

pub fn build_file_list(dir: &Path, allow_symlinks: bool) -> Result<Vec<ManifestFile>, Error> {
    let res = scan_dir(dir, allow_symlinks, ScanMode::Strict)?;
    Ok(res.files)
}

pub fn scan_dir_soft(dir: &Path) -> Result<ScanResult, Error> {
    scan_dir(dir, true, ScanMode::Soft)
}

fn scan_dir(dir: &Path, allow_symlinks: bool, mode: ScanMode) -> Result<ScanResult, Error> {
    let mut files = Vec::new();
    let mut symlinks = Vec::new();

    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry?;
        let ft = entry.file_type();

        if ft.is_symlink() {
            let rel = entry.path().strip_prefix(dir)?;
            match mode {
                ScanMode::Strict => {
                    if !allow_symlinks {
                        return Err(Error::SymlinkNotAllowed { path: rel.to_path_buf() });
                    }
                    // v1 registry schema: ignore symlinks when allowed (still policy-guarded on writes)
                }
                ScanMode::Soft => {
                    symlinks.push(rel.to_path_buf());
                }
            }
            continue;
        }

        if !ft.is_file() {
            continue;
        }

        let rel = entry.path().strip_prefix(dir)?;
        let meta = entry.metadata()?;
        let mtime = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        files.push(ManifestFile {
            path: rel.to_path_buf(),
            size: meta.len(),
            mtime,
        });
    }

    Ok(ScanResult { files, symlinks })
}
```

### Atomic Artifact Swap

Projection never writes directly to destination.
Stage and backup are ephemeral and MUST live on the same filesystem as the destination to preserve atomic renames.
Phora MAY create temporary directories under the target base during sync, but MUST remove them afterward.

**Paths:**

* stage root = `<target_base>/.phora-stage/`
* backup root = `<target_base>/.phora-backup/`

**Flow:**

1. Build stage by reflink/copy from cache
2. If destination exists, rename dst → backup
3. Rename stage → dst
4. Persist registry record (authoritative) only after successful swap
5. Delete stage/backup (best-effort)

```rust
pub fn deploy_artifact(
    cache_artifact: &Path,
    target_base: &Path,
    dst: &Path,
    strategy: ProjectionStrategy,
    record: RegistryRecord,
    registry: &dyn Registry,
) -> Result<(), Error> {
    let stage_root = target_base.join(".phora-stage");
    let backup_root = target_base.join(".phora-backup");

    let nonce = nonce();
    let stage = stage_root.join(format!("{}-{}", record.key.artifact, nonce));
    let backup = backup_root.join(format!("{}-{}", record.key.artifact, nonce));

    // Clean any leftover stage
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    std::fs::create_dir_all(&stage)?;
    std::fs::create_dir_all(&backup_root)?;

    // 1. Populate stage (enforces symlink policy)
    project_tree(cache_artifact, &stage, strategy, record.allow_symlinks)?;

    // 2. Atomic swap
    if dst.exists() {
        std::fs::rename(dst, &backup)?;
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::rename(&stage, dst)?;

    // 3. Cleanup (best-effort)
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }
    let _ = std::fs::remove_dir(&stage_root);
    let _ = std::fs::remove_dir(&backup_root);

    // 4. Persist registry record only after successful swap
    registry.put(&record)?;

    Ok(())
}
```

### Layouts

| Layout           | Structure                  | Collisions |
| ---------------- | -------------------------- | ---------- |
| `flat` (default) | `<artifact>/`              | Error      |
| `by-source`      | `<source>/<artifact>/`     | Impossible |
| `prefixed`       | `<source><sep><artifact>/` | Impossible |

```toml
# String form
layout = "flat"
layout = "prefixed"        # separator: "-"
layout = "by-source"

# Table form (custom separator)
layout = { type = "prefixed", separator = "_" }
layout = { type = "prefixed", separator = "/" }
```

Note on `separator`
`separator` is ignored unless `kind = Prefixed`.
For `Flat` and `BySource`, the effective separator is an empty string and MUST NOT affect computed paths.

### Modification Detection

**Behavior:**

* **Write operations** (export/projection): Strict — error on disallowed symlinks
* **Read operations** (status/sync check): Soft — treat as Modified, don't crash

**Rationale: size/mtime over content hashing (stat-first drift detection)**

Phora prioritizes a "feels instant" UX on the hot path (status checks and sync preflight).
Many real-world artifacts contain thousands of files; re-hashing all content would make `phora list`
and routine drift checks noticeably slower as total deployed size grows.

Design:
  * Hot path (`phora list`, sync drift checks): use `stat` (size + mtime) per file
  * Cold path (`phora verify`): optionally hash file contents for maximum correctness

Why `stat` is preferred on the hot path:
  * Speed: `stat` is a metadata read (one syscall per file) and does not read file contents.
  * Scaling: hashing requires reading every byte; runtime scales with total artifact size.
  * Predictability: results are stable and fast across reflink/copy strategies.

Failure mode (why this is acceptable by default):
  * Size/mtime can miss changes when content is modified while preserving both:
      - identical byte length AND
      - unchanged mtime (including explicit timestamp restoration), OR
      - edits within the filesystem's timestamp resolution (some filesystems can be coarse).
    This is considered an acceptable edge case for a developer tool managing config artifacts.

Why this matches the broader ecosystem:
  * Git uses the same optimization strategy for `git status`: it checks file stats first and only
    re-hashes content when something looks "suspicious" (stat mismatch).
  * Phora applies the same stat-cache validation pattern in its registry-driven design.

Relationship to atomic swaps:
  * Phora deploys via stage + atomic directory swap. It replaces directories rather than patching
    files in-place, reducing the risk of partial writes. Drift detection primarily covers user edits
    between syncs, not incomplete deployments.

`phora verify`:
  * Provides a correctness-first mode that hashes deployed content and reports any mismatches.
  * Intended for "I suspect corruption/tampering" workflows, CI checks, or audits—not the default
    interactive path.

```rust
pub enum ArtifactState {
    /// Matches cache exactly
    Clean,
    /// Local modifications detected
    Modified { changed: Vec<PathBuf> },
    /// No registry record or wrong provenance
    Foreign,
    /// Doesn't exist yet
    Missing,
    /// Explicitly ejected
    Ejected,
}

pub fn check_artifact_state(
    target_path: &Path,
    expected_source: &str,
    expected_commit: &str,
    expected_digest: &str,
    ejected: &[EjectedEntry],
    artifact_name: &str,
    registry: &dyn Registry,
    key: &ArtifactKey,
) -> Result<ArtifactState, Error> {
    // Check if artifact is ejected (match both source and artifact name)
    let is_ejected = ejected.iter().any(|e| e.artifact == artifact_name && e.source == expected_source);
    if is_ejected {
        return Ok(ArtifactState::Ejected);
    }

    if !target_path.exists() {
        return Ok(ArtifactState::Missing);
    }

    let record = match registry.get(key)? {
        None => return Ok(ArtifactState::Foreign),
        Some(r) => r,
    };

    // Check provenance (including digest)
    if record.key.source != expected_source
        || record.commit != expected_commit
        || record.digest != expected_digest
    {
        return Ok(ArtifactState::Foreign);
    }

    // Fast modification check (dedup + stable order)
    let mut changed: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();

    for mf in &record.files {
        let file_path = target_path.join(&mf.path);

        if !file_path.exists() {
            changed.insert(mf.path.clone());
            continue;
        }

        let meta = std::fs::metadata(&file_path)?;
        let mtime = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        if meta.len() != mf.size || mtime != mf.mtime {
            changed.insert(mf.path.clone());
        }
    }

    // Check for new files and unexpected symlinks (soft scan)
    let scan = scan_dir_soft(target_path)?;

    let known: std::collections::HashSet<_> =
        record.files.iter().map(|f| &f.path).collect();

    for cf in &scan.files {
        if !known.contains(&cf.path) {
            changed.insert(cf.path.clone());
        }
    }

    // If symlinks are disallowed for this artifact, treat any symlink as Modified
    if !record.allow_symlinks {
        for s in &scan.symlinks {
            changed.insert(s.clone());
        }
    }

    if changed.is_empty() {
        Ok(ArtifactState::Clean)
    } else {
        Ok(ArtifactState::Modified { changed: changed.into_iter().collect() })
    }
}
```

### Interactive Conflict Resolution

When `phora sync` encounters Modified or Foreign artifacts in interactive mode (TTY detected), it prompts the user:

```
⚠ Modified locally: snippets
    python.json (size changed)
    new-file.txt (added)

  [s]kip  [o]verwrite  [e]ject  [a]bort?
```

**Options:**
  * **Skip (s)**: Leave local files, don't update this artifact (default for Modified)
  * **Overwrite (o)**: Replace with upstream version (equivalent to `--force` for this artifact)
  * **Eject (e)**: Mark as ejected, keep local files, stop managing
  * **Abort (a)**: Stop sync entirely, make no changes

**For Foreign content:**
```
⚠ Foreign content at: ~/.config/nvim/snippets
  Directory exists but is not managed by Phora.

  [s]kip  [o]verwrite  [a]bort?
```

**Batch mode (`--force` or non-TTY):**
  * `--force`: Overwrite all conflicts without prompting
  * Non-interactive (CI, piped): Skip all conflicts, log warnings

**Remembering choices:**
  * `--skip-all`: Skip all conflicts for this run
  * Future: persist skip/eject choices to config

## Sync Flow

```rust
pub fn sync(
    base_config: &Config,
    local_config: Option<&Config>,
    base_lock: Option<Lock>,
    local_lock: Option<Lock>,
    force: bool,
    interactive: bool,
) -> Result<(Lock, Option<Lock>), Error> {
    let git = GitBackend::new(phora_dir().join("git"));
    let cache = Cache::new(phora_dir().join("git"), phora_dir().join("cache"));
    let registry: Box<dyn Registry> = Box::new(FileRegistry::open(phora_dir().join("state"))?);

    // Merge configs: local overlays base
    let effective_config = merge_configs(base_config, local_config);

    // Merge locks: local entries override base by name
    let effective_lock = match (&base_lock, &local_lock) {
        (Some(base), local) => Some(merge_locks(base, local.as_ref())),
        (None, Some(local)) => Some(local.clone()),
        (None, None) => None,
    };

    // Output: separate locks for base vs local sources
    let mut new_base_lock = Lock { version: 1, sources: Vec::new() };
    let mut new_local_lock = Lock { version: 1, sources: Vec::new() };

    // ─────────────────────────────────────────────────────────
    // Phase 1: Fetch and cache sources
    // ─────────────────────────────────────────────────────────

    for (name, source) in &effective_config.sources {
        let locked = effective_lock.as_ref().and_then(|l| l.find_source(name));

        let commit = match locked {
            Some(l) if source_matches(source, l) => {
                l.commit.clone()
            }
            _ => {
                git.fetch(name, &source.git)?;
                git.resolve_ref(name, &source.refspec())?
            }
        };

        let matcher = PathMatcher::new(&source.include, &source.exclude)?;
        let policy = source.export_policy();

        let snapshot_path = cache.ensure_snapshot(
            name,
            &commit,
            source.root.as_deref(),
            &matcher,
            &policy,
        )?;

        let digest = compute_digest(&snapshot_path)?;

        let locked_source = LockedSource {
            name: name.clone(),
            git: source.git.clone(),
            resolved: source.refspec().to_string(),
            commit,
            digest,
        };

        // Route to correct lock based on whether source is overridden locally
        if local_config.map(|lc| lc.sources.contains_key(name)).unwrap_or(false) {
            new_local_lock.sources.push(locked_source);
        } else {
            new_base_lock.sources.push(locked_source);
        }
    }

    // ─────────────────────────────────────────────────────────
    // Phase 2: Project to targets
    // ─────────────────────────────────────────────────────────

    // Merge new locks for lookup during projection
    let new_effective_lock = merge_locks(&new_base_lock, Some(&new_local_lock));

    for (target_name, target) in &effective_config.targets {
        let target_path = target.expanded_path();
        let sources = target.resolve_sources(&effective_config.sources);

        let strategy = detect_strategy(&phora_dir().join("cache"), &target_path);

        // Load ejected from registry (authoritative)
        let mut ejected = registry.load_ejected(target_name)?;

        let mut seen: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();

        for source_name in sources {
            let source = &effective_config.sources[source_name];
            let locked = new_effective_lock.find_source(source_name).unwrap();

            let matcher = PathMatcher::new(&source.include, &source.exclude)?;
            let cache_root = cache.snapshot_path(source_name, &locked.commit, source.root.as_deref());

            let discovered = discover_artifacts(&cache_root, &matcher)?;

            for artifact_name in discovered {
                // Collision check for flat layout
                if target.layout.kind == LayoutKind::Flat {
                    if let Some(other) = seen.get(&artifact_name) {
                        return Err(Error::Collision {
                            artifact: artifact_name,
                            sources: vec![other.to_string(), source_name.to_string()],
                            target: target_name.clone(),
                        });
                    }
                    seen.insert(artifact_name.clone(), source_name);
                }

                let artifact_dst = target_path.join(
                    target.layout.artifact_path(source_name, &artifact_name)
                );

                let key = ArtifactKey {
                    target: target_name.clone(),
                    source: source_name.to_string(),
                    artifact: artifact_name.clone(),
                };

                let state = check_artifact_state(
                    &artifact_dst,
                    source_name,
                    &locked.commit,
                    &locked.digest,
                    &ejected,
                    &artifact_name,
                    registry.as_ref(),
                    &key,
                )?;

                match state {
                    ArtifactState::Ejected => {
                        continue;
                    }

                    ArtifactState::Modified { changed } if !force => {
                        eprintln!("⚠ Modified locally: {}", artifact_name);
                        for path in &changed {
                            eprintln!("    {}", path.display());
                        }
                        eprintln!("  Skipping. Use --force to overwrite.");
                        continue;
                    }

                    ArtifactState::Foreign if !force => {
                        eprintln!("⚠ Foreign content at: {}", artifact_dst.display());
                        eprintln!("  Skipping. Use --force to overwrite.");
                        continue;
                    }

                    ArtifactState::Clean => {
                        // Already up to date
                    }

                    ArtifactState::Missing
                    | ArtifactState::Modified { .. }
                    | ArtifactState::Foreign => {
                        let cache_artifact = cache_root.join(&artifact_name);

                        // Compute cache_key for origin linkage
                        let root_str = source.root
                            .as_ref()
                            .map(|r| r.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let cache_key = format!("{}/{}/{}", source_name, locked.commit, root_str);

                        let record = RegistryRecord {
                            version: 1,
                            key: key.clone(),
                            commit: locked.commit.clone(),
                            digest: locked.digest.clone(),
                            origin: Origin {
                                cache_key,
                                digest: Some(locked.digest.clone()),
                            },
                            projected_at: chrono::Utc::now().to_rfc3339(),
                            strategy: format!("{:?}", strategy).to_lowercase(),
                            layout: format!("{:?}", target.layout.kind).to_lowercase(),
                            allow_symlinks: source.allow_symlinks,
                            preserve_executable: source.preserve_executable,
                            files: build_file_list(&cache_artifact, source.allow_symlinks)?,
                        };

                        deploy_artifact(
                            &cache_artifact,
                            &target_path,
                            &artifact_dst,
                            strategy,
                            record,
                            registry.as_ref(),
                        )?;

                        // Clearing ejected on restore:
                        // If the user previously ejected this artifact and then deleted it,
                        // a successful projection MUST remove it from the ejected list
                        // and persist the change to registry.
                        if let Some(pos) = ejected.iter().position(|e| e.artifact == artifact_name) {
                            ejected.swap_remove(pos);
                            registry.save_ejected(target_name, &ejected)?;
                        }
                    }
                }
            }
        }
    }

    // Return separate locks for writing
    let local_result = if new_local_lock.sources.is_empty() {
        None
    } else {
        Some(new_local_lock)
    };

    Ok((new_base_lock, local_result))
}
```

## Eject

Remove artifact from management, keep file. Requires full identity: (target, source, artifact).

Eject does NOT modify lock files — ejected state lives in the registry.

```rust
pub fn eject(
    config: &Config,
    registry: &dyn Registry,
    artifact: &str,
    source: &str,
    target: &str,
) -> Result<(), Error> {
    // Validate target exists in config
    let target_config = config.targets.get(target).ok_or(Error::TargetNotFound {
        target: target.to_string(),
    })?;

    // Validate artifact is currently managed (has registry record)
    let key = ArtifactKey {
        target: target.to_string(),
        source: source.to_string(),
        artifact: artifact.to_string(),
    };

    let record = registry.get(&key)?;
    if record.is_none() {
        return Err(Error::ArtifactNotManaged {
            target: target.to_string(),
            source: source.to_string(),
            artifact: artifact.to_string(),
        });
    }

    // Remove registry record (no longer managed)
    registry.remove(&key)?;

    // Add to ejected list in registry meta.toml
    let mut ejected = registry.load_ejected(target)?;
    let already_ejected = ejected.iter().any(|e| e.artifact == artifact && e.source == source);
    if !already_ejected {
        ejected.push(EjectedEntry {
            source: source.to_string(),
            artifact: artifact.to_string(),
            ejected_at: chrono::Utc::now().to_rfc3339(),
        });
        registry.save_ejected(target, &ejected)?;
    }

    // Compute path only for UX messaging (Phora does not modify target contents on eject)
    let target_base = target_config.expanded_path();
    let rel = target_config.layout.artifact_path(source, artifact);
    let artifact_path = target_base.join(rel);
    eprintln!("Ejected: {} (files kept at {})", artifact, artifact_path.display());

    Ok(())
}
```

**To restore:** delete the ejected files, run `phora sync`.

## List Output

```
$ phora list

Sources:
  dotfiles         main (abc123) ✓ cached
  company-configs  v2.1 (def456) ✓ cached
  loqui            v1.0 (789xyz) ✓ cached

Targets:
  neovim (~/.config/nvim) [strategy: reflink]:
    nvim/              dotfiles@main

  vscode (~/.config/Code/User) [strategy: copy]:
    settings/          dotfiles@main
    keybindings/       dotfiles@main
    snippets/          company-configs@v2.1 [modified]
      python.json (size changed)
      unexpected.lnk [symlink]

  cupcake-policies (~/.cupcake/policies/claude) [strategy: reflink]:
    loqui/python/      loqui@v1.0
    loqui/go/          loqui@v1.0
    old-policy/        [ejected]
    unknown-dir/       [foreign]
```

### phora list --plan

Shows what `phora sync` would do without making changes. Computes artifacts from effective config + locks, diffs against registry.

```
$ phora list --plan

Pending changes:

  neovim (~/.config/nvim):
    + nvim/              dotfiles@main (new)

  vscode (~/.config/Code/User):
    ~ snippets/          company-configs@v2.1 → v2.2 (update)
    - old-keybindings/   (removed from source)

  cupcake-policies:
    (no changes)

Run `phora sync` to apply.
```

## Error Messages

### Collision

```
$ phora sync

✗ Artifact collision in target 'vscode': settings
  Sources: dotfiles, company-configs

  Options:
    1. Exclude from one source:
       [sources.company-configs]
       exclude = ["settings"]

    2. Change layout:
       [targets.vscode]
       layout = "prefixed"

    3. Use separate targets
```

### Symlink Not Allowed

```
$ phora sync

✗ Symlink not allowed in source 'company-configs'
  Path: configs/editor/link.txt

  To allow symlinks:
    [sources.company-configs]
    allow_symlinks = true

Windows note (v1):
  Phora creates file symlinks only. Directory symlinks are not supported in v1.
```

### Symlink Artifact (v1)

```
$ phora sync

✗ Symlink as artifact not supported in v1
  Path: my-link

  Symlinks are allowed within artifacts, but not as artifact roots.
```

### Artifact Not Managed

```
$ phora eject unknown-thing --source company --target vscode

✗ Artifact not managed
  Target: vscode
  Source: company
  Artifact: unknown-thing

  Run `phora list` to see managed artifacts.
```

## Operational Commands

### phora add

Add a source to `phora.toml` by parsing a URL or shorthand.

**Usage:**
```
phora add <url> [--name <name>] [--branch <branch>] [--tag <tag>] [--root <path>]
```

**URL Parsing:**

Phora supports multiple URL formats and expands them using host templates:

| Input | Parsed As |
|-------|-----------|
| `owner/repo` | GitHub shorthand → `https://github.com/owner/repo.git` |
| `owner/repo/path/to/dir` | GitHub + root → git + `root = "path/to/dir"` |
| `github.com/owner/repo` | Full host shorthand |
| `https://github.com/owner/repo` | Full URL |
| `https://github.com/owner/repo/tree/main/path` | URL with branch + root extraction |
| `gitlab.com/owner/repo` | GitLab (uses host template) |
| `git@github.com:owner/repo.git` | SSH URL |

**Host Templates:**

Hosts can define URL templates for shorthand expansion:

```toml
[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }

[hosts.gitlab]
git_url = "https://gitlab.com/{owner}/{repo}.git"
auth = { type = "ssh" }

[hosts.company]
git_url = "https://git.company.com/{owner}/{repo}.git"
auth = { type = "token", env = "COMPANY_GIT_TOKEN" }
```

Template variables: `{owner}`, `{repo}`, `{ref}`, `{path}`

**Default hosts (built-in):**
  * `github` → `https://github.com/{owner}/{repo}.git`
  * `gitlab` → `https://gitlab.com/{owner}/{repo}.git`

**Behavior:**
  1. Parse URL to extract: host, owner, repo, ref (branch/tag), path (root)
  2. Look up host template (or use default)
  3. Generate source name (default: `repo` or `owner-repo` if collision)
  4. Append to `phora.toml`:
     ```toml
     [sources.<name>]
     git = "<expanded-url>"
     branch = "<ref>"      # if detected
     root = "<path>"       # if detected
     ```
  5. Print added source for confirmation

**Examples:**
```
$ phora add srnnkls/loqui
Added source 'loqui':
  git = "https://github.com/srnnkls/loqui.git"

$ phora add https://github.com/company/configs/tree/main/editor --name editor-config
Added source 'editor-config':
  git = "https://github.com/company/configs.git"
  branch = "main"
  root = "editor"
```

### phora clean (smart GC)

Garbage-collect cache snapshots that are not referenced by any active deployment.

**Mark:**
  * Scan all registry records under `~/.phora/state/targets/**/artifacts/**.toml`
  * Collect every referenced `record.origin.cache_key` into a set

**Sweep:**
  * Enumerate cache snapshots under `~/.phora/cache/`
  * Delete snapshots whose corresponding `cache_key` is not in the marked set
  * Implementations MAY also consider active lockfiles as additional roots (future), but registry is authoritative for deployed state.

**Safety:**
  * Default SHOULD be conservative (e.g., only remove unreferenced snapshots older than N days, default 30).
  * Supports `--dry-run`.
  * Note: removing cache reduces ability to heal without fetching, but does not break existing projections.

### phora rebuild-registry

Rebuild `~/.phora/state/...` from:

1. Current `phora.lock` projections (source, artifact, commit, digest)
2. Target filesystem scan at expected deployment paths

Any mismatches become "Foreign" or "Modified" in status until next successful deploy.

### phora where

Query the global registry to answer "where is this used?" and related questions.

**Inputs (any combination):**
  * `phora where --digest <hash>`: find all deployments of this exact exported content digest
  * `phora where --source <name>`: find all deployments from a source
  * `phora where --artifact <name>`: find all deployments with this artifact name
  * `phora where --cache-key <key>`: find all deployments that share the same cache snapshot origin

**Behavior:**
  * Reads `~/.phora/state/...` (authoritative).
  * In v1, implementation MAY scan all state records and filter results in-memory.
  * Output groups by (source, artifact) and lists target paths.

**Example output:**
```
Artifact: company-skills/python (commit def456, digest sha256:...)
  • ~/.config/nvim/lua/skills
  • ~/work/agent-1/resources/skills
```

### phora verify

Correctness-first verification of deployed artifacts by hashing file contents.

Properties:
  * Intended as a cold path (audit/CI / "suspect corruption") rather than default interactive status.
  * Uses `sha256` hashes stored in registry records (computed at projection time).
  * Works independently of cache state — verify succeeds even if cache is GC'd.
  * Reports mismatches as Modified-like output, but backed by content hashes rather than size/mtime heuristics.

Algorithm:
  1. Load registry record for each managed artifact
  2. For each file in `record.files`, hash deployed file content
  3. Compare against stored `sha256`
  4. Report mismatches

Notes:
  * Hashing reads file contents; runtime scales with deployed size.
  * `phora verify` is explicitly opt-in so `phora list` remains instant.

### phora check-match --source <source> <path>

Debug include/exclude matching. Prints which patterns match:

* Artifact-level evaluation (artifact name)
* Path-level evaluation (relative path)
* Normalized patterns (including the implicit `**/` prefix convention)

## Dependencies

```toml
[package]
name = "phora"
version = "0.1.0"
edition = "2024"

[dependencies]
gix = { version = "0.68", features = ["blocking-network-client"] }
reflink = "0.1"
filetime = "0.2"
walkdir = "2"
globset = "0.4"
blake3 = "1"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "2"
clap = { version = "4", features = ["derive"] }
chrono = { version = "0.4", features = ["serde"] }
dirs = "5"
fs2 = "0.4"  # file locks for ~/.phora/state/locks/state.lock (or equivalent)
```

## Acceptance Criteria

### Sync

* Given valid `phora.toml`, no lock
* When `phora sync`
* Then sources fetched, snapshots exported, lock created, artifacts projected with preserved timestamps, registry updated
* Given lock exists, sources unchanged
* When `phora sync`
* Then no fetch, no export, projection verified via registry
* Given local modification to projected file
* When `phora sync`
* Then warning with changed files listed, local changes preserved
* Given local modification
* When `phora sync --force`
* Then local changes overwritten
* Given user added symlink to managed artifact, `allow_symlinks=false`
* When `phora list`
* Then shows [modified] with symlink path listed (no crash)
* Given directory exists at target path with no registry record
* When `phora list`
* Then shows [foreign]

### Update

* Given source pinned to branch
* When `phora update`
* Then lock updated to latest commit, re-exported, re-projected, registry updated

### Eject

* Given managed artifact
* When `phora eject <artifact> --source <source> --target <target>`
* Then artifact marked ejected in registry, registry record deleted, files untouched
* Given ejected artifact deleted by user
* When `phora sync`
* Then artifact re-projected (ejected list cleared for that artifact)
* Given non-existent artifact
* When `phora eject ...`
* Then error: artifact not managed

### Layout

* Given `layout = "flat"`, collision
* When `phora sync`
* Then error with resolution options
* Given `layout = { type = "prefixed", separator = "/" }`
* When `phora sync`
* Then artifacts at `<source>/<artifact>/`

### Export Policy

* Given symlink in source tree, `allow_symlinks = false`
* When `phora sync`
* Then error during export with path and opt-in instructions
* Given symlink at artifact root level
* When `phora sync`
* Then error (v1: not supported, regardless of `allow_symlinks`)
* Given executable file in source, `preserve_executable = true`
* When `phora sync`
* Then projected file has executable bit set and original mtime preserved (Unix)

### Timestamp Preservation

* Given artifact projected via copy
* When `phora sync` runs again
* Then no false "modified" detection (mtime matches)

### Verify (content hashing, cold path)

* Given a managed artifact is deployed and registry indicates Clean
* When `phora verify`
* Then Phora hashes deployed file contents and reports any mismatches
* Given a file's content changes without changing size and mtime
* When `phora list`
* Then Phora MAY still report Clean (stat-first limitation)
* And when `phora verify`
* Then Phora MUST report a mismatch

### Registry (global state, no target metadata)

* Given managed artifact is deployed

  * When `phora list`
  * Then state is read from `~/.phora/state/...` and no `.phora-*` files appear in target directories
* Given user deletes `~/.phora/state` (or migrates machines)

  * When `phora list`
  * Then artifacts on disk appear as `[foreign/untracked]` (no crash)
* Given artifact previously ejected and later deleted by user

  * When `phora sync` successfully re-projects it
  * Then it is removed from the registry ejected list (clearing ejected on restore)

### No Target Metadata

* Given successful `phora sync`
* When inspecting target directory
* Then no `.phora-*` files or directories exist

## Non-Goals

- Artifact transformation (henia)
- Harness detection (henia)
- Variable substitution (henia)
- Transitive dependencies (future)
- Registry / index (future)
- Signing / verification (future)
