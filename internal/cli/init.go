package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora/internal/manifest"
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize phora package manifest",
	Long:  "Scan for artifacts and generate .phora/manifest.yaml",
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

	return nil
}
