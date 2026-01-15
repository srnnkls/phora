package phora

import (
	"fmt"
	"os"
	"strings"

	toml "github.com/pelletier/go-toml/v2"
)

// Config represents the phora configuration.
type Config struct {
	Hosts   map[string]Host   `toml:"hosts"`
	Sources map[string]Source `toml:"sources"`
}

// Host represents a git host configuration.
type Host struct {
	GitURL string `toml:"git_url"`
}

// Source represents a source repository configuration.
type Source struct {
	Repo           string            `toml:"repo"`
	Ref            string            `toml:"ref"`
	Path           string            `toml:"path"`
	IgnoreManifest bool              `toml:"ignore_manifest"`
	Paths          map[string]string `toml:"paths"`
	Host           string            `toml:"host"`
}

// LoadConfig loads a phora configuration from a TOML file.
func LoadConfig(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var cfg Config
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}

	for name, source := range cfg.Sources {
		if source.Ref == "" {
			source.Ref = "main"
			cfg.Sources[name] = source
		}
	}

	return &cfg, nil
}

// Validate validates the configuration.
func (c *Config) Validate() error {
	for name, source := range c.Sources {
		_, _, err := source.ParseRepo()
		if err != nil {
			return fmt.Errorf("source %q: %w", name, err)
		}

		if source.Host != "" {
			if c.Hosts == nil {
				return fmt.Errorf("source %q references unknown host %q", name, source.Host)
			}
			if _, ok := c.Hosts[source.Host]; !ok {
				return fmt.Errorf("source %q references unknown host %q", name, source.Host)
			}
		}
	}
	return nil
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
