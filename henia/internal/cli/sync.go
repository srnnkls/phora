package cli

import (
	"fmt"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/henia/internal/config"
	"github.com/srnnkls/henia/internal/sync"
	"github.com/srnnkls/phora"
)

var syncCmd = &cobra.Command{
	Use:   "sync",
	Short: "Fetch sources and deploy to harnesses",
	Long:  "Fetch all configured sources via phora and deploy artifacts to harnesses.",
	RunE:  runSync,
}

var updateCmd = &cobra.Command{
	Use:   "update",
	Short: "Fetch sources and deploy (alias for sync)",
	Long:  "Alias for 'henia sync'. Fetches sources and deploys to harnesses.",
	RunE:  runSync,
}

func init() {
	rootCmd.AddCommand(syncCmd)
	rootCmd.AddCommand(updateCmd)
}

func runSync(cmd *cobra.Command, args []string) error {
	cfg, err := config.Load(configPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	phoraCfg := &phora.Config{
		Sources: cfg.Sources,
	}
	client := phora.NewClient(phoraCfg, phora.WithDataDir(dataDir))

	syncer := sync.NewSyncer(client, cfg.Harness)
	result, err := syncer.Sync()
	if err != nil {
		return fmt.Errorf("sync: %w", err)
	}

	fmt.Fprintf(cmd.OutOrStdout(), "Synced %d artifact(s)\n", result.Synced)
	if len(result.Errors) > 0 {
		for _, e := range result.Errors {
			fmt.Fprintf(cmd.ErrOrStderr(), "Error: %v\n", e)
		}
	}

	return nil
}

func runSyncDeployOnly(cmd *cobra.Command, args []string) error {
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
