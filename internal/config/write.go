package config

import (
	"bytes"
	"os"
	"path/filepath"

	"github.com/BurntSushi/toml"
)

// WriteFile writes the config to a TOML file
func WriteFile(path string, cfg *Config) error {
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	var buf bytes.Buffer
	enc := toml.NewEncoder(&buf)
	if err := enc.Encode(cfg); err != nil {
		return err
	}
	return os.WriteFile(path, buf.Bytes(), 0644)
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
func AddSource(configPath string, src Source) error {
	cfg, err := LoadFile(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			cfg = &Config{
				Harness: make(map[string]Harness),
			}
		} else {
			return err
		}
	}

	if containsSource(cfg.Sources, src) {
		return nil
	}

	cfg.Sources = append(cfg.Sources, src)
	return WriteFile(configPath, cfg)
}
