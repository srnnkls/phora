package sync_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/sync"
)

func TestIntegrationFullDeploy(t *testing.T) {
	// Setup directories
	srcDir := t.TempDir()
	claudeTarget := t.TempDir()
	opencodeTarget := t.TempDir()

	// Create source artifacts

	// 1. Directory-based skill with resources and templates
	skillDir := filepath.Join(srcDir, "skills", "code-test")
	os.MkdirAll(filepath.Join(skillDir, "reference"), 0755)

	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: code-test
description: "TDD workflow using {{.model_strong}}"
model: "{{.model_strong}}"
user-invocable: true
allowed_tools:
  - read
  - write
  - bash
---

# Test-Driven Development

Use {{.model_strong}} for complex reasoning tasks.
Use {{.model_weak}} for simple validations.

## Workflow

1. Write failing test (RED)
2. Write minimal code (GREEN)
3. Refactor
`), 0644)

	os.WriteFile(filepath.Join(skillDir, "reference", "best-practices.md"), []byte(`# TDD Best Practices

- Test behavior, not implementation
- One assertion per test
`), 0644)

	// 2. Single-file skill
	os.WriteFile(filepath.Join(srcDir, "skills", "simple.md"), []byte(`---
name: simple
description: A simple skill
---

# Simple Skill

Just a basic skill without resources.
`), 0644)

	// 3. Command
	cmdDir := filepath.Join(srcDir, "commands", "test.run")
	os.MkdirAll(cmdDir, 0755)
	os.WriteFile(filepath.Join(cmdDir, "COMMAND.md"), []byte(`---
name: test.run
description: "Run tests with {{.model_weak}}"
---

# Run Tests

Execute test suite using {{.model_weak}} for speed.
`), 0644)

	// 4. Agent
	agentsDir := filepath.Join(srcDir, "agents")
	os.MkdirAll(agentsDir, 0755)
	os.WriteFile(filepath.Join(agentsDir, "tester.md"), []byte(`---
name: tester
description: Test execution agent
model: "{{.model_weak}}"
---

# Tester Agent

Runs tests and reports results.
`), 0644)

	// Create config
	cfg := &config.Config{
		DefaultHarnesses: []string{"claude", "opencode"},
		DefaultArtifacts: []string{"skills", "commands", "agents"},
		Harness: map[string]config.Harness{
			"claude": {
				Path:                       claudeTarget,
				GenerateCommandsFromSkills: false,
				Variables: map[string]string{
					"model_strong": "opus",
					"model_weak":   "haiku",
				},
			},
			"opencode": {
				Path:                       opencodeTarget,
				
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

	// Run sync
	opts := sync.Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude", "opencode"},
	}

	result, err := sync.Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	// Verify counts
	// Claude: 2 skills + 1 command + 1 agent = 4
	// OpenCode: 2 skills + 1 command + 1 agent + 1 generated command = 5
	// Total synced should be 9
	expectedSynced := 9
	if result.Synced != expectedSynced {
		t.Errorf("Synced = %d, want %d", result.Synced, expectedSynced)
	}

	// OpenCode should generate 1 command from user-invocable skill
	if result.Generated != 1 {
		t.Errorf("Generated = %d, want 1", result.Generated)
	}

	// === Verify Claude target (nested structure) ===

	// Check skill with transformed variables
	claudeSkillPath := filepath.Join(claudeTarget, "skills", "code-test", "SKILL.md")
	claudeSkillContent, err := os.ReadFile(claudeSkillPath)
	if err != nil {
		t.Fatalf("Claude skill not written: %v", err)
	}

	claudeSkill := string(claudeSkillContent)
	if !strings.Contains(claudeSkill, "model: opus") {
		t.Error("Claude skill: model not transformed to 'opus'")
	}
	if !strings.Contains(claudeSkill, "Use opus for complex") {
		t.Error("Claude skill: body template not transformed")
	}
	if !strings.Contains(claudeSkill, "Use haiku for simple") {
		t.Error("Claude skill: model_weak not transformed")
	}

	// Check resources copied
	claudeRefPath := filepath.Join(claudeTarget, "skills", "code-test", "reference", "best-practices.md")
	if _, err := os.Stat(claudeRefPath); err != nil {
		t.Errorf("Claude skill reference not copied: %v", err)
	}

	// Check command
	claudeCmdPath := filepath.Join(claudeTarget, "commands", "test.run", "COMMAND.md")
	claudeCmdContent, err := os.ReadFile(claudeCmdPath)
	if err != nil {
		t.Fatalf("Claude command not written: %v", err)
	}
	if !strings.Contains(string(claudeCmdContent), "haiku") {
		t.Error("Claude command: template not transformed")
	}

	// Check agent
	claudeAgentPath := filepath.Join(claudeTarget, "agents", "tester", "AGENT.md")
	if _, err := os.Stat(claudeAgentPath); err != nil {
		t.Errorf("Claude agent not written: %v", err)
	}

	// === Verify OpenCode target (nested structure, same as Claude) ===

	// Check skill with transformed variables and mappings
	opencodeSkillPath := filepath.Join(opencodeTarget, "skills", "code-test", "SKILL.md")
	opencodeSkillContent, err := os.ReadFile(opencodeSkillPath)
	if err != nil {
		t.Fatalf("OpenCode skill not written: %v", err)
	}

	opencodeSkill := string(opencodeSkillContent)
	if !strings.Contains(opencodeSkill, "anthropic/claude-sonnet-4-5") {
		t.Error("OpenCode skill: model_strong not transformed")
	}
	if !strings.Contains(opencodeSkill, "anthropic/claude-haiku-4-5") {
		t.Error("OpenCode skill: model_weak not transformed")
	}
	// Check key mapping: allowed_tools -> tools
	if !strings.Contains(opencodeSkill, "tools:") {
		t.Error("OpenCode skill: allowed_tools not mapped to 'tools'")
	}
	if strings.Contains(opencodeSkill, "allowed_tools:") {
		t.Error("OpenCode skill: allowed_tools should be renamed to tools")
	}

	// Check resources copied
	opencodeRefPath := filepath.Join(opencodeTarget, "skills", "code-test", "reference", "best-practices.md")
	if _, err := os.Stat(opencodeRefPath); err != nil {
		t.Errorf("OpenCode skill reference not copied: %v", err)
	}

	// Check auto-generated command from user-invocable skill
	opencodeGenCmdPath := filepath.Join(opencodeTarget, "commands", "code-test", "COMMAND.md")
	opencodeGenCmdContent, err := os.ReadFile(opencodeGenCmdPath)
	if err != nil {
		t.Fatalf("OpenCode generated command not written: %v", err)
	}

	genCmd := string(opencodeGenCmdContent)
	if !strings.Contains(genCmd, "Invoke skill: code-test") {
		t.Error("Generated command missing invoke directive")
	}
	if !strings.Contains(genCmd, "name: code-test") {
		t.Error("Generated command missing name")
	}

	// Check agent
	opencodeAgentPath := filepath.Join(opencodeTarget, "agents", "tester", "AGENT.md")
	opencodeAgentContent, err := os.ReadFile(opencodeAgentPath)
	if err != nil {
		t.Fatalf("OpenCode agent not written: %v", err)
	}
	if !strings.Contains(string(opencodeAgentContent), "anthropic/claude-haiku-4-5") {
		t.Error("OpenCode agent: model not transformed")
	}
}

func TestIntegrationConflictResolution(t *testing.T) {
	srcDir := t.TempDir()
	targetDir := t.TempDir()

	// Create source skill
	skillDir := filepath.Join(srcDir, "skills", "existing")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: existing
description: New version
---

# New Content
`), 0644)

	// Pre-create target (conflict)
	targetSkillDir := filepath.Join(targetDir, "skills", "existing")
	os.MkdirAll(targetSkillDir, 0755)
	os.WriteFile(filepath.Join(targetSkillDir, "SKILL.md"), []byte("Old content"), 0644)

	cfg := &config.Config{
		DefaultHarnesses: []string{"test"},
		DefaultArtifacts: []string{"skills"},
		Harness: map[string]config.Harness{
			"test": {
				Path: targetDir,
			},
		},
	}

	// Without force, conflicts should be skipped
	opts := sync.Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"test"},
	}

	result, err := sync.Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}
	if result.Skipped != 1 {
		t.Errorf("Skipped = %d, want 1", result.Skipped)
	}

	// Verify old content unchanged
	content, _ := os.ReadFile(filepath.Join(targetSkillDir, "SKILL.md"))
	if string(content) != "Old content" {
		t.Error("File was modified despite conflict")
	}

	// Should succeed with force
	opts.Force = true
	result, err = sync.Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() with force error = %v", err)
	}
	if result.Synced != 1 {
		t.Errorf("Synced = %d, want 1", result.Synced)
	}

	// Verify new content
	content, _ = os.ReadFile(filepath.Join(targetSkillDir, "SKILL.md"))
	if !strings.Contains(string(content), "New Content") {
		t.Error("File not overwritten with force")
	}
}

func TestIntegrationDryRun(t *testing.T) {
	srcDir := t.TempDir()
	targetDir := t.TempDir()

	// Create source
	skillDir := filepath.Join(srcDir, "skills", "test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: test
---

# Test
`), 0644)

	cfg := &config.Config{
		DefaultHarnesses: []string{"test"},
		DefaultArtifacts: []string{"skills"},
		Harness: map[string]config.Harness{
			"test": {
				Path:      targetDir,
				
			},
		},
	}

	opts := sync.Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"test"},
		DryRun:      true,
	}

	result, err := sync.Sync(cfg, opts)
	if err != nil {
		t.Fatalf("Sync() dry-run error = %v", err)
	}

	if result.Synced != 1 {
		t.Errorf("Synced = %d, want 1 (planned)", result.Synced)
	}

	// Verify nothing written
	targetPath := filepath.Join(targetDir, "skills", "test", "SKILL.md")
	if _, err := os.Stat(targetPath); !os.IsNotExist(err) {
		t.Error("Dry-run should not write files")
	}
}
