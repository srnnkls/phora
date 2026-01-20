package phora

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	toml "github.com/pelletier/go-toml/v2"
)

// Config represents the phora configuration.
type Config struct {
	Version   int                `toml:"version"`
	Hosts     map[string]Host    `toml:"hosts"`
	Sources   map[string]Source  `toml:"sources"`
	Manifest  *Manifest          `toml:"manifest,omitempty"`
	Harness   map[string]Harness `toml:"harness,omitempty"`
	Artifacts []string           `toml:"artifacts,omitempty"`
}

// Manifest represents export configuration for this repo as a source.
type Manifest struct {
	Artifacts []string `toml:"artifacts,omitempty"`
}

// ContainsArtifact checks if an artifact is in the manifest.
func (m *Manifest) ContainsArtifact(name string) bool {
	if len(m.Artifacts) == 0 {
		return false
	}
	for _, a := range m.Artifacts {
		if a == name {
			return true
		}
	}
	return false
}

// ValidatePath checks if a path is allowed by the manifest.
// Returns nil if path is within an allowed artifact directory.
// Rejects path traversal attempts (e.g., "skills/../commands").
// If artifacts is empty, returns error (deny-by-default).
func (m *Manifest) ValidatePath(path string) error {
	if len(m.Artifacts) == 0 {
		return fmt.Errorf("path '%s' not in source artifacts (manifest is empty)", path)
	}

	cleaned := filepath.Clean(path)
	if cleaned != path || strings.HasPrefix(cleaned, "..") {
		return fmt.Errorf("path '%s' contains invalid traversal", path)
	}

	for _, a := range m.Artifacts {
		if a == cleaned || strings.HasPrefix(cleaned, a+"/") {
			return nil
		}
	}
	return fmt.Errorf("path '%s' not in source artifacts", path)
}

// FilterDirectories filters directories to only those in artifacts.
// If artifacts is empty, returns empty slice (deny-by-default).
func (m *Manifest) FilterDirectories(dirs []string) []string {
	if len(m.Artifacts) == 0 {
		return []string{}
	}
	artifactSet := make(map[string]bool)
	for _, a := range m.Artifacts {
		artifactSet[a] = true
	}
	var result []string
	for _, dir := range dirs {
		if artifactSet[dir] {
			result = append(result, dir)
		}
	}
	return result
}

// Harness represents a harness configuration.
type Harness struct {
	Path                       string                       `toml:"path,omitempty"`
	Structure                  string                       `toml:"structure,omitempty"`
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

// ArtifactMapping represents per-artifact key/value mappings.
type ArtifactMapping struct {
	Keys   map[string]string            `toml:"keys,omitempty"`
	Values map[string]map[string]string `toml:"values,omitempty"`
}

// ReferenceConfig represents reference output configuration.
type ReferenceConfig struct {
	Output string `toml:"output,omitempty"`
}

// Host represents a git host configuration.
type Host struct {
	GitURL string `toml:"git_url"`
}

// Source represents a source repository configuration.
type Source struct {
	Repo           string            `toml:"repo"`
	Ref            string            `toml:"ref"`
	Path           string            `toml:"path,omitempty"`
	IgnoreManifest bool              `toml:"ignore_manifest"`
	Paths          map[string]string `toml:"paths"`
	Host           string            `toml:"host"`
	Git            string            `toml:"git"`
	Branch         string            `toml:"branch,omitempty"`
	Tag            string            `toml:"tag,omitempty"`
	Rev            string            `toml:"rev,omitempty"`
	Target         string            `toml:"target,omitempty"`
	Include        []string          `toml:"include,omitempty"`
	Exclude        []string          `toml:"exclude,omitempty"`
}

// LoadConfig loads a phora configuration from a TOML file.
func LoadConfig(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var raw map[string]interface{}
	if err := toml.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	if _, ok := raw["version"]; !ok {
		return nil, fmt.Errorf("missing required field: version")
	}

	var cfg Config
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}

	if cfg.Version != 1 {
		return nil, fmt.Errorf("unsupported config version: %d", cfg.Version)
	}

	for name, source := range cfg.Sources {
		// Only default Ref for legacy Repo field when no new-style refs specified
		if source.Repo != "" && source.Ref == "" &&
			source.Branch == "" && source.Tag == "" && source.Rev == "" {
			source.Ref = "main"
		}
		if source.Target == "" {
			source.Target = name
		}
		cfg.Sources[name] = source
	}

	return &cfg, nil
}

// Validate validates the configuration.
func (c *Config) Validate() error {
	for name, source := range c.Sources {
		// Git and Repo are mutually exclusive
		if source.Git != "" && source.Repo != "" {
			return fmt.Errorf("source %q: git and repo are mutually exclusive", name)
		}

		// Must have either Git or Repo
		if source.Git == "" && source.Repo == "" {
			return fmt.Errorf("source %q: must specify either git or repo", name)
		}

		// Validate Repo format if using legacy field
		if source.Repo != "" {
			_, _, err := source.ParseRepo()
			if err != nil {
				return fmt.Errorf("source %q: %w", name, err)
			}
		}

		if source.Host != "" {
			if c.Hosts == nil {
				return fmt.Errorf("source %q references unknown host %q", name, source.Host)
			}
			if _, ok := c.Hosts[source.Host]; !ok {
				return fmt.Errorf("source %q references unknown host %q", name, source.Host)
			}
		}

		var refFields []string
		if source.Branch != "" {
			refFields = append(refFields, "branch")
		}
		if source.Tag != "" {
			refFields = append(refFields, "tag")
		}
		if source.Rev != "" {
			refFields = append(refFields, "rev")
		}
		if len(refFields) > 1 {
			return fmt.Errorf("source %q: only one of branch, tag, or rev may be specified, got: %s", name, strings.Join(refFields, ", "))
		}
	}
	return nil
}

// Digest computes the SHA256 hash of the source config for lazy sync comparison.
// Includes: git, branch, tag, rev, path, include, exclude.
// Excludes: target (target changes don't require re-fetch).
func (s *Source) Digest() string {
	type digestData struct {
		Git     string   `json:"git"`
		Branch  string   `json:"branch,omitempty"`
		Tag     string   `json:"tag,omitempty"`
		Rev     string   `json:"rev,omitempty"`
		Path    string   `json:"path,omitempty"`
		Include []string `json:"include,omitempty"`
		Exclude []string `json:"exclude,omitempty"`
	}

	var include []string
	if len(s.Include) > 0 {
		include = s.Include
	}

	var exclude []string
	if len(s.Exclude) > 0 {
		exclude = s.Exclude
	}

	data := digestData{
		Git:     s.Git,
		Branch:  s.Branch,
		Tag:     s.Tag,
		Rev:     s.Rev,
		Path:    s.Path,
		Include: include,
		Exclude: exclude,
	}

	jsonBytes, _ := json.Marshal(data)
	hash := sha256.Sum256(jsonBytes)
	return hex.EncodeToString(hash[:])
}

// NeedsSync determines if a source needs to be synced by comparing digests.
// Returns true if lock is nil, source name not found in lock, or digest differs.
func (s *Source) NeedsSync(name string, lock *Lock) bool {
	if lock == nil || len(lock.Sources) == 0 {
		return true
	}

	for _, locked := range lock.Sources {
		if locked.Name == name {
			return s.Digest() != locked.Digest
		}
	}

	return true
}

// ParseRepo splits the Repo field into owner and repo parts.
func (s *Source) ParseRepo() (owner, repo string, err error) {
	if s.Repo == "" {
		return "", "", fmt.Errorf("repo is empty")
	}

	if strings.HasPrefix(s.Repo, "/") || strings.HasSuffix(s.Repo, "/") {
		return "", "", fmt.Errorf("repo %q has invalid format", s.Repo)
	}

	parts := strings.Split(s.Repo, "/")
	if len(parts) != 2 {
		return "", "", fmt.Errorf("repo %q must be in format owner/repo", s.Repo)
	}

	if parts[0] == "" || parts[1] == "" {
		return "", "", fmt.Errorf("repo %q has empty owner or repo", s.Repo)
	}

	return parts[0], parts[1], nil
}

// loadConfigFile loads a config from file without version validation.
func loadConfigFile(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var cfg Config
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}
	return &cfg, nil
}

// Load loads config from project dir and global config path.
func Load(projectDir, globalConfigPath string) (*Config, error) {
	var global *Config
	if globalConfigPath != "" {
		if _, err := os.Stat(globalConfigPath); err == nil {
			g, err := loadConfigFile(globalConfigPath)
			if err != nil {
				return nil, err
			}
			global = g
		}
	}

	var project *Config
	projectConfigPath := projectDir + "/phora.toml"
	if _, err := os.Stat(projectConfigPath); err == nil {
		p, err := loadConfigFile(projectConfigPath)
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

// Merge merges global and project configs.
func Merge(global, project *Config) *Config {
	result := &Config{
		Sources:   make(map[string]Source),
		Artifacts: global.Artifacts,
		Harness:   make(map[string]Harness),
	}

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
