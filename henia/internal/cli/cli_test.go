package cli

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

func TestExecuteRootCommand(t *testing.T) {
	out := &bytes.Buffer{}
	rootCmd.SetOut(out)
	rootCmd.SetArgs([]string{"--help"})

	err := rootCmd.Execute()
	if err != nil {
		t.Fatalf("Execute() error = %v", err)
	}

	output := out.String()
	if output == "" {
		t.Error("Expected help output, got empty string")
	}
}

func TestSyncCommandExists(t *testing.T) {
	found := false
	for _, cmd := range rootCmd.Commands() {
		if cmd.Use == "sync" {
			found = true
			break
		}
	}
	if !found {
		t.Error("Expected 'sync' command to exist")
	}
}

func TestDeployCommandExists(t *testing.T) {
	found := false
	for _, cmd := range rootCmd.Commands() {
		if cmd.Use == "deploy" {
			found = true
			break
		}
	}
	if !found {
		t.Error("Expected 'deploy' command to exist")
	}
}

func TestUpdateCommandExists(t *testing.T) {
	found := false
	for _, cmd := range rootCmd.Commands() {
		if cmd.Use == "update" {
			found = true
			break
		}
	}
	if !found {
		t.Error("Expected 'update' command to exist")
	}
}

func TestAddCommandExists(t *testing.T) {
	found := false
	for _, cmd := range rootCmd.Commands() {
		if cmd.Use == "add <repo>" {
			found = true
			break
		}
	}
	if !found {
		t.Error("Expected 'add <repo>' command to exist")
	}
}

func TestSyncCommandWithMockFetcher(t *testing.T) {
	tmpDir := t.TempDir()

	sourceDir := filepath.Join(tmpDir, "sources", "test-source")
	skillsDir := filepath.Join(sourceDir, "skills", "my-skill")
	os.MkdirAll(skillsDir, 0755)
	os.WriteFile(filepath.Join(skillsDir, "SKILL.md"), []byte(`---
name: my-skill
---
# My Skill
`), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	os.MkdirAll(targetDir, 0755)

	configContent := `
[sources.test-source]
repo = "owner/repo"

[harness.claude]
path = "` + targetDir + `"
artifacts = ["skills"]
`
	configPath := filepath.Join(tmpDir, "henia.toml")
	os.WriteFile(configPath, []byte(configContent), 0644)

	out := &bytes.Buffer{}
	rootCmd.SetOut(out)
	rootCmd.SetArgs([]string{"deploy", "--config", configPath, "--data-dir", filepath.Join(tmpDir, "sources")})

	err := rootCmd.Execute()
	if err != nil {
		t.Fatalf("Execute() error = %v", err)
	}

	skillPath := filepath.Join(targetDir, "skills", "my-skill", "SKILL.md")
	if _, err := os.Stat(skillPath); os.IsNotExist(err) {
		t.Errorf("Expected skill to be synced at %s", skillPath)
	}
}

func TestDeployCommandWithConfig(t *testing.T) {
	tmpDir := t.TempDir()

	sourceDir := filepath.Join(tmpDir, "sources", "test-source")
	skillsDir := filepath.Join(sourceDir, "skills", "my-skill")
	os.MkdirAll(skillsDir, 0755)
	os.WriteFile(filepath.Join(skillsDir, "SKILL.md"), []byte(`---
name: my-skill
---
# My Skill
`), 0644)

	targetDir := filepath.Join(tmpDir, "target")
	os.MkdirAll(targetDir, 0755)

	configContent := `
[sources.test-source]
repo = "owner/repo"

[harness.claude]
path = "` + targetDir + `"
artifacts = ["skills"]
`
	configPath := filepath.Join(tmpDir, "henia.toml")
	os.WriteFile(configPath, []byte(configContent), 0644)

	out := &bytes.Buffer{}
	rootCmd.SetOut(out)
	rootCmd.SetArgs([]string{"deploy", "--config", configPath, "--data-dir", filepath.Join(tmpDir, "sources")})

	err := rootCmd.Execute()
	if err != nil {
		t.Fatalf("Execute() error = %v", err)
	}

	skillPath := filepath.Join(targetDir, "skills", "my-skill", "SKILL.md")
	if _, err := os.Stat(skillPath); os.IsNotExist(err) {
		t.Errorf("Expected skill to be deployed at %s", skillPath)
	}
}
