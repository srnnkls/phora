package sync

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/henia"
	"github.com/srnnkls/henia/internal/artifact"
	"github.com/srnnkls/phora"
)

type mockPhoraClient struct {
	fetchAllFunc func() ([]phora.FetchResult, error)
}

func (m *mockPhoraClient) FetchAll() ([]phora.FetchResult, error) {
	return m.fetchAllFunc()
}

func TestSyncerSync(t *testing.T) {
	tmpDir := t.TempDir()

	sourceDir := filepath.Join(tmpDir, "source")
	skillsDir := filepath.Join(sourceDir, "skills", "code-test")
	os.MkdirAll(skillsDir, 0755)
	os.WriteFile(filepath.Join(skillsDir, "SKILL.md"), []byte(`---
name: code-test
description: TDD workflow
---

# Code Test

Write tests first.
`), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	os.MkdirAll(targetDir, 0755)

	client := &mockPhoraClient{
		fetchAllFunc: func() ([]phora.FetchResult, error) {
			return []phora.FetchResult{
				{
					Name:      "test-source",
					LocalPath: sourceDir,
					Commit:    "abc123",
					Files:     []string{"skills/code-test/SKILL.md"},
				},
			}, nil
		},
	}

	harnesses := map[string]henia.Harness{
		"claude": {
			Path:      targetDir,
			Artifacts: []string{"skills"},
		},
	}

	syncer := NewSyncer(client, harnesses)

	result, err := syncer.Sync()
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	if result.Synced != 1 {
		t.Errorf("Synced = %d, want 1", result.Synced)
	}

	targetPath := filepath.Join(targetDir, "skills", "code-test", "SKILL.md")
	if _, err := os.Stat(targetPath); os.IsNotExist(err) {
		t.Errorf("Expected file %s to exist", targetPath)
	}
}

func TestSyncerDeploy(t *testing.T) {
	tmpDir := t.TempDir()

	sourceDir := filepath.Join(tmpDir, "source")
	skillsDir := filepath.Join(sourceDir, "skills")
	os.MkdirAll(skillsDir, 0755)
	os.WriteFile(filepath.Join(skillsDir, "simple.md"), []byte(`---
name: simple
---

# Simple Skill
`), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	os.MkdirAll(targetDir, 0755)

	harnesses := map[string]henia.Harness{
		"claude": {
			Path:      targetDir,
			Artifacts: []string{"skills"},
		},
	}

	sources := []FetchedSource{
		{
			Name:      "test-source",
			LocalPath: sourceDir,
		},
	}

	syncer := NewSyncer(nil, harnesses)

	result, err := syncer.Deploy(sources)
	if err != nil {
		t.Fatalf("Deploy() error = %v", err)
	}

	if result.Synced != 1 {
		t.Errorf("Synced = %d, want 1", result.Synced)
	}
}

func TestSyncerTransformApplied(t *testing.T) {
	tmpDir := t.TempDir()

	sourceDir := filepath.Join(tmpDir, "source")
	skillsDir := filepath.Join(sourceDir, "skills")
	os.MkdirAll(skillsDir, 0755)
	os.WriteFile(filepath.Join(skillsDir, "test.md"), []byte(`---
name: test
model: strong
---

# Test
`), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	os.MkdirAll(targetDir, 0755)

	harnesses := map[string]henia.Harness{
		"claude": {
			Path:      targetDir,
			Artifacts: []string{"skills"},
			Keys: map[string]string{
				"model": "model_preference",
			},
			Values: map[string]map[string]string{
				"model_preference": {
					"strong": "opus",
				},
			},
		},
	}

	sources := []FetchedSource{
		{
			Name:      "test-source",
			LocalPath: sourceDir,
		},
	}

	syncer := NewSyncer(nil, harnesses)

	result, err := syncer.Deploy(sources)
	if err != nil {
		t.Fatalf("Deploy() error = %v", err)
	}

	if result.Synced != 1 {
		t.Errorf("Synced = %d, want 1", result.Synced)
	}

	targetPath := filepath.Join(targetDir, "skills", "test", "SKILL.md")
	data, err := os.ReadFile(targetPath)
	if err != nil {
		t.Fatalf("ReadFile() error = %v", err)
	}

	content := string(data)
	art, err := artifact.Parse(data)
	if err != nil {
		t.Fatalf("Parse() error = %v", err)
	}

	if art.Frontmatter["model_preference"] != "opus" {
		t.Errorf("model_preference = %v, want opus (content: %s)", art.Frontmatter["model_preference"], content)
	}
}

func TestSyncerFiltersByHarnessArtifacts(t *testing.T) {
	tmpDir := t.TempDir()

	sourceDir := filepath.Join(tmpDir, "source")

	skillsDir := filepath.Join(sourceDir, "skills")
	os.MkdirAll(skillsDir, 0755)
	os.WriteFile(filepath.Join(skillsDir, "my-skill.md"), []byte(`---
name: my-skill
---
# My Skill
`), 0644)

	commandsDir := filepath.Join(sourceDir, "commands")
	os.MkdirAll(commandsDir, 0755)
	os.WriteFile(filepath.Join(commandsDir, "my-command.md"), []byte(`---
name: my-command
---
# My Command
`), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	os.MkdirAll(targetDir, 0755)

	harnesses := map[string]henia.Harness{
		"skills-only": {
			Path:      targetDir,
			Artifacts: []string{"skills"},
		},
	}

	sources := []FetchedSource{
		{
			Name:      "test-source",
			LocalPath: sourceDir,
		},
	}

	syncer := NewSyncer(nil, harnesses)

	result, err := syncer.Deploy(sources)
	if err != nil {
		t.Fatalf("Deploy() error = %v", err)
	}

	if result.Synced != 1 {
		t.Errorf("Synced = %d, want 1 (only skill)", result.Synced)
	}

	skillPath := filepath.Join(targetDir, "skills", "my-skill", "SKILL.md")
	if _, err := os.Stat(skillPath); os.IsNotExist(err) {
		t.Error("Expected skill to be synced")
	}

	commandPath := filepath.Join(targetDir, "commands", "my-command", "COMMAND.md")
	if _, err := os.Stat(commandPath); !os.IsNotExist(err) {
		t.Error("Expected command to NOT be synced (harness only wants skills)")
	}
}
