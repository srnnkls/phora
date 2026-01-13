package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora/internal/config"
	"github.com/srnnkls/phora/internal/source"
	"github.com/srnnkls/phora/internal/sync"
)

var (
	addHarnesses []string
	addRef       string
	addPath      string
	addLocal     bool
	addForce     bool
)

var addCmd = &cobra.Command{
	Use:   "add <owner/repo>",
	Short: "Add artifacts from a repository",
	Long:  "Clone repo to data directory and sync to harnesses",
	Args:  cobra.ExactArgs(1),
	RunE:  runAdd,
}

func init() {
	addCmd.Flags().StringSliceVar(&addHarnesses, "harness", nil, "Target harnesses (default: all enabled)")
	addCmd.Flags().StringVar(&addRef, "ref", "main", "Branch, tag, or commit")
	addCmd.Flags().StringVar(&addPath, "path", "", "Subdirectory within repo containing artifacts")
	addCmd.Flags().BoolVar(&addLocal, "local", false, "Save source to local phora.toml instead of global config")
	addCmd.Flags().BoolVarP(&addForce, "force", "f", false, "Overwrite existing unmanaged files")
	rootCmd.AddCommand(addCmd)
}

func runAdd(cmd *cobra.Command, args []string) error {
	repoStr := args[0]

	cfg, err := config.Load("", globalConfigPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	repoSrc := source.NewRepo(repoStr, addRef, dataDir, cfg.DefaultArtifacts)

	fmt.Printf("Fetching config from %s...\n", repoSrc.ConfigURL())
	configData, err := repoSrc.FetchConfig()
	if err != nil {
		fmt.Printf("Warning: %v\n", err)
		fmt.Println("Proceeding without remote config...")
	} else {
		remoteConfig, err := config.ParseTOML(configData)
		if err != nil {
			fmt.Printf("Warning: failed to parse remote config: %v\n", err)
		} else if remoteConfig.Manifest != nil {
			fmt.Printf("Found manifest with %d skill(s), %d command(s), %d agent(s)\n",
				len(remoteConfig.Manifest.Skills),
				len(remoteConfig.Manifest.Commands),
				len(remoteConfig.Manifest.Agents))
		}
	}

	localPath := repoSrc.LocalPath()
	if _, err := os.Stat(localPath); os.IsNotExist(err) {
		fmt.Printf("Cloning %s to %s...\n", repoStr, localPath)
	} else {
		fmt.Printf("Updating %s...\n", repoStr)
	}

	if err := repoSrc.Clone(); err != nil {
		return fmt.Errorf("clone/update repo: %w", err)
	}

	sourcePath := localPath
	if addPath != "" {
		sourcePath = filepath.Join(localPath, addPath)
	}

	repoCfg, err := config.Load(sourcePath, globalConfigPath)
	if err != nil {
		return fmt.Errorf("load repo config: %w", err)
	}

	targets := addHarnesses
	if len(targets) == 0 {
		for name := range repoCfg.Harness {
			targets = append(targets, name)
		}
	}

	opts := sync.Options{
		SourcePaths: []string{sourcePath},
		Targets:     targets,
	}

	fmt.Printf("Syncing to %v...\n", targets)

	detection, err := sync.Detect(repoCfg, opts)
	if err != nil {
		return err
	}

	var allConflicts []sync.Conflict
	var freshTargets []string
	for _, det := range detection {
		allConflicts = append(allConflicts, det.Conflicts...)
		if det.LockFile.IsEmpty() {
			freshTargets = append(freshTargets, det.Target)
		}
	}

	if len(freshTargets) > 0 {
		fmt.Printf("First install to: %v (no lockfile found)\n", freshTargets)
	}

	resolutions := make(sync.ResolutionMap)
	if len(allConflicts) > 0 {
		if addForce {
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				resolutions[key] = sync.ResolutionOverwrite
			}
			fmt.Printf("Overwriting %d existing file(s) (--force)\n", len(allConflicts))
		} else {
			fmt.Printf("Skipping %d existing file(s) not managed by phora:\n", len(allConflicts))
			for _, c := range allConflicts {
				key := sync.ConflictKey(c.Target, c.Artifact.Name)
				resolutions[key] = sync.ResolutionSkip
				fmt.Printf("  %s: %s\n", c.Target, c.Artifact.Name)
			}
		}
	}

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

	src := config.Source{
		Repo: repoStr,
		Path: addPath,
		Ref:  addRef,
	}

	var configPath string
	if addLocal {
		cwd, err := os.Getwd()
		if err != nil {
			return fmt.Errorf("get working directory: %w", err)
		}
		configPath = filepath.Join(cwd, "phora.toml")
	} else {
		configPath = globalConfigPath
	}

	if err := config.AddSource(configPath, repoStr, src); err != nil {
		return fmt.Errorf("save source to config: %w", err)
	}
	fmt.Printf("Added source to %s\n", configPath)

	return nil
}
