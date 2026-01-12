package cli

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/BurntSushi/toml"
	"github.com/spf13/cobra"
	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize tropos configuration",
	Long:  "Check repo structure and create tropos.toml with default mappings",
	RunE:  runInit,
}

func runInit(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	configPath := filepath.Join(cwd, "tropos.toml")
	if _, err := os.Stat(configPath); err == nil {
		return fmt.Errorf("tropos.toml already exists")
	}

	// Discover artifacts
	var skills, commands, agents []string

	for _, typeDir := range []string{"skills", "commands", "agents"} {
		dir := filepath.Join(cwd, typeDir)
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}

		artType := artifact.TypeFromPath(typeDir + "/x")

		for _, entry := range entries {
			name := entry.Name()

			if entry.IsDir() {
				mainFile := artifact.MainFileName(artType)
				mainPath := filepath.Join(dir, name, mainFile)
				if _, err := os.Stat(mainPath); err == nil {
					switch typeDir {
					case "skills":
						skills = append(skills, name)
					case "commands":
						commands = append(commands, name)
					case "agents":
						agents = append(agents, name)
					}
				}
			} else if filepath.Ext(name) == ".md" {
				baseName := name[:len(name)-3]
				switch typeDir {
				case "skills":
					skills = append(skills, baseName)
				case "commands":
					commands = append(commands, baseName)
				case "agents":
					agents = append(agents, baseName)
				}
			}
		}
	}

	// Generate config
	cfg := struct {
		Manifest         config.Manifest            `toml:"manifest"`
		DefaultHarnesses []string                   `toml:"default_harnesses"`
		DefaultArtifacts []string                   `toml:"default_artifacts"`
		Conflict         config.ConflictConfig      `toml:"conflict"`
		Harness          map[string]config.Harness  `toml:"harness"`
	}{
		Manifest: config.Manifest{
			Skills:   skills,
			Commands: commands,
			Agents:   agents,
		},
		DefaultHarnesses: []string{"claude"},
		DefaultArtifacts: []string{"skills", "commands", "agents"},
		Conflict: config.ConflictConfig{
			FileExists:        "error",
			DuplicateArtifact: "error",
		},
		Harness: map[string]config.Harness{
			"claude": {
				Path:                       "~/.claude",
				Structure:                  "nested",
				GenerateCommandsFromSkills: false,
				Variables: map[string]string{
					"model_strong": "opus",
					"model_weak":   "haiku",
				},
			},
			"opencode": {
				Path:                       "~/.config/opencode",
				Structure:                  "flat",
				GenerateCommandsFromSkills: true,
				Mappings: map[string]string{
					"allowed_tools": "tools",
				},
				Variables: map[string]string{
					"model_strong": "anthropic/claude-sonnet-4-5",
					"model_weak":   "anthropic/claude-haiku-4-5",
				},
			},
		},
	}

	f, err := os.Create(configPath)
	if err != nil {
		return err
	}
	defer f.Close()

	enc := toml.NewEncoder(f)
	if err := enc.Encode(cfg); err != nil {
		return err
	}

	fmt.Printf("Created tropos.toml\n")
	fmt.Printf("  Skills:   %d\n", len(skills))
	fmt.Printf("  Commands: %d\n", len(commands))
	fmt.Printf("  Agents:   %d\n", len(agents))

	return nil
}
