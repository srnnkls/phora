package cli

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/srnnkls/phora/internal/config"
)

var configCmd = &cobra.Command{
	Use:   "config",
	Short: "Manage phora configuration",
}

var configListCmd = &cobra.Command{
	Use:   "list",
	Short: "List configuration from all sources",
	RunE:  runConfigList,
}

func init() {
	configCmd.AddCommand(configListCmd)
}

func runConfigList(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	// Load configs separately to show hierarchy
	var global, project *config.Config

	if globalConfigPath != "" {
		if _, err := os.Stat(globalConfigPath); err == nil {
			g, err := config.LoadFile(globalConfigPath)
			if err != nil {
				return fmt.Errorf("load global config: %w", err)
			}
			global = g
		}
	}

	projectConfigPath := cwd + "/phora.toml"
	if _, err := os.Stat(projectConfigPath); err == nil {
		p, err := config.LoadFile(projectConfigPath)
		if err != nil {
			return fmt.Errorf("load project config: %w", err)
		}
		project = p
	}

	// Show global config
	fmt.Println("=== Global Config ===")
	if global != nil {
		fmt.Printf("Path: %s\n", globalConfigPath)
		printConfig(global)
	} else {
		fmt.Println("(not found)")
	}

	// Show project config
	fmt.Println("\n=== Project Config ===")
	if project != nil {
		fmt.Printf("Path: %s\n", projectConfigPath)
		printConfig(project)
	} else {
		fmt.Println("(not found)")
	}

	// Show merged result
	if global != nil || project != nil {
		fmt.Println("\n=== Merged (Effective) ===")
		merged, _ := config.Load(cwd, globalConfigPath)
		printConfig(merged)
	}

	return nil
}

func printConfig(cfg *config.Config) {
	if len(cfg.Sources) > 0 {
		fmt.Println("Sources:")
		for _, src := range cfg.Sources {
			rev := src.ResolveRev()
			if rev == "" {
				rev = "main"
			}
			if src.Path != "" {
				fmt.Printf("  - %s (path: %s, rev: %s)\n", src.Git, src.Path, rev)
			} else {
				fmt.Printf("  - %s (rev: %s)\n", src.Git, rev)
			}
		}
	}

	if len(cfg.Artifacts) > 0 {
		fmt.Printf("Artifacts: %v\n", cfg.Artifacts)
	}

	if len(cfg.Harness) > 0 {
		fmt.Println("Harnesses:")
		for name, h := range cfg.Harness {
			fmt.Printf("  [%s]\n", name)
			if h.Path != "" {
				fmt.Printf("    path: %s\n", h.Path)
			}
			if h.GenerateCommandsFromSkills {
				fmt.Printf("    generate_commands_from_skills: true\n")
			}
			if len(h.Variables) > 0 {
				fmt.Printf("    variables: %v\n", h.Variables)
			}
			if len(h.Keys) > 0 {
				fmt.Printf("    keys: %v\n", h.Keys)
			}
			if len(h.Values) > 0 {
				fmt.Printf("    values: %v\n", h.Values)
			}
			if len(h.ArtifactMappings) > 0 {
				fmt.Printf("    artifact_mappings: %v\n", h.ArtifactMappings)
			}
			if len(h.Include) > 0 {
				fmt.Printf("    include: %v\n", h.Include)
			}
			if len(h.Exclude) > 0 {
				fmt.Printf("    exclude: %v\n", h.Exclude)
			}
		}
	}
}
