package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora/internal/config"
	"github.com/srnnkls/phora/internal/manifest"
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize phora config with manifest",
	Long:  "Scan for artifacts and add [manifest] section to phora.toml",
	PreRunE: func(cmd *cobra.Command, args []string) error {
		fmt.Fprintln(cmd.ErrOrStderr(), "Warning: 'phora init' is deprecated. Use 'henia init' instead.")
		return nil
	},
	RunE: runInit,
}

func runInit(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	configPath := filepath.Join(cwd, "phora.toml")
	artifactTypes := []string{"skills", "commands", "agents"}

	// Load existing config or create new one
	var cfg *config.Config
	if _, err := os.Stat(configPath); err == nil {
		cfg, err = config.LoadFile(configPath)
		if err != nil {
			return fmt.Errorf("load existing config: %w", err)
		}
		fmt.Println("Updating existing phora.toml...")
	} else {
		cfg = &config.Config{
			Artifacts: artifactTypes,
			Sources:   make(map[string]config.Source),
			Harness:   make(map[string]config.Harness),
		}
		fmt.Println("Creating new phora.toml...")
	}

	// Generate manifest from discovered artifacts
	m, err := manifest.Generate(cwd, artifactTypes)
	if err != nil {
		return fmt.Errorf("generate manifest: %w", err)
	}

	// Update config with manifest - combine all artifact names
	var allArtifacts []string
	allArtifacts = append(allArtifacts, m.Skills...)
	allArtifacts = append(allArtifacts, m.Commands...)
	allArtifacts = append(allArtifacts, m.Agents...)
	cfg.Manifest = &config.Manifest{
		Artifacts: allArtifacts,
	}

	// Write updated config
	if err := config.WriteFile(configPath, cfg); err != nil {
		return fmt.Errorf("write config: %w", err)
	}

	fmt.Printf("Updated phora.toml\n")
	fmt.Printf("  [manifest]\n")
	fmt.Printf("  Artifacts: %d\n", len(allArtifacts))

	return nil
}
