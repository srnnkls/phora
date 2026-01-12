package lockfile

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoad_NonExistent(t *testing.T) {
	lf, err := Load("/nonexistent/path")
	if err != nil {
		t.Fatalf("Load should not error for nonexistent path: %v", err)
	}
	if len(lf.Files) != 0 {
		t.Errorf("expected empty Files, got %d entries", len(lf.Files))
	}
}

func TestLoad_ExistingFile(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, ".tropos.lock")

	content := `[[files]]
path = "skills/code-test/SKILL.md"
checksum = "sha256:abc123"
artifact = "code-test"
type = "skill"

[[files]]
path = "skills/code-test/ref.md"
checksum = "sha256:def456"
artifact = "code-test"
type = "skill"
resource = true
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	lf, err := Load(dir)
	if err != nil {
		t.Fatalf("Load failed: %v", err)
	}
	if len(lf.Files) != 2 {
		t.Fatalf("expected 2 files, got %d", len(lf.Files))
	}

	f := lf.Files[0]
	if f.Path != "skills/code-test/SKILL.md" {
		t.Errorf("unexpected path: %s", f.Path)
	}
	if f.Checksum != "sha256:abc123" {
		t.Errorf("unexpected checksum: %s", f.Checksum)
	}
	if f.Artifact != "code-test" {
		t.Errorf("unexpected artifact: %s", f.Artifact)
	}
	if f.Type != "skill" {
		t.Errorf("unexpected type: %s", f.Type)
	}
	if f.Resource {
		t.Error("expected resource=false for main file")
	}

	f2 := lf.Files[1]
	if !f2.Resource {
		t.Error("expected resource=true for resource file")
	}
}

func TestSave(t *testing.T) {
	dir := t.TempDir()

	lf := &LockFile{
		Files: []FileEntry{
			{
				Path:     "commands/test/COMMAND.md",
				Checksum: "sha256:xyz789",
				Artifact: "test",
				Type:     "command",
			},
		},
	}

	if err := lf.Save(dir); err != nil {
		t.Fatalf("Save failed: %v", err)
	}

	loaded, err := Load(dir)
	if err != nil {
		t.Fatalf("Load after save failed: %v", err)
	}
	if len(loaded.Files) != 1 {
		t.Fatalf("expected 1 file, got %d", len(loaded.Files))
	}
	if loaded.Files[0].Path != "commands/test/COMMAND.md" {
		t.Errorf("unexpected path after roundtrip: %s", loaded.Files[0].Path)
	}
}

func TestIsManaged(t *testing.T) {
	lf := &LockFile{
		Files: []FileEntry{
			{Path: "skills/code-test/SKILL.md"},
			{Path: "skills/code-test/ref.md"},
		},
	}

	tests := []struct {
		path     string
		expected bool
	}{
		{"skills/code-test/SKILL.md", true},
		{"skills/code-test/ref.md", true},
		{"skills/other/SKILL.md", false},
		{"commands/test/COMMAND.md", false},
	}

	for _, tc := range tests {
		if got := lf.IsManaged(tc.path); got != tc.expected {
			t.Errorf("IsManaged(%q) = %v, want %v", tc.path, got, tc.expected)
		}
	}
}

func TestAdd_NewEntry(t *testing.T) {
	lf := &LockFile{}

	lf.Add(FileEntry{
		Path:     "skills/new/SKILL.md",
		Checksum: "sha256:new123",
		Artifact: "new",
		Type:     "skill",
	})

	if len(lf.Files) != 1 {
		t.Fatalf("expected 1 file, got %d", len(lf.Files))
	}
	if lf.Files[0].Path != "skills/new/SKILL.md" {
		t.Errorf("unexpected path: %s", lf.Files[0].Path)
	}
}

func TestAdd_UpdateExisting(t *testing.T) {
	lf := &LockFile{
		Files: []FileEntry{
			{
				Path:     "skills/existing/SKILL.md",
				Checksum: "sha256:old",
				Artifact: "existing",
				Type:     "skill",
			},
		},
	}

	lf.Add(FileEntry{
		Path:     "skills/existing/SKILL.md",
		Checksum: "sha256:updated",
		Artifact: "existing",
		Type:     "skill",
	})

	if len(lf.Files) != 1 {
		t.Fatalf("expected 1 file after update, got %d", len(lf.Files))
	}
	if lf.Files[0].Checksum != "sha256:updated" {
		t.Errorf("checksum not updated: %s", lf.Files[0].Checksum)
	}
}

func TestRemove(t *testing.T) {
	lf := &LockFile{
		Files: []FileEntry{
			{Path: "skills/a/SKILL.md"},
			{Path: "skills/b/SKILL.md"},
			{Path: "skills/c/SKILL.md"},
		},
	}

	lf.Remove("skills/b/SKILL.md")

	if len(lf.Files) != 2 {
		t.Fatalf("expected 2 files after remove, got %d", len(lf.Files))
	}
	if lf.IsManaged("skills/b/SKILL.md") {
		t.Error("skills/b/SKILL.md should have been removed")
	}
}

func TestGetByArtifact(t *testing.T) {
	lf := &LockFile{
		Files: []FileEntry{
			{Path: "skills/code-test/SKILL.md", Artifact: "code-test"},
			{Path: "skills/code-test/ref.md", Artifact: "code-test", Resource: true},
			{Path: "skills/other/SKILL.md", Artifact: "other"},
		},
	}

	entries := lf.GetByArtifact("code-test")
	if len(entries) != 2 {
		t.Fatalf("expected 2 entries for code-test, got %d", len(entries))
	}

	entries = lf.GetByArtifact("nonexistent")
	if len(entries) != 0 {
		t.Errorf("expected 0 entries for nonexistent, got %d", len(entries))
	}
}

func TestComputeChecksum(t *testing.T) {
	dir := t.TempDir()
	testFile := filepath.Join(dir, "test.txt")

	if err := os.WriteFile(testFile, []byte("hello world"), 0644); err != nil {
		t.Fatal(err)
	}

	checksum, err := ComputeChecksum(testFile)
	if err != nil {
		t.Fatalf("ComputeChecksum failed: %v", err)
	}

	if checksum[:7] != "sha256:" {
		t.Errorf("checksum should start with 'sha256:', got %s", checksum)
	}

	checksum2, _ := ComputeChecksum(testFile)
	if checksum != checksum2 {
		t.Error("same file should produce same checksum")
	}
}

func TestComputeChecksum_NonExistent(t *testing.T) {
	_, err := ComputeChecksum("/nonexistent/file")
	if err == nil {
		t.Error("expected error for nonexistent file")
	}
}
