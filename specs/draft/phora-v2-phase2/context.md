# Context: Phora v2 Phase 2

## Native Plan

**Source:** `specs/archive/phora-v2-phase1/resources/buzzing-cuddling-backus.md`

Phase 2 section of the Phora v2 roadmap. Builds on Phase 1's namespace and reference systems.

## Key Files

| File | Purpose |
|------|---------|
| `internal/config/config.go` | Move Include/Exclude from Harness to Source |
| `internal/source/source.go` | Add Include/Exclude to LocalSource/RepoSource |
| `internal/sync/sync.go` | Filter during discovery, not harness write |
| `internal/cli/add.go` | Source instantiation based on config type |
| `internal/cli/deploy.go` | Handle local sources from config |

## Tech Decisions

### Source-Level Filtering

**Decision:** Filter during `Discover()` call, not in sync

**Rationale:**
- Filtering at source reduces artifacts passed through pipeline
- Cleaner separation: source determines what's available, harness determines where it goes
- Enables per-source customization (different patterns per repo)

### Local Source Config Detection

**Decision:** Auto-detect based on `path` vs `repo` field presence

**Rationale:**
- No explicit `type` field needed
- `repo` → RepoSource, `path` → LocalSource
- Simple, unambiguous

### Glob Pattern Matching

**Decision:** Use `filepath.Match` for pattern matching

**Rationale:**
- Standard library, no dependencies
- Familiar glob syntax (`*`, `?`, `[...]`)
- Sufficient for artifact name patterns

## Data Model

### Config Changes

```go
type Source struct {
    Type    string   `toml:"type,omitempty"`
    Repo    string   `toml:"repo,omitempty"`
    Path    string   `toml:"path,omitempty"`
    Ref     string   `toml:"ref,omitempty"`
    Global  bool     `toml:"global,omitempty"`
    Include []string `toml:"include,omitempty"`  // NEW: moved from Harness
    Exclude []string `toml:"exclude,omitempty"`  // NEW: moved from Harness
}

type Harness struct {
    // Include/Exclude deprecated here (will log warning if used)
    Include []string `toml:"include,omitempty"` // DEPRECATED
    Exclude []string `toml:"exclude,omitempty"` // DEPRECATED
    // ... rest unchanged
}
```

### Source Struct Changes

Filter fields injected via constructor (no interface change needed):

```go
// Source interface unchanged
type Source interface {
    Name() string
    Discover() ([]*artifact.Artifact, error)
}

type LocalSource struct {
    path          string
    artifactTypes []string
    include       []string  // NEW: injected via constructor
    exclude       []string  // NEW: injected via constructor
}

type RepoSource struct {
    // ... existing fields
    include []string  // NEW: injected via constructor
    exclude []string  // NEW: injected via constructor
}

// Updated constructors
func NewLocal(path string, artifactTypes, include, exclude []string) *LocalSource
func NewRepo(repoStr, ref, dataDir string, artifactTypes, include, exclude []string) *RepoSource
```

### Filter Application Order

1. If `include` is set and non-empty, artifact.Name must match at least one pattern
2. If artifact.Name matches any `exclude` pattern, artifact is excluded
3. Exclude wins when artifact matches both include and exclude patterns
4. Patterns match against `artifact.Name` only (not `FullName` with namespace)

## Config Hierarchy

**Merge behavior for new fields:**

| Section | Strategy |
|---------|----------|
| `sources.*.include` | Project overrides (replace, not merge) |
| `sources.*.exclude` | Project overrides (replace, not merge) |
| `harness.*.include` | Deprecated, warn if used |
| `harness.*.exclude` | Deprecated, warn if used |

## Gotchas

- `filterArtifacts()` in sync.go currently uses harness include/exclude - needs refactoring
- `shouldSync()` helper function moves from sync to source package
- Glob matching should handle edge cases: empty pattern = match all, empty artifact name
- Local source paths need `config.ExpandPath()` for `~` expansion
