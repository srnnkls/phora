---
issue_type: Feature
created: 2026-01-25
status: Draft
stage: draft
claude_plan: .claude/plans/linked-swinging-goose.md
---

# Phora Targets Config

Add a `[targets]` config section to phora defining WHERE artifacts are deployed, with symlink (default) or copy modes. Targets declare which sources they receive.

## Goal

Enable phora to be a pure git artifact fetcher by:
1. Decoupling destination config from harness-specific logic
2. Supporting package mode (symlinks to cache) and vendor mode (copies)
3. Target-side mapping: targets declare which sources they pull from

## Scope

### In Scope

1. **Config schema changes**
   - Rename `Source.Path` to `Source.Root` (subdir within repo)
   - Remove `Source.Target` field
   - New `Target` struct with `Path`, `Mode`, and `Sources` fields
   - `Config.Targets map[string]Target`

2. **Cache restructure**
   - Content-addressable: `~/.phora/cache/<source>/<commit>/`
   - Enables symlinks to specific versions

3. **Deployment modes**
   - `mode = "symlink"` (default): per-artifact symlinks to cache
   - `mode = "copy"`: file copies for vendor/editable use

4. **Lock file extension**
   - Track which targets received which artifacts

### Out of Scope

- Harness transformations (henia's job)
- Artifact type detection (henia's job)
- Variable substitution (henia's job)
- Transitive dependencies (future spec)

## Config Schema

### Common case: multiple sources → one target

```toml
version = 1

[sources.company]
git = "https://github.com/company/shared.git"
root = "skills"

[sources.personal]
git = "https://github.com/me/dotfiles.git"
root = ".claude/skills"

[targets.claude]
path = "~/.claude/skills"
sources = ["company", "personal"]
```

Sources are pure dependency declarations. Routing lives on the target.

### Default target (all sources)

```toml
version = 1

[sources.company]
git = "https://github.com/company/shared.git"
root = "skills"

[sources.personal]
git = "https://github.com/me/dotfiles.git"
root = ".claude/skills"

[targets.claude]
path = "~/.claude/skills"
# no sources = all sources
```

### Multiplexing: one source → multiple targets

```toml
version = 1

[sources.company]
git = "https://github.com/company/shared.git"
root = "skills"

[targets.claude]
path = "~/.claude/skills"
sources = ["company"]

[targets.vscode]
path = "~/.config/Code/User/snippets"
sources = ["company"]
mode = "copy"
```

### Mixed modes: symlink + vendor

```toml
version = 1

[sources.stable]
git = "https://github.com/company/shared.git"
tag = "v1.0"
root = "skills"

[sources.experimental]
git = "https://github.com/me/wip.git"
branch = "dev"

[targets.claude]
path = "~/.claude/skills"
sources = ["stable"]
# mode defaults to "symlink"

[targets.local]
path = "./vendor/skills"
sources = ["experimental"]
mode = "copy"
```

## Data Model

```go
type Source struct {
    Git            string   `toml:"git"`
    Branch         string   `toml:"branch,omitempty"`
    Tag            string   `toml:"tag,omitempty"`
    Rev            string   `toml:"rev,omitempty"`
    Root           string   `toml:"root,omitempty"`
    Include        []string `toml:"include,omitempty"`
    Exclude        []string `toml:"exclude,omitempty"`
    IgnoreManifest bool     `toml:"ignore_manifest"`
}

type Target struct {
    Path    string   `toml:"path"`
    Mode    string   `toml:"mode,omitempty"`    // "symlink" (default) | "copy"
    Sources []string `toml:"sources,omitempty"` // nil = all sources
}

type Config struct {
    Version  int               `toml:"version"`
    Hosts    map[string]Host   `toml:"hosts,omitempty"`
    Sources  map[string]Source `toml:"sources,omitempty"`
    Targets  map[string]Target `toml:"targets,omitempty"`
    Manifest *Manifest         `toml:"manifest,omitempty"`
}
```

## Behavior

### Cache Structure

```
~/.phora/
├── cache/
│   └── <source>/
│       └── <commit-sha>/
│           └── <root>/
│               ├── artifact-a/
│               └── artifact-b/
└── phora.lock
```

### Symlink Mode

Per-artifact symlinks from target path to cache:

```bash
~/.claude/skills/code-review → ~/.phora/cache/company/abc123/skills/code-review
~/.claude/skills/debugging   → ~/.phora/cache/company/abc123/skills/debugging
```

### Sync Flow

```
phora sync
├── For each source:
│   ├── Check digest (lazy sync)
│   └── Fetch to ~/.phora/cache/<source>/<commit>/
├── For each target:
│   ├── Resolve sources (explicit list or all)
│   └── For each source:
│       ├── symlink: ln -s <cache>/<artifact> <target.path>/<artifact>
│       └── copy: cp -r <cache>/<artifact> <target.path>/<artifact>
└── Save phora.lock
```

### Drift Detection

| Scenario | Behavior |
|----------|----------|
| Missing symlink | Error unless --force |
| Broken symlink (dangling) | Re-fetch source, recreate |
| Symlink → wrong commit | Update symlink |
| File replaced symlink | Error unless --force |

### Platform Behavior

| Platform | Default | Behavior |
|----------|---------|----------|
| Unix | symlink | Works as expected |
| Windows (Dev Mode) | symlink | Works |
| Windows (no Dev Mode) | symlink | Warn, suggest `mode = "copy"` or `--copy` |

On symlink failure (Windows without Developer Mode):

```
$ phora sync

⚠ Cannot create symlinks (requires Developer Mode or admin)
  Use copy mode for this target:

    [targets.claude-skills]
    mode = "copy"

  Or run with --copy to copy without changing config
```

### Cache Mutation Detection

`phora status` detects modifications to cached files through symlinks:

```
$ phora status

⚠ Modified symlink target: ~/.phora/cache/company/abc123/skills/code-review
  This modifies the cached source, not a local copy.

  To create an editable copy:
    phora vendor code-review --target claude-skills
```

### Vendoring

`phora vendor` converts a symlinked artifact to an editable copy:

```bash
$ phora vendor code-review --target claude-skills
Copied code-review to ~/.claude/skills/code-review (editable)
```

This replaces the symlink with a file copy. `phora status` tracks it as vendored.

## Acceptance Criteria

### Config Parsing

- **Given** target with `sources = ["a", "b"]`
- **When** config loaded
- **Then** Target.Sources contains both names

- **Given** target without `sources`
- **When** sync runs
- **Then** all sources are deployed to this target

- **Given** target without `mode`
- **When** sync runs
- **Then** mode defaults to "symlink"

- **Given** target references undefined source
- **When** config validated
- **Then** error: "undefined source 'x' in target 'y'"

- **Given** source with `root = "skills"`
- **When** fetched
- **Then** only `skills/` subdir is available

### Sync Behavior

- **Given** `mode = "symlink"`
- **When** sync completes
- **Then** target contains per-artifact symlinks to cache

- **Given** `mode = "copy"`
- **When** sync completes
- **Then** target contains copied files

- **Given** unchanged source digest
- **When** sync runs
- **Then** source skipped (lazy sync)

### Drift Detection

- **Given** user deleted symlink
- **When** sync runs without --force
- **Then** error reported

- **Given** symlink points to old commit
- **When** sync runs
- **Then** symlink updated to new commit

## Harness Removal

No users exist. No deprecation path needed. Remove all harness/henia code:

- Delete `Harness` struct from config packages
- Delete `internal/artifact`, `internal/transform`, `internal/target`
- Delete `internal/reference`, `internal/defaults`
- Delete `deploy` command
- Zero harness references remaining in codebase

## Non-Goals

- Artifact transformation (out of scope for phora)
- Template variable substitution (out of scope for phora)
- Backward compatibility with harness config
