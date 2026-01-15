# Context: Phora/Henia Split

## Native Plan

**Source:** `.claude/plans/federated-imagining-moon.md`

- **Goal:** Split phora into fetcher (phora) and workflow manager (henia)
- **Approach:** Monorepo with two Go modules, phora at root, henia nested
- **Open questions resolved:** Library vs subprocess (library), manifest optional with --ignore-manifest flag

---

## Key Files

### Git Operations (stays in phora)

| File | Lines | Description |
|------|-------|-------------|
| `internal/source/source.go` | 70-221 | ParseRepoString, Clone, Pull - extract to repo.go |
| `internal/config/config.go` | 19-39 | Host, Source structs - extract minimal config |

### Artifact Management (moves to henia)

| File | Lines | Description |
|------|-------|-------------|
| `internal/artifact/artifact.go` | all | Artifact types, parsing, discovery |
| `internal/transform/transform.go` | all | Variable substitution, key mappings |
| `internal/target/target.go` | all | Harness target writing |
| `internal/sync/sync.go` | all | Orchestration - refactor to use phora.Client |
| `internal/reference/reference.go` | all | Reference parsing ($skill, /command, etc.) |
| `internal/defaults/` | all | Embedded harness configs |
| `internal/lockfile/lockfile.go` | all | Adapt to henia.lock format |

### CLI (split between both)

| File | Lines | Description |
|------|-------|-------------|
| `internal/cli/add.go` | all | Split: phora add (fetch) vs henia add (full) |
| `internal/cli/deploy.go` | all | Moves to henia |
| `internal/cli/init.go` | all | Moves to henia |

---

## Architecture Decisions

### AD-1: Monorepo with Nested Modules

**Context:** Need to split into two packages while maintaining development velocity.

**Decision:** Single repo with phora at root, henia as nested module at `./henia/` (temporary location, will be moved later).

**Alternatives:**
- Separate repos: Clean separation but coordination overhead
- Single module: No clear API boundary

**Impact:** henia uses `replace` directive during dev, released as separate module. Initial development in `./henia/`, final location TBD.

### AD-2: phora as Library-First

**Context:** henia needs to call phora for fetching.

**Decision:** phora exposes public Go API, CLI is thin wrapper.

**Alternatives:**
- CLI subprocess: Simpler but less efficient, harder to pass structured data
- Both: Adds maintenance burden

**Impact:** Public types in phora package root (not internal/).

### AD-3: Optional Manifest with Ignore Flag

**Context:** Some source repos won't have manifests, some consumers want raw access.

**Decision:** Manifest optional. `--ignore-manifest` flag on `phora add` persists to config.

**Alternatives:**
- Manifest required: Breaks existing workflows
- Runtime flag: Requires flag on every sync

**Impact:** `ignore_manifest` field in Source config struct.

### AD-4: phora Source Interface Returns File Paths

**Context:** Current `Source` interface returns `[]*artifact.Artifact`, coupling phora to artifact types.

**Decision:** Redesign interface to return file paths only. phora is artifact-agnostic.

```go
// OLD (coupled)
type Source interface {
    Discover() ([]*artifact.Artifact, error)
}

// NEW (decoupled)
type FetchResult struct {
    Name      string
    LocalPath string
    Commit    string
    Files     []string  // relative paths
}
```

**Alternatives:**
- Keep current interface: Defeats separation goal
- Return raw bytes: Too low-level

**Impact:** Requires new task before extraction. henia handles artifact discovery from file paths.

### AD-5: henia.toml Sources Pass-Through to phora

**Context:** Need to decide if henia.toml sources are pass-through or extended.

**Decision:** henia.toml sources section passes through to phora unchanged. henia-specific config (harnesses) is separate.

**Alternatives:**
- Extended sources: Adds complexity, mixing concerns
- Separate files: Two configs to manage

**Impact:** henia parses sources section, passes to phora.Client as-is.

### AD-6: phora.lock Location

**Context:** Need to decide where phora.lock lives when henia is consumer.

**Decision:** phora.lock lives in `{data_dir}/phora.lock` (global). Tracks all fetched repos across projects.

**Alternatives:**
- Per-project: Duplication across projects using same sources
- Per-source: Too granular

**Impact:** Single global lock managed by phora.Client.

---

## Constraints

### Technical

- Go module compatibility: henia must import phora cleanly
- Existing tests: Must pass after migration with minimal changes

### Business

- Existing users: `phora deploy` workflow should map to `henia sync && henia deploy`

---

## Data Model

### phora Types

| Entity | Purpose | Key Fields |
|--------|---------|------------|
| Config | Root config | Hosts, Sources |
| Source | Repo definition | Repo, Ref, Path, IgnoreManifest, Paths |
| Host | Git host config | GitURL template |
| FetchResult | Fetch output | Name, LocalPath, Commit, Files |

### henia Types

| Entity | Purpose | Key Fields |
|--------|---------|------------|
| Config | Root config | Global, Sources, Harnesses |
| Harness | Target config | Path, Structure, Variables, Tools, References |
| Source | Extended source | phora.Source + Global flag |

---

## Open Questions

- [x] Should henia.toml sources section be pass-through to phora, or extended? → **Pass-through (AD-5)**
- [x] Where does phora.lock live when henia is the consumer? → **Global in data_dir (AD-6)**

---

## Future Considerations

- phora could support non-git sources (HTTP archives, local tarballs)
- henia could support plugin harnesses beyond embedded defaults
