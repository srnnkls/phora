---
issue_type: Feature
created: 2026-01-13
status: Draft
stage: draft
---

# Phora v2 Phase 3

Artifact management, linting, dependency tracking, and config templating.

## Goal

Implement Phase 3 of Phora v2: the `phora manage` command for taking over existing artifacts, a linting system for validation, dependency tracking at both artifact and source levels, and environment variable templating in configs.

## Scope

### In Scope

1. **`phora manage` Command**
   - Initialize target directory with `phora init`
   - Copy existing artifacts from harness to managed package
   - Deploy managed package back to harness
   - Migrate existing unmanaged artifacts (detect and import)

2. **Linting System**
   - Schema validation (frontmatter fields, required keys, types)
   - Reference validation (resolve `$skill`, `/command`, `@agent`, `#file`, `!tool`)
   - Custom rules per harness
   - `phora lint` command with configurable severity

3. **Dependency Tracking**
   - Artifact dependencies (skill A depends on skill B)
   - Source dependencies (source A depends on source B)
   - Deploy order resolution based on dependency graph
   - Circular dependency detection

4. **Config Environment Variable Templating**
   - Go template syntax in config values
   - Access to environment variables via `{{.Env.VAR}}`
   - Consistent with existing `{{variable}}` pattern

### Out of Scope

- Remote artifact registries
- Version pinning for artifacts
- Automatic dependency resolution from artifact content

## Acceptance Criteria

### `phora manage`

- **Given** a harness directory with existing skills at `~/.claude/skills/`
- **When** user runs `phora manage ~/.claude`
- **Then** artifacts are copied to a new phora package, `phora.toml` created, and deployed back

- **Given** existing unmanaged artifacts in target
- **When** `phora manage` runs
- **Then** existing artifacts are detected and offered for import

- **Given** a target directory
- **When** `phora manage <target>` completes
- **Then** target has `phora.toml` and artifact directories (skills/, commands/, agents/)

### Linting

- **Given** an artifact with missing required frontmatter key
- **When** `phora lint` runs
- **Then** error reported with file path and missing key

- **Given** an artifact referencing `` `$nonexistent-skill` ``
- **When** `phora lint` runs
- **Then** warning reported about unresolved reference

- **Given** harness-specific lint rules in config
- **When** `phora lint --target claude` runs
- **Then** only claude-specific rules applied

- **Given** `phora deploy` with linting enabled
- **When** lint errors exist
- **Then** deploy blocked with error summary

### Dependencies

- **Given** artifact A declares `depends_on: [B, C]` in frontmatter
- **When** `phora deploy` runs
- **Then** B and C deployed before A

- **Given** source X declares `depends_on: [Y]`
- **When** `phora add X` runs
- **Then** source Y added first (or error if not available)

- **Given** circular dependency A → B → A
- **When** dependency graph resolved
- **Then** error reported with cycle path

### Environment Variable Templating

- **Given** config with `path = "{{.Env.HOME}}/.claude"`
- **When** config loaded
- **Then** path resolved to `/Users/username/.claude`

- **Given** undefined environment variable `{{.Env.UNDEFINED}}`
- **When** config loaded
- **Then** empty string used (no error)

## Non-Functional Requirements

- Lint rules configurable via `[harness.*.lint]` section
- Default lint rules for common harnesses (claude, opencode)
- `phora manage` should be idempotent (safe to run multiple times)
- Dependency resolution uses topological sort
