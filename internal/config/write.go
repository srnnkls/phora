package config

import (
	"os"
	"path/filepath"

	"github.com/pelletier/go-toml/v2"
)

// WriteFile writes the config to a TOML file, only if parsed content differs
func WriteFile(path string, cfg *Config) error {
	// Check if existing file parses to the same config
	if existing, err := LoadFile(path); err == nil {
		if Equal(existing, cfg) {
			return nil
		}
	}

	newContent, err := toml.Marshal(cfg)
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	return os.WriteFile(path, newContent, 0644)
}

// Equal returns true if two configs are semantically equal
func Equal(a, b *Config) bool {
	if !slicesEqual(a.DefaultHarnesses, b.DefaultHarnesses) {
		return false
	}
	if !slicesEqual(a.DefaultArtifacts, b.DefaultArtifacts) {
		return false
	}
	if !sourcesEqual(a.Sources, b.Sources) {
		return false
	}
	if !harnessesEqual(a.Harness, b.Harness) {
		return false
	}
	return true
}

func slicesEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func sourcesEqual(a, b map[string]Source) bool {
	if len(a) != len(b) {
		return false
	}
	for k, va := range a {
		vb, ok := b[k]
		if !ok || va != vb {
			return false
		}
	}
	return true
}

func harnessesEqual(a, b map[string]Harness) bool {
	if len(a) != len(b) {
		return false
	}
	for k, va := range a {
		vb, ok := b[k]
		if !ok {
			return false
		}
		if va.Path != vb.Path || va.Structure != vb.Structure || va.GenerateCommandsFromSkills != vb.GenerateCommandsFromSkills {
			return false
		}
		if !mapsEqual(va.Mappings, vb.Mappings) || !mapsEqual(va.Variables, vb.Variables) {
			return false
		}
		if !slicesEqual(va.Include, vb.Include) || !slicesEqual(va.Exclude, vb.Exclude) {
			return false
		}
	}
	return true
}

func mapsEqual(a, b map[string]string) bool {
	if len(a) != len(b) {
		return false
	}
	for k, v := range a {
		if b[k] != v {
			return false
		}
	}
	return true
}

// AddExclusion adds an artifact to a harness's exclude list and saves the config
func AddExclusion(path string, harnessName string, artifactName string) error {
	cfg, err := LoadFile(path)
	if err != nil {
		// If file doesn't exist, create new config
		if os.IsNotExist(err) {
			cfg = &Config{
				Harness: make(map[string]Harness),
			}
		} else {
			return err
		}
	}

	harness, exists := cfg.Harness[harnessName]
	if !exists {
		harness = Harness{
			Mappings:  make(map[string]string),
			Variables: make(map[string]string),
		}
	}

	// Check if already excluded
	for _, exc := range harness.Exclude {
		if exc == artifactName {
			return nil // Already excluded
		}
	}

	harness.Exclude = append(harness.Exclude, artifactName)
	cfg.Harness[harnessName] = harness

	return WriteFile(path, cfg)
}

// RemoveExclusion removes an artifact from a harness's exclude list
func RemoveExclusion(path string, harnessName string, artifactName string) error {
	cfg, err := LoadFile(path)
	if err != nil {
		return err
	}

	harness, exists := cfg.Harness[harnessName]
	if !exists {
		return nil // Nothing to remove
	}

	var newExclude []string
	for _, exc := range harness.Exclude {
		if exc != artifactName {
			newExclude = append(newExclude, exc)
		}
	}

	harness.Exclude = newExclude
	cfg.Harness[harnessName] = harness

	return WriteFile(path, cfg)
}

// AddSource adds a source to the config and saves it
func AddSource(configPath string, name string, src Source) error {
	cfg, err := LoadFile(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			cfg = &Config{
				Sources: make(map[string]Source),
				Harness: make(map[string]Harness),
			}
		} else {
			return err
		}
	}

	if cfg.Sources == nil {
		cfg.Sources = make(map[string]Source)
	}

	cfg.Sources[name] = src
	return WriteFile(configPath, cfg)
}
