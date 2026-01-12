package transform

import (
	"testing"

	"github.com/srnnkls/tropos/internal/artifact"
)

func TestExecuteTemplate(t *testing.T) {
	content := `---
name: code-test
model: {{.model_strong}}
---

# Code Test

Use {{.model_strong}} for complex tasks.
Use {{.model_weak}} for simple tasks.
`

	vars := map[string]string{
		"model_strong": "opus",
		"model_weak":   "haiku",
	}

	result, err := ExecuteTemplate(content, vars)
	if err != nil {
		t.Fatalf("ExecuteTemplate() error = %v", err)
	}

	expected := `---
name: code-test
model: opus
---

# Code Test

Use opus for complex tasks.
Use haiku for simple tasks.
`

	if result != expected {
		t.Errorf("ExecuteTemplate() =\n%s\nwant:\n%s", result, expected)
	}
}

func TestExecuteTemplateConditional(t *testing.T) {
	content := `{{if .feature_enabled}}Feature is ON{{else}}Feature is OFF{{end}}`

	vars := map[string]string{
		"feature_enabled": "true",
	}

	result, err := ExecuteTemplate(content, vars)
	if err != nil {
		t.Fatalf("ExecuteTemplate() error = %v", err)
	}

	if result != "Feature is ON" {
		t.Errorf("ExecuteTemplate() = %q, want %q", result, "Feature is ON")
	}
}

func TestApplyMappings(t *testing.T) {
	fm := map[string]any{
		"name":          "code-test",
		"allowed_tools": []string{"read", "write"},
		"resources":     []string{"reference/"},
	}

	mappings := map[string]string{
		"allowed_tools": "tools",
		"resources":     "files",
	}

	result := ApplyMappings(fm, mappings)

	if _, ok := result["allowed_tools"]; ok {
		t.Error("allowed_tools should be renamed to tools")
	}
	if _, ok := result["tools"]; !ok {
		t.Error("tools key missing")
	}
	if _, ok := result["files"]; !ok {
		t.Error("files key missing")
	}
	if result["name"] != "code-test" {
		t.Error("name should be unchanged")
	}
}

func TestApplyMappingsEmpty(t *testing.T) {
	fm := map[string]any{
		"name": "test",
	}

	result := ApplyMappings(fm, nil)

	if result["name"] != "test" {
		t.Error("name should be unchanged with nil mappings")
	}
}

func TestTransformArtifact(t *testing.T) {
	art := &artifact.Artifact{
		Name: "code-test",
		Type: artifact.TypeSkill,
		Frontmatter: map[string]any{
			"name":          "code-test",
			"model":         "{{.model_strong}}",
			"allowed_tools": []string{"read"},
		},
		Body: "Use {{.model_strong}} model.\n",
	}

	tr := &Transformer{
		Variables: map[string]string{
			"model_strong": "opus",
		},
		Mappings: map[string]string{
			"allowed_tools": "tools",
		},
	}

	result, err := tr.Transform(art)
	if err != nil {
		t.Fatalf("Transform() error = %v", err)
	}

	if result.Frontmatter["model"] != "opus" {
		t.Errorf("model = %v, want opus", result.Frontmatter["model"])
	}
	if _, ok := result.Frontmatter["tools"]; !ok {
		t.Error("tools key missing after mapping")
	}
	if result.Body != "Use opus model.\n" {
		t.Errorf("Body = %q", result.Body)
	}
}

func TestTransformPreservesMetadata(t *testing.T) {
	art := &artifact.Artifact{
		Name:        "test",
		Type:        artifact.TypeSkill,
		SourcePath:  "/some/path",
		IsDirectory: true,
		Resources:   []string{"ref/"},
		Frontmatter: map[string]any{"name": "test"},
		Body:        "Content",
	}

	tr := &Transformer{}
	result, err := tr.Transform(art)
	if err != nil {
		t.Fatalf("Transform() error = %v", err)
	}

	if result.Name != art.Name {
		t.Error("Name not preserved")
	}
	if result.Type != art.Type {
		t.Error("Type not preserved")
	}
	if result.SourcePath != art.SourcePath {
		t.Error("SourcePath not preserved")
	}
	if result.IsDirectory != art.IsDirectory {
		t.Error("IsDirectory not preserved")
	}
	if len(result.Resources) != len(art.Resources) {
		t.Error("Resources not preserved")
	}
}
