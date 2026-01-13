---
issue_type: Feature
created: 2026-01-13
status: Complete
stage: archive
promoted: 2026-01-13
completed: 2026-01-13
claude_plan: .claude/plans/buzzing-cuddling-backus.md
---

# Phora v2 Phase 1

Package manager and multiplexer for agent artifacts with enhanced configuration, namespacing, and reference transformation.

## Goal

Implement Phase 1 of Phora v2: command renames, namespace support, canonical reference parsing, and restructured mappings system.

## Scope

### In Scope

1. **Command Renames**
   - `phora install` → `phora add`
   - `phora sync` → `phora deploy`

2. **Namespaces with Dot Notation**
   - Default: artifacts prefixed with source name (`company.skill-name`)
   - `source.global = true`: no prefix (bare names)
   - Dot notation throughout: `namespace.artifact-name`

3. **Canonical Reference Parsing**
   - Fixed sigils: `$skill`, `/command`, `@agent`, `#file`, `!tool`
   - Only parsed inside inline code fences (backticks)
   - Configurable output templates per harness
   - New `internal/reference` package (shared by transform + lint)

4. **Tool Definitions**
   - Global tool list with optional metadata
   - Harness-specific tool name mappings (e.g., `bash` → `Bash`)
   - Harness-specific additional tools

5. **New Mappings Structure**
   - `keys`: frontmatter key remapping
   - `values`: frontmatter value remapping per key
   - Per-artifact-type overrides (`skills.keys`, `skills.values`)

### Out of Scope (Later Phases)

- Include/exclude per source (Phase 2)
- Local sources in config (Phase 2)
- `phora manage` command (Phase 3)
- Config env var templating (Phase 3)
- Linting system (Phase 3)
- Dependencies (Phase 3)

## Acceptance Criteria

### Command Renames

- **Given** a user runs `phora add owner/repo`
- **When** the command executes
- **Then** artifacts are fetched and added (same behavior as old `install`)

- **Given** a user runs `phora deploy`
- **When** the command executes
- **Then** artifacts are synced to harnesses (same behavior as old `sync`)

### Namespaces

- **Given** a source `company` with artifact `code-test`
- **When** deployed without `global = true`
- **Then** artifact is named `company.code-test` in target

- **Given** a source with `global = true`
- **When** deployed
- **Then** artifacts use bare names (no prefix)

### Reference Parsing

- **Given** artifact body contains `` `$skill-name` `` (in backticks)
- **When** deployed to claude harness with `references.skill.output = "/{{name}}"`
- **Then** output contains `/skill-name`

- **Given** artifact body contains `$skill-name` outside backticks
- **When** deployed
- **Then** text is unchanged (sigils only parsed in code fences)

- **Given** malformed reference `` `$` ``
- **When** deployed with default config
- **Then** warning logged, reference unchanged

### Namespace Reference Resolution

- **Given** artifact `code-test` in source `company` (not global)
- **When** referenced as `` `$code-test` `` from within same source
- **Then** resolves to `company.code-test`

- **Given** artifact `code-test` in source `company` (not global)
- **When** referenced as `` `$code-test` `` from different source
- **Then** requires full qualification `` `$company.code-test` ``

- **Given** artifact with dot in name (e.g., `my.skill`) in global source
- **When** validated
- **Then** warning issued (conventional misnomer)

### Tool Mappings

- **Given** artifact body contains `!bash`
- **When** deployed to claude with `tools.bash = "Bash"`
- **Then** output contains `Bash`

### Key/Value Mappings

- **Given** artifact frontmatter `allowed-tools: [bash, read]`
- **When** deployed with `keys.allowed-tools = "tools"` and `values.tools = {bash = "Bash"}`
- **Then** output has `tools: [Bash, read]`

## Non-Functional Requirements

- Breaking change: old `mappings` field not supported
- Malformed reference handling: configurable, default warn
- All existing tests must pass after refactor
