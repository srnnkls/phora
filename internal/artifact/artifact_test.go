package artifact

import (
	"os"
	"path/filepath"
	"testing"
)

func TestParseFrontmatter(t *testing.T) {
	content := `---
name: code-test
description: TDD workflow
model: strong
allowed_tools:
  - read
  - write
user-invocable: true
---

# Code Test

Body content here.
`

	art, err := Parse([]byte(content))
	if err != nil {
		t.Fatalf("Parse() error = %v", err)
	}

	if art.Frontmatter["name"] != "code-test" {
		t.Errorf("name = %v, want code-test", art.Frontmatter["name"])
	}
	if art.Frontmatter["model"] != "strong" {
		t.Errorf("model = %v, want strong", art.Frontmatter["model"])
	}
	if art.Frontmatter["user-invocable"] != true {
		t.Errorf("user-invocable = %v, want true", art.Frontmatter["user-invocable"])
	}

	tools, ok := art.Frontmatter["allowed_tools"].([]any)
	if !ok || len(tools) != 2 {
		t.Errorf("allowed_tools = %v, want [read, write]", art.Frontmatter["allowed_tools"])
	}

	if art.Body != "# Code Test\n\nBody content here.\n" {
		t.Errorf("Body = %q", art.Body)
	}
}

func TestParseNoFrontmatter(t *testing.T) {
	content := `# Just Content

No frontmatter here.
`

	art, err := Parse([]byte(content))
	if err != nil {
		t.Fatalf("Parse() error = %v", err)
	}

	if len(art.Frontmatter) != 0 {
		t.Errorf("Frontmatter = %v, want empty", art.Frontmatter)
	}
	if art.Body != content {
		t.Errorf("Body = %q, want %q", art.Body, content)
	}
}

func TestTypeFromPath(t *testing.T) {
	tests := []struct {
		path string
		want Type
	}{
		{"skills/code-test/SKILL.md", TypeSkill},
		{"skills/code-test.md", TypeSkill},
		{"commands/spec.create/COMMAND.md", TypeCommand},
		{"commands/spec.create.md", TypeCommand},
		{"agents/tester/AGENT.md", TypeAgent},
		{"agents/tester.md", TypeAgent},
		{"other/file.md", TypeUnknown},
	}

	for _, tt := range tests {
		t.Run(tt.path, func(t *testing.T) {
			got := TypeFromPath(tt.path)
			if got != tt.want {
				t.Errorf("TypeFromPath(%q) = %v, want %v", tt.path, got, tt.want)
			}
		})
	}
}

func TestMainFileName(t *testing.T) {
	tests := []struct {
		artType Type
		want    string
	}{
		{TypeSkill, "SKILL.md"},
		{TypeCommand, "COMMAND.md"},
		{TypeAgent, "AGENT.md"},
		{TypeUnknown, ""},
	}

	for _, tt := range tests {
		t.Run(string(tt.artType), func(t *testing.T) {
			got := MainFileName(tt.artType)
			if got != tt.want {
				t.Errorf("MainFileName(%v) = %q, want %q", tt.artType, got, tt.want)
			}
		})
	}
}

func TestDiscoverArtifacts(t *testing.T) {
	tmpDir := t.TempDir()

	// Directory-based skill
	skillDir := filepath.Join(tmpDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: code-test
---
# Code Test
`), 0644)
	os.MkdirAll(filepath.Join(skillDir, "reference"), 0755)
	os.WriteFile(filepath.Join(skillDir, "reference", "guide.md"), []byte("Guide"), 0644)

	// Single-file agent
	agentsDir := filepath.Join(tmpDir, "agents")
	os.MkdirAll(agentsDir, 0755)
	os.WriteFile(filepath.Join(agentsDir, "tester.md"), []byte(`---
name: tester
---
# Tester
`), 0644)

	// Command directory
	cmdDir := filepath.Join(tmpDir, "commands", "spec.create")
	os.MkdirAll(cmdDir, 0755)
	os.WriteFile(filepath.Join(cmdDir, "COMMAND.md"), []byte(`---
name: spec.create
---
# Spec Create
`), 0644)

	artifacts, err := Discover(tmpDir, []string{"skills", "commands", "agents"})
	if err != nil {
		t.Fatalf("Discover() error = %v", err)
	}

	if len(artifacts) != 3 {
		t.Errorf("Discover() = %d artifacts, want 3", len(artifacts))
		for _, a := range artifacts {
			t.Logf("  - %s (%s)", a.Name, a.Type)
		}
	}

	// Check skill has resources
	var skill *Artifact
	for _, a := range artifacts {
		if a.Name == "code-test" {
			skill = a
			break
		}
	}
	if skill == nil {
		t.Fatal("skill code-test not found")
	}
	if !skill.IsDirectory {
		t.Error("skill.IsDirectory = false, want true")
	}
	if len(skill.Resources) == 0 {
		t.Error("skill.Resources is empty, want reference/")
	}
}

func TestArtifactRender(t *testing.T) {
	art := &Artifact{
		Name: "code-test",
		Type: TypeSkill,
		Frontmatter: map[string]any{
			"name":        "code-test",
			"description": "TDD workflow",
		},
		Body: "# Code Test\n\nContent.\n",
	}

	got := art.Render()
	want := `---
description: TDD workflow
name: code-test
---

# Code Test

Content.
`

	if got != want {
		t.Errorf("Render() =\n%s\nwant:\n%s", got, want)
	}
}

func TestArtifactRenderEmptyFrontmatter(t *testing.T) {
	art := &Artifact{
		Name:        "test",
		Type:        TypeSkill,
		Frontmatter: map[string]any{},
		Body:        "# Test\n",
	}

	got := art.Render()
	want := "# Test\n"

	if got != want {
		t.Errorf("Render() = %q, want %q", got, want)
	}
}
