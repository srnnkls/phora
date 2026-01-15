package phora

import (
	"os"
	"path/filepath"
	"testing"
)

func TestNewPathMap(t *testing.T) {
	exports := map[string]string{
		"skills/internal/my-skill": "my-skill",
	}
	rewrites := map[string]string{
		"my-skill": "custom/location/my-skill",
	}

	pm := NewPathMap(exports, rewrites)

	if pm == nil {
		t.Fatal("NewPathMap returned nil")
	}
	if pm.Exports == nil {
		t.Error("Exports should not be nil")
	}
	if pm.Rewrites == nil {
		t.Error("Rewrites should not be nil")
	}
}

func TestPathMap_ResolveExport_Found(t *testing.T) {
	exports := map[string]string{
		"skills/internal/my-skill": "my-skill",
		"skills/internal/other":    "other-skill",
	}
	pm := NewPathMap(exports, nil)

	got := pm.ResolveExport("skills/internal/my-skill")
	want := "my-skill"
	if got != want {
		t.Errorf("ResolveExport() = %q, want %q", got, want)
	}
}

func TestPathMap_ResolveExport_NotFound(t *testing.T) {
	exports := map[string]string{
		"skills/internal/my-skill": "my-skill",
	}
	pm := NewPathMap(exports, nil)

	got := pm.ResolveExport("unknown/path")
	want := "unknown/path"
	if got != want {
		t.Errorf("ResolveExport() for unknown path = %q, want original %q", got, want)
	}
}

func TestPathMap_ResolveExport_NilExports(t *testing.T) {
	pm := NewPathMap(nil, nil)

	got := pm.ResolveExport("any/path")
	want := "any/path"
	if got != want {
		t.Errorf("ResolveExport() with nil exports = %q, want original %q", got, want)
	}
}

func TestPathMap_Resolve_ExportsOnly(t *testing.T) {
	exports := map[string]string{
		"skills/internal/my-skill": "my-skill",
	}
	pm := NewPathMap(exports, nil)

	got := pm.Resolve("skills/internal/my-skill")
	want := "my-skill"
	if got != want {
		t.Errorf("Resolve() with exports only = %q, want %q", got, want)
	}
}

func TestPathMap_Resolve_RewritesOnly(t *testing.T) {
	rewrites := map[string]string{
		"my-skill": "custom/location/my-skill",
	}
	pm := NewPathMap(nil, rewrites)

	got := pm.Resolve("my-skill")
	want := "custom/location/my-skill"
	if got != want {
		t.Errorf("Resolve() with rewrites only = %q, want %q", got, want)
	}
}

func TestPathMap_Resolve_BothMappings(t *testing.T) {
	exports := map[string]string{
		"skills/internal/my-skill": "my-skill",
	}
	rewrites := map[string]string{
		"my-skill": "custom/location/my-skill",
	}
	pm := NewPathMap(exports, rewrites)

	got := pm.Resolve("skills/internal/my-skill")
	want := "custom/location/my-skill"
	if got != want {
		t.Errorf("Resolve() with both mappings = %q, want %q", got, want)
	}
}

func TestPathMap_Resolve_RewriteOverridesExport(t *testing.T) {
	exports := map[string]string{
		"internal/skill-a": "skill-a",
	}
	rewrites := map[string]string{
		"skill-a": "overridden/path",
	}
	pm := NewPathMap(exports, rewrites)

	got := pm.Resolve("internal/skill-a")
	want := "overridden/path"
	if got != want {
		t.Errorf("Resolve() rewrite should override export: got %q, want %q", got, want)
	}
}

func TestPathMap_Resolve_NoMapping(t *testing.T) {
	exports := map[string]string{
		"other/path": "other",
	}
	rewrites := map[string]string{
		"other": "somewhere/else",
	}
	pm := NewPathMap(exports, rewrites)

	got := pm.Resolve("unmapped/path")
	want := "unmapped/path"
	if got != want {
		t.Errorf("Resolve() with no matching mapping = %q, want original %q", got, want)
	}
}

func TestPathMap_Resolve_ChainedMapping(t *testing.T) {
	exports := map[string]string{
		"deep/nested/internal/skill": "skill",
	}
	rewrites := map[string]string{
		"skill": "my-custom/skill-location",
	}
	pm := NewPathMap(exports, rewrites)

	got := pm.Resolve("deep/nested/internal/skill")
	want := "my-custom/skill-location"
	if got != want {
		t.Errorf("Resolve() chained mapping = %q, want %q", got, want)
	}
}

func TestManifest_Fields(t *testing.T) {
	m := Manifest{
		Name: "my-source",
		Exports: map[string]string{
			"skills/internal/my-skill": "my-skill",
		},
	}

	if m.Name != "my-source" {
		t.Errorf("Name = %q, want %q", m.Name, "my-source")
	}
	if m.Exports == nil {
		t.Error("Exports should not be nil")
	}
	if m.Exports["skills/internal/my-skill"] != "my-skill" {
		t.Errorf("Exports mapping incorrect")
	}
}

func TestManifest_ZeroValue(t *testing.T) {
	var m Manifest

	if m.Name != "" {
		t.Error("zero value Name should be empty")
	}
	if m.Exports != nil {
		t.Error("zero value Exports should be nil")
	}
}

func TestLoadManifest_FromPhoraTOML(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "phora.toml")

	content := `name = "my-source"

[exports]
"skills/internal/my-skill" = "my-skill"
"skills/internal/other" = "other-skill"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test phora.toml: %v", err)
	}

	m, err := LoadManifest(dir)
	if err != nil {
		t.Fatalf("LoadManifest() error: %v", err)
	}

	if m == nil {
		t.Fatal("LoadManifest() returned nil")
	}
	if m.Name != "my-source" {
		t.Errorf("Name = %q, want %q", m.Name, "my-source")
	}
	if len(m.Exports) != 2 {
		t.Errorf("Exports count = %d, want 2", len(m.Exports))
	}
	if m.Exports["skills/internal/my-skill"] != "my-skill" {
		t.Errorf("Exports mapping incorrect for my-skill")
	}
	if m.Exports["skills/internal/other"] != "other-skill" {
		t.Errorf("Exports mapping incorrect for other")
	}
}

func TestLoadManifest_FileNotFound(t *testing.T) {
	dir := t.TempDir()

	_, err := LoadManifest(dir)
	if err == nil {
		t.Error("LoadManifest() should return error when phora.toml not found")
	}
}

func TestLoadManifest_MalformedTOML(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "phora.toml")

	content := `[exports
invalid toml syntax`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test file: %v", err)
	}

	_, err := LoadManifest(dir)
	if err == nil {
		t.Error("LoadManifest() should return error for malformed TOML")
	}
}

func TestLoadManifest_EmptyExports(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "phora.toml")

	content := `name = "empty-source"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test file: %v", err)
	}

	m, err := LoadManifest(dir)
	if err != nil {
		t.Fatalf("LoadManifest() error: %v", err)
	}

	if m.Name != "empty-source" {
		t.Errorf("Name = %q, want %q", m.Name, "empty-source")
	}
	if m.Exports == nil {
		m.Exports = map[string]string{}
	}
	if len(m.Exports) != 0 {
		t.Errorf("Exports should be empty, got %d", len(m.Exports))
	}
}

func TestPathMap_EmptyMaps(t *testing.T) {
	pm := NewPathMap(map[string]string{}, map[string]string{})

	got := pm.Resolve("any/path")
	want := "any/path"
	if got != want {
		t.Errorf("Resolve() with empty maps = %q, want original %q", got, want)
	}
}
