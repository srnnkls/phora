package sync

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/phora/internal/config"
)

// =============================================================================
// Behavioral Tests for v1 Config Redesign (M2)
// =============================================================================
// These tests verify the v1 behavioral change: ALL sources are namespaced,
// there is no "global" source concept. The Global field is removed.

// Test that former "global" sources ALSO get namespaced in v1.
// The Global:true flag should have no special effect - it doesn't exist in v1.
//
// EXPECTED: This test FAILS with current implementation because Global:true
// causes empty namespace. After the fix, all sources should be namespaced.
func TestDetect_V1_FormerGlobalSourceNowNamespaced(t *testing.T) {
	srcDir := t.TempDir()
	targetDir := t.TempDir()

	skillDir := filepath.Join(srcDir, "skills", "test-skill")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: test-skill
---

# Test
`), 0644)

	cfg := &config.Config{
		Artifacts: []string{"skills"},
		Sources: map[string]config.Source{
			"shared": {
				Path: srcDir,
			},
		},
		Harness: map[string]config.Harness{
			"claude": {
				Path: targetDir,
			},
		},
	}

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	results, err := Detect(cfg, opts)
	if err != nil {
		t.Fatalf("Detect() error = %v", err)
	}

	if len(results) != 1 || len(results[0].Artifacts) != 1 {
		t.Fatalf("expected 1 artifact, got %d results", len(results))
	}

	art := results[0].Artifacts[0]

	// In v1, ALL sources namespace - even formerly "global" ones
	// This test FAILS with current implementation because Global:true causes empty namespace
	if art.Namespace != "shared" {
		t.Errorf("Namespace = %q, want %q (v1: all sources namespaced, no global concept)",
			art.Namespace, "shared")
	}
	if art.FullName() != "shared.test-skill" {
		t.Errorf("FullName() = %q, want %q", art.FullName(), "shared.test-skill")
	}
}

// Test that source without config entry still works (no namespace applied)
func TestDetect_UnknownSource_NoNamespace(t *testing.T) {
	srcDir := t.TempDir()
	targetDir := t.TempDir()

	skillDir := filepath.Join(srcDir, "skills", "orphan-skill")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: orphan-skill
---

# Orphan
`), 0644)

	cfg := &config.Config{
		Artifacts: []string{"skills"},
		// No source entry for srcDir - it's an ad-hoc path not in config
		Sources: map[string]config.Source{},
		Harness: map[string]config.Harness{
			"claude": {
				Path: targetDir,
			},
		},
	}

	opts := Options{
		SourcePaths: []string{srcDir},
		Targets:     []string{"claude"},
	}

	results, err := Detect(cfg, opts)
	if err != nil {
		t.Fatalf("Detect() error = %v", err)
	}

	if len(results) != 1 || len(results[0].Artifacts) != 1 {
		t.Fatalf("expected 1 artifact")
	}

	art := results[0].Artifacts[0]

	// Source not in config - no namespace applied (empty)
	if art.Namespace != "" {
		t.Errorf("Namespace = %q, want empty (unknown source)", art.Namespace)
	}
	if art.FullName() != "orphan-skill" {
		t.Errorf("FullName() = %q, want %q", art.FullName(), "orphan-skill")
	}
}

// =============================================================================
// Tests for findSourceByPath signature change (COMPILE-TIME FAILURE)
// =============================================================================
// These tests verify the function signature change from:
//   func findSourceByPath(cfg *config.Config, srcPath string) (name string, isGlobal bool)
// to:
//   func findSourceByPath(cfg *config.Config, srcPath string) string
//
// Uncomment these tests AFTER removing the Global field and isGlobal return value.
// They will fail to compile until that change is made.

/*
func TestFindSourceByPath_ReturnsOnlyName(t *testing.T) {
	srcDir := t.TempDir()

	cfg := &config.Config{
		Sources: map[string]config.Source{
			"company": {
				Type: "local",
				Path: srcDir,
			},
		},
	}

	name := findSourceByPath(cfg, srcDir)

	if name != "company" {
		t.Errorf("findSourceByPath() = %q, want %q", name, "company")
	}
}

func TestFindSourceByPath_NotFound_ReturnsEmpty(t *testing.T) {
	srcDir := t.TempDir()
	otherDir := t.TempDir()

	cfg := &config.Config{
		Sources: map[string]config.Source{
			"company": {
				Type: "local",
				Path: srcDir,
			},
		},
	}

	name := findSourceByPath(cfg, otherDir)

	if name != "" {
		t.Errorf("findSourceByPath() for unknown path = %q, want empty string", name)
	}
}

func TestFindSourceByPath_ExpandsPath(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("cannot get home directory")
	}

	tempDir, err := os.MkdirTemp(home, "phora-test-*")
	if err != nil {
		t.Skip("cannot create temp dir in home")
	}
	defer os.RemoveAll(tempDir)

	relPath := filepath.Base(tempDir)
	tildeNotation := "~/" + relPath

	cfg := &config.Config{
		Sources: map[string]config.Source{
			"home-source": {
				Type: "local",
				Path: tildeNotation,
			},
		},
	}

	name := findSourceByPath(cfg, tempDir)

	if name != "home-source" {
		t.Errorf("findSourceByPath() with expanded path = %q, want %q", name, "home-source")
	}
}
*/
