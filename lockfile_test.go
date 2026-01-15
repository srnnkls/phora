package phora

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLock_Empty(t *testing.T) {
	var lock Lock
	if len(lock.Repos) != 0 {
		t.Errorf("expected empty Lock to have 0 repos, got %d", len(lock.Repos))
	}
}

func TestRepoEntry_Fields(t *testing.T) {
	now := time.Now()
	entry := RepoEntry{
		Name:      "company",
		Repo:      "company/shared-skills",
		Ref:       "main",
		Commit:    "abc123def456",
		FetchedAt: now,
	}

	if entry.Name != "company" {
		t.Errorf("Name = %q, want %q", entry.Name, "company")
	}
	if entry.Repo != "company/shared-skills" {
		t.Errorf("Repo = %q, want %q", entry.Repo, "company/shared-skills")
	}
	if entry.Ref != "main" {
		t.Errorf("Ref = %q, want %q", entry.Ref, "main")
	}
	if entry.Commit != "abc123def456" {
		t.Errorf("Commit = %q, want %q", entry.Commit, "abc123def456")
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
	if len(lock.Repos) != 0 {
		t.Errorf("expected empty Lock, got %d repos", len(lock.Repos))
	}
}

func TestLoadLock_ExistingFile(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `[[repos]]
name = "company"
repo = "company/shared-skills"
ref = "main"
commit = "abc123def456"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	lock, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock failed: %v", err)
	}
	if len(lock.Repos) != 1 {
		t.Fatalf("expected 1 repo, got %d", len(lock.Repos))
	}

	r := lock.Repos[0]
	if r.Name != "company" {
		t.Errorf("Name = %q, want %q", r.Name, "company")
	}
	if r.Repo != "company/shared-skills" {
		t.Errorf("Repo = %q, want %q", r.Repo, "company/shared-skills")
	}
	if r.Ref != "main" {
		t.Errorf("Ref = %q, want %q", r.Ref, "main")
	}
	if r.Commit != "abc123def456" {
		t.Errorf("Commit = %q, want %q", r.Commit, "abc123def456")
	}

	expected := time.Date(2026, 1, 14, 10, 0, 0, 0, time.UTC)
	if !r.FetchedAt.Equal(expected) {
		t.Errorf("FetchedAt = %v, want %v", r.FetchedAt, expected)
	}
}

func TestSaveLock(t *testing.T) {
	dir := t.TempDir()

	now := time.Date(2026, 1, 14, 12, 30, 0, 0, time.UTC)
	lock := &Lock{
		Repos: []RepoEntry{
			{
				Name:      "personal",
				Repo:      "user/dotfiles",
				Ref:       "main",
				Commit:    "deadbeef",
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
	if len(loaded.Repos) != 1 {
		t.Fatalf("expected 1 repo after roundtrip, got %d", len(loaded.Repos))
	}
	if loaded.Repos[0].Name != "personal" {
		t.Errorf("Name after roundtrip = %q, want %q", loaded.Repos[0].Name, "personal")
	}
	if loaded.Repos[0].Commit != "deadbeef" {
		t.Errorf("Commit after roundtrip = %q, want %q", loaded.Repos[0].Commit, "deadbeef")
	}
}

func TestLock_AddRepo_New(t *testing.T) {
	var lock Lock

	now := time.Now()
	lock.AddRepo(RepoEntry{
		Name:      "new-source",
		Repo:      "org/new-repo",
		Ref:       "main",
		Commit:    "abc123",
		FetchedAt: now,
	})

	if len(lock.Repos) != 1 {
		t.Fatalf("expected 1 repo after add, got %d", len(lock.Repos))
	}
	if lock.Repos[0].Name != "new-source" {
		t.Errorf("Name = %q, want %q", lock.Repos[0].Name, "new-source")
	}
}

func TestLock_AddRepo_UpdateExisting(t *testing.T) {
	now := time.Now()
	lock := Lock{
		Repos: []RepoEntry{
			{
				Name:      "existing",
				Repo:      "org/repo",
				Ref:       "main",
				Commit:    "old-commit",
				FetchedAt: now.Add(-1 * time.Hour),
			},
		},
	}

	lock.AddRepo(RepoEntry{
		Name:      "existing",
		Repo:      "org/repo",
		Ref:       "main",
		Commit:    "new-commit",
		FetchedAt: now,
	})

	if len(lock.Repos) != 1 {
		t.Fatalf("expected 1 repo after update, got %d", len(lock.Repos))
	}
	if lock.Repos[0].Commit != "new-commit" {
		t.Errorf("Commit = %q, want %q", lock.Repos[0].Commit, "new-commit")
	}
}

func TestLock_FindByName(t *testing.T) {
	lock := Lock{
		Repos: []RepoEntry{
			{Name: "alpha", Commit: "aaa"},
			{Name: "beta", Commit: "bbb"},
			{Name: "gamma", Commit: "ccc"},
		},
	}

	tests := []struct {
		name   string
		want   string
		exists bool
	}{
		{"alpha", "aaa", true},
		{"beta", "bbb", true},
		{"gamma", "ccc", true},
		{"delta", "", false},
		{"", "", false},
	}

	for _, tc := range tests {
		entry, ok := lock.FindByName(tc.name)
		if ok != tc.exists {
			t.Errorf("FindByName(%q) exists = %v, want %v", tc.name, ok, tc.exists)
			continue
		}
		if ok && entry.Commit != tc.want {
			t.Errorf("FindByName(%q) Commit = %q, want %q", tc.name, entry.Commit, tc.want)
		}
	}
}

func TestLock_RemoveByName(t *testing.T) {
	lock := Lock{
		Repos: []RepoEntry{
			{Name: "keep-a"},
			{Name: "remove-me"},
			{Name: "keep-b"},
		},
	}

	lock.RemoveByName("remove-me")

	if len(lock.Repos) != 2 {
		t.Fatalf("expected 2 repos after remove, got %d", len(lock.Repos))
	}
	if _, found := lock.FindByName("remove-me"); found {
		t.Error("removed repo should not be found")
	}
	if _, found := lock.FindByName("keep-a"); !found {
		t.Error("keep-a should still exist")
	}
	if _, found := lock.FindByName("keep-b"); !found {
		t.Error("keep-b should still exist")
	}
}

func TestLock_RemoveByName_NotFound(t *testing.T) {
	lock := Lock{
		Repos: []RepoEntry{
			{Name: "existing"},
		},
	}

	lock.RemoveByName("nonexistent")

	if len(lock.Repos) != 1 {
		t.Errorf("expected 1 repo unchanged, got %d", len(lock.Repos))
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
		Repos: []RepoEntry{{Name: "test"}},
	}
	if nonEmpty.IsEmpty() {
		t.Error("non-empty Lock should report IsEmpty() = false")
	}
}
