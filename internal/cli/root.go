package cli

import (
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

var (
	globalConfigPath string
	dataDir          string
)

func Execute() error {
	return rootCmd.Execute()
}

var rootCmd = &cobra.Command{
	Use:   "phora",
	Short: "Sync AI assistant artifacts across harnesses",
	Long:  "Phora syncs skills, commands, and agents across different AI coding assistant harnesses (Claude, OpenCode, Codex).",
}

func init() {
	home, _ := os.UserHomeDir()

	defaultConfig := filepath.Join(home, ".config", "phora", "config.toml")
	defaultData := filepath.Join(home, ".local", "share", "phora", "repos")

	rootCmd.PersistentFlags().StringVar(&globalConfigPath, "config", defaultConfig, "Global config file")
	rootCmd.PersistentFlags().StringVar(&dataDir, "data-dir", defaultData, "Data directory for cloned repos")

	rootCmd.AddCommand(configCmd)
	rootCmd.AddCommand(initCmd)
	rootCmd.AddCommand(installCmd)
	rootCmd.AddCommand(syncCmd)
}
