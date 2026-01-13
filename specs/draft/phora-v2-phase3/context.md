# Context: Phora v2 Phase 3

## Key Files

| File | Purpose |
|------|---------|
| `internal/cli/manage.go` | New manage command (to create) |
| `internal/cli/lint.go` | New lint command (to create) |
| `internal/lint/lint.go` | Linting engine (to create) |
| `internal/lint/rules.go` | Built-in lint rules (to create) |
| `internal/deps/deps.go` | Dependency graph (to create) |
| `internal/config/config.go` | Add env templating, lint config |
| `internal/config/template.go` | Template expansion (to create) |
| `internal/artifact/artifact.go` | Add DependsOn field |
| `internal/source/source.go` | Add DependsOn field |

## Existing Patterns

### CLI Command Structure
Commands use cobra with `init()` registration pattern. See `internal/cli/add.go` and `internal/cli/deploy.go`.

### Config Loading
`config.Load()` merges global + project configs. Template expansion should happen after merge.

### Reference Parsing
`internal/reference/reference.go` already parses sigils. Lint reference validation can reuse this.

### Artifact Discovery
`internal/source/source.go` discovers artifacts by walking directories. Same pattern for `phora manage` import.

## Tech Decisions

- **Linting engine**: Custom implementation, not external linter
- **Dependency graph**: Simple adjacency list with topological sort
- **Template engine**: Go `text/template` (already used for variables)

## Data Model Changes

### Artifact Frontmatter
```yaml
---
name: my-skill
depends_on:
  - other-skill
  - another-skill
---
```

### Source Config
```toml
[sources.my-source]
repo = "owner/repo"
depends_on = ["other-source"]
```

### Lint Config
```toml
[harness.claude.lint]
required_keys = ["name", "description"]
warn_missing_description = true

[harness.claude.lint.rules]
no-unresolved-refs = "error"
frontmatter-schema = "error"
```
