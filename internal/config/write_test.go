package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWriteFile(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "tropos.toml")

	cfg := &Config{
		DefaultHarnesses: []string{"claude"},
		Harness: map[string]Harness{
			"claude": {
				Path:    "~/.claude",
				Exclude: []string{"old-skill"},
			},
		},
	}

	err := WriteFile(path, cfg)
	if err != nil {
		t.Fatalf("WriteFile() error = %v", err)
	}

	// Verify file exists and can be loaded
	loaded, err := LoadFile(path)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	if len(loaded.DefaultHarnesses) != 1 || loaded.DefaultHarnesses[0] != "claude" {
		t.Errorf("DefaultHarnesses not preserved")
	}

	harness := loaded.Harness["claude"]
	if harness.Path != "~/.claude" {
		t.Errorf("Harness.Path = %q, want %q", harness.Path, "~/.claude")
	}
	if len(harness.Exclude) != 1 || harness.Exclude[0] != "old-skill" {
		t.Errorf("Harness.Exclude = %v, want [old-skill]", harness.Exclude)
	}
}

func TestAddExclusion(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "tropos.toml")

	// Create initial config
	cfg := &Config{
		Harness: map[string]Harness{
			"claude": {
				Path: "~/.claude",
			},
		},
	}
	WriteFile(path, cfg)

	// Add exclusion
	err := AddExclusion(path, "claude", "test-skill")
	if err != nil {
		t.Fatalf("AddExclusion() error = %v", err)
	}

	// Verify
	loaded, _ := LoadFile(path)
	harness := loaded.Harness["claude"]
	if len(harness.Exclude) != 1 || harness.Exclude[0] != "test-skill" {
		t.Errorf("Exclude = %v, want [test-skill]", harness.Exclude)
	}

	// Add another exclusion
	AddExclusion(path, "claude", "another-skill")
	loaded, _ = LoadFile(path)
	harness = loaded.Harness["claude"]
	if len(harness.Exclude) != 2 {
		t.Errorf("Exclude length = %d, want 2", len(harness.Exclude))
	}

	// Adding same exclusion again should be idempotent
	AddExclusion(path, "claude", "test-skill")
	loaded, _ = LoadFile(path)
	harness = loaded.Harness["claude"]
	if len(harness.Exclude) != 2 {
		t.Errorf("Duplicate exclusion added, length = %d, want 2", len(harness.Exclude))
	}
}

func TestAddExclusionNewHarness(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "tropos.toml")

	// Start with empty config
	cfg := &Config{
		Harness: map[string]Harness{},
	}
	WriteFile(path, cfg)

	// Add exclusion to non-existent harness
	err := AddExclusion(path, "newharness", "skill")
	if err != nil {
		t.Fatalf("AddExclusion() error = %v", err)
	}

	loaded, _ := LoadFile(path)
	harness := loaded.Harness["newharness"]
	if len(harness.Exclude) != 1 || harness.Exclude[0] != "skill" {
		t.Errorf("New harness exclusion not added correctly")
	}
}

func TestAddExclusionNewFile(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "new-tropos.toml")

	// File doesn't exist yet
	err := AddExclusion(path, "claude", "skill")
	if err != nil {
		t.Fatalf("AddExclusion() on new file error = %v", err)
	}

	// Verify file was created
	if _, err := os.Stat(path); os.IsNotExist(err) {
		t.Error("Config file was not created")
	}

	loaded, _ := LoadFile(path)
	if len(loaded.Harness["claude"].Exclude) != 1 {
		t.Error("Exclusion not saved to new file")
	}
}

func TestRemoveExclusion(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "tropos.toml")

	cfg := &Config{
		Harness: map[string]Harness{
			"claude": {
				Exclude: []string{"a", "b", "c"},
			},
		},
	}
	WriteFile(path, cfg)

	err := RemoveExclusion(path, "claude", "b")
	if err != nil {
		t.Fatalf("RemoveExclusion() error = %v", err)
	}

	loaded, _ := LoadFile(path)
	harness := loaded.Harness["claude"]
	if len(harness.Exclude) != 2 {
		t.Errorf("Exclude length = %d, want 2", len(harness.Exclude))
	}
	for _, exc := range harness.Exclude {
		if exc == "b" {
			t.Error("Exclusion 'b' was not removed")
		}
	}
}
