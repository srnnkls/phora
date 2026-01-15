package henia

import (
	"os"
	"path/filepath"
	"strings"
)

type ArtifactMapping struct {
	Keys   map[string]string            `toml:"keys,omitempty"`
	Values map[string]map[string]string `toml:"values,omitempty"`
}

type ReferenceConfig struct {
	Output string `toml:"output,omitempty"`
}

type Harness struct {
	Path                       string                       `toml:"path,omitempty"`
	Structure                  string                       `toml:"structure,omitempty"` // "flat" or "nested" (default)
	GenerateCommandsFromSkills bool                         `toml:"generate_commands_from_skills,omitempty"`
	Artifacts                  []string                     `toml:"artifacts,omitempty"`
	Keys                       map[string]string            `toml:"keys,omitempty"`
	Values                     map[string]map[string]string `toml:"values,omitempty"`
	ArtifactMappings           map[string]ArtifactMapping   `toml:"artifact_mappings,omitempty"`
	Variables                  map[string]string            `toml:"variables,omitempty"`
	Tools                      map[string]string            `toml:"tools,omitempty"`
	References                 map[string]ReferenceConfig   `toml:"references,omitempty"`
	Include                    []string                     `toml:"include,omitempty"`
	Exclude                    []string                     `toml:"exclude,omitempty"`
}

func ExpandPath(path string) string {
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return path
		}
		return filepath.Join(home, path[2:])
	}
	return path
}
