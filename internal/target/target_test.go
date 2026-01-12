package target

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
)

func TestTargetPath(t *testing.T) {
	target := New("/home/user/.claude")

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

func TestWrite(t *testing.T) {
	tmpDir := t.TempDir()
	target := New(tmpDir)

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

func TestWriteWithResources(t *testing.T) {
	tmpDir := t.TempDir()

	srcDir := filepath.Join(tmpDir, "src", "skills", "code-test")
	os.MkdirAll(filepath.Join(srcDir, "reference"), 0755)
	os.WriteFile(filepath.Join(srcDir, "SKILL.md"), []byte("# Skill"), 0644)
	os.WriteFile(filepath.Join(srcDir, "reference", "guide.md"), []byte("# Guide"), 0644)
	os.WriteFile(filepath.Join(srcDir, "script.sh"), []byte("#!/bin/bash"), 0755)

	targetDir := filepath.Join(tmpDir, "target")
	target := New(targetDir)

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

	refPath := filepath.Join(targetDir, "skills", "code-test", "reference", "guide.md")
	if _, err := os.Stat(refPath); err != nil {
		t.Errorf("reference/guide.md not copied: %v", err)
	}

	scriptPath := filepath.Join(targetDir, "skills", "code-test", "script.sh")
	if _, err := os.Stat(scriptPath); err != nil {
		t.Errorf("script.sh not copied: %v", err)
	}
}

func TestExists(t *testing.T) {
	tmpDir := t.TempDir()
	target := New(tmpDir)

	art := &artifact.Artifact{
		Name: "code-test",
		Type: artifact.TypeSkill,
	}

	exists, _ := target.Exists(art)
	if exists {
		t.Error("Exists() = true before write")
	}

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
		Path: "/home/user/.claude",
	}

	target := NewFromConfig("claude", harness)
	if target.Name() != "claude" {
		t.Errorf("Name() = %q, want %q", target.Name(), "claude")
	}
	if target.Path() != "/home/user/.claude" {
		t.Errorf("Path() = %q", target.Path())
	}
}

func TestFlatTargetPath(t *testing.T) {
	target := NewFromConfig("opencode", config.Harness{
		Path:      "/home/user/.opencode",
		Structure: "flat",
	})

	tests := []struct {
		art  *artifact.Artifact
		want string
	}{
		{
			art:  &artifact.Artifact{Name: "code-test", Type: artifact.TypeSkill},
			want: "/home/user/.opencode/skills/code-test.md",
		},
		{
			art:  &artifact.Artifact{Name: "spec.create", Type: artifact.TypeCommand},
			want: "/home/user/.opencode/commands/spec.create.md",
		},
		{
			art:  &artifact.Artifact{Name: "tester", Type: artifact.TypeAgent},
			want: "/home/user/.opencode/agents/tester.md",
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

func TestFlatWriteWithResources(t *testing.T) {
	tmpDir := t.TempDir()

	srcDir := filepath.Join(tmpDir, "src", "skills", "code-test")
	os.MkdirAll(filepath.Join(srcDir, "reference"), 0755)
	os.WriteFile(filepath.Join(srcDir, "SKILL.md"), []byte("# Skill"), 0644)
	os.WriteFile(filepath.Join(srcDir, "reference", "guide.md"), []byte("# Guide"), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	target := NewFromConfig("opencode", config.Harness{
		Path:      targetDir,
		Structure: "flat",
	})

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

	// Main file should be flat
	mainPath := filepath.Join(targetDir, "skills", "code-test.md")
	if _, err := os.Stat(mainPath); err != nil {
		t.Errorf("main file not written at flat path: %v", err)
	}

	// Resources should be in sibling directory
	refPath := filepath.Join(targetDir, "skills", "code-test", "reference", "guide.md")
	if _, err := os.Stat(refPath); err != nil {
		t.Errorf("reference/guide.md not copied to sibling dir: %v", err)
	}
}
