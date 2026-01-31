package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora"
)

var (
	syncForce bool
)

func shortSHA(sha string) string {
	if len(sha) > 8 {
		return sha[:8]
	}
	return sha
}

var syncCmd = &cobra.Command{
	Use:   "sync",
	Short: "Sync configured sources to local targets",
	Long: `Sync all sources defined in phora.toml to their local targets.

Detects drift (local modifications) and errors unless --force is used.
Uses locked SHA for fetching (run 'phora update' to re-resolve refs).`,
	RunE: runSync,
}

var updateCmd = &cobra.Command{
	Use:   "update [source]",
	Short: "Re-resolve refs and update lock file",
	Long: `Re-resolve branch/tag refs to latest commit SHA and update lock file.

If a source name is provided, only that source is updated.
Otherwise, all sources are updated.`,
	RunE: runUpdate,
}

func init() {
	syncCmd.Flags().BoolVarP(&syncForce, "force", "f", false, "Overwrite drifted files without prompting")
	rootCmd.AddCommand(syncCmd)
	rootCmd.AddCommand(updateCmd)
}

func runSync(cmd *cobra.Command, args []string) error {
	cfg, err := loadPhoraConfig()
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	if len(cfg.Sources) == 0 {
		fmt.Println("No sources configured")
		return nil
	}

	cwd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("get working directory: %w", err)
	}

	lock, err := phora.LoadLock(cwd)
	if err != nil {
		lock = &phora.Lock{}
	}

	driftBySource := make(map[string][]phora.DriftResult)
	var driftedFiles []phora.DriftResult
	for name, source := range cfg.Sources {
		targetDir := source.Target
		if targetDir == "" {
			targetDir = name
		}
		targetPath := filepath.Join(cwd, targetDir)

		drift, err := phora.DetectDrift(lock, name, targetPath)
		if err != nil {
			return fmt.Errorf("detect drift for %s: %w", name, err)
		}
		if len(drift) > 0 {
			driftBySource[name] = drift
		}
		driftedFiles = append(driftedFiles, drift...)
	}

	if len(driftedFiles) > 0 && !syncForce {
		fmt.Printf("Drift detected in %d file(s):\n", len(driftedFiles))
		for _, d := range driftedFiles {
			status := "modified"
			if d.Status == phora.DriftMissing {
				status = "missing"
			}
			fmt.Printf("  %s (%s)\n", d.Path, status)
		}
		return fmt.Errorf("drift detected; use --force to overwrite")
	}

	if len(driftedFiles) > 0 {
		fmt.Printf("Overwriting %d drifted file(s) (--force)\n", len(driftedFiles))
	}

	client := phora.NewClient(cfg, phora.WithDataDir(dataDir), phora.WithLockDir(cwd))

	var synced int
	for name, source := range cfg.Sources {
		sourceDrift := driftBySource[name]
		hasDrift := len(sourceDrift) > 0

		if !source.NeedsSync(name, lock) && !(syncForce && hasDrift) {
			fmt.Printf("Skipping %s (config unchanged)\n", name)
			continue
		}

		result, err := client.Fetch(name)
		if err != nil {
			return fmt.Errorf("fetch sources: %w", err)
		}
		fmt.Printf("Synced %s (%s)\n", result.Name, shortSHA(result.Commit))
		synced++
	}

	if synced > 0 {
		fmt.Printf("Synced %d source(s)\n", synced)
	}
	return nil
}

func runUpdate(cmd *cobra.Command, args []string) error {
	cfg, err := loadPhoraConfig()
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	if len(cfg.Sources) == 0 {
		fmt.Println("No sources configured")
		return nil
	}

	cwd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("get working directory: %w", err)
	}

	if len(args) > 0 {
		sourceName := args[0]
		result, err := updateSource(cfg, sourceName, cwd)
		if err != nil {
			return fmt.Errorf("update source: %w", err)
		}
		if result.OldSHA != "" && result.OldSHA != result.NewSHA {
			fmt.Printf("Updated %s: %s -> %s\n", result.SourceName, shortSHA(result.OldSHA), shortSHA(result.NewSHA))
		} else {
			fmt.Printf("Updated %s: %s\n", result.SourceName, shortSHA(result.NewSHA))
		}
		return nil
	}

	results, err := updateAllSources(cfg, cwd)
	if err != nil {
		return fmt.Errorf("update sources: %w", err)
	}

	for _, result := range results {
		if result.OldSHA != "" && result.OldSHA != result.NewSHA {
			fmt.Printf("Updated %s: %s -> %s\n", result.SourceName, shortSHA(result.OldSHA), shortSHA(result.NewSHA))
		} else {
			fmt.Printf("Updated %s: %s\n", result.SourceName, shortSHA(result.NewSHA))
		}
	}

	fmt.Printf("Updated %d source(s)\n", len(results))
	return nil
}

func loadPhoraConfig() (*phora.Config, error) {
	cwd, err := os.Getwd()
	if err != nil {
		return nil, err
	}

	localConfig := filepath.Join(cwd, "phora.toml")
	if _, err := os.Stat(localConfig); err == nil {
		return phora.LoadConfig(localConfig)
	}

	if globalConfigPath != "" {
		if _, err := os.Stat(globalConfigPath); err == nil {
			return phora.LoadConfig(globalConfigPath)
		}
	}

	return &phora.Config{
		Sources: make(map[string]phora.Source),
	}, nil
}
