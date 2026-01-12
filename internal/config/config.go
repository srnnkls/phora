package config

import (
	"os"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
)

type Config struct {
	DefaultHarnesses []string           `toml:"default_harnesses"`
	DefaultArtifacts []string           `toml:"default_artifacts"`
	Conflict         ConflictConfig     `toml:"conflict"`
	Harness          map[string]Harness `toml:"harness"`
	Manifest         Manifest           `toml:"manifest"`
}

type ConflictConfig struct {
	FileExists        string `toml:"file_exists"`
	DuplicateArtifact string `toml:"duplicate_artifact"`
}

type Harness struct {
	Path                       string            `toml:"path"`
	GenerateCommandsFromSkills bool              `toml:"generate_commands_from_skills"`
	Mappings                   map[string]string `toml:"mappings"`
	Variables                  map[string]string `toml:"variables"`
	Include                    []string          `toml:"include"`
	Exclude                    []string          `toml:"exclude"`
}

type Manifest struct {
	Skills   []string `toml:"skills"`
	Commands []string `toml:"commands"`
	Agents   []string `toml:"agents"`
}

func LoadFile(path string) (*Config, error) {
	var cfg Config
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, err
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
		DefaultHarnesses: global.DefaultHarnesses,
		DefaultArtifacts: global.DefaultArtifacts,
		Conflict:         global.Conflict,
		Harness:          make(map[string]Harness),
		Manifest:         global.Manifest,
	}

	for name, harness := range global.Harness {
		h := Harness{
			Path:                       harness.Path,
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
	if project.Conflict.FileExists != "" {
		result.Conflict.FileExists = project.Conflict.FileExists
	}
	if project.Conflict.DuplicateArtifact != "" {
		result.Conflict.DuplicateArtifact = project.Conflict.DuplicateArtifact
	}

	if len(project.Manifest.Skills) > 0 {
		result.Manifest.Skills = project.Manifest.Skills
	}
	if len(project.Manifest.Commands) > 0 {
		result.Manifest.Commands = project.Manifest.Commands
	}
	if len(project.Manifest.Agents) > 0 {
		result.Manifest.Agents = project.Manifest.Agents
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
