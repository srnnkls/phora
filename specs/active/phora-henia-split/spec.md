---
issue_type: Feature
created: 2026-01-14
status: Active
stage: active
promoted: 2026-01-15
claude_plan: .claude/plans/federated-imagining-moon.md
---

# Phora/Henia Split

Split phora into two packages: phora (git artifact fetcher) and henia (AI workflow manager).

## Goal

Separate concerns between artifact fetching (phora) and AI harness management (henia), enabling:
- phora as a standalone git artifact fetcher usable by any tool
- henia as the user-facing AI workflow manager that uses phora as a library

## Scope

### In Scope

1. **phora** - Pure git artifact fetcher (library + CLI)
   - Git clone/pull/cache operations
   - Source config with hosts and repos
   - Optional manifest support for path exports
   - Call-side path rewrites
   - phora.lock for tracking fetched commits
   - Standalone CLI: `phora add`, `phora sync`, `phora update`, `phora list`
   - **New Source interface returning file paths, not artifact types**

2. **henia** - AI workflow manager (frontend)
   - Created in `./henia/` directory (temporary, will be moved later)
   - Imports phora as Go library
   - Harness configuration (variables, tools, references, keys)
   - Artifact discovery, transformation, deployment
   - henia.lock per harness for tracking deployed files
   - CLI: `henia sync`, `henia deploy`, `henia update`, `henia add`

3. **Path Rewrite System**
   - Manifest-based exports (source-side): `internal/path = exported-name`
   - Call-side rewrites (consumer-side): `exported-name = local/path`
   - `--ignore-manifest` flag on `phora add`
   - Precedence: call-side rewrites override manifest exports

## Config Schemas

### phora.toml (sources config)

```toml
[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"

[sources.company]
repo = "company/shared-skills"
ref = "main"
ignore_manifest = false

[sources.company.paths]  # call-side rewrites
"my-skill" = "custom/location/my-skill"
```

### Remote Manifest (phora.toml in source repo)

```toml
[manifest]
name = "shared-skills"

[exports]  # source-side path exports
"skills/internal/my-skill" = "my-skill"
```

### henia.toml

```toml
global = ["personal"]

[sources.company]  # pass-through to phora with henia-specific fields
repo = "company/shared-skills"
ref = "main"

[harnesses.claude]
path = "~/.claude"
artifacts = ["skills", "commands", "agents"]
```

### phora.lock (in data_dir, global)

```toml
[[repos]]
name = "company"
repo = "company/shared-skills"
ref = "main"
commit = "abc123..."
fetched_at = "2026-01-14T10:00:00Z"
```

### henia.lock (per harness directory)

```toml
[[files]]
path = "skills/my-skill/SKILL.md"
checksum = "sha256:..."
artifact = "my-skill"
source = "company"
type = "skill"
```

### Out of Scope

- v2 Phase 2 features (source-level include/exclude)
- v2 Phase 3 features (linting, dependencies, manage command)
- Breaking changes to existing phora.toml format during transition

## Acceptance Criteria

### phora Library

- **Given** phora config with sources defined
- **When** `phora.Client.FetchAll()` is called
- **Then** repos are cloned to data directory and FetchResult returned

- **Given** source repo has phora.toml manifest with exports
- **When** `phora sync` runs
- **Then** path exports are applied (internal paths mapped to exported names)

- **Given** source added with `--ignore-manifest` flag
- **When** `phora sync` runs
- **Then** manifest is ignored, raw repo structure used

- **Given** call-side config has path rewrites
- **When** sync completes
- **Then** files land at rewritten paths on disk

### henia Integration

- **Given** henia.toml with sources section
- **When** `henia sync` runs
- **Then** phora library is called to fetch sources

- **Given** sources fetched by phora
- **When** `henia deploy` runs
- **Then** artifacts are discovered, transformed, and written to harnesses

- **Given** deployed artifact
- **When** henia.lock is checked
- **Then** file is tracked with checksum, source, and artifact name

### CLI Parity

- **Given** current `phora deploy` workflow
- **When** user runs `henia sync && henia deploy`
- **Then** equivalent functionality is achieved

## Non-Goals

- Maintaining backward compatibility with old phora CLI commands long-term
- Supporting phora without Go (subprocess-only integration)
- Automatic migration of existing phora.toml to henia.toml
