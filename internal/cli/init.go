package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/defaults"
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

	if err := os.WriteFile(configPath, []byte(defaults.ConfigTOML), 0644); err != nil {
		return err
	}

	fmt.Println("Created tropos.toml")

	return nil
}
