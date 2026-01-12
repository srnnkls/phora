package config

import (
	"os"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
)

type Config struct {
	Sources          map[string]Source  `toml:"sources,omitempty"`
	DefaultHarnesses []string           `toml:"default_harnesses,omitempty"`
	DefaultArtifacts []string           `toml:"default_artifacts,omitempty"`
	Harness          map[string]Harness `toml:"harness,omitempty"`
}

type Source struct {
	Repo string `toml:"repo"`
	Path string `toml:"path,omitempty"`
	Ref  string `toml:"ref,omitempty"`
}

type Harness struct {
	Path                       string            `toml:"path,omitempty"`
	Structure                  string            `toml:"structure,omitempty"` // "flat" or "nested" (default)
	GenerateCommandsFromSkills bool              `toml:"generate_commands_from_skills,omitempty"`
	Mappings                   map[string]string `toml:"mappings,omitempty"`
	Variables                  map[string]string `toml:"variables,omitempty"`
	Include                    []string          `toml:"include,omitempty"`
	Exclude                    []string          `toml:"exclude,omitempty"`
}

func LoadFile(path string) (*Config, error) {
	var cfg Config
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, err
	}
	if cfg.Sources == nil {
		cfg.Sources = make(map[string]Source)
	}
	if cfg.Harness == nil {
		cfg.Harness = make(map[string]Harness)
	}
	return &cfg, nil
}

func Load(projectDir, globalConfigPath string) (*Config, error) {
	var global *Config
	if globalConfigPath != "" {
		if _, err := os.Stat(globalConfigPath); err == nil {
			g, err := LoadFile(globalConfigPath)
			if err != nil {
				return nil, err
			}
			global = g
		}
	}

	var project *Config
	projectConfigPath := filepath.Join(projectDir, "tropos.toml")
	if _, err := os.Stat(projectConfigPath); err == nil {
		p, err := LoadFile(projectConfigPath)
		if err != nil {
			return nil, err
		}
		project = p
	}

	if global == nil && project == nil {
		return &Config{
			Sources: make(map[string]Source),
			Harness: make(map[string]Harness),
		}, nil
	}

	if global == nil {
		return project, nil
	}
	if project == nil {
		return global, nil
	}

	return Merge(global, project), nil
}

func Merge(global, project *Config) *Config {
	result := &Config{
		Sources:          make(map[string]Source),
		DefaultHarnesses: global.DefaultHarnesses,
		DefaultArtifacts: global.DefaultArtifacts,
		Harness:          make(map[string]Harness),
	}

	// Copy global sources
	for name, src := range global.Sources {
		result.Sources[name] = src
	}

	for name, harness := range global.Harness {
		h := Harness{
			Path:                       harness.Path,
			Structure:                  harness.Structure,
			GenerateCommandsFromSkills: harness.GenerateCommandsFromSkills,
			Mappings:                   make(map[string]string),
			Variables:                  make(map[string]string),
			Include:                    append([]string{}, harness.Include...),
			Exclude:                    append([]string{}, harness.Exclude...),
		}
		for k, v := range harness.Mappings {
			h.Mappings[k] = v
		}
		for k, v := range harness.Variables {
			h.Variables[k] = v
		}
		result.Harness[name] = h
	}

	if len(project.DefaultHarnesses) > 0 {
		result.DefaultHarnesses = project.DefaultHarnesses
	}
	if len(project.DefaultArtifacts) > 0 {
		result.DefaultArtifacts = project.DefaultArtifacts
	}

	// Merge project sources (override global)
	for name, src := range project.Sources {
		result.Sources[name] = src
	}

	for name, harness := range project.Harness {
		h, exists := result.Harness[name]
		if !exists {
			h = Harness{
				Mappings:  make(map[string]string),
				Variables: make(map[string]string),
			}
		}
		if harness.Path != "" {
			h.Path = harness.Path
		}
		if harness.Structure != "" {
			h.Structure = harness.Structure
		}
		if harness.GenerateCommandsFromSkills {
			h.GenerateCommandsFromSkills = true
		}
		for k, v := range harness.Mappings {
			h.Mappings[k] = v
		}
		for k, v := range harness.Variables {
			h.Variables[k] = v
		}
		h.Include = appendUnique(h.Include, harness.Include...)
		h.Exclude = appendUnique(h.Exclude, harness.Exclude...)
		result.Harness[name] = h
	}

	return result
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

func appendUnique(slice []string, items ...string) []string {
	seen := make(map[string]bool)
	for _, s := range slice {
		seen[s] = true
	}
	for _, item := range items {
		if !seen[item] {
			slice = append(slice, item)
			seen[item] = true
		}
	}
	return slice
}

