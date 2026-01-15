package cli

import (
	"fmt"

	"github.com/spf13/cobra"
)

var listCmd = &cobra.Command{
	Use:   "list",
	Short: "List configured sources",
	Long:  "List all sources defined in phora.toml",
	RunE:  runList,
}

func init() {
	rootCmd.AddCommand(listCmd)
}

func runList(cmd *cobra.Command, args []string) error {
	cfg, err := loadPhoraConfig()
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	if len(cfg.Sources) == 0 {
		fmt.Println("No sources configured")
		return nil
	}

	fmt.Println("Sources:")
	for name, source := range cfg.Sources {
		if source.Host != "" {
			fmt.Printf("  %s: %s (host: %s, ref: %s)\n", name, source.Repo, source.Host, source.Ref)
		} else {
			fmt.Printf("  %s: %s (ref: %s)\n", name, source.Repo, source.Ref)
		}
	}

	return nil
}
