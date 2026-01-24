package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWriteFile(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "phora.toml")

	cfg := &Config{
		Artifacts: []string{"skills"},
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

	if len(loaded.Artifacts) != 1 || loaded.Artifacts[0] != "skills" {
		t.Errorf("Artifacts not preserved")
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
	path := filepath.Join(tmpDir, "phora.toml")

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
	path := filepath.Join(tmpDir, "phora.toml")

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
	path := filepath.Join(tmpDir, "new-phora.toml")

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
	path := filepath.Join(tmpDir, "phora.toml")

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

func TestAddSource(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "phora.toml")

	cfg := &Config{
		Harness: map[string]Harness{},
	}
	WriteFile(path, cfg)

	src := Source{Git: "https://github.com/owner/repo.git", Path: "skills", Branch: "main"}
	err := AddSource(path, "owner/repo", src)
	if err != nil {
		t.Fatalf("AddSource() error = %v", err)
	}

	loaded, _ := LoadFile(path)
	if len(loaded.Sources) != 1 {
		t.Fatalf("Sources length = %d, want 1", len(loaded.Sources))
	}
	if loaded.Sources["owner/repo"].Git != "https://github.com/owner/repo.git" {
		t.Errorf("Source.Git = %q, want %q", loaded.Sources["owner/repo"].Git, "https://github.com/owner/repo.git")
	}
	if loaded.Sources["owner/repo"].Path != "skills" {
		t.Errorf("Source.Path = %q, want %q", loaded.Sources["owner/repo"].Path, "skills")
	}
}

func TestAddSourceOverride(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "phora.toml")

	cfg := &Config{
		Sources: map[string]Source{
			"owner/repo": {Git: "https://github.com/owner/repo.git", Path: "skills"},
		},
		Harness: map[string]Harness{},
	}
	WriteFile(path, cfg)

	// Adding same name should override
	src := Source{Git: "https://github.com/owner/repo.git", Path: "skills", Branch: "v2"}
	err := AddSource(path, "owner/repo", src)
	if err != nil {
		t.Fatalf("AddSource() error = %v", err)
	}

	loaded, _ := LoadFile(path)
	if len(loaded.Sources) != 1 {
		t.Errorf("Source count = %d, want 1", len(loaded.Sources))
	}
	if loaded.Sources["owner/repo"].Branch != "v2" {
		t.Errorf("Source.Branch = %q, want %q", loaded.Sources["owner/repo"].Branch, "v2")
	}
}

func TestAddSourceNewFile(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "new-config.toml")

	src := Source{Git: "https://github.com/owner/repo.git"}
	err := AddSource(path, "owner/repo", src)
	if err != nil {
		t.Fatalf("AddSource() on new file error = %v", err)
	}

	if _, err := os.Stat(path); os.IsNotExist(err) {
		t.Error("Config file was not created")
	}

	loaded, _ := LoadFile(path)
	if len(loaded.Sources) != 1 {
		t.Error("Source not saved to new file")
	}
}

func TestAddSource_RejectsEmptyGit(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "phora.toml")

	cfg := &Config{
		Version: 1,
		Harness: map[string]Harness{},
	}
	WriteFile(path, cfg)

	src := Source{Path: "skills", Branch: "main"}
	err := AddSource(path, "test-source", src)
	if err == nil {
		t.Fatal("AddSource() should return error when Git field is empty")
	}

	if _, err := LoadFile(path); err == nil {
		loaded, _ := LoadFile(path)
		if _, exists := loaded.Sources["test-source"]; exists {
			t.Error("Source with empty Git field should not be saved")
		}
	}
}

func TestAddSource_RejectsLegacyFormat(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "phora.toml")

	cfg := &Config{
		Version: 1,
		Harness: map[string]Harness{},
	}
	WriteFile(path, cfg)

	src := Source{
		Host:   "github.com",
		Owner:  "owner",
		Repo:   "repo",
		Branch: "main",
	}
	err := AddSource(path, "legacy-source", src)
	if err == nil {
		t.Fatal("AddSource() should return error for legacy format (Host/Owner/Repo without Git)")
	}

	loaded, loadErr := LoadFile(path)
	if loadErr == nil {
		if _, exists := loaded.Sources["legacy-source"]; exists {
			t.Error("Source with legacy format should not be saved")
		}
	}
}

func TestAddSource_AcceptsValidGit(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "phora.toml")

	cfg := &Config{
		Version: 1,
		Harness: map[string]Harness{},
	}
	WriteFile(path, cfg)

	src := Source{
		Git:    "https://github.com/owner/repo.git",
		Branch: "main",
		Path:   "skills",
	}
	err := AddSource(path, "valid-source", src)
	if err != nil {
		t.Fatalf("AddSource() with valid Git field should succeed, got error: %v", err)
	}

	loaded, err := LoadFile(path)
	if err != nil {
		t.Fatalf("LoadFile() error = %v", err)
	}

	savedSrc, exists := loaded.Sources["valid-source"]
	if !exists {
		t.Fatal("Source with valid Git field should be saved")
	}
	if savedSrc.Git != "https://github.com/owner/repo.git" {
		t.Errorf("Source.Git = %q, want %q", savedSrc.Git, "https://github.com/owner/repo.git")
	}
	if savedSrc.Branch != "main" {
		t.Errorf("Source.Branch = %q, want %q", savedSrc.Branch, "main")
	}
}
