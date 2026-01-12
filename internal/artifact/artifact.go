package artifact

import (
	"bytes"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

type Type string

const (
	TypeSkill   Type = "skill"
	TypeCommand Type = "command"
	TypeAgent   Type = "agent"
	TypeUnknown Type = ""
)

type Artifact struct {
	Name        string
	Type        Type
	SourcePath  string
	IsDirectory bool
	Frontmatter map[string]any
	Body        string
	Resources   []string
}

func Parse(data []byte) (*Artifact, error) {
	art := &Artifact{
		Frontmatter: make(map[string]any),
	}

	content := string(data)
	if !strings.HasPrefix(content, "---\n") {
		art.Body = content
		return art, nil
	}

	rest := content[4:]
	endIdx := strings.Index(rest, "\n---\n")
	if endIdx == -1 {
		art.Body = content
		return art, nil
	}

	fmContent := rest[:endIdx]
	if err := yaml.Unmarshal([]byte(fmContent), &art.Frontmatter); err != nil {
		return nil, err
	}

	art.Body = strings.TrimPrefix(rest[endIdx+5:], "\n")
	return art, nil
}

func TypeFromPath(path string) Type {
	parts := strings.Split(filepath.ToSlash(path), "/")
	if len(parts) == 0 {
		return TypeUnknown
	}

	typeDir := parts[0]
	switch typeDir {
	case "skills":
		return TypeSkill
	case "commands":
		return TypeCommand
	case "agents":
		return TypeAgent
	default:
		return TypeUnknown
	}
}

func MainFileName(t Type) string {
	switch t {
	case TypeSkill:
		return "SKILL.md"
	case TypeCommand:
		return "COMMAND.md"
	case TypeAgent:
		return "AGENT.md"
	default:
		return ""
	}
}

func TypeDirName(t Type) string {
	switch t {
	case TypeSkill:
		return "skills"
	case TypeCommand:
		return "commands"
	case TypeAgent:
		return "agents"
	default:
		return ""
	}
}

func Discover(rootDir string, artifactTypes []string) ([]*Artifact, error) {
	var artifacts []*Artifact

	for _, typeDir := range artifactTypes {
		dir := filepath.Join(rootDir, typeDir)
		entries, err := os.ReadDir(dir)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, err
		}

		artType := TypeFromPath(typeDir + "/x")

		for _, entry := range entries {
			name := entry.Name()
			entryPath := filepath.Join(dir, name)

			var art *Artifact
			var err error

			if entry.IsDir() {
				art, err = loadDirectoryArtifact(entryPath, name, artType)
			} else if strings.HasSuffix(name, ".md") {
				art, err = loadFileArtifact(entryPath, strings.TrimSuffix(name, ".md"), artType)
			}

			if err != nil {
				return nil, err
			}
			if art != nil {
				artifacts = append(artifacts, art)
			}
		}
	}

	return artifacts, nil
}

func loadDirectoryArtifact(dir, name string, artType Type) (*Artifact, error) {
	mainFile := MainFileName(artType)
	if mainFile == "" {
		return nil, nil
	}

	mainPath := filepath.Join(dir, mainFile)
	data, err := os.ReadFile(mainPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	art, err := Parse(data)
	if err != nil {
		return nil, err
	}

	art.Name = name
	art.Type = artType
	art.SourcePath = dir
	art.IsDirectory = true

	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}

	for _, entry := range entries {
		entryName := entry.Name()
		if entryName == mainFile {
			continue
		}
		art.Resources = append(art.Resources, entryName)
	}

	return art, nil
}

func loadFileArtifact(path, name string, artType Type) (*Artifact, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	art, err := Parse(data)
	if err != nil {
		return nil, err
	}

	art.Name = name
	art.Type = artType
	art.SourcePath = path
	art.IsDirectory = false

	return art, nil
}

func (a *Artifact) Render() string {
	if len(a.Frontmatter) == 0 {
		return a.Body
	}

	var keys []string
	for k := range a.Frontmatter {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	ordered := make(map[string]any)
	for _, k := range keys {
		ordered[k] = a.Frontmatter[k]
	}

	var buf bytes.Buffer
	enc := yaml.NewEncoder(&buf)
	enc.SetIndent(2)
	enc.Encode(ordered)

	return "---\n" + buf.String() + "---\n\n" + a.Body
}
