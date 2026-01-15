package cli

import (
	"github.com/spf13/cobra"
)

var (
	configPath string
	dataDir    string
)

func Execute() error {
	return rootCmd.Execute()
}

var rootCmd = &cobra.Command{
	Use:   "henia",
	Short: "Deploy artifacts to AI coding assistants",
	Long:  "Henia syncs skills, commands, and agents from phora sources to harness targets.",
}

func init() {
	rootCmd.PersistentFlags().StringVar(&configPath, "config", "", "Config file path")
	rootCmd.PersistentFlags().StringVar(&dataDir, "data-dir", "", "Data directory for sources")
}
