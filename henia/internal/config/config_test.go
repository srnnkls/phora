package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadConfig(t *testing.T) {
	tmpDir := t.TempDir()

	configContent := `
artifacts = ["skills"]

[sources.my-source]
repo = "owner/repo"
ref = "main"

[harness.claude]
path = "~/.claude"
artifacts = ["skills", "commands"]
`
	configPath := filepath.Join(tmpDir, "henia.toml")
	os.WriteFile(configPath, []byte(configContent), 0644)

	cfg, err := Load(configPath)
	if err != nil {
		t.Fatalf("Load() error = %v", err)
	}

	if len(cfg.Sources) != 1 {
		t.Errorf("Sources count = %d, want 1", len(cfg.Sources))
	}

	src, ok := cfg.Sources["my-source"]
	if !ok {
		t.Fatal("Expected 'my-source' to exist")
	}

	if src.Repo != "owner/repo" {
		t.Errorf("Repo = %q, want 'owner/repo'", src.Repo)
	}

	if len(cfg.Harness) != 1 {
		t.Errorf("Harness count = %d, want 1", len(cfg.Harness))
	}

	harness, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("Expected 'claude' harness to exist")
	}

	if harness.Path != "~/.claude" {
		t.Errorf("Harness path = %q, want '~/.claude'", harness.Path)
	}
}

func TestLoadConfigMissing(t *testing.T) {
	_, err := Load("/nonexistent/path/henia.toml")
	if err == nil {
		t.Error("Expected error for missing config file")
	}
}

func TestLoadConfigEmpty(t *testing.T) {
	tmpDir := t.TempDir()

	configPath := filepath.Join(tmpDir, "henia.toml")
	os.WriteFile(configPath, []byte(""), 0644)

	cfg, err := Load(configPath)
	if err != nil {
		t.Fatalf("Load() error = %v", err)
	}

	if cfg.Sources == nil {
		t.Error("Sources should be initialized")
	}
	if cfg.Harness == nil {
		t.Error("Harness should be initialized")
	}
}
