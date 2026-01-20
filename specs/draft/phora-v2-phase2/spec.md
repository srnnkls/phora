---
issue_type: Feature
created: 2026-01-13
status: Draft
stage: draft
claude_plan: specs/archive/phora-v2-phase1/resources/buzzing-cuddling-backus.md
---

# Phora v2 Phase 2

Source-level filtering and local source configuration.

## Goal

Move include/exclude filtering from harness to source level and add explicit local source support in config files.

## Scope

### In Scope

1. **Include/Exclude per Source**
   - Move `include`/`exclude` from `Harness` to `Source` struct
   - Filter during source discovery, not harness write
   - Support glob patterns (e.g., `code-*`, `*-debug`)

2. **Local Sources in Config**
   - Support `path` field for local directories
   - Auto-detect source type based on presence of `repo` vs `path`
   - Combine with namespace system from Phase 1

### Out of Scope

- `phora manage` command (Phase 3)
- Config env var templating (Phase 3)
- Linting system (Phase 3)
- Dependencies (Phase 3)

## Acceptance Criteria

### Include/Exclude per Source

- **Given** source config with `include = ["code-*"]`
- **When** artifacts are discovered
- **Then** only artifacts matching `code-*` are returned

- **Given** source config with `exclude = ["code-debug"]`
- **When** artifacts are discovered
- **Then** `code-debug` is excluded from results

- **Given** source config with both `include` and `exclude`
- **When** artifact matches include but also matches exclude
- **Then** artifact is excluded (exclude wins)

- **Given** harness still has `include`/`exclude` fields (legacy)
- **When** sync runs
- **Then** harness-level filtering is deprecated with warning, source-level takes precedence

### Local Sources in Config

- **Given** config with `[sources.local]` containing `path = "~/my-skills"`
- **When** `phora deploy` runs
- **Then** artifacts are loaded from local path

- **Given** source with `path` field but no `repo` field
- **When** source is instantiated
- **Then** `LocalSource` is created (not `RepoSource`)

- **Given** local source with `global = false`
- **When** artifacts are discovered
- **Then** artifacts get namespace prefix (e.g., `local.code-test`)

- **Given** local source with `global = true`
- **When** artifacts are discovered
- **Then** artifacts use bare names

### Deploy from Config Sources

- **Given** `phora deploy` runs without `--source` flag
- **When** config has `[sources.*]` entries
- **Then** all config sources are instantiated and deployed

- **Given** local source path doesn't exist
- **When** source is instantiated
- **Then** error is returned and deploy aborts (fail fast)

### Glob Pattern Edge Cases

- **Given** source config with empty `include = []`
- **When** artifacts are discovered
- **Then** all artifacts pass (no filtering)

- **Given** glob pattern with invalid syntax
- **When** pattern is matched
- **Then** pattern returns no match (treated as literal)

- **Given** artifact in namespaced source (e.g., `company.code-test`)
- **When** include pattern `code-*` is evaluated
- **Then** pattern matches against `Name` only (`code-test`), not `FullName`

## Non-Functional Requirements

- Backward compatible: existing harness-level include/exclude works but logs deprecation warning
- Glob pattern matching for include/exclude (not just exact match)
