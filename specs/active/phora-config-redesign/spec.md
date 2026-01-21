---
issue_type: Feature
created: 2026-01-18
status: Active
stage: active
promoted: 2026-01-20
claude_plan: .claude/plans/silly-baking-rossum.md
---

# Phora Config Redesign

Redesign phora.toml to follow Cargo/Poetry conventions with proper lock file support.

## Goal

Replace current config format with Cargo/Poetry-style inline table syntax for sources, add lock file with integrity verification, and implement smart URL parsing for `phora add`.

## Scope

### In Scope

1. **Cargo-style Source Definitions**
   - Inline table syntax: `name = { git = "https://github.com/owner/repo.git", tag = "v1.0" }`
   - Single canonical format: `git` field with full URL (no github/gitlab shorthand)
   - Mutually exclusive ref specifiers: `branch`, `tag`, `rev`
   - `path` (source subdir) and `target` (local destination) fields
   - `include`/`exclude` glob filters

2. **Manifest Format**
   - `artifacts` field: export list of what downstream consumers can import (organized by directory)
   - `version` field

3. **Lock File Enhancement**
   - `digest`: SHA256 of source config (JSON with sorted keys) for lazy sync
   - `sha`: Resolved commit SHA (authoritative until `phora update`)
   - Per-file `sha256` hashes for integrity verification
   - `phora update [source]` command to re-resolve refs (Cargo/Poetry pattern)

4. **URL Parser for `phora add`**
   - Parse `owner/repo/path` shorthand
   - Parse `https://github.com/owner/repo/tree/ref/path` URLs
   - Extract host, owner, repo, ref, path components
   - Refs with slashes (e.g., `feature/my-branch`) require explicit `--ref` flag

5. **Implementation Consolidation**
   - Root package types (`phora.Config`, `phora.Lock`) become canonical
   - Deprecate `internal/config` and `internal/lockfile` packages
   - Single unified `phora.lock` at project root (replaces both repo-level and file-level locks)

### Out of Scope

- Henia config changes (separate concern)
- Authentication/secrets management
- Non-git source types (HTTP, OCI)

## Acceptance Criteria

### Config Parsing

- **Given** config with `skills = { git = "https://github.com/company/shared.git", tag = "v1.0" }`
- **When** config is loaded
- **Then** Source has Git="https://github.com/company/shared.git", Tag="v1.0"

- **Given** config with both `branch` and `tag` specified
- **When** config is loaded
- **Then** validation error is returned (mutually exclusive)

- **Given** config with `rev = "a1b2c3d"`
- **When** config is loaded
- **Then** Source has Rev="a1b2c3d", Branch and Tag are empty

- **Given** config without `version` field
- **When** config is loaded
- **Then** validation error is returned (version required)

- **Given** config with `version = 2`
- **When** config is loaded
- **Then** error "unsupported config version" is returned

- **Given** config with `path = "skills"` and `target = ".claude/skills"`
- **When** source is synced
- **Then** files from `skills/` dir land in `.claude/skills/`

- **Given** config with no `target` specified
- **When** source is synced
- **Then** target defaults to source key name

### Manifest

- **Given** manifest with `artifacts = ["skills", "commands"]`
- **When** source is fetched
- **Then** only `skills/` and `commands/` directories are exposed

- **Given** producer manifest restricts `artifacts = ["skills"]` and consumer config has `path = "commands"`
- **When** consumer syncs source
- **Then** error is returned: "path 'commands' not in source artifacts"

- **Given** manifest section exists with empty `artifacts = []`
- **When** ValidatePath or FilterDirectories is called
- **Then** no paths are allowed, no directories are exposed (strict deny-by-default)

- **Given** path input contains traversal sequence `skills/../commands`
- **When** ValidatePath is called
- **Then** path is cleaned via filepath.Clean() before validation; cleaned path must match artifact prefix

### Lock File

- **Given** source synced successfully
- **When** lock file is written
- **Then** lock contains `sha`, `digest`, `fetched_at`, and per-file hashes

- **Given** `digest` matches previous sync
- **When** `phora sync` runs
- **Then** fetch is skipped (lazy sync)

- **Given** `digest` differs from previous sync
- **When** `phora sync` runs
- **Then** source is re-fetched

### Drift Detection

- **Given** synced file has been manually edited (per-file hash mismatch)
- **When** `phora sync` runs
- **Then** error is returned listing drifted files (use `--force` to overwrite)

- **Given** synced file matches per-file hash
- **When** `phora sync` runs
- **Then** file is considered up-to-date

- **Given** `digest` matches but local files have drifted
- **When** `phora sync` runs
- **Then** local integrity check still runs and detects drift (no fetch, but always verify)

- **Given** local file from lock is missing (deleted by user)
- **When** `phora sync` runs
- **Then** treated as drift (error unless `--force`)

- **Given** multiple files have drifted (some modified, some missing)
- **When** `phora sync --force` runs
- **Then** only drifted files are overwritten, clean files are untouched

### Branch Updates

- **Given** source with `branch = "main"` and existing lock SHA
- **When** `phora sync` runs
- **Then** locked SHA is used (no remote check)

- **Given** source with `branch = "main"`
- **When** `phora update shared` runs
- **Then** branch is re-resolved to latest SHA, lock updated

### URL Parser

- **Given** input `srnnkls/dotfiles`
- **When** parsed
- **Then** Git="https://github.com/srnnkls/dotfiles.git", Path=""

- **Given** input `srnnkls/dotfiles/.claude/skills`
- **When** parsed
- **Then** Git="https://github.com/srnnkls/dotfiles.git", Path=".claude/skills"

- **Given** input `https://github.com/srnnkls/dotfiles/tree/main/.claude/skills`
- **When** parsed
- **Then** Git="https://github.com/srnnkls/dotfiles.git", Branch="main", Path=".claude/skills"

- **Given** input `gitlab.com/company/repo/artifacts`
- **When** parsed
- **Then** Git="https://gitlab.com/company/repo.git", Path="artifacts"

**Note:** URL parser outputs components that are assembled into a full `git` URL for the Source struct. The GitHub/GitLab distinction is internal parsing only.

### `phora add` CLI

**Usage:** `phora add <url> [flags]`

**Flags (all optional with sensible defaults):**
- `--ref <ref>`: Specify branch, tag, or rev (required for refs containing `/`)
- `--target <dir>`: Override target directory (default: source key name)
- `--path <subdir>`: Specify source subdirectory to sync
- `--name <key>`: Override source key name (default: derived from repo name)

**Examples:**
```bash
# Add from shorthand (GitHub default)
phora add srnnkls/dotfiles/.claude/skills

# Add with explicit ref (required for feature/xyz branches)
phora add srnnkls/dotfiles --ref feature/new-skills --path .claude/skills

# Add with custom name and target
phora add company/shared --name my-skills --target .claude/skills --path artifacts/skills
```

**Behavior:**
1. Parse URL to extract git URL, ref, path
2. Validate no existing source with same name (error if collision)
3. Append source entry to `phora.toml` under `[sources]`
4. Print added source configuration

## Lock File Schema

The new `phora.lock` schema completely replaces both existing lock structures (root `Lock` and `internal/lockfile.LockFile`). Old lock files are ignored with no migration path.

```toml
version = 1

[[sources]]
name = "shared"                      # Source key from config
repo = "company/shared"              # Resolved repo identifier
ref = "v1.0"                         # Original ref from config
sha = "a1b2c3d4e5f6..."              # Resolved git commit SHA
digest = "8f3a2b..."                  # SHA256 of source config (JSON with sorted keys)
fetched_at = 2026-01-18T10:00:00Z

[[sources.files]]                    # Per-file integrity hashes
path = "skills/code-review.md"       # Relative to source path
sha256 = "9e8d7c6b..."
size = 2048
```

**Field definitions (all required unless noted):**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `version` | integer | yes | Lock format version (currently 1) |
| `sources[].name` | string | yes | Source key from config |
| `sources[].repo` | string | yes | Resolved repo identifier |
| `sources[].ref` | string | yes | Original ref from config (branch/tag/rev value) |
| `sources[].sha` | string | yes | Resolved git commit SHA (40-char hex) |
| `sources[].digest` | string | yes | SHA256 of source config (64-char hex) |
| `sources[].fetched_at` | datetime | yes | ISO 8601 timestamp of last fetch |
| `sources[].files[].path` | string | yes | File path relative to source path |
| `sources[].files[].sha256` | string | yes | SHA256 of file content (64-char hex) |
| `sources[].files[].size` | integer | yes | File size in bytes |

**Digest computation:**
- Input: Source config serialized as JSON with sorted keys
- Includes: `git`, `branch`/`tag`/`rev`, `path`, `include`, `exclude`
- Excludes: `target` (target changes trigger local move, not re-fetch)

**File hashing semantics:**
- Symlinks: Follow and hash target content (not the link itself)
- Drift detection: Missing local file = drift, extra local files ignored
- Hash input: File bytes only (no metadata like permissions or timestamps)

## Filter Semantics

**Syntax:** Glob patterns (not full gitignore)
- `*` matches anything except `/`
- `**` matches any path component(s)
- Patterns are relative to source `path` (or repo root if no path)

**Algorithm:** Include-then-exclude (simpler than gitignore)
1. `include` patterns evaluated first (if empty, all files included)
2. `exclude` patterns evaluated second (removes from included set)
3. No negation support (differs from gitignore)

**Matching depth:**
- `*.md` matches only root-level files (e.g., `README.md`, not `skills/guide.md`)
- `**/*.md` matches files at any depth (e.g., `README.md`, `skills/guide.md`, `a/b/c.md`)

**Empty behavior:**
- `include = []` → include all files
- `exclude = []` → exclude nothing

## Path/Target Semantics

- `path`: Subdirectory within the source repo to sync (default: repo root)
- `target`: Local destination directory (default: source key name)

**Resolution:**
- `target` is relative to current working directory
- Directory structure within `path` is preserved under `target`
- Collision: Error if target already exists with different source

**Example:**
```toml
[sources]
skills = { git = "https://github.com/company/shared.git", path = "artifacts/skills", target = ".claude/skills" }
```
Fetches `artifacts/skills/*` from repo → writes to `.claude/skills/*`

## Config Schema

```toml
# phora.toml
version = 1

[sources]
skills = { git = "https://github.com/company/shared.git", tag = "v1.0", path = "skills", target = ".claude/skills" }
prompts = { git = "https://github.com/company/prompts.git", branch = "main" }

[manifest]
artifacts = ["skills", "commands", "agents"]
```

**Top-level sections:**
- `version`: Config format version (required)
- `[sources]`: Source definitions with inline table syntax
- `[manifest]`: Export configuration for this repo as a source

## Non-Functional Requirements

- **Breaking change:** Old config format not supported (parse error, no migration)
- Lock file format versioned for future changes
- URL parser handles edge cases gracefully (no panic on malformed input)
