package target

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
)

func TestNestedTargetPath(t *testing.T) {
	target := NewNested("/home/user/.claude")

	tests := []struct {
		art  *artifact.Artifact
		want string
	}{
		{
			art:  &artifact.Artifact{Name: "code-test", Type: artifact.TypeSkill},
			want: "/home/user/.claude/skills/code-test/SKILL.md",
		},
		{
			art:  &artifact.Artifact{Name: "spec.create", Type: artifact.TypeCommand},
			want: "/home/user/.claude/commands/spec.create/COMMAND.md",
		},
		{
			art:  &artifact.Artifact{Name: "tester", Type: artifact.TypeAgent},
			want: "/home/user/.claude/agents/tester/AGENT.md",
		},
	}

	for _, tt := range tests {
		t.Run(tt.art.Name, func(t *testing.T) {
			got := target.TargetPath(tt.art)
			if got != tt.want {
				t.Errorf("TargetPath() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestFlatTargetPath(t *testing.T) {
	target := NewFlat("/home/user/.config/opencode")

	tests := []struct {
		art  *artifact.Artifact
		want string
	}{
		{
			art:  &artifact.Artifact{Name: "code-test", Type: artifact.TypeSkill},
			want: "/home/user/.config/opencode/skills/code-test.md",
		},
		{
			art:  &artifact.Artifact{Name: "tester", Type: artifact.TypeAgent},
			want: "/home/user/.config/opencode/agents/tester.md",
		},
	}

	for _, tt := range tests {
		t.Run(tt.art.Name, func(t *testing.T) {
			got := target.TargetPath(tt.art)
			if got != tt.want {
				t.Errorf("TargetPath() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestNestedWrite(t *testing.T) {
	tmpDir := t.TempDir()
	target := NewNested(tmpDir)

	art := &artifact.Artifact{
		Name: "code-test",
		Type: artifact.TypeSkill,
		Frontmatter: map[string]any{
			"name":        "code-test",
			"description": "TDD workflow",
		},
		Body: "# Code Test\n\nContent.\n",
	}

	if err := target.Write(art); err != nil {
		t.Fatalf("Write() error = %v", err)
	}

	expectedPath := filepath.Join(tmpDir, "skills", "code-test", "SKILL.md")
	content, err := os.ReadFile(expectedPath)
	if err != nil {
		t.Fatalf("file not written: %v", err)
	}

	if len(content) == 0 {
		t.Error("file is empty")
	}
}

func TestNestedWriteWithResources(t *testing.T) {
	tmpDir := t.TempDir()

	// Create source with resources
	srcDir := filepath.Join(tmpDir, "src", "skills", "code-test")
	os.MkdirAll(filepath.Join(srcDir, "reference"), 0755)
	os.WriteFile(filepath.Join(srcDir, "SKILL.md"), []byte("# Skill"), 0644)
	os.WriteFile(filepath.Join(srcDir, "reference", "guide.md"), []byte("# Guide"), 0644)
	os.WriteFile(filepath.Join(srcDir, "script.sh"), []byte("#!/bin/bash"), 0755)

	targetDir := filepath.Join(tmpDir, "target")
	target := NewNested(targetDir)

	art := &artifact.Artifact{
		Name:        "code-test",
		Type:        artifact.TypeSkill,
		SourcePath:  srcDir,
		IsDirectory: true,
		Resources:   []string{"reference", "script.sh"},
		Frontmatter: map[string]any{"name": "code-test"},
		Body:        "# Skill\n",
	}

	if err := target.Write(art); err != nil {
		t.Fatalf("Write() error = %v", err)
	}

	// Check resources copied
	refPath := filepath.Join(targetDir, "skills", "code-test", "reference", "guide.md")
	if _, err := os.Stat(refPath); err != nil {
		t.Errorf("reference/guide.md not copied: %v", err)
	}

	scriptPath := filepath.Join(targetDir, "skills", "code-test", "script.sh")
	if _, err := os.Stat(scriptPath); err != nil {
		t.Errorf("script.sh not copied: %v", err)
	}
}

func TestFlatWriteWithResources(t *testing.T) {
	tmpDir := t.TempDir()

	srcDir := filepath.Join(tmpDir, "src", "skills", "code-test")
	os.MkdirAll(filepath.Join(srcDir, "reference"), 0755)
	os.WriteFile(filepath.Join(srcDir, "SKILL.md"), []byte("# Skill"), 0644)
	os.WriteFile(filepath.Join(srcDir, "reference", "guide.md"), []byte("# Guide"), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	target := NewFlat(targetDir)

	art := &artifact.Artifact{
		Name:        "code-test",
		Type:        artifact.TypeSkill,
		SourcePath:  srcDir,
		IsDirectory: true,
		Resources:   []string{"reference"},
		Frontmatter: map[string]any{"name": "code-test"},
		Body:        "# Skill\n",
	}

	if err := target.Write(art); err != nil {
		t.Fatalf("Write() error = %v", err)
	}

	// Check main file
	mainPath := filepath.Join(targetDir, "skills", "code-test.md")
	if _, err := os.Stat(mainPath); err != nil {
		t.Errorf("main file not written: %v", err)
	}

	// Check resources in sibling directory
	refPath := filepath.Join(targetDir, "skills", "code-test", "reference", "guide.md")
	if _, err := os.Stat(refPath); err != nil {
		t.Errorf("reference/guide.md not copied: %v", err)
	}
}

func TestTargetExists(t *testing.T) {
	tmpDir := t.TempDir()
	target := NewNested(tmpDir)

	art := &artifact.Artifact{
		Name: "code-test",
		Type: artifact.TypeSkill,
	}

	exists, _ := target.Exists(art)
	if exists {
		t.Error("Exists() = true before write")
	}

	// Create the file
	targetPath := target.TargetPath(art)
	os.MkdirAll(filepath.Dir(targetPath), 0755)
	os.WriteFile(targetPath, []byte("content"), 0644)

	exists, path := target.Exists(art)
	if !exists {
		t.Error("Exists() = false after write")
	}
	if path != targetPath {
		t.Errorf("Exists() path = %q, want %q", path, targetPath)
	}
}

func TestNewFromConfig(t *testing.T) {
	harness := config.Harness{
		Path:      "/home/user/.claude",
		Structure: "nested",
	}

	target := NewFromConfig("claude", harness)
	if target.Name() != "claude" {
		t.Errorf("Name() = %q, want %q", target.Name(), "claude")
	}
	if target.Path() != "/home/user/.claude" {
		t.Errorf("Path() = %q", target.Path())
	}
}
