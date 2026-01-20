package phora

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSourceLock_Fields(t *testing.T) {
	now := time.Now()
	source := SourceLock{
		Name:      "shared",
		Repo:      "company/shared",
		Ref:       "v1.0",
		SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
		Digest:    "8f3a2b1c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2",
		FetchedAt: now,
		Files:     []FileLock{},
	}

	if source.Name != "shared" {
		t.Errorf("Name = %q, want %q", source.Name, "shared")
	}
	if source.Repo != "company/shared" {
		t.Errorf("Repo = %q, want %q", source.Repo, "company/shared")
	}
	if source.Ref != "v1.0" {
		t.Errorf("Ref = %q, want %q", source.Ref, "v1.0")
	}
	if source.SHA != "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" {
		t.Errorf("SHA = %q, want full commit SHA", source.SHA)
	}
	if source.Digest != "8f3a2b1c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2" {
		t.Errorf("Digest = %q, want 64-char hex", source.Digest)
	}
	if !source.FetchedAt.Equal(now) {
		t.Errorf("FetchedAt = %v, want %v", source.FetchedAt, now)
	}
	if source.Files == nil {
		t.Error("Files should not be nil")
	}
}

func TestFileLock_Fields(t *testing.T) {
	file := FileLock{
		Path:   "skills/code-review.md",
		SHA256: "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7",
		Size:   2048,
	}

	if file.Path != "skills/code-review.md" {
		t.Errorf("Path = %q, want %q", file.Path, "skills/code-review.md")
	}
	if file.SHA256 != "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7" {
		t.Errorf("SHA256 = %q, want 64-char hex", file.SHA256)
	}
	if file.Size != 2048 {
		t.Errorf("Size = %d, want %d", file.Size, 2048)
	}
}

func TestComputeFileHash_RegularFile(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.txt")

	content := []byte("hello world\n")
	if err := os.WriteFile(filePath, content, 0644); err != nil {
		t.Fatal(err)
	}

	hash, size, err := ComputeFileHash(filePath)
	if err != nil {
		t.Fatalf("ComputeFileHash failed: %v", err)
	}

	// SHA256 of "hello world\n"
	expectedHash := "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"
	if hash != expectedHash {
		t.Errorf("hash = %q, want %q", hash, expectedHash)
	}
	if size != int64(len(content)) {
		t.Errorf("size = %d, want %d", size, len(content))
	}
}

func TestComputeFileHash_EmptyFile(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "empty.txt")

	if err := os.WriteFile(filePath, []byte{}, 0644); err != nil {
		t.Fatal(err)
	}

	hash, size, err := ComputeFileHash(filePath)
	if err != nil {
		t.Fatalf("ComputeFileHash failed: %v", err)
	}

	// SHA256 of empty string is e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
	expectedHash := "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
	if hash != expectedHash {
		t.Errorf("hash = %q, want %q", hash, expectedHash)
	}
	if size != 0 {
		t.Errorf("size = %d, want 0", size)
	}
}

func TestComputeFileHash_Symlink(t *testing.T) {
	dir := t.TempDir()
	targetPath := filepath.Join(dir, "target.txt")
	linkPath := filepath.Join(dir, "link.txt")

	content := []byte("symlink target content\n")
	if err := os.WriteFile(targetPath, content, 0644); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(targetPath, linkPath); err != nil {
		t.Fatal(err)
	}

	hash, size, err := ComputeFileHash(linkPath)
	if err != nil {
		t.Fatalf("ComputeFileHash failed: %v", err)
	}

	// Should hash target content, not the symlink itself
	targetHash, targetSize, err := ComputeFileHash(targetPath)
	if err != nil {
		t.Fatalf("ComputeFileHash target failed: %v", err)
	}

	if hash != targetHash {
		t.Errorf("symlink hash = %q, target hash = %q, should be equal", hash, targetHash)
	}
	if size != targetSize {
		t.Errorf("symlink size = %d, target size = %d, should be equal", size, targetSize)
	}
}

func TestComputeFileHash_NonExistent(t *testing.T) {
	_, _, err := ComputeFileHash("/nonexistent/path/file.txt")
	if err == nil {
		t.Error("ComputeFileHash should return error for non-existent file")
	}
}

func TestComputeFileHash_Directory(t *testing.T) {
	dir := t.TempDir()
	_, _, err := ComputeFileHash(dir)
	if err == nil {
		t.Error("ComputeFileHash should return error for directory")
	}
}

func TestLockV2_ParseTOML(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, LockFileName)

	content := `version = 1

[[sources]]
name = "shared"
repo = "company/shared"
ref = "v1.0"
sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
digest = "8f3a2b1c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2"
fetched_at = 2026-01-18T10:00:00Z

[[sources.files]]
path = "skills/code-review.md"
sha256 = "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7"
size = 2048

[[sources.files]]
path = "skills/debug.md"
sha256 = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3"
size = 1024
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	lock, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock failed: %v", err)
	}

	if lock.Version != 1 {
		t.Errorf("Version = %d, want 1", lock.Version)
	}
	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source, got %d", len(lock.Sources))
	}

	source := lock.Sources[0]
	if source.Name != "shared" {
		t.Errorf("Name = %q, want %q", source.Name, "shared")
	}
	if source.Repo != "company/shared" {
		t.Errorf("Repo = %q, want %q", source.Repo, "company/shared")
	}
	if source.Ref != "v1.0" {
		t.Errorf("Ref = %q, want %q", source.Ref, "v1.0")
	}
	if source.SHA != "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" {
		t.Errorf("SHA = %q, want full commit SHA", source.SHA)
	}
	if source.Digest != "8f3a2b1c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2" {
		t.Errorf("Digest = %q, want 64-char hex", source.Digest)
	}

	expectedTime := time.Date(2026, 1, 18, 10, 0, 0, 0, time.UTC)
	if !source.FetchedAt.Equal(expectedTime) {
		t.Errorf("FetchedAt = %v, want %v", source.FetchedAt, expectedTime)
	}

	if len(source.Files) != 2 {
		t.Fatalf("expected 2 files, got %d", len(source.Files))
	}

	file1 := source.Files[0]
	if file1.Path != "skills/code-review.md" {
		t.Errorf("Files[0].Path = %q, want %q", file1.Path, "skills/code-review.md")
	}
	if file1.SHA256 != "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7" {
		t.Errorf("Files[0].SHA256 = %q, want 64-char hex", file1.SHA256)
	}
	if file1.Size != 2048 {
		t.Errorf("Files[0].Size = %d, want 2048", file1.Size)
	}
}

func TestLockV2_WriteTOML(t *testing.T) {
	dir := t.TempDir()
	fetchedAt := time.Date(2026, 1, 18, 10, 0, 0, 0, time.UTC)

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Ref:       "v1.0",
				SHA:       "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
				Digest:    "8f3a2b1c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2",
				FetchedAt: fetchedAt,
				Files: []FileLock{
					{
						Path:   "skills/code-review.md",
						SHA256: "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7",
						Size:   2048,
					},
				},
			},
		},
	}

	if err := lock.Save(dir); err != nil {
		t.Fatalf("Save failed: %v", err)
	}

	// Verify roundtrip
	loaded, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock after save failed: %v", err)
	}

	if loaded.Version != 1 {
		t.Errorf("Version = %d, want 1", loaded.Version)
	}
	if len(loaded.Sources) != 1 {
		t.Fatalf("expected 1 source after roundtrip, got %d", len(loaded.Sources))
	}

	source := loaded.Sources[0]
	if source.Name != "shared" {
		t.Errorf("Name after roundtrip = %q, want %q", source.Name, "shared")
	}
	if source.SHA != "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" {
		t.Errorf("SHA after roundtrip = %q, want full commit SHA", source.SHA)
	}
	if len(source.Files) != 1 {
		t.Fatalf("expected 1 file after roundtrip, got %d", len(source.Files))
	}

	file := source.Files[0]
	if file.Path != "skills/code-review.md" {
		t.Errorf("Files[0].Path after roundtrip = %q, want %q", file.Path, "skills/code-review.md")
	}
	if file.Size != 2048 {
		t.Errorf("Files[0].Size after roundtrip = %d, want 2048", file.Size)
	}
}

func TestLock_FindSourceByName(t *testing.T) {
	now := time.Now()
	lock := Lock{
		Version: 1,
		Sources: []SourceLock{
			{Name: "alpha", Repo: "org/alpha", FetchedAt: now},
			{Name: "beta", Repo: "org/beta", FetchedAt: now},
			{Name: "gamma", Repo: "org/gamma", FetchedAt: now},
		},
	}

	tests := []struct {
		name     string
		wantRepo string
		exists   bool
	}{
		{"alpha", "org/alpha", true},
		{"beta", "org/beta", true},
		{"gamma", "org/gamma", true},
		{"delta", "", false},
		{"", "", false},
	}

	for _, tc := range tests {
		source, ok := lock.FindSourceByName(tc.name)
		if ok != tc.exists {
			t.Errorf("FindSourceByName(%q) exists = %v, want %v", tc.name, ok, tc.exists)
			continue
		}
		if ok && source.Repo != tc.wantRepo {
			t.Errorf("FindSourceByName(%q) Repo = %q, want %q", tc.name, source.Repo, tc.wantRepo)
		}
	}
}

func TestLock_AddSource_New(t *testing.T) {
	var lock Lock
	lock.Version = 1

	now := time.Now()
	lock.AddSource(SourceLock{
		Name:      "new-source",
		Repo:      "org/new-repo",
		Ref:       "main",
		SHA:       "abc123",
		FetchedAt: now,
	})

	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source after add, got %d", len(lock.Sources))
	}
	if lock.Sources[0].Name != "new-source" {
		t.Errorf("Name = %q, want %q", lock.Sources[0].Name, "new-source")
	}
	if lock.Sources[0].Repo != "org/new-repo" {
		t.Errorf("Repo = %q, want %q", lock.Sources[0].Repo, "org/new-repo")
	}
}

func TestLock_AddSource_UpdateExisting(t *testing.T) {
	now := time.Now()
	lock := Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "existing",
				Repo:      "org/repo",
				Ref:       "main",
				SHA:       "old-sha",
				FetchedAt: now.Add(-1 * time.Hour),
			},
		},
	}

	lock.AddSource(SourceLock{
		Name:      "existing",
		Repo:      "org/repo",
		Ref:       "main",
		SHA:       "new-sha",
		FetchedAt: now,
	})

	if len(lock.Sources) != 1 {
		t.Fatalf("expected 1 source after update, got %d", len(lock.Sources))
	}
	if lock.Sources[0].SHA != "new-sha" {
		t.Errorf("SHA = %q, want %q", lock.Sources[0].SHA, "new-sha")
	}
}

func TestLock_RemoveSource(t *testing.T) {
	now := time.Now()
	lock := Lock{
		Version: 1,
		Sources: []SourceLock{
			{Name: "keep-a", Repo: "org/a", FetchedAt: now},
			{Name: "remove-me", Repo: "org/remove", FetchedAt: now},
			{Name: "keep-b", Repo: "org/b", FetchedAt: now},
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
	now := time.Now()
	lock := Lock{
		Version: 1,
		Sources: []SourceLock{
			{Name: "existing", Repo: "org/existing", FetchedAt: now},
		},
	}

	lock.RemoveSource("nonexistent")

	if len(lock.Sources) != 1 {
		t.Errorf("expected 1 source unchanged, got %d", len(lock.Sources))
	}
}

func TestLock_TrackFiles(t *testing.T) {
	now := time.Now()
	lock := Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Ref:       "v1.0",
				SHA:       "abc123",
				FetchedAt: now,
				Files: []FileLock{
					{Path: "skills/review.md", SHA256: "hash1", Size: 1024},
					{Path: "skills/debug.md", SHA256: "hash2", Size: 2048},
				},
			},
		},
	}

	source, ok := lock.FindSourceByName("shared")
	if !ok {
		t.Fatal("source not found")
	}

	if len(source.Files) != 2 {
		t.Fatalf("expected 2 files, got %d", len(source.Files))
	}
	if source.Files[0].Path != "skills/review.md" {
		t.Errorf("Files[0].Path = %q, want %q", source.Files[0].Path, "skills/review.md")
	}
	if source.Files[1].SHA256 != "hash2" {
		t.Errorf("Files[1].SHA256 = %q, want %q", source.Files[1].SHA256, "hash2")
	}
}
