package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/BurntSushi/toml"
	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/manifest"
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize tropos configuration",
	Long:  "Check repo structure and create tropos.toml with default mappings",
	RunE:  runInit,
}

func runInit(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	artifactTypes := []string{"skills", "commands", "agents"}

	// Generate manifest
	m, err := manifest.Generate(cwd, artifactTypes)
	if err != nil {
		return fmt.Errorf("generate manifest: %w", err)
	}

	manifestDir := filepath.Join(cwd, manifest.Dir)
	if err := os.MkdirAll(manifestDir, 0755); err != nil {
		return fmt.Errorf("create manifest dir: %w", err)
	}

	manifestPath := manifest.FilePath(cwd)
	if err := m.Write(manifestPath); err != nil {
		return fmt.Errorf("write manifest: %w", err)
	}

	fmt.Printf("Created %s/%s\n", manifest.Dir, manifest.FileName)
	fmt.Printf("  Skills:   %d\n", len(m.Skills))
	fmt.Printf("  Commands: %d\n", len(m.Commands))
	fmt.Printf("  Agents:   %d\n", len(m.Agents))

	// Generate config if it doesn't exist
	configPath := filepath.Join(cwd, "tropos.toml")
	if _, err := os.Stat(configPath); err == nil {
		return nil
	}

	cfg := struct {
		DefaultHarnesses []string                  `toml:"default_harnesses"`
		DefaultArtifacts []string                  `toml:"default_artifacts"`
		Harness          map[string]config.Harness `toml:"harness"`
	}{
		DefaultHarnesses: []string{"claude"},
		DefaultArtifacts: artifactTypes,
		Harness: map[string]config.Harness{
			"claude": {
				Path: "~/.claude",
				Variables: map[string]string{
					"model_strong": "opus",
					"model_weak":   "haiku",
				},
			},
			"opencode": {
				Path:                       "~/.opencode",
				GenerateCommandsFromSkills: true,
				Mappings: map[string]string{
					"allowed-tools": "tools",
				},
				Variables: map[string]string{
					"model_strong": "anthropic/claude-sonnet-4-5",
					"model_weak":   "anthropic/claude-haiku-4-5",
				},
			},
		},
	}

	f, err := os.Create(configPath)
	if err != nil {
		return err
	}
	defer f.Close()

	enc := toml.NewEncoder(f)
	if err := enc.Encode(cfg); err != nil {
		return err
	}

	fmt.Println("Created tropos.toml")

	return nil
}
