package cli

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/sync"
)

var (
	syncSources []string
	syncTargets []string
	syncDryRun  bool
	syncForce   bool
)

var syncCmd = &cobra.Command{
	Use:   "sync",
	Short: "Sync artifacts to harnesses",
	Long:  "Sync from sources to target harnesses with transformation",
	RunE:  runSync,
}

func init() {
	syncCmd.Flags().StringSliceVar(&syncSources, "source", nil, "Source paths (default: current directory)")
	syncCmd.Flags().StringSliceVar(&syncTargets, "target", nil, "Target harnesses (default: all enabled)")
	syncCmd.Flags().BoolVar(&syncDryRun, "dry-run", false, "Show what would be synced")
	syncCmd.Flags().BoolVar(&syncForce, "force", false, "Overwrite existing files")
}

func runSync(cmd *cobra.Command, args []string) error {
	// Determine project directory
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	// Load config
	cfg, err := config.Load(cwd, globalConfigPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	// Determine sources
	sources := syncSources
	if len(sources) == 0 {
		sources = []string{cwd}
	}

	// Determine targets
	targets := syncTargets
	if len(targets) == 0 {
		targets = cfg.DefaultHarnesses
	}

	opts := sync.Options{
		SourcePaths: sources,
		Targets:     targets,
		DryRun:      syncDryRun,
		Force:       syncForce,
	}

	if syncDryRun {
		fmt.Println("Dry run - no files will be written")
	}

	result, err := sync.Sync(cfg, opts)

	// Report results
	if syncDryRun {
		fmt.Printf("Would sync %d artifact(s)\n", result.Synced)
	} else {
		fmt.Printf("Synced %d artifact(s)\n", result.Synced)
	}

	if result.Generated > 0 {
		fmt.Printf("Generated %d command(s) from user-invocable skills\n", result.Generated)
	}

	if result.Skipped > 0 {
		fmt.Printf("Skipped %d (already exist, use --force to overwrite)\n", result.Skipped)
	}

	if len(result.Conflicts) > 0 {
		fmt.Printf("\nConflicts:\n")
		for _, c := range result.Conflicts {
			fmt.Printf("  - %s: %s\n", c.Artifact.Name, c.Path)
		}
	}

	for _, e := range result.Errors {
		fmt.Fprintf(os.Stderr, "Error: %v\n", e)
	}

	return err
}
