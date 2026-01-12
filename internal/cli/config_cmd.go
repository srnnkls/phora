package cli

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/config"
)

var configCmd = &cobra.Command{
	Use:   "config",
	Short: "Manage tropos configuration",
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

	projectConfigPath := cwd + "/tropos.toml"
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
			if src.Path != "" {
				fmt.Printf("  - %s (path: %s, ref: %s)\n", src.Repo, src.Path, src.Ref)
			} else {
				fmt.Printf("  - %s (ref: %s)\n", src.Repo, src.Ref)
			}
		}
	}

	if len(cfg.DefaultHarnesses) > 0 {
		fmt.Printf("Default harnesses: %v\n", cfg.DefaultHarnesses)
	}

	if len(cfg.DefaultArtifacts) > 0 {
		fmt.Printf("Default artifacts: %v\n", cfg.DefaultArtifacts)
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
			if len(h.Mappings) > 0 {
				fmt.Printf("    mappings: %v\n", h.Mappings)
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
