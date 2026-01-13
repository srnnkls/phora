package config

import (
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

type Config struct {
	Artifacts []string           `toml:"artifacts,omitempty"`
	Hosts     map[string]Host    `toml:"hosts,omitempty"`
	Manifest  *Manifest          `toml:"manifest,omitempty"`
	Sources   map[string]Source  `toml:"sources,omitempty"`
	Harness   map[string]Harness `toml:"harness,omitempty"`
}

type Host struct {
	GitURL string `toml:"git_url,omitempty"`
	RawURL string `toml:"raw_url,omitempty"`
}

type Manifest struct {
	Skills   []string `toml:"skills,omitempty"`
	Commands []string `toml:"commands,omitempty"`
	Agents   []string `toml:"agents,omitempty"`
	Version  string   `toml:"version,omitempty"`
}

type Source struct {
	Type   string `toml:"type,omitempty"`
	Host   string `toml:"host,omitempty"`
	Owner  string `toml:"owner,omitempty"`
	Repo   string `toml:"repo,omitempty"`
	Path   string `toml:"path,omitempty"`
	Ref    string `toml:"ref,omitempty"`
	Global bool   `toml:"global,omitempty"`
}

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

func ParseTOML(data []byte) (*Config, error) {
	var cfg Config
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}
	if cfg.Hosts == nil {
		cfg.Hosts = make(map[string]Host)
	}
	if cfg.Sources == nil {
		cfg.Sources = make(map[string]Source)
	}
	if cfg.Harness == nil {
		cfg.Harness = make(map[string]Harness)
	}
	return &cfg, nil
}

func LoadFile(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return ParseTOML(data)
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
	projectConfigPath := filepath.Join(projectDir, "phora.toml")
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
		Sources:   make(map[string]Source),
		Artifacts: global.Artifacts,
		Harness:   make(map[string]Harness),
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
			Artifacts:                  append([]string{}, harness.Artifacts...),
			Keys:                       make(map[string]string),
			Values:                     make(map[string]map[string]string),
			ArtifactMappings:           make(map[string]ArtifactMapping),
			Variables:                  make(map[string]string),
			Tools:                      make(map[string]string),
			References:                 make(map[string]ReferenceConfig),
			Include:                    append([]string{}, harness.Include...),
			Exclude:                    append([]string{}, harness.Exclude...),
		}
		for k, v := range harness.Keys {
			h.Keys[k] = v
		}
		for k, v := range harness.Values {
			h.Values[k] = copyStringMap(v)
		}
		for k, v := range harness.ArtifactMappings {
			h.ArtifactMappings[k] = copyArtifactMapping(v)
		}
		for k, v := range harness.Variables {
			h.Variables[k] = v
		}
		for k, v := range harness.Tools {
			h.Tools[k] = v
		}
		for k, v := range harness.References {
			h.References[k] = v
		}
		result.Harness[name] = h
	}

	if len(project.Artifacts) > 0 {
		result.Artifacts = project.Artifacts
	}

	// Merge project sources (override global)
	for name, src := range project.Sources {
		result.Sources[name] = src
	}

	for name, harness := range project.Harness {
		h, exists := result.Harness[name]
		if !exists {
			h = Harness{
				Keys:             make(map[string]string),
				Values:           make(map[string]map[string]string),
				ArtifactMappings: make(map[string]ArtifactMapping),
				Variables:        make(map[string]string),
				Tools:            make(map[string]string),
				References:       make(map[string]ReferenceConfig),
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
		if len(harness.Artifacts) > 0 {
			h.Artifacts = harness.Artifacts
		}
		for k, v := range harness.Keys {
			h.Keys[k] = v
		}
		for k, v := range harness.Values {
			if h.Values[k] == nil {
				h.Values[k] = make(map[string]string)
			}
			for vk, vv := range v {
				h.Values[k][vk] = vv
			}
		}
		for k, v := range harness.ArtifactMappings {
			h.ArtifactMappings[k] = mergeArtifactMapping(h.ArtifactMappings[k], v)
		}
		for k, v := range harness.Variables {
			h.Variables[k] = v
		}
		if h.Tools == nil {
			h.Tools = make(map[string]string)
		}
		for k, v := range harness.Tools {
			h.Tools[k] = v
		}
		if h.References == nil {
			h.References = make(map[string]ReferenceConfig)
		}
		for k, v := range harness.References {
			h.References[k] = v
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

func copyStringMap(m map[string]string) map[string]string {
	result := make(map[string]string)
	for k, v := range m {
		result[k] = v
	}
	return result
}

func copyArtifactMapping(am ArtifactMapping) ArtifactMapping {
	result := ArtifactMapping{
		Keys:   make(map[string]string),
		Values: make(map[string]map[string]string),
	}
	for k, v := range am.Keys {
		result.Keys[k] = v
	}
	for k, v := range am.Values {
		result.Values[k] = copyStringMap(v)
	}
	return result
}

func mergeArtifactMapping(base, overlay ArtifactMapping) ArtifactMapping {
	result := ArtifactMapping{
		Keys:   make(map[string]string),
		Values: make(map[string]map[string]string),
	}
	for k, v := range base.Keys {
		result.Keys[k] = v
	}
	for k, v := range overlay.Keys {
		result.Keys[k] = v
	}
	for k, v := range base.Values {
		result.Values[k] = copyStringMap(v)
	}
	for k, v := range overlay.Values {
		if result.Values[k] == nil {
			result.Values[k] = make(map[string]string)
		}
		for vk, vv := range v {
			result.Values[k][vk] = vv
		}
	}
	return result
}

