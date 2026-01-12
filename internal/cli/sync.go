package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/sync"
)

var (
	syncSources     []string
	syncTargets     []string
	syncDryRun      bool
	syncForce       bool
	syncInteractive bool
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
	syncCmd.Flags().BoolVarP(&syncInteractive, "interactive", "i", false, "Prompt for each conflict")
}

func runSync(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	cfg, err := config.Load(cwd, globalConfigPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	sources := syncSources
	if len(sources) == 0 {
		sources = []string{cwd}
	}

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

	// Phase 1: Detect
	detection, err := sync.Detect(cfg, opts)
	if err != nil {
		return err
	}

	// Collect all conflicts
	var allConflicts []sync.Conflict
	for _, det := range detection {
		allConflicts = append(allConflicts, det.Conflicts...)
	}

	// Phase 2: Resolve conflicts
	resolutions := make(sync.ResolutionMap)

	if len(allConflicts) > 0 && !syncForce {
		if shouldPrompt() {
			prompter := NewPrompter()
			promptResult, err := prompter.ResolveConflicts(allConflicts)
			if err != nil {
				return err
			}
			if promptResult.Aborted {
				fmt.Println("Aborted.")
				return nil
			}
			resolutions = promptResult.Resolutions

			// Save skip choices to config as exclusions
			configPath := filepath.Join(cwd, "tropos.toml")
			var savedExclusions int
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				if res, ok := resolutions[key]; ok && res == sync.ResolutionSkip {
					if err := config.AddExclusion(configPath, c.Target, c.Artifact.Name); err != nil {
						fmt.Fprintf(os.Stderr, "Warning: could not save exclusion: %v\n", err)
					} else {
						savedExclusions++
					}
				}
			}
			if savedExclusions > 0 {
				fmt.Printf("Saved %d exclusion(s) to tropos.toml\n", savedExclusions)
			}
		} else {
			// Non-interactive with conflicts: report and exit
			fmt.Printf("\n%d conflict(s) detected - use --force to overwrite or -i for interactive:\n", len(allConflicts))
			for _, c := range allConflicts {
				fmt.Printf("  - %s (%s): %s\n", c.Artifact.Name, c.Target, c.Path)
			}
			return fmt.Errorf("conflicts detected")
		}
	}

	// Phase 3: Apply
	result, err := sync.Apply(cfg, detection, sync.ApplyOptions{
		Options:     opts,
		Resolutions: resolutions,
	})

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
		fmt.Printf("Skipped %d (excluded or already exist)\n", result.Skipped)
	}

	for _, e := range result.Errors {
		fmt.Fprintf(os.Stderr, "Error: %v\n", e)
	}

	return err
}

// shouldPrompt determines if we should enter interactive mode
func shouldPrompt() bool {
	if syncInteractive {
		return true
	}
	// Auto-detect TTY: prompt if stdin is terminal
	return isTerminal()
}
