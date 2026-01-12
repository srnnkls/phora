package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/source"
	"github.com/srnnkls/tropos/internal/sync"
)

var (
	installHarnesses []string
	installRef       string
)

var installCmd = &cobra.Command{
	Use:   "install <owner/repo>",
	Short: "Install artifacts from a repository",
	Long:  "Clone repo to data directory and sync to harnesses",
	Args:  cobra.ExactArgs(1),
	RunE:  runInstall,
}

func init() {
	installCmd.Flags().StringSliceVar(&installHarnesses, "harness", nil, "Target harnesses (default: all enabled)")
	installCmd.Flags().StringVar(&installRef, "ref", "main", "Branch, tag, or commit")
}

func runInstall(cmd *cobra.Command, args []string) error {
	repoStr := args[0]

	// Load global config
	cfg, err := config.Load("", globalConfigPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	// Create repo source
	repoSrc := source.NewRepo(repoStr, installRef, dataDir, cfg.DefaultArtifacts)

	// Fetch manifest first
	fmt.Printf("Fetching manifest from %s...\n", repoSrc.ManifestURL())
	manifestData, err := repoSrc.FetchManifest()
	if err != nil {
		fmt.Printf("Warning: %v\n", err)
		fmt.Println("Proceeding without manifest...")
	} else {
		fmt.Printf("Found manifest (%d bytes)\n", len(manifestData))
	}

	// Clone/update repo
	fmt.Printf("Cloning %s to %s...\n", repoStr, repoSrc.LocalPath())

	// For now, check if already cloned
	localPath := repoSrc.LocalPath()
	if _, err := os.Stat(localPath); os.IsNotExist(err) {
		// Create directory and inform user to clone manually
		if err := os.MkdirAll(filepath.Dir(localPath), 0755); err != nil {
			return err
		}
		fmt.Printf("\nPlease clone manually:\n")
		fmt.Printf("  git clone https://github.com/%s.git %s\n\n", repoStr, localPath)
		return fmt.Errorf("repo not cloned yet")
	}

	// Load repo config if present
	repoCfg, err := config.Load(localPath, globalConfigPath)
	if err != nil {
		return fmt.Errorf("load repo config: %w", err)
	}

	// Determine targets
	targets := installHarnesses
	if len(targets) == 0 {
		targets = repoCfg.DefaultHarnesses
	}

	// Sync
	opts := sync.Options{
		SourcePaths: []string{localPath},
		Targets:     targets,
	}

	fmt.Printf("Syncing to %v...\n", targets)
	result, err := sync.Sync(repoCfg, opts)
	if err != nil {
		return err
	}

	fmt.Printf("Synced %d artifact(s)\n", result.Synced)
	if result.Generated > 0 {
		fmt.Printf("Generated %d command(s)\n", result.Generated)
	}
	if result.Skipped > 0 {
		fmt.Printf("Skipped %d (already exist)\n", result.Skipped)
	}

	return nil
}
