package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadConfig(t *testing.T) {
	tmpDir := t.TempDir()

	configContent := `
default_harnesses = ["claude", "opencode"]
default_artifacts = ["skills", "commands", "agents"]

[conflict]
file_exists = "error"
duplicate_artifact = "error"

[harness.claude]
path = "~/.claude"
structure = "nested"
generate_commands_from_skills = false

[harness.claude.mappings]

[harness.claude.variables]
model_strong = "opus"
model_weak = "haiku"

[harness.opencode]
path = "~/.config/opencode"
structure = "flat"
generate_commands_from_skills = true

[harness.opencode.mappings]
allowed_tools = "tools"

[harness.opencode.variables]
model_strong = "anthropic/claude-sonnet-4-5"
model_weak = "anthropic/claude-haiku-4-5"
`

	configPath := filepath.Join(tmpDir, "config.toml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	if len(cfg.DefaultHarnesses) != 2 {
		t.Errorf("DefaultHarnesses = %v, want 2 items", cfg.DefaultHarnesses)
	}

	if cfg.Conflict.FileExists != "error" {
		t.Errorf("Conflict.FileExists = %q, want %q", cfg.Conflict.FileExists, "error")
	}

	claude, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("missing claude harness")
	}
	if claude.Structure != "nested" {
		t.Errorf("claude.Structure = %q, want %q", claude.Structure, "nested")
	}
	if claude.Variables["model_strong"] != "opus" {
		t.Errorf("claude.Variables[model_strong] = %q, want %q", claude.Variables["model_strong"], "opus")
	}

	opencode, ok := cfg.Harness["opencode"]
	if !ok {
		t.Fatal("missing opencode harness")
	}
	if !opencode.GenerateCommandsFromSkills {
		t.Error("opencode.GenerateCommandsFromSkills = false, want true")
	}
	if opencode.Mappings["allowed_tools"] != "tools" {
		t.Errorf("opencode.Mappings[allowed_tools] = %q, want %q", opencode.Mappings["allowed_tools"], "tools")
	}
}

func TestMergeConfigs(t *testing.T) {
	global := &Config{
		DefaultHarnesses: []string{"claude"},
		DefaultArtifacts: []string{"skills"},
		Conflict: ConflictConfig{
			FileExists:        "error",
			DuplicateArtifact: "error",
		},
		Harness: map[string]Harness{
			"claude": {
				Path:      "~/.claude",
				Structure: "nested",
				Variables: map[string]string{
					"model_strong": "opus",
				},
			},
		},
	}

	project := &Config{
		DefaultHarnesses: []string{"claude", "opencode"},
		Conflict: ConflictConfig{
			FileExists: "overwrite",
		},
		Harness: map[string]Harness{
			"claude": {
				Variables: map[string]string{
					"project_name": "tropos",
				},
			},
		},
	}

	merged := Merge(global, project)

	if len(merged.DefaultHarnesses) != 2 {
		t.Errorf("merged.DefaultHarnesses = %v, want 2 items", merged.DefaultHarnesses)
	}

	if merged.Conflict.FileExists != "overwrite" {
		t.Errorf("merged.Conflict.FileExists = %q, want %q", merged.Conflict.FileExists, "overwrite")
	}

	claude := merged.Harness["claude"]
	if claude.Variables["model_strong"] != "opus" {
		t.Errorf("merged claude.Variables[model_strong] = %q, want %q", claude.Variables["model_strong"], "opus")
	}
	if claude.Variables["project_name"] != "tropos" {
		t.Errorf("merged claude.Variables[project_name] = %q, want %q", claude.Variables["project_name"], "tropos")
	}
}

func TestExpandPath(t *testing.T) {
	home, _ := os.UserHomeDir()

	tests := []struct {
		input string
		want  string
	}{
		{"~/.claude", filepath.Join(home, ".claude")},
		{"/absolute/path", "/absolute/path"},
		{"relative/path", "relative/path"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			got := ExpandPath(tt.input)
			if got != tt.want {
				t.Errorf("ExpandPath(%q) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}

func TestManifest(t *testing.T) {
	configContent := `
[manifest]
skills = ["code-test", "code-debug"]
commands = ["spec.create"]
agents = ["tester"]
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "tropos.toml")
	os.WriteFile(configPath, []byte(configContent), 0644)

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	if len(cfg.Manifest.Skills) != 2 {
		t.Errorf("Manifest.Skills = %v, want 2 items", cfg.Manifest.Skills)
	}
	if len(cfg.Manifest.Commands) != 1 {
		t.Errorf("Manifest.Commands = %v, want 1 item", cfg.Manifest.Commands)
	}
	if len(cfg.Manifest.Agents) != 1 {
		t.Errorf("Manifest.Agents = %v, want 1 item", cfg.Manifest.Agents)
	}
}

func TestLoadWithDiscovery(t *testing.T) {
	tmpDir := t.TempDir()

	globalDir := filepath.Join(tmpDir, ".config", "tropos")
	os.MkdirAll(globalDir, 0755)
	os.WriteFile(filepath.Join(globalDir, "config.toml"), []byte(`
default_harnesses = ["claude"]

[harness.claude]
path = "~/.claude"
structure = "nested"

[harness.claude.variables]
model_strong = "opus"
`), 0644)

	projectDir := filepath.Join(tmpDir, "project")
	os.MkdirAll(projectDir, 0755)
	os.WriteFile(filepath.Join(projectDir, "tropos.toml"), []byte(`
default_harnesses = ["claude", "opencode"]

[harness.claude.variables]
project_name = "test"
`), 0644)

	cfg, err := Load(projectDir, filepath.Join(globalDir, "config.toml"))
	if err != nil {
		t.Fatalf("Load() error = %v", err)
	}

	if len(cfg.DefaultHarnesses) != 2 {
		t.Errorf("DefaultHarnesses = %v, want 2", cfg.DefaultHarnesses)
	}

	claude := cfg.Harness["claude"]
	if claude.Variables["model_strong"] != "opus" {
		t.Error("global variable not inherited")
	}
	if claude.Variables["project_name"] != "test" {
		t.Error("project variable not set")
	}
}
