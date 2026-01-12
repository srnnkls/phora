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

	// Determine targets
	targets := installHarnesses
	if len(targets) == 0 {
		targets = repoCfg.DefaultHarnesses
	}

	// Sync
	opts := sync.Options{
		SourcePaths: []string{sourcePath},
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
