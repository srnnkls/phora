package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora/internal/config"
	"github.com/srnnkls/phora/internal/sync"
)

var (
	deploySources     []string
	deployTargets     []string
	deployDryRun      bool
	deployInteractive bool
	deploySkip        bool
)

var deployCmd = &cobra.Command{
	Use:   "deploy",
	Short: "Deploy artifacts to harnesses",
	Long:  "Deploy from sources to target harnesses with transformation",
	RunE:  runDeploy,
}

func init() {
	deployCmd.Flags().StringSliceVar(&deploySources, "source", nil, "Source paths (default: current directory)")
	deployCmd.Flags().StringSliceVar(&deployTargets, "target", nil, "Target harnesses (default: all enabled)")
	deployCmd.Flags().BoolVar(&deployDryRun, "dry-run", false, "Show what would be deployed")
	deployCmd.Flags().BoolVarP(&deployInteractive, "interactive", "i", false, "Prompt for each conflict")
	deployCmd.Flags().BoolVar(&deploySkip, "skip", false, "Skip existing files instead of updating")

	rootCmd.AddCommand(deployCmd)
}

func runDeploy(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	cfg, err := config.Load(cwd, globalConfigPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	sources := deploySources
	if len(sources) == 0 {
		sources = []string{cwd}
	}

	targets := deployTargets
	if len(targets) == 0 {
		targets = cfg.DefaultHarnesses
	}

	opts := sync.Options{
		SourcePaths: sources,
		Targets:     targets,
		DryRun:      deployDryRun,
	}

	if deployDryRun {
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

	if len(allConflicts) > 0 {
		if deploySkip {
			// Skip all conflicts
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				resolutions[key] = sync.ResolutionSkip
			}
			fmt.Printf("Skipping %d conflict(s)\n", len(allConflicts))
		} else if deployInteractive && shouldPromptDeploy() {
			// Interactive mode: prompt for each conflict
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
			configPath := filepath.Join(cwd, "phora.toml")
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
				fmt.Printf("Saved %d exclusion(s) to phora.toml\n", savedExclusions)
			}
		} else {
			// Default: overwrite all conflicts (phora-managed files)
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				resolutions[key] = sync.ResolutionOverwrite
			}
			fmt.Printf("Updating %d existing file(s)\n", len(allConflicts))
		}
	}

	// Phase 3: Apply
	result, err := sync.Apply(cfg, detection, sync.ApplyOptions{
		Options:     opts,
		Resolutions: resolutions,
	})

	// Report results
	if deployDryRun {
		fmt.Printf("Would deploy %d artifact(s)\n", result.Synced)
	} else {
		fmt.Printf("Deployed %d artifact(s)\n", result.Synced)
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

// shouldPromptDeploy determines if we should enter interactive mode
func shouldPromptDeploy() bool {
	if deployInteractive {
		return true
	}
	// Auto-detect TTY: prompt if stdin is terminal
	return isTerminal()
}
