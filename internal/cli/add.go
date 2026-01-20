package cli

import (
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora"
	"github.com/srnnkls/phora/internal/config"
	"github.com/srnnkls/phora/internal/source"
	"github.com/srnnkls/phora/internal/sync"
)

var (
	addHarnesses []string
	addRef       string
	addPath      string
	addTarget    string
	addName      string
	addGlobal    bool
	addForce     bool
)

var addCmd = &cobra.Command{
	Use:   "add <url> [flags]",
	Short: "Add artifacts from a repository",
	Long: `Add artifacts from a repository to your project.

Supported URL formats:
  - owner/repo              GitHub shorthand
  - owner/repo/path         GitHub shorthand with subdirectory
  - https://github.com/owner/repo/tree/ref/path
  - gitlab.com/owner/repo/path

The command parses the URL, clones the repository, and syncs artifacts
to the configured harnesses. The source is saved to phora.toml.

Flags:
  --ref      Branch, tag, or commit (required for refs containing "/")
  --path     Subdirectory within repo containing artifacts
  --target   Override target directory (default: source key name)
  --name     Override source key name (default: derived from repo name)
  --harness  Target harnesses (default: all enabled)
  --global   Save source to global config instead of local phora.toml
  --force    Overwrite existing unmanaged files`,
	Example: `  # Add from GitHub shorthand
  phora add srnnkls/dotfiles/.claude/skills

  # Add with explicit ref (required for feature/xyz branches)
  phora add srnnkls/dotfiles --ref feature/new-skills --path .claude/skills

  # Add from full GitHub URL
  phora add https://github.com/company/shared/tree/v1.0/artifacts/skills

  # Save to global config
  phora add company/shared --global`,
	Args: cobra.ExactArgs(1),
	RunE: runAdd,
}

func init() {
	addCmd.Flags().StringSliceVar(&addHarnesses, "harness", nil, "Target harnesses (default: all enabled)")
	addCmd.Flags().StringVar(&addRef, "ref", "main", "Branch, tag, or commit")
	addCmd.Flags().StringVar(&addPath, "path", "", "Subdirectory within repo containing artifacts")
	addCmd.Flags().StringVar(&addTarget, "target", "", "Override target directory (default: source key name)")
	addCmd.Flags().StringVar(&addName, "name", "", "Override source key name (default: derived from repo name)")
	addCmd.Flags().BoolVarP(&addGlobal, "global", "g", false, "Save source to global config instead of local phora.toml")
	addCmd.Flags().BoolVarP(&addForce, "force", "f", false, "Overwrite existing unmanaged files")
	rootCmd.AddCommand(addCmd)
}

func runAdd(cmd *cobra.Command, args []string) error {
	repoStr := args[0]

	parsed, err := phora.ParseURL(repoStr)
	if err != nil {
		return fmt.Errorf("parse URL: %w", err)
	}

	host, owner, repo := extractHostOwnerRepo(parsed.Git)

	sourceName := addName
	if sourceName == "" {
		sourceName = repo
	}

	var configPath string
	if addGlobal {
		configPath = globalConfigPath
	} else {
		cwd, err := os.Getwd()
		if err != nil {
			return fmt.Errorf("get working directory: %w", err)
		}
		configPath = filepath.Join(cwd, "phora.toml")
	}

	if err := checkSourceNameCollision(configPath, sourceName); err != nil {
		return err
	}

	cfg, err := config.Load("", globalConfigPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	// Look up host config (may be nil for unknown hosts)
	var hostConfig *config.Host
	if cfg.Hosts != nil {
		if hc, ok := cfg.Hosts[host]; ok {
			hostConfig = &hc
		}
	}

	// Use global artifacts config or default
	artifactTypes := cfg.Artifacts
	if len(artifactTypes) == 0 {
		artifactTypes = []string{"skills", "commands", "agents"}
	}
	repoSrc := source.NewRepo(repoStr, addRef, dataDir, hostConfig, artifactTypes)

	configURL := repoSrc.ConfigURL()
	if configURL != "" {
		fmt.Printf("Fetching config from %s...\n", configURL)
		configData, err := repoSrc.FetchConfig()
		if err != nil {
			fmt.Printf("Warning: %v\n", err)
			fmt.Println("Proceeding without remote config...")
		} else {
			remoteConfig, err := config.ParseTOML(configData)
			if err != nil {
				fmt.Printf("Warning: failed to parse remote config: %v\n", err)
			} else if remoteConfig.Manifest != nil {
				fmt.Printf("Found manifest with %d artifact(s)\n",
					len(remoteConfig.Manifest.Artifacts))
			}
		}
	} else {
		fmt.Println("No host configuration for direct config fetch - will clone and discover locally")
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

	configSourcePath := addPath
	if configSourcePath == "" {
		configSourcePath = parsed.Path
	}

	targetDir := addTarget
	if targetDir == "" {
		targetDir = sourceName
	}

	ref := addRef
	if ref == "main" && parsed.Branch != "" {
		ref = parsed.Branch
	}

	src := config.Source{
		Host:  host,
		Owner: owner,
		Repo:  repo,
		Path:  configSourcePath,
		Ref:   ref,
	}

	if err := config.AddSource(configPath, sourceName, src); err != nil {
		return fmt.Errorf("save source to config: %w", err)
	}
	fmt.Printf("Added source '%s' to %s (target: %s)\n", sourceName, configPath, targetDir)

	return nil
}

func checkSourceNameCollision(configPath, sourceName string) error {
	cfg, err := config.LoadFile(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return nil
	}

	if _, exists := cfg.Sources[sourceName]; exists {
		return fmt.Errorf("source '%s' already exists", sourceName)
	}
	return nil
}

func extractHostOwnerRepo(gitURL string) (host, owner, repo string) {
	u, err := url.Parse(gitURL)
	if err != nil {
		return "", "", ""
	}

	host = u.Host
	path := strings.TrimPrefix(u.Path, "/")
	path = strings.TrimSuffix(path, ".git")

	parts := strings.Split(path, "/")
	if len(parts) >= 2 {
		owner = parts[0]
		repo = parts[1]
	}
	return host, owner, repo
}
