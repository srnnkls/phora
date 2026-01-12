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

[harness.claude]
path = "~/.claude"

[harness.claude.variables]
model_strong = "opus"
model_weak = "haiku"

[harness.opencode]
path = "~/.opencode"
generate_commands_from_skills = true

[harness.opencode.mappings]
allowed-tools = "tools"

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

	claude, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("missing claude harness")
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
	if opencode.Mappings["allowed-tools"] != "tools" {
		t.Errorf("opencode.Mappings[allowed-tools] = %q, want %q", opencode.Mappings["allowed-tools"], "tools")
	}
}

func TestMergeConfigs(t *testing.T) {
	global := &Config{
		DefaultHarnesses: []string{"claude"},
		DefaultArtifacts: []string{"skills"},
		Harness: map[string]Harness{
			"claude": {
				Path: "~/.claude",
				Variables: map[string]string{
					"model_strong": "opus",
				},
			},
		},
	}

	project := &Config{
		DefaultHarnesses: []string{"claude", "opencode"},
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

func TestLoadWithDiscovery(t *testing.T) {
	tmpDir := t.TempDir()

	globalDir := filepath.Join(tmpDir, ".config", "tropos")
	os.MkdirAll(globalDir, 0755)
	os.WriteFile(filepath.Join(globalDir, "config.toml"), []byte(`
default_harnesses = ["claude"]

[harness.claude]
path = "~/.claude"

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

func TestMergeSources(t *testing.T) {
	global := &Config{
		Sources: []Source{
			{Repo: "global/repo", Path: "skills"},
		},
		Harness: map[string]Harness{},
	}

	project := &Config{
		Sources: []Source{
			{Repo: "project/repo"},
			{Repo: "global/repo", Path: "skills"}, // duplicate
		},
		Harness: map[string]Harness{},
	}

	merged := Merge(global, project)

	if len(merged.Sources) != 2 {
		t.Errorf("Sources length = %d, want 2 (deduplicated)", len(merged.Sources))
	}

	foundGlobal := false
	foundProject := false
	for _, src := range merged.Sources {
		if src.Repo == "global/repo" && src.Path == "skills" {
			foundGlobal = true
		}
		if src.Repo == "project/repo" {
			foundProject = true
		}
	}
	if !foundGlobal {
		t.Error("global source not found in merged config")
	}
	if !foundProject {
		t.Error("project source not found in merged config")
	}
}

func TestLoadSources(t *testing.T) {
	configContent := `
[[sources]]
repo = "owner/repo"
path = "skills/claude"
ref = "v1.0"

[[sources]]
repo = "another/repo"
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "tropos.toml")
	os.WriteFile(configPath, []byte(configContent), 0644)

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	if len(cfg.Sources) != 2 {
		t.Fatalf("Sources length = %d, want 2", len(cfg.Sources))
	}

	if cfg.Sources[0].Repo != "owner/repo" {
		t.Errorf("Sources[0].Repo = %q, want %q", cfg.Sources[0].Repo, "owner/repo")
	}
	if cfg.Sources[0].Path != "skills/claude" {
		t.Errorf("Sources[0].Path = %q, want %q", cfg.Sources[0].Path, "skills/claude")
	}
	if cfg.Sources[0].Ref != "v1.0" {
		t.Errorf("Sources[0].Ref = %q, want %q", cfg.Sources[0].Ref, "v1.0")
	}
	if cfg.Sources[1].Repo != "another/repo" {
		t.Errorf("Sources[1].Repo = %q, want %q", cfg.Sources[1].Repo, "another/repo")
	}
}
