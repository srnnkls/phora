package phora

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLock_Empty(t *testing.T) {
	var lock Lock
	if len(lock.Sources) != 0 {
		t.Errorf("expected empty Lock to have 0 sources, got %d", len(lock.Sources))
	}
}

func TestSourceLock_Fields(t *testing.T) {
	now := time.Now()
	entry := SourceLock{
		Name:      "company",
		Repo:      "company/shared-skills",
		Rev:       "main",
		SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
		Digest:    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
		FetchedAt: now,
	}

	if entry.Name != "company" {
		t.Errorf("Name = %q, want %q", entry.Name, "company")
	}
	if entry.Repo != "company/shared-skills" {
		t.Errorf("Repo = %q, want %q", entry.Repo, "company/shared-skills")
	}
	if entry.Rev != "main" {
		t.Errorf("Rev = %q, want %q", entry.Rev, "main")
	}
	if entry.SHA != "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" {
		t.Errorf("SHA = %q, want %q", entry.SHA, "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")
	}
	if !entry.FetchedAt.Equal(now) {
		t.Errorf("FetchedAt = %v, want %v", entry.FetchedAt, now)
	}
}

func TestLoadLock_NonExistent(t *testing.T) {
	dir := t.TempDir()
	lock, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock should not error for nonexistent lockfile: %v", err)
	}
	if len(lock.Sources) != 0 {
		t.Errorf("expected empty Lock, got %d sources", len(lock.Sources))
	}
}

func TestLoadLock_ExistingFile(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "company"
repo = "company/shared-skills"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	lock, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock failed: %v", err)
	}
	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source, got %d", len(lock.Sources))
	}

	s := lock.Sources[0]
	if s.Name != "company" {
		t.Errorf("Name = %q, want %q", s.Name, "company")
	}
	if s.Repo != "company/shared-skills" {
		t.Errorf("Repo = %q, want %q", s.Repo, "company/shared-skills")
	}
	if s.Rev != "main" {
		t.Errorf("Rev = %q, want %q", s.Rev, "main")
	}
	if s.SHA != "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" {
		t.Errorf("SHA = %q, want %q", s.SHA, "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")
	}

	expected := time.Date(2026, 1, 14, 10, 0, 0, 0, time.UTC)
	if !s.FetchedAt.Equal(expected) {
		t.Errorf("FetchedAt = %v, want %v", s.FetchedAt, expected)
	}
}

func TestSaveLock(t *testing.T) {
	dir := t.TempDir()

	now := time.Date(2026, 1, 14, 12, 30, 0, 0, time.UTC)
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:      "personal",
				Repo:      "user/dotfiles",
				Rev:       "main",
				SHA:       "deadbeef1234567890abcdef1234567890abcdef",
				Digest:    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				FetchedAt: now,
			},
		},
	}

	if err := lock.Save(dir); err != nil {
		t.Fatalf("Save failed: %v", err)
	}

	lockPath := filepath.Join(dir, "phora.lock")
	if _, err := os.Stat(lockPath); os.IsNotExist(err) {
		t.Fatal("lockfile was not created")
	}

	loaded, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock after save failed: %v", err)
	}
	if len(loaded.Sources) != 1 {
		t.Fatalf("expected 1 source after roundtrip, got %d", len(loaded.Sources))
	}
	if loaded.Sources[0].Name != "personal" {
		t.Errorf("Name after roundtrip = %q, want %q", loaded.Sources[0].Name, "personal")
	}
	if loaded.Sources[0].SHA != "deadbeef1234567890abcdef1234567890abcdef" {
		t.Errorf("SHA after roundtrip = %q, want %q", loaded.Sources[0].SHA, "deadbeef1234567890abcdef1234567890abcdef")
	}
}

func TestLock_AddSource_New(t *testing.T) {
	var lock Lock

	now := time.Now()
	lock.AddSource(SourceLock{
		Name:      "new-source",
		Repo:      "org/new-repo",
		Rev:       "main",
		SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
		FetchedAt: now,
	})

	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source after add, got %d", len(lock.Sources))
	}
	if lock.Sources[0].Name != "new-source" {
		t.Errorf("Name = %q, want %q", lock.Sources[0].Name, "new-source")
	}
}

func TestLock_AddSource_UpdateExisting(t *testing.T) {
	now := time.Now()
	lock := Lock{
		Sources: []SourceLock{
			{
				Name:      "existing",
				Repo:      "org/repo",
				Rev:       "main",
				SHA:       "1111111111111111111111111111111111111111",
				FetchedAt: now.Add(-1 * time.Hour),
			},
		},
	}

	lock.AddSource(SourceLock{
		Name:      "existing",
		Repo:      "org/repo",
		Rev:       "main",
		SHA:       "2222222222222222222222222222222222222222",
		FetchedAt: now,
	})

	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source after update, got %d", len(lock.Sources))
	}
	if lock.Sources[0].SHA != "2222222222222222222222222222222222222222" {
		t.Errorf("SHA = %q, want %q", lock.Sources[0].SHA, "2222222222222222222222222222222222222222")
	}
}

func TestLock_FindSourceByName(t *testing.T) {
	lock := Lock{
		Sources: []SourceLock{
			{Name: "alpha", SHA: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
			{Name: "beta", SHA: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
			{Name: "gamma", SHA: "cccccccccccccccccccccccccccccccccccccccc"},
		},
	}

	tests := []struct {
		name   string
		want   string
		exists bool
	}{
		{"alpha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", true},
		{"beta", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", true},
		{"gamma", "cccccccccccccccccccccccccccccccccccccccc", true},
		{"delta", "", false},
		{"", "", false},
	}

	for _, tc := range tests {
		entry, ok := lock.FindSourceByName(tc.name)
		if ok != tc.exists {
			t.Errorf("FindSourceByName(%q) exists = %v, want %v", tc.name, ok, tc.exists)
			continue
		}
		if ok && entry.SHA != tc.want {
			t.Errorf("FindSourceByName(%q) SHA = %q, want %q", tc.name, entry.SHA, tc.want)
		}
	}
}

func TestLock_RemoveSource(t *testing.T) {
	lock := Lock{
		Sources: []SourceLock{
			{Name: "keep-a"},
			{Name: "remove-me"},
			{Name: "keep-b"},
		},
	}

	lock.RemoveSource("remove-me")

	if len(lock.Sources) != 2 {
		t.Fatalf("expected 2 sources after remove, got %d", len(lock.Sources))
	}
	if _, found := lock.FindSourceByName("remove-me"); found {
		t.Error("removed source should not be found")
	}
	if _, found := lock.FindSourceByName("keep-a"); !found {
		t.Error("keep-a should still exist")
	}
	if _, found := lock.FindSourceByName("keep-b"); !found {
		t.Error("keep-b should still exist")
	}
}

func TestLock_RemoveSource_NotFound(t *testing.T) {
	lock := Lock{
		Sources: []SourceLock{
			{Name: "existing"},
		},
	}

	lock.RemoveSource("nonexistent")

	if len(lock.Sources) != 1 {
		t.Errorf("expected 1 source unchanged, got %d", len(lock.Sources))
	}
}

func TestLoadLock_InvalidTOML(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	if err := os.WriteFile(lockPath, []byte("invalid toml [[["), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for invalid TOML, got nil")
	}
}

func TestLock_IsEmpty(t *testing.T) {
	var empty Lock
	if !empty.IsEmpty() {
		t.Error("empty Lock should report IsEmpty() = true")
	}

	nonEmpty := Lock{
		Sources: []SourceLock{{Name: "test"}},
	}
	if nonEmpty.IsEmpty() {
		t.Error("non-empty Lock should report IsEmpty() = false")
	}
}
