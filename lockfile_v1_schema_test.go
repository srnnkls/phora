package phora

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestSave_SetsVersionOne(t *testing.T) {
	dir := t.TempDir()
	fetchedAt := time.Date(2026, 1, 18, 10, 0, 0, 0, time.UTC)

	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:      "test",
				Repo:      "org/test",
				Rev:       "main",
				SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				Digest:    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				FetchedAt: fetchedAt,
				Files:     []FileLock{},
			},
		},
	}

	if err := lock.Save(dir); err != nil {
		t.Fatalf("Save failed: %v", err)
	}

	lockPath := filepath.Join(dir, LockFileName)
	data, err := os.ReadFile(lockPath)
	if err != nil {
		t.Fatalf("ReadFile failed: %v", err)
	}

	content := string(data)
	if !strings.HasPrefix(content, "version = 1") {
		t.Errorf("lock file should start with 'version = 1', got:\n%s", content)
	}
}

func TestSave_SetsVersionOneEvenWhenZero(t *testing.T) {
	dir := t.TempDir()
	fetchedAt := time.Date(2026, 1, 18, 10, 0, 0, 0, time.UTC)

	lock := &Lock{
		Version: 0,
		Sources: []SourceLock{
			{
				Name:      "test",
				Repo:      "org/test",
				Rev:       "main",
				SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				Digest:    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				FetchedAt: fetchedAt,
				Files:     []FileLock{},
			},
		},
	}

	if err := lock.Save(dir); err != nil {
		t.Fatalf("Save failed: %v", err)
	}

	lockPath := filepath.Join(dir, LockFileName)
	data, err := os.ReadFile(lockPath)
	if err != nil {
		t.Fatalf("ReadFile failed: %v", err)
	}

	content := string(data)
	if !strings.HasPrefix(content, "version = 1") {
		t.Errorf("lock file should start with 'version = 1' even when Version=0, got:\n%s", content)
	}
}

func TestSave_NoReposSection(t *testing.T) {
	dir := t.TempDir()
	fetchedAt := time.Date(2026, 1, 18, 10, 0, 0, 0, time.UTC)

	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:      "test",
				Repo:      "org/test",
				Rev:       "main",
				SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				Digest:    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				FetchedAt: fetchedAt,
				Files:     []FileLock{},
			},
		},
	}

	if err := lock.Save(dir); err != nil {
		t.Fatalf("Save failed: %v", err)
	}

	lockPath := filepath.Join(dir, LockFileName)
	data, err := os.ReadFile(lockPath)
	if err != nil {
		t.Fatalf("ReadFile failed: %v", err)
	}

	content := string(data)
	if strings.Contains(content, "[[repos]]") {
		t.Errorf("lock file should not contain [[repos]] section, got:\n%s", content)
	}
	if strings.Contains(content, "repos") {
		t.Errorf("lock file should not contain 'repos' at all, got:\n%s", content)
	}
}

func TestLoadLock_MissingVersion(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for lock file without version, got nil")
	}
}

func TestLoadLock_InvalidVersion(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 99

[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for invalid version (99), got nil")
	}
}

func TestLoadLock_VersionZero(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 0

[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for version = 0 (unsupported), got nil")
	}
}

func TestLoadLock_InvalidSHA_TooShort(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "abc123"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for SHA with only 6 chars (not 40-char hex), got nil")
	}
}

func TestLoadLock_InvalidSHA_NotHex(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "xyz123ghijklmnopqrstuvwxyz12345678901234"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for SHA with non-hex chars, got nil")
	}
}

func TestLoadLock_InvalidDigest_TooShort(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "abc123"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for Digest with only 6 chars (not 64-char hex), got nil")
	}
}

func TestLoadLock_InvalidDigest_NotHex(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "test"
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "xyz123ghijklmnopqrstuvwxyz1234567890123456789012345678901234567890"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for Digest with non-hex chars, got nil")
	}
}

func TestLoadLock_EmptyRepo(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "test"
repo = ""
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for empty Repo field, got nil")
	}
}

func TestLoadLock_EmptyName(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = ""
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error for empty Name field, got nil")
	}
}

func TestLoadLock_ValidLockFile(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "test"
repo = "owner/repo"
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
		t.Fatalf("expected valid lock file to load successfully, got error: %v", err)
	}
	if lock.Version != 1 {
		t.Errorf("Version = %d, want 1", lock.Version)
	}
	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source, got %d", len(lock.Sources))
	}
	s := lock.Sources[0]
	if s.Name != "test" {
		t.Errorf("Name = %q, want %q", s.Name, "test")
	}
	if s.SHA != "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" {
		t.Errorf("SHA = %q, want 40-char hex", s.SHA)
	}
}

func TestLoadLock_MultipleSourcesWithOneInvalid(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	content := `version = 1

[[sources]]
name = "valid"
repo = "owner/repo"
rev = "main"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z

[[sources]]
name = "invalid"
repo = "other/repo"
rev = "main"
sha = "bad-sha"
digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
fetched_at = 2026-01-14T10:00:00Z
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := LoadLock(dir)
	if err == nil {
		t.Error("expected error when one of multiple sources has invalid SHA, got nil")
	}
}

