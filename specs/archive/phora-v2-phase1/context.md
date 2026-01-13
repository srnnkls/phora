# Context: Phora v2 Phase 1

## Native Plan

**Source:** `resources/buzzing-cuddling-backus.md`

Comprehensive design for Phora v2 covering 3 phases. This spec covers Phase 1 only.

## Key Files

| File | Purpose |
|------|---------|
| `internal/cli/install.go` | Rename to `add.go` |
| `internal/cli/sync.go` | Rename to `deploy.go` |
| `internal/cli/root.go` | Update command registration |
| `internal/config/config.go` | New structs: Source.Global, Harness.Tools, References, Keys, Values |
| `internal/artifact/artifact.go` | Add Namespace field, FullName() method |
| `internal/source/source.go` | Apply namespace during discovery |
| `internal/reference/reference.go` | NEW: Sigil parsing, position tracking |
| `internal/transform/transform.go` | Reference transformation, value mappings |
| `internal/sync/sync.go` | Use FullName() for conflict detection |
| `internal/target/target.go` | Use FullName() for paths |

## Tech Decisions

### Reference System Design

**Decision:** Fixed canonical sigils, configurable output templates

**Rationale:**
- Sigils (`$`, `/`, `@`, `#`, `!`) are canonical - same in all source files
- Output templates vary per harness (Claude uses `/skill`, OpenCode uses backticks)
- Separates parsing (fixed) from rendering (configurable)

**Alternatives considered:**
- Configurable sigils: Rejected - makes source files non-portable
- No sigils, just names: Rejected - ambiguous, hard to parse

### Tool Name Mapping

**Decision:** Lowercase canonical names, harness-specific mappings

**Rationale:**
- Canonical: `bash`, `read`, `write` (lowercase)
- Claude maps to PascalCase: `Bash`, `Read`, `Write`
- OpenCode uses lowercase (no mapping needed)
- Consistent canonical form simplifies cross-harness authoring

### Breaking Changes

**Decision:** No backwards compatibility for `mappings` field

**Rationale:**
- Clean break simplifies implementation
- Old `mappings` → new `keys` is straightforward migration
- Phora is pre-1.0, breaking changes acceptable

## Data Model

### Config Structs

```go
type Config struct {
    DefaultHarnesses []string
    DefaultArtifacts []string
    Sources          map[string]Source
    Harness          map[string]Harness
    Tools            []string                    // NEW: global tool list
    References       map[string]ReferenceConfig  // NEW: sigil → output
}

type Source struct {
    Type   string
    Repo   string
    Path   string
    Ref    string
    Global bool  // NEW: promotes to global namespace
}

type Harness struct {
    Path                       string
    Structure                  string
    GenerateCommandsFromSkills bool
    Variables                  map[string]string
    Include                    []string
    Exclude                    []string
    // NEW
    Tools      map[string]string            // canonical → harness name
    References map[string]ReferenceConfig   // output overrides
    Keys       map[string]string            // replaces Mappings
    Values     map[string]map[string]string
    Skills     *ArtifactMappings
    Commands   *ArtifactMappings
    Agents     *ArtifactMappings
}

type ArtifactMappings struct {
    Keys   map[string]string
    Values map[string]map[string]string
}

type ReferenceConfig struct {
    Sigil  string  // fixed at global level
    Output string  // template string
}
```

### Artifact Struct

```go
type Artifact struct {
    Namespace   string  // NEW: source namespace (empty if global)
    Name        string
    Type        Type
    SourcePath  string
    IsDirectory bool
    Frontmatter map[string]any
    Body        string
    Resources   []string
}

func (a *Artifact) FullName() string {
    if a.Namespace == "" {
        return a.Name
    }
    return a.Namespace + "." + a.Name
}
```

### Reference Struct

```go
type RefType string

const (
    RefSkill   RefType = "skill"
    RefCommand RefType = "command"
    RefAgent   RefType = "agent"
    RefFile    RefType = "file"
    RefTool    RefType = "tool"
)

var Sigils = map[RefType]string{
    RefSkill:   "$",
    RefCommand: "/",
    RefAgent:   "@",
    RefFile:    "#",
    RefTool:    "!",
}

type Reference struct {
    Type      RefType
    Name      string
    Namespace string
    Raw       string
    Start     int
    End       int
}
```

## Config Hierarchy

**Load order:**
1. Global: `--config` flag
2. Project: `./phora.toml`

**Merge behavior:**

| Section | Strategy |
|---------|----------|
| `tools` (list) | Union |
| `tools.*` (metadata) | Project overrides |
| `references.*.output` | Project overrides |
| `harness.*.tools` | Merged (project wins) |
| `harness.*.references.*.output` | Merged (project wins) |
| `harness.*.keys` | Merged (project wins) |
| `harness.*.values` | Deep merge (project wins) |
| `harness.*.skills.keys` | Extends harness.*.keys |
| `harness.*.skills.values` | Extends harness.*.values |
| `sources.*` | Project overrides by name |

## Gotchas

- `FullName()` must be used consistently for lockfile keys, conflict detection, and target paths
- References only parsed inside backticks - avoids edge cases like `$100`, `foo$bar`
- Tool mapping happens before template application
- ArtifactMappings only has keys/values, not tools/references
- Bare names resolve within same source; cross-source non-global refs need full qualification

## Archive Notes

**Archived:** 2026-01-13

### Summary

Implemented Phase 1 of Phora v2: renamed commands (install→add, sync→deploy), added namespace support with dot notation, created reference parsing system with canonical sigils, and restructured mappings to use keys/values pattern.

### Key Outcomes

- Commands renamed: `phora add` and `phora deploy` replace old names
- Namespace support: artifacts prefixed with source name unless `global = true`
- Reference system: canonical sigils ($, /, @, #, !) parsed in backticks with configurable output templates
- New mappings structure: `keys`, `values`, and per-artifact-type overrides

### Technical Debt / Future Work

- Phase 2: Include/exclude per source, local sources in config
- Phase 3: `phora manage` command, config env var templating, linting system, dependencies

### Lessons Learned

- Separating parsing (fixed sigils) from rendering (configurable templates) simplifies cross-harness authoring
