package config

import (
	"os"

	toml "github.com/pelletier/go-toml/v2"
	"github.com/srnnkls/henia"
	"github.com/srnnkls/phora"
)

type Config struct {
	Artifacts []string                 `toml:"artifacts,omitempty"`
	Sources   map[string]phora.Source  `toml:"sources,omitempty"`
	Harness   map[string]henia.Harness `toml:"harness,omitempty"`
}

func Load(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var cfg Config
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}

	if cfg.Sources == nil {
		cfg.Sources = make(map[string]phora.Source)
	}
	if cfg.Harness == nil {
		cfg.Harness = make(map[string]henia.Harness)
	}

	return &cfg, nil
}
