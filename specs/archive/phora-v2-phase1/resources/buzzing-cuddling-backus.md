# Phora v2 Roadmap

## Config Hierarchy

**Load order:**
1. Global: `--config` flag (e.g., `~/.config/phora/config.toml`)
2. Project: `./phora.toml`

**Merge behavior:**

| Section | Strategy |
|---------|----------|
| `tools` (list) | Union |
| `tools.*` (metadata) | Project overrides |
| `references.*.output` | Project overrides |
| `harness.*.tools` | Merged (project wins on conflict) |
| `harness.*.references.*.output` | Merged (project wins on conflict) |
| `harness.*.keys` | Merged (project wins on conflict) |
| `harness.*.values` | Deep merge (project wins on conflict) |
| `harness.*.skills/commands/agents.keys` | Merged (extends harness.*.keys) |
| `harness.*.skills/commands/agents.values` | Deep merge (extends harness.*.values) |
| `sources.*` | Project overrides by name |

---

## Phase 1: Priority Features

### 1. Command Renames
- `phora install` → `phora add`
- `phora sync` → `phora deploy`

**Files:**
- `internal/cli/install.go` → `internal/cli/add.go`
- `internal/cli/sync.go` → `internal/cli/deploy.go`
- `internal/cli/root.go` (update command registration)
- Update README.md

### 2. Namespaces with Dot Notation

**Behavior:**
- Default: artifacts prefixed with source name (`srnnkls.code-test`)
- `source.global = true`: no prefix (bare names)
- Dot notation throughout: `namespace.artifact-name`

**Config:**
```toml
[sources.srnnkls]
repo = "srnnkls/dotfiles"
global = true  # artifacts use bare names

[sources.company]
repo = "company/shared-skills"
# default: artifacts become company.skill-name
```

**Struct changes:**

```go
// config.go
type Source struct {
    Type   string `toml:"type,omitempty"`
    Repo   string `toml:"repo,omitempty"`
    Path   string `toml:"path,omitempty"`
    Ref    string `toml:"ref,omitempty"`
    Global bool   `toml:"global,omitempty"`  // NEW
}

// artifact.go
type Artifact struct {
    Namespace   string         // NEW: source namespace (empty if global)
    Name        string
    Type        Type
    // ...
}

func (a *Artifact) FullName() string {
    if a.Namespace == "" {
        return a.Name
    }
    return a.Namespace + "." + a.Name
}
```

**Files:**
- `internal/config/config.go`: Add `Global bool` to Source
- `internal/artifact/artifact.go`: Add `Namespace` field + `FullName()` method
- `internal/source/source.go`: Set namespace during discovery based on source name
- `internal/sync/sync.go`: Use `FullName()` for conflict detection, lockfile keys
- `internal/target/target.go`: Use `FullName()` for directory paths

### 3. Canonical Artifact Reference Parsing

**Canonical sigils:**
- `$skill-name` - skill reference
- `/command-name` - command reference
- `@agent-name` - agent reference
- `#file/path` - file reference
- `!tool` - tool reference

**Global tool definitions:**

```toml
# Global tools - simple form (just declares existence)
tools = ["bash", "read", "write", "edit", "glob", "grep"]

# Or with optional metadata (for template expansion)
[tools.bash]
description = "Execute shell commands"

[tools.read]
description = "Read file contents"
```

**Harness-specific tool mappings:**

```toml
[harness.claude.tools]
# Map canonical → harness name
bash = "Bash"
read = "Read"
write = "Write"
# Claude-specific tools
task = "Task"

[harness.opencode.tools]
# OpenCode-specific tools (canonical names work as-is)
agent = "agent"
```

**Reference configuration:**

Each reference type has a fixed sigil and configurable output template.

```toml
# Global defaults (sigils are fixed, outputs are defaults)
[references.skill]
sigil = "$"                    # fixed, canonical
output = "`/{{name}}`"

[references.command]
sigil = "/"
output = "`/{{name}}`"

[references.agent]
sigil = "@"
output = "`@{{name}}`"

[references.file]
sigil = "#"
output = "`@{{name}}`"

[references.tool]
sigil = "!"
output = "`{{name}}`"

# Harness-specific output overrides
[harness.claude.references.skill]
output = "`/{{name}}`"         # $create-plan → `/create-plan`

[harness.claude.references.tool]
output = "`{{name}}`"          # !bash → `Bash`

[harness.opencode.references.skill]
output = "`{{name}}`"          # $create-plan → `create-plan`

[harness.opencode.references.file]
output = "`#{{name}}`"         # #README.md → `#README.md`
```

Template variables: `name`, `description`, `namespace`

**New package:** `internal/reference` (shared by transform + lint)

```go
// reference/reference.go
type RefType string

const (
    RefSkill   RefType = "skill"
    RefCommand RefType = "command"
    RefAgent   RefType = "agent"
    RefFile    RefType = "file"
    RefTool    RefType = "tool"
)

// Canonical sigils (fixed)
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
    Namespace string  // for artifacts only (not tools/files)
    Raw       string
    Start     int
    End       int
}

func Parse(content string) []Reference
```

**Transform flow for tools:**
1. Parse `!bash` → Reference{Type: tool, Name: "bash"}
2. Look up in `harness.tools["bash"]` → mapped name (e.g., "Bash" for claude)
3. Apply template with mapped name

**Files:**
- `internal/reference/reference.go` (NEW): Fixed sigil parsing, position tracking
- `internal/config/config.go`: Add global `Tools`, harness `Tools` (mappings + definitions), `Templates`
- `internal/transform/transform.go`: Use `reference.Parse()`, apply tool mappings, then templates
- `internal/lint/lint.go` (Phase 3): Validate tool refs against global + harness-specific tools

---

### 4. New Mappings Structure

**Current:**
```toml
[harness.claude.mappings]
allowed-tools = "tools"
```

**New:**
```toml
[harness.claude.keys]
allowed-tools = "tools"

[harness.claude.values.tools]
bash = "Bash"
read = "Read"

[harness.claude.skills.keys]
# artifact-type-specific (merges with generic)

[harness.claude.skills.values.tools]
# artifact-type-specific
```

**Struct changes:**

```go
// config.go
type Harness struct {
    Path                       string              `toml:"path,omitempty"`
    Structure                  string              `toml:"structure,omitempty"`
    GenerateCommandsFromSkills bool                `toml:"generate_commands_from_skills,omitempty"`
    Variables                  map[string]string   `toml:"variables,omitempty"`
    Include                    []string            `toml:"include,omitempty"`
    Exclude                    []string            `toml:"exclude,omitempty"`
    // NEW
    Tools                      map[string]string            `toml:"tools,omitempty"`      // canonical → harness name
    References                 map[string]ReferenceConfig   `toml:"references,omitempty"` // ref type → output config
    Keys                       map[string]string            `toml:"keys,omitempty"`      // replaces Mappings
    Values                     map[string]map[string]string `toml:"values,omitempty"`
    // Per-artifact-type overrides (keys/values only, no tools/templates)
    Skills                     *ArtifactMappings            `toml:"skills,omitempty"`
    Commands                   *ArtifactMappings            `toml:"commands,omitempty"`
    Agents                     *ArtifactMappings            `toml:"agents,omitempty"`
}

type ArtifactMappings struct {
    Keys   map[string]string            `toml:"keys,omitempty"`
    Values map[string]map[string]string `toml:"values,omitempty"`
}

type ReferenceConfig struct {
    Sigil  string `toml:"sigil,omitempty"`  // fixed at global level
    Output string `toml:"output,omitempty"` // template string
}

// transform.go
type Transformer struct {
    Variables map[string]string
    Keys      map[string]string            // merged harness + artifact-type keys
    Values    map[string]map[string]string // merged harness + artifact-type values
}
```

**Transform logic:**
1. Apply key mappings: `allowed-tools` → `tools`
2. Apply value mappings: for key `tools`, map `bash` → `Bash`
3. Merge: harness.Keys + harness.Skills.Keys (specific wins on conflict)

**Files:**
- `internal/config/config.go`: New structs, merge logic in `Merge()`
- `internal/transform/transform.go`: Add `ApplyValueMappings()`, update `Transform()`
- `internal/sync/sync.go`: Build transformer with merged mappings for artifact type

---

## Phase 2: Source & Include/Exclude

### 5. Include/Exclude per Source

**Current:** include/exclude on harness
**New:** include/exclude on source

```toml
[sources.company]
repo = "company/shared"
include = ["code-*"]
exclude = ["code-debug"]
```

**Files:**
- `internal/config/config.go`: Move Include/Exclude to Source
- `internal/sync/sync.go`: Filter during source discovery, not harness write

### 6. Local Sources in Config

Already supported via `LocalSource`, but config needs explicit support:

```toml
[sources.local]
path = "~/my-skills"
global = true
```

**Files:**
- `internal/config/config.go`: Source.Type detection (repo vs local)
- `internal/sync/sync.go`: Instantiate LocalSource or RepoSource based on config

---

## Phase 3: Advanced Features

### 7. `phora manage <target-dir>`

**Behavior:**
1. Run `phora init` on target dir
2. Copy artifacts to target
3. Create `phora.toml` in target pointing back to source
4. Run `phora deploy`

Creates ongoing management relationship.

**Files:**
- `internal/cli/manage.go` (new command)

### 8. Config Templating with Env Vars

```toml
[harness.claude]
path = "{{env.CLAUDE_CONFIG_DIR | default:~/.claude}}"
```

**Files:**
- `internal/config/config.go`: Template expansion during Load()

### 9. Linting System

New command: `phora lint`

Checks:
- Undefined skill/command references in artifact bodies
- Broken markdown links
- Missing dependencies

**Files:**
- `internal/cli/lint.go` (new command)
- `internal/lint/lint.go` (new package)

### 10. Dependencies

```yaml
---
name: code-test
depends:
  - code-implement
  - code-debug
---
```

**Files:**
- `internal/artifact/artifact.go`: Parse depends field
- `internal/sync/sync.go`: Resolve dependency order

---

## Verification

### Phase 1 Tests

```bash
# Command renames work
phora add --help
phora deploy --help

# Namespace prefixing
phora deploy --dry-run  # verify namespaced artifact names in output

# Canonical references
# Source: "Use $code-test skill and /login command"
# harness.claude.templates.skill = "/${name}"
# Output: "Use /code-test skill and /login command"
phora deploy --dry-run

# Value mappings
# Create test artifact with allowed-tools: [bash, read]
# Configure harness.claude.values.tools = { bash = "Bash", read = "Read" }
phora deploy --dry-run  # verify transformed output
```

### Unit Tests to Add
- `internal/artifact/artifact_test.go`: `TestFullName`
- `internal/reference/reference_test.go`: `TestParseSkills`, `TestParseCommands`, `TestParseAgents`, `TestParseNamespaced`
- `internal/transform/transform_test.go`: `TestApplyValueMappings`, `TestMergedMappings`, `TestTransformReferences`
- `internal/config/config_test.go`: `TestMergeArtifactMappings`

### Run Existing Tests
```bash
go test ./...
```
