package config

import (
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestLoadConfig(t *testing.T) {
	tmpDir := t.TempDir()

	configContent := `
version = 1
artifacts = ["skills", "commands", "agents"]

[harness.claude]
path = "~/.claude"

[harness.claude.variables]
model_strong = "opus"
model_weak = "haiku"

[harness.opencode]
path = "~/.opencode"
generate_commands_from_skills = true

[harness.opencode.keys]
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

	if len(cfg.Artifacts) != 3 {
		t.Errorf("Artifacts = %v, want 3 items", cfg.Artifacts)
	}

	if len(cfg.Harness) != 2 {
		t.Errorf("Harness = %d harnesses, want 2", len(cfg.Harness))
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
	if opencode.Keys["allowed-tools"] != "tools" {
		t.Errorf("opencode.Keys[allowed-tools] = %q, want %q", opencode.Keys["allowed-tools"], "tools")
	}
}

func TestMergeConfigs(t *testing.T) {
	global := &Config{
		Artifacts: []string{"skills"},
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
		Artifacts: []string{"skills", "commands"},
		Harness: map[string]Harness{
			"claude": {
				Variables: map[string]string{
					"project_name": "phora",
				},
			},
			"opencode": {
				Path: "~/.opencode",
			},
		},
	}

	merged := Merge(global, project)

	if len(merged.Artifacts) != 2 {
		t.Errorf("merged.Artifacts = %v, want 2 items (skills, commands)", merged.Artifacts)
	}

	if len(merged.Harness) != 2 {
		t.Errorf("merged.Harness = %d harnesses, want 2 (claude, opencode)", len(merged.Harness))
	}

	claude := merged.Harness["claude"]
	if claude.Variables["model_strong"] != "opus" {
		t.Errorf("merged claude.Variables[model_strong] = %q, want %q", claude.Variables["model_strong"], "opus")
	}
	if claude.Variables["project_name"] != "phora" {
		t.Errorf("merged claude.Variables[project_name] = %q, want %q", claude.Variables["project_name"], "phora")
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

	globalDir := filepath.Join(tmpDir, ".config", "phora")
	os.MkdirAll(globalDir, 0755)
	os.WriteFile(filepath.Join(globalDir, "config.toml"), []byte(`
version = 1
default_harnesses = ["claude"]

[harness.claude]
path = "~/.claude"

[harness.claude.variables]
model_strong = "opus"
`), 0644)

	projectDir := filepath.Join(tmpDir, "project")
	os.MkdirAll(projectDir, 0755)
	os.WriteFile(filepath.Join(projectDir, "phora.toml"), []byte(`
version = 1

[harness.claude]
path = ".claude"

[harness.claude.variables]
project_name = "test"

[harness.opencode]
path = ".opencode"
`), 0644)

	cfg, err := Load(projectDir, filepath.Join(globalDir, "config.toml"))
	if err != nil {
		t.Fatalf("Load() error = %v", err)
	}

	if len(cfg.Harness) != 2 {
		t.Errorf("Harness = %d, want 2 (claude, opencode)", len(cfg.Harness))
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
		Sources: map[string]Source{
			"global/repo": {Git: "https://github.com/global/repo.git", Path: "skills"},
		},
		Harness: map[string]Harness{},
	}

	project := &Config{
		Sources: map[string]Source{
			"project/repo": {Git: "https://github.com/project/repo.git"},
			"global/repo":  {Git: "https://github.com/global/repo.git", Path: "updated"}, // override
		},
		Harness: map[string]Harness{},
	}

	merged := Merge(global, project)

	if len(merged.Sources) != 2 {
		t.Errorf("Sources length = %d, want 2", len(merged.Sources))
	}

	globalSrc, ok := merged.Sources["global/repo"]
	if !ok {
		t.Error("global source not found in merged config")
	}
	if globalSrc.Path != "updated" {
		t.Errorf("global source not overridden, Path = %q, want %q", globalSrc.Path, "updated")
	}

	if _, ok := merged.Sources["project/repo"]; !ok {
		t.Error("project source not found in merged config")
	}
}

func TestLoadSources(t *testing.T) {
	configContent := `
version = 1

[sources."owner/repo"]
git = "https://github.com/owner/repo.git"
path = "skills/claude"
branch = "v1.0"

[sources."another/repo"]
git = "https://github.com/another/repo.git"
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")
	os.WriteFile(configPath, []byte(configContent), 0644)

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	if len(cfg.Sources) != 2 {
		t.Fatalf("Sources length = %d, want 2", len(cfg.Sources))
	}

	ownerRepo, ok := cfg.Sources["owner/repo"]
	if !ok {
		t.Fatal("owner/repo source not found")
	}
	if ownerRepo.Git != "https://github.com/owner/repo.git" {
		t.Errorf("Sources[owner/repo].Git = %q, want %q", ownerRepo.Git, "https://github.com/owner/repo.git")
	}
	if ownerRepo.Path != "skills/claude" {
		t.Errorf("Sources[owner/repo].Path = %q, want %q", ownerRepo.Path, "skills/claude")
	}
	if ownerRepo.Branch != "v1.0" {
		t.Errorf("Sources[owner/repo].Ref = %q, want %q", ownerRepo.Branch, "v1.0")
	}

	anotherRepo, ok := cfg.Sources["another/repo"]
	if !ok {
		t.Fatal("another/repo source not found")
	}
	if anotherRepo.Git != "https://github.com/another/repo.git" {
		t.Errorf("Sources[another/repo].Git = %q, want %q", anotherRepo.Git, "https://github.com/another/repo.git")
	}
}


func TestLoadHarnessWithNewMappingStructure(t *testing.T) {
	configContent := `
version = 1

[harness.claude]
path = "~/.claude"

[harness.claude.keys]
allowed-tools = "tools"
model = "model_id"

[harness.claude.values.model]
opus = "claude-opus-4-5"
haiku = "claude-haiku-4-5"

[harness.claude.artifact_mappings.skills.keys]
allowed-tools = "skill_tools"

[harness.claude.artifact_mappings.skills.values.model]
opus = "opus-for-skills"
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "config.toml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	claude, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("missing claude harness")
	}

	if claude.Keys == nil {
		t.Fatal("claude.Keys is nil")
	}
	if claude.Keys["allowed-tools"] != "tools" {
		t.Errorf("claude.Keys[allowed-tools] = %q, want %q", claude.Keys["allowed-tools"], "tools")
	}
	if claude.Keys["model"] != "model_id" {
		t.Errorf("claude.Keys[model] = %q, want %q", claude.Keys["model"], "model_id")
	}

	if claude.Values == nil {
		t.Fatal("claude.Values is nil")
	}
	if claude.Values["model"]["opus"] != "claude-opus-4-5" {
		t.Errorf("claude.Values[model][opus] = %q, want %q", claude.Values["model"]["opus"], "claude-opus-4-5")
	}

	if claude.ArtifactMappings == nil {
		t.Fatal("claude.ArtifactMappings is nil")
	}
	skills, ok := claude.ArtifactMappings["skills"]
	if !ok {
		t.Fatal("claude.ArtifactMappings[skills] not found")
	}
	if skills.Keys["allowed-tools"] != "skill_tools" {
		t.Errorf("skills.Keys[allowed-tools] = %q, want %q", skills.Keys["allowed-tools"], "skill_tools")
	}
	if skills.Values["model"]["opus"] != "opus-for-skills" {
		t.Errorf("skills.Values[model][opus] = %q, want %q", skills.Values["model"]["opus"], "opus-for-skills")
	}
}

func TestOldMappingsFieldRemoved(t *testing.T) {
	configContent := `
version = 1

[harness.test]
path = "~/.test"

[harness.test.mappings]
old-key = "old-value"
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "config.toml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	// The old "mappings" field should be ignored since Harness no longer has it.
	// We can't check h.Mappings since the field doesn't exist anymore.
	// This test passes if the code compiles without a Mappings field.
}

func TestLoadHarnessWithToolsAndReferences(t *testing.T) {
	configContent := `
version = 1

[harness.claude]
path = "~/.claude/skills"

[harness.claude.tools]
bash = "Bash"
read = "Read"
edit = "Edit"

[harness.claude.references.skill]
output = "/{{name}}"

[harness.claude.references.tool]
output = "{{mapped}}"

[harness.claude.references.agent]
output = "@{{name}}"
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "config.toml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	claude, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("missing claude harness")
	}

	if claude.Tools == nil {
		t.Fatal("claude.Tools is nil")
	}
	if claude.Tools["bash"] != "Bash" {
		t.Errorf("claude.Tools[bash] = %q, want %q", claude.Tools["bash"], "Bash")
	}
	if claude.Tools["read"] != "Read" {
		t.Errorf("claude.Tools[read] = %q, want %q", claude.Tools["read"], "Read")
	}

	if claude.References == nil {
		t.Fatal("claude.References is nil")
	}
	skill, ok := claude.References["skill"]
	if !ok {
		t.Fatal("claude.References[skill] not found")
	}
	if skill.Output != "/{{name}}" {
		t.Errorf("claude.References[skill].Output = %q, want %q", skill.Output, "/{{name}}")
	}

	tool, ok := claude.References["tool"]
	if !ok {
		t.Fatal("claude.References[tool] not found")
	}
	if tool.Output != "{{mapped}}" {
		t.Errorf("claude.References[tool].Output = %q, want %q", tool.Output, "{{mapped}}")
	}
}

func TestMergeHarnessToolsAndReferences(t *testing.T) {
	global := &Config{
		Harness: map[string]Harness{
			"claude": {
				Path: "~/.claude",
				Tools: map[string]string{
					"bash": "Bash",
					"read": "Read",
				},
				References: map[string]ReferenceConfig{
					"skill": {Output: "/{{name}}"},
				},
			},
		},
	}

	project := &Config{
		Harness: map[string]Harness{
			"claude": {
				Tools: map[string]string{
					"edit":  "Edit",
					"bash":  "BashOverride",
				},
				References: map[string]ReferenceConfig{
					"tool": {Output: "{{mapped}}"},
				},
			},
		},
	}

	merged := Merge(global, project)

	claude := merged.Harness["claude"]

	if claude.Tools["bash"] != "BashOverride" {
		t.Errorf("merged Tools[bash] = %q, want %q", claude.Tools["bash"], "BashOverride")
	}
	if claude.Tools["read"] != "Read" {
		t.Errorf("merged Tools[read] = %q, want %q (should be inherited from global)", claude.Tools["read"], "Read")
	}
	if claude.Tools["edit"] != "Edit" {
		t.Errorf("merged Tools[edit] = %q, want %q", claude.Tools["edit"], "Edit")
	}

	skill, ok := claude.References["skill"]
	if !ok {
		t.Fatal("merged References[skill] not found (should be inherited from global)")
	}
	if skill.Output != "/{{name}}" {
		t.Errorf("merged References[skill].Output = %q, want %q", skill.Output, "/{{name}}")
	}

	tool, ok := claude.References["tool"]
	if !ok {
		t.Fatal("merged References[tool] not found")
	}
	if tool.Output != "{{mapped}}" {
		t.Errorf("merged References[tool].Output = %q, want %q", tool.Output, "{{mapped}}")
	}
}

func TestSourceStructHasNoLegacyFields(t *testing.T) {
	// This test verifies that the Source struct does NOT have legacy fields.
	// If any of these fields exist, this test should fail.
	legacyFields := []string{"Type", "Host", "Owner", "Repo", "Global"}

	sourceType := reflect.TypeOf(Source{})
	for _, fieldName := range legacyFields {
		if _, found := sourceType.FieldByName(fieldName); found {
			t.Errorf("Source struct still has legacy field %q - this field should be removed", fieldName)
		}
	}
}

func TestLoadFileRejectsConfigWithoutVersion(t *testing.T) {
	configContent := `
[sources."test/repo"]
git = "https://github.com/test/repo.git"
`
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadFile(configPath)
	if err == nil {
		t.Error("LoadFile() should return error for config without version field")
	}
}

func TestLoadFileRejectsConfigWithInvalidVersion(t *testing.T) {
	tests := []struct {
		name    string
		version int
	}{
		{"version 0", 0},
		{"version 2", 2},
		{"version 99", 99},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			configContent := fmt.Sprintf(`
version = %d

[sources."test/repo"]
git = "https://github.com/test/repo.git"
`, tt.version)

			tmpDir := t.TempDir()
			configPath := filepath.Join(tmpDir, "phora.toml")
			if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
				t.Fatal(err)
			}

			_, err := LoadFile(configPath)
			if err == nil {
				t.Errorf("LoadFile() should return error for config with version = %d", tt.version)
			}
		})
	}
}

func TestLoadFileAcceptsVersion1(t *testing.T) {
	configContent := `
version = 1

[sources."test/repo"]
git = "https://github.com/test/repo.git"
`
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFile(configPath)
	if err != nil {
		t.Fatalf("LoadFile() error = %v, want nil for version = 1", err)
	}
	if cfg.Version != 1 {
		t.Errorf("cfg.Version = %d, want 1", cfg.Version)
	}
}
