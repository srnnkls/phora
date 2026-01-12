package sync

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/lockfile"
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
		Harness: map[string]config.Harness{
			"claude": {
				Path: targetDir,
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

func TestSyncConflictFileExists_Unmanaged(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	// Pre-create target file (NOT in lock file = user-created = conflict)
	skillDir := filepath.Join(targetDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("existing"), 0644)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	result, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	// New behavior: unmanaged files (not in lock file) are skipped
	if result.Skipped != 1 {
		t.Errorf("Skipped = %d, want 1 (unmanaged file)", result.Skipped)
	}

	// Verify file was NOT overwritten
	content, _ := os.ReadFile(filepath.Join(skillDir, "SKILL.md"))
	if string(content) != "existing" {
		t.Error("unmanaged file should not have been overwritten")
	}
}

func TestSyncConflictFileExists_Managed(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	// Pre-create target file
	skillDir := filepath.Join(targetDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("existing"), 0644)

	// Add to lock file (managed by tropos)
	lf := &lockfile.LockFile{
		Files: []lockfile.FileEntry{
			{Path: "skills/code-test/SKILL.md", Artifact: "code-test", Type: "skill"},
		},
	}
	lf.Save(targetDir)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	result, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	// Managed files are overwritten (no conflict)
	if result.Synced < 1 {
		t.Errorf("Synced = %d, want >= 1", result.Synced)
	}

	// Verify file was overwritten
	content, _ := os.ReadFile(filepath.Join(skillDir, "SKILL.md"))
	if string(content) == "existing" {
		t.Error("managed file should have been overwritten")
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

func TestSyncCreatesLockFile(t *testing.T) {
	srcDir, targetDir, cfg := setupSyncTest(t)

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	_, err := Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	// Verify lock file was created
	lf, err := lockfile.Load(targetDir)
	if err != nil {
		t.Fatalf("Failed to load lock file: %v", err)
	}

	if len(lf.Files) != 2 {
		t.Errorf("Lock file has %d entries, want 2", len(lf.Files))
	}

	// Verify entries
	if !lf.IsManaged("skills/code-test/SKILL.md") {
		t.Error("skills/code-test/SKILL.md should be in lock file")
	}
	if !lf.IsManaged("commands/spec.create/COMMAND.md") {
		t.Error("commands/spec.create/COMMAND.md should be in lock file")
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
	lf := &lockfile.LockFile{}
	conflicts := DetectFileConflicts(arts, "claude", harness, lf)

	if len(conflicts) != 1 {
		t.Errorf("DetectFileConflicts() = %d conflicts, want 1", len(conflicts))
	}
}

func TestDetectConflicts_ManagedFileNoConflict(t *testing.T) {
	arts := []*artifact.Artifact{
		{Name: "test", Type: artifact.TypeSkill},
	}

	targetDir := t.TempDir()
	skillDir := filepath.Join(targetDir, "skills", "test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte("existing"), 0644)

	harness := config.Harness{Path: targetDir}
	lf := &lockfile.LockFile{
		Files: []lockfile.FileEntry{
			{Path: "skills/test/SKILL.md", Artifact: "test", Type: "skill"},
		},
	}
	conflicts := DetectFileConflicts(arts, "claude", harness, lf)

	if len(conflicts) != 0 {
		t.Errorf("DetectFileConflicts() = %d conflicts, want 0 (file is managed)", len(conflicts))
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
