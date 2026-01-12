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
	installPath      string
	installLocal     bool
	installForce     bool
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
	installCmd.Flags().StringVar(&installPath, "path", "", "Subdirectory within repo containing artifacts")
	installCmd.Flags().BoolVar(&installLocal, "local", false, "Save source to local tropos.toml instead of global config")
	installCmd.Flags().BoolVarP(&installForce, "force", "f", false, "Overwrite existing unmanaged files")
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
	localPath := repoSrc.LocalPath()
	if _, err := os.Stat(localPath); os.IsNotExist(err) {
		fmt.Printf("Cloning %s to %s...\n", repoStr, localPath)
	} else {
		fmt.Printf("Updating %s...\n", repoStr)
	}

	if err := repoSrc.Clone(); err != nil {
		return fmt.Errorf("clone/update repo: %w", err)
	}

	// Determine source path (repo root or subdirectory)
	sourcePath := localPath
	if installPath != "" {
		sourcePath = filepath.Join(localPath, installPath)
	}

	// Load repo config if present
	repoCfg, err := config.Load(sourcePath, globalConfigPath)
	if err != nil {
		return fmt.Errorf("load repo config: %w", err)
	}

	// Determine targets (all defined harnesses by default)
	targets := installHarnesses
	if len(targets) == 0 {
		for name := range repoCfg.Harness {
			targets = append(targets, name)
		}
	}

	// Sync
	opts := sync.Options{
		SourcePaths: []string{sourcePath},
		Targets:     targets,
	}

	fmt.Printf("Syncing to %v...\n", targets)

	// Phase 1: Detect conflicts
	detection, err := sync.Detect(repoCfg, opts)
	if err != nil {
		return err
	}

	// Collect conflicts and check for fresh installs
	var allConflicts []sync.Conflict
	var freshTargets []string
	for _, det := range detection {
		allConflicts = append(allConflicts, det.Conflicts...)
		if det.LockFile.IsEmpty() {
			freshTargets = append(freshTargets, det.Target)
		}
	}

	// Warn about fresh installs
	if len(freshTargets) > 0 {
		fmt.Printf("First install to: %v (no lockfile found)\n", freshTargets)
	}

	// Phase 2: Handle conflicts
	// Skip unmanaged existing files, overwrite if --force
	resolutions := make(sync.ResolutionMap)
	if len(allConflicts) > 0 {
		if installForce {
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				resolutions[key] = sync.ResolutionOverwrite
			}
			fmt.Printf("Overwriting %d existing file(s) (--force)\n", len(allConflicts))
		} else {
			fmt.Printf("Skipping %d existing file(s) not managed by tropos:\n", len(allConflicts))
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				resolutions[key] = sync.ResolutionSkip
				fmt.Printf("  %s: %s\n", c.Target, c.Artifact.Name)
			}
		}
	}

	// Phase 3: Apply
	result, err := sync.Apply(repoCfg, detection, sync.ApplyOptions{
		Options:     opts,
		Resolutions: resolutions,
	})
	if err != nil {
		return err
	}

	fmt.Printf("Synced %d artifact(s)\n", result.Synced)
	if result.Generated > 0 {
		fmt.Printf("Generated %d command(s)\n", result.Generated)
	}
	if result.Skipped > 0 {
		fmt.Printf("Skipped %d (excluded or already exist)\n", result.Skipped)
	}

	// Save source to config
	src := config.Source{
		Repo: repoStr,
		Path: installPath,
		Ref:  installRef,
	}

	var configPath string
	if installLocal {
		cwd, err := os.Getwd()
		if err != nil {
			return fmt.Errorf("get working directory: %w", err)
		}
		configPath = filepath.Join(cwd, "tropos.toml")
	} else {
		configPath = globalConfigPath
	}

	if err := config.AddSource(configPath, src); err != nil {
		return fmt.Errorf("save source to config: %w", err)
	}
	fmt.Printf("Added source to %s\n", configPath)

	return nil
}
