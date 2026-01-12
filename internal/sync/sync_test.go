package sync

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
)

func setupSyncTest(t *testing.T) (srcDir, targetDir string, cfg *config.Config) {
	t.Helper()

	srcDir = t.TempDir()
	targetDir = t.TempDir()

	// Create source skill
	skillDir := filepath.Join(srcDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: code-test
model: "{{.model_strong}}"
description: TDD workflow
---

# Code Test

Use {{.model_strong}} for this.
`), 0644)

	// Create source command
	cmdDir := filepath.Join(srcDir, "commands", "spec.create")
	os.MkdirAll(cmdDir, 0755)
	os.WriteFile(filepath.Join(cmdDir, "COMMAND.md"), []byte(`---
name: spec.create
description: Create spec
---

# Spec Create
`), 0644)

	cfg = &config.Config{
		DefaultHarnesses: []string{"claude"},
		DefaultArtifacts: []string{"skills", "commands"},
		Conflict: config.ConflictConfig{
			FileExists:        "error",
			DuplicateArtifact: "error",
		},
		Harness: map[string]config.Harness{
			"claude": {
				Path:      targetDir,
				
				Variables: map[string]string{
					"model_strong": "opus",
				},
			},
		},
	}

	return srcDir, targetDir, cfg
}

func TestSync(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	result, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	if result.Synced != 2 {
		t.Errorf("Synced = %d, want 2", result.Synced)
	}

	// Check skill was written with transformed content
	skillPath := filepath.Join(targetDir, "skills", "code-test", "SKILL.md")
	content, err := os.ReadFile(skillPath)
	if err != nil {
		t.Fatalf("skill not written: %v", err)
	}

	contentStr := string(content)
	if !contains(contentStr, "model: opus") {
		t.Error("template variable not substituted in frontmatter")
	}
	if !contains(contentStr, "Use opus for this") {
		t.Error("template variable not substituted in body")
	}
}

func TestSyncDryRun(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
		DryRun:      true,
	}

	result, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	if result.Synced != 2 {
		t.Errorf("Synced = %d, want 2", result.Synced)
	}

	// File should not exist
	skillPath := filepath.Join(targetDir, "skills", "code-test", "SKILL.md")
	if _, err := os.Stat(skillPath); !os.IsNotExist(err) {
		t.Error("file should not be written in dry-run mode")
	}
}

func TestSyncConflictFileExists(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	// Pre-create target file
	skillDir := filepath.Join(targetDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("existing"), 0644)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	result, err := Sync(cfg, opts)
	if err == nil {
		t.Fatal("Sync() should error on conflict")
	}

	if len(result.Conflicts) == 0 {
		t.Error("should have conflicts")
	}
}

func TestSyncForce(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	// Pre-create target file
	skillDir := filepath.Join(targetDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("existing"), 0644)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
		Force:       true,
	}

	result, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() with force error = %v", err)
	}

	if result.Synced != 2 {
		t.Errorf("Synced = %d, want 2", result.Synced)
	}

	// File should be overwritten
	content, _ := os.ReadFile(filepath.Join(skillDir, "SKILL.md"))
	if string(content) == "existing" {
		t.Error("file not overwritten with --force")
	}
}

func TestSyncGenerateCommands(t *testing.T) {
	srcDir := t.TempDir()
	targetDir := t.TempDir()

	// Create skill with user-invocable
	skillDir := filepath.Join(srcDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: code-test
user-invocable: true
description: TDD workflow
---

# Code Test
`), 0644)

	cfg := &config.Config{
		DefaultHarnesses: []string{"opencode"},
		DefaultArtifacts: []string{"skills"},
		Harness: map[string]config.Harness{
			"opencode": {
				Path:                       targetDir,
				
				GenerateCommandsFromSkills: true,
			},
		},
	}

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"opencode"},
	}

	result, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	// Should have synced skill + generated command
	if result.Synced < 1 {
		t.Errorf("Synced = %d, want >= 1", result.Synced)
	}
	if result.Generated != 1 {
		t.Errorf("Generated = %d, want 1", result.Generated)
	}

	// Check generated command exists
	cmdPath := filepath.Join(targetDir, "commands", "code-test", "COMMAND.md")
	content, err := os.ReadFile(cmdPath)
	if err != nil {
		t.Fatalf("generated command not written: %v", err)
	}

	if !contains(string(content), "Invoke skill: code-test") {
		t.Error("generated command missing invoke directive")
	}
}

func TestDetectConflicts(t *testing.T) {
	arts := []*artifact.Artifact{
		{Name: "test", Type: artifact.TypeSkill},
	}

	targetDir := t.TempDir()
	skillDir := filepath.Join(targetDir, "skills", "test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("existing"), 0644)

	harness := config.Harness{Path: targetDir}
	conflicts := DetectFileConflicts(arts, "claude", harness)

	if len(conflicts) != 1 {
		t.Errorf("DetectFileConflicts() = %d conflicts, want 1", len(conflicts))
	}
}

func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(s) > 0 && containsHelper(s, substr))
}

func containsHelper(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
