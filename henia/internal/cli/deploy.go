package cli

import (
	"fmt"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/henia/internal/config"
	"github.com/srnnkls/henia/internal/sync"
)

var deployCmd = &cobra.Command{
	Use:   "deploy",
	Short: "Deploy to harnesses (assumes sources already fetched)",
	Long:  "Deploy artifacts from already-fetched sources to configured harnesses.",
	RunE:  runDeploy,
}

func init() {
	rootCmd.AddCommand(deployCmd)
}

func runDeploy(cmd *cobra.Command, args []string) error {
	cfg, err := config.Load(configPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	var sources []sync.FetchedSource
	for name := range cfg.Sources {
		localPath := filepath.Join(dataDir, name)
		sources = append(sources, sync.FetchedSource{
			Name:      name,
			LocalPath: localPath,
		})
	}

	syncer := sync.NewSyncer(nil, cfg.Harness)
	result, err := syncer.Deploy(sources)
	if err != nil {
		return fmt.Errorf("deploy: %w", err)
	}

	fmt.Fprintf(cmd.OutOrStdout(), "Deployed %d artifact(s)\n", result.Synced)
	if len(result.Errors) > 0 {
		for _, e := range result.Errors {
			fmt.Fprintf(cmd.ErrOrStderr(), "Error: %v\n", e)
		}
	}

	return nil
}
