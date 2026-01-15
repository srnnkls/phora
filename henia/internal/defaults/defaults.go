package defaults

import (
	_ "embed"

	"github.com/pelletier/go-toml/v2"
	"github.com/srnnkls/henia"
)

//go:embed henia.toml
var ConfigTOML string

type Config struct {
	Artifacts []string                   `toml:"artifacts,omitempty"`
	Harness   map[string]henia.Harness   `toml:"harness,omitempty"`
}

func DefaultConfig() (*Config, error) {
	var cfg Config
	if err := toml.Unmarshal([]byte(ConfigTOML), &cfg); err != nil {
		return nil, err
	}
	if cfg.Harness == nil {
		cfg.Harness = make(map[string]henia.Harness)
	}
	return &cfg, nil
}
