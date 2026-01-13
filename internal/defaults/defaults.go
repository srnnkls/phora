package defaults

import (
	_ "embed"

	"github.com/pelletier/go-toml/v2"
	"github.com/srnnkls/tropos/internal/config"
)

//go:embed tropos.toml
var ConfigTOML string

func Config() (*config.Config, error) {
	var cfg config.Config
	if err := toml.Unmarshal([]byte(ConfigTOML), &cfg); err != nil {
		return nil, err
	}
	if cfg.Harness == nil {
		cfg.Harness = make(map[string]config.Harness)
	}
	return &cfg, nil
}
