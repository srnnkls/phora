# Code Artifacts

## Config Structs (config.go)

```go
type Config struct {
    DefaultHarnesses []string           `toml:"default_harnesses,omitempty"`
    DefaultArtifacts []string           `toml:"default_artifacts,omitempty"`
    Sources          map[string]Source  `toml:"sources,omitempty"`
    Harness          map[string]Harness `toml:"harness,omitempty"`
    Tools            []string           `toml:"tools,omitempty"`
    References       map[string]ReferenceConfig `toml:"references,omitempty"`
}

type Source struct {
    Type   string `toml:"type,omitempty"`
    Repo   string `toml:"repo,omitempty"`
    Path   string `toml:"path,omitempty"`
    Ref    string `toml:"ref,omitempty"`
    Global bool   `toml:"global,omitempty"`
}

type Harness struct {
    Path                       string              `toml:"path,omitempty"`
    Structure                  string              `toml:"structure,omitempty"`
    GenerateCommandsFromSkills bool                `toml:"generate_commands_from_skills,omitempty"`
    Variables                  map[string]string   `toml:"variables,omitempty"`
    Include                    []string            `toml:"include,omitempty"`
    Exclude                    []string            `toml:"exclude,omitempty"`
    Tools                      map[string]string            `toml:"tools,omitempty"`
    References                 map[string]ReferenceConfig   `toml:"references,omitempty"`
    Keys                       map[string]string            `toml:"keys,omitempty"`
    Values                     map[string]map[string]string `toml:"values,omitempty"`
    Skills                     *ArtifactMappings            `toml:"skills,omitempty"`
    Commands                   *ArtifactMappings            `toml:"commands,omitempty"`
    Agents                     *ArtifactMappings            `toml:"agents,omitempty"`
}

type ArtifactMappings struct {
    Keys   map[string]string            `toml:"keys,omitempty"`
    Values map[string]map[string]string `toml:"values,omitempty"`
}

type ReferenceConfig struct {
    Sigil  string `toml:"sigil,omitempty"`
    Output string `toml:"output,omitempty"`
}
```

## Artifact Struct (artifact.go)

```go
type Artifact struct {
    Namespace   string
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

## Reference Package (reference/reference.go)

```go
package reference

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

func Parse(content string) []Reference {
    // Implementation: regex match all sigil patterns
    // Return slice of references with positions
}
```

## Transformer Updates (transform.go)

```go
type Transformer struct {
    Variables  map[string]string
    Keys       map[string]string
    Values     map[string]map[string]string
    Tools      map[string]string
    References map[string]ReferenceConfig
}

func (t *Transformer) TransformReferences(content string) (string, error) {
    refs := reference.Parse(content)
    // For each reference:
    // 1. Look up tool mapping if RefTool
    // 2. Apply output template
    // 3. Replace in content
}

func ApplyValueMappings(fm map[string]any, values map[string]map[string]string) map[string]any {
    // For each key in fm:
    // If values[key] exists, map each value in fm[key]
}
```
