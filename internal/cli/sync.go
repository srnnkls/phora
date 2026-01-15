package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora"
)

var syncCmd = &cobra.Command{
	Use:   "sync",
	Short: "Fetch all configured sources",
	Long:  "Fetch all sources defined in phora.toml",
	RunE:  runSync,
}

var updateCmd = &cobra.Command{
	Use:   "update",
	Short: "Fetch all configured sources (alias for sync)",
	Long:  "Fetch all sources defined in phora.toml",
	RunE:  runSync,
}

func init() {
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

	client := phora.NewClient(cfg, phora.WithDataDir(dataDir))

	results, err := client.FetchAll()
	if err != nil {
		return fmt.Errorf("fetch sources: %w", err)
	}

	for _, result := range results {
		fmt.Printf("Fetched %s (%s)\n", result.Name, result.Commit[:8])
	}

	fmt.Printf("Synced %d source(s)\n", len(results))
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
