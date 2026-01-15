package cli

import (
	"fmt"

	"github.com/spf13/cobra"
	"github.com/srnnkls/henia/internal/config"
	"github.com/srnnkls/henia/internal/sync"
	"github.com/srnnkls/phora"
)

var (
	addRef string
)

var addCmd = &cobra.Command{
	Use:   "add <repo>",
	Short: "Add source to config and sync",
	Long:  "Add a repository as a source and immediately sync it.",
	Args:  cobra.ExactArgs(1),
	RunE:  runAdd,
}

func init() {
	addCmd.Flags().StringVar(&addRef, "ref", "main", "Branch, tag, or commit")
	rootCmd.AddCommand(addCmd)
}

func runAdd(cmd *cobra.Command, args []string) error {
	repo := args[0]

	cfg, err := config.Load(configPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	sourceName := repo
	cfg.Sources[sourceName] = phora.Source{
		Repo: repo,
		Ref:  addRef,
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

	fmt.Fprintf(cmd.OutOrStdout(), "Added %s\n", repo)
	fmt.Fprintf(cmd.OutOrStdout(), "Synced %d artifact(s)\n", result.Synced)

	return nil
}
