package phora

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestNewClient_CreatesClientWithConfig(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {Git: "https://github.com/owner/repo.git", Branch: "main"},
		},
	}

	client := NewClient(cfg)

	if client == nil {
		t.Fatal("NewClient() returned nil")
	}
	if client.Config != cfg {
		t.Error("NewClient() did not set config")
	}
}

func TestNewClient_DefaultDataDir(t *testing.T) {
	cfg := &Config{}
	client := NewClient(cfg)

	if client.DataDir == "" {
		t.Error("NewClient() should set default DataDir")
	}
}

func TestWithDataDir_SetsDataDirectory(t *testing.T) {
	cfg := &Config{}
	customDir := "/custom/data/dir"

	client := NewClient(cfg, WithDataDir(customDir))

	if client.DataDir != customDir {
		t.Errorf("WithDataDir() DataDir = %q, want %q", client.DataDir, customDir)
	}
}

func TestExtractRepoFromGit(t *testing.T) {
	tests := []struct {
		gitURL   string
		wantRepo string
	}{
		{"https://github.com/owner/repo.git", "owner/repo"},
		{"https://github.com/owner/repo", "owner/repo"},
		{"git@github.com:owner/repo.git", "owner/repo"},
		{"https://gitlab.com/org/project.git", "org/project"},
		{"", ""},
	}

	for _, tc := range tests {
		got := extractRepoFromGit(tc.gitURL)
		if got != tc.wantRepo {
			t.Errorf("extractRepoFromGit(%q) = %q, want %q", tc.gitURL, got, tc.wantRepo)
		}
	}
}

func TestClient_Fetch_ReturnsFetchResult(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping network-dependent test in short mode")
	}

	tmpDir := t.TempDir()

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {Git: "https://github.com/go-git/go-billy.git", Branch: "master"},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir))

	result, err := client.Fetch("test-source")

	if err != nil {
		t.Fatalf("Fetch() error = %v", err)
	}
	if result == nil {
		t.Fatal("Fetch() returned nil result")
	}
	if result.Name != "test-source" {
		t.Errorf("Fetch() Name = %q, want %q", result.Name, "test-source")
	}
	if result.LocalPath == "" {
		t.Error("Fetch() LocalPath should not be empty")
	}
	if result.Commit == "" {
		t.Error("Fetch() Commit should not be empty")
	}
}

func TestClient_Fetch_UnknownSourceReturnsError(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{},
	}
	client := NewClient(cfg)

	_, err := client.Fetch("nonexistent")

	if err == nil {
		t.Error("Fetch() should return error for unknown source")
	}
}

func TestClient_FetchAll_ReturnsResultsForAllSources(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping network-dependent test in short mode")
	}

	tmpDir := t.TempDir()

	cfg := &Config{
		Sources: map[string]Source{
			"source-a": {Git: "https://github.com/go-git/go-billy.git", Branch: "master"},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir))

	results, err := client.FetchAll()

	if err != nil {
		t.Fatalf("FetchAll() error = %v", err)
	}
	if len(results) != 1 {
		t.Errorf("FetchAll() returned %d results, want 1", len(results))
	}
}

func TestClient_FetchAll_EmptySourcesReturnsEmptySlice(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{},
	}
	client := NewClient(cfg)

	results, err := client.FetchAll()

	if err != nil {
		t.Fatalf("FetchAll() error = %v", err)
	}
	if results == nil {
		t.Error("FetchAll() should return empty slice, not nil")
	}
	if len(results) != 0 {
		t.Errorf("FetchAll() returned %d results, want 0", len(results))
	}
}

func TestClient_Fetch_UpdatesLockfile(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping network-dependent test in short mode")
	}

	tmpDir := t.TempDir()
	lockDir := t.TempDir()

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {Git: "https://github.com/go-git/go-billy.git", Branch: "master"},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir), WithLockDir(lockDir))

	_, err := client.Fetch("test-source")
	if err != nil {
		t.Fatalf("Fetch() error = %v", err)
	}

	lockPath := filepath.Join(lockDir, LockFileName)
	if _, err := os.Stat(lockPath); os.IsNotExist(err) {
		t.Error("Fetch() should create lockfile")
	}

	lock, err := LoadLock(lockDir)
	if err != nil {
		t.Fatalf("LoadLock() error = %v", err)
	}

	entry, found := lock.FindSourceByName("test-source")
	if !found {
		t.Error("lockfile should contain entry for fetched source")
	}
	if entry.Repo != "go-git/go-billy" {
		t.Errorf("lockfile entry Repo = %q, want %q", entry.Repo, "go-git/go-billy")
	}
	if entry.SHA == "" {
		t.Error("lockfile entry SHA should not be empty")
	}
}

func TestWithLockDir_SetsLockDirectory(t *testing.T) {
	cfg := &Config{}
	lockDir := "/custom/lock/dir"

	client := NewClient(cfg, WithLockDir(lockDir))

	if client.LockDir != lockDir {
		t.Errorf("WithLockDir() LockDir = %q, want %q", client.LockDir, lockDir)
	}
}

func TestClient_ZeroValue(t *testing.T) {
	var client Client

	if client.Config != nil {
		t.Error("zero value Config should be nil")
	}
	if client.DataDir != "" {
		t.Error("zero value DataDir should be empty")
	}
	if client.LockDir != "" {
		t.Error("zero value LockDir should be empty")
	}
}

func TestClient_FieldsExist(t *testing.T) {
	cfg := &Config{}
	client := Client{
		Config:  cfg,
		DataDir: "/data",
		LockDir: "/lock",
	}

	if client.Config != cfg {
		t.Error("Client.Config not set correctly")
	}
	if client.DataDir != "/data" {
		t.Errorf("Client.DataDir = %q, want %q", client.DataDir, "/data")
	}
	if client.LockDir != "/lock" {
		t.Errorf("Client.LockDir = %q, want %q", client.LockDir, "/lock")
	}
}

func TestClient_Fetch_ValidatesPathAgainstManifest(t *testing.T) {
	tmpDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(repoDir, 0755); err != nil {
		t.Fatalf("failed to create repo dir: %v", err)
	}

	manifestContent := `version = 1

[manifest]
artifacts = ["skills", "prompts"]
`
	if err := os.WriteFile(filepath.Join(repoDir, "phora.toml"), []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to write manifest: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:    "file://" + repoDir,
				Branch: "main",
				Path:   "commands",
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir))

	_, err := client.Fetch("test-source")

	if err == nil {
		t.Error("Fetch() should return error when path is not in manifest artifacts")
	}
	if err != nil && !strings.Contains(err.Error(), "not in source artifacts") {
		t.Errorf("error should mention 'not in source artifacts', got: %v", err)
	}
}

func TestClient_Fetch_AllowsPathInManifestArtifacts(t *testing.T) {
	tmpDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(filepath.Join(repoDir, "skills"), 0755); err != nil {
		t.Fatalf("failed to create skills dir: %v", err)
	}

	manifestContent := `version = 1

[manifest]
artifacts = ["skills", "prompts"]
`
	if err := os.WriteFile(filepath.Join(repoDir, "phora.toml"), []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to write manifest: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:    "file://" + repoDir,
				Branch: "main",
				Path:   "skills",
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir))

	result, err := client.Fetch("test-source")

	if err != nil {
		t.Errorf("Fetch() should succeed when path is in manifest artifacts: %v", err)
	}
	if result == nil {
		t.Error("Fetch() should return non-nil result when path is valid")
	}
}

func TestClient_Fetch_RejectsPathTraversal(t *testing.T) {
	tmpDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(filepath.Join(repoDir, "skills"), 0755); err != nil {
		t.Fatalf("failed to create skills dir: %v", err)
	}
	if err := os.MkdirAll(filepath.Join(repoDir, "commands"), 0755); err != nil {
		t.Fatalf("failed to create commands dir: %v", err)
	}

	manifestContent := `version = 1

[manifest]
artifacts = ["skills", "commands"]
`
	if err := os.WriteFile(filepath.Join(repoDir, "phora.toml"), []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to write manifest: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:    "file://" + repoDir,
				Branch: "main",
				Path:   "skills/../commands",
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir))

	_, err := client.Fetch("test-source")

	if err == nil {
		t.Error("Fetch() should reject path traversal attempts")
	}
	if err != nil && !strings.Contains(err.Error(), "traversal") {
		t.Errorf("error should mention 'traversal', got: %v", err)
	}
}

func TestClient_Fetch_IgnoreManifestBypasses(t *testing.T) {
	tmpDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(filepath.Join(repoDir, "private"), 0755); err != nil {
		t.Fatalf("failed to create private dir: %v", err)
	}

	manifestContent := `version = 1

[manifest]
artifacts = ["skills"]
`
	if err := os.WriteFile(filepath.Join(repoDir, "phora.toml"), []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to write manifest: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:            "file://" + repoDir,
				Branch:         "main",
				Path:           "private",
				IgnoreManifest: true,
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir))

	result, err := client.Fetch("test-source")

	if err != nil {
		t.Errorf("Fetch() with IgnoreManifest should succeed: %v", err)
	}
	if result == nil {
		t.Error("Fetch() should return non-nil result with IgnoreManifest")
	}
}

func initGitRepo(dir string) error {
	cmd := exec.Command("git", "init", "-b", "main")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "config", "user.email", "test@test.com")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "config", "user.name", "Test")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "add", "-A")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "commit", "-m", "initial")
	cmd.Dir = dir
	return cmd.Run()
}

func TestClient_Fetch_PopulatesFilesInLockfile(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(repoDir, 0755); err != nil {
		t.Fatalf("failed to create repo dir: %v", err)
	}

	file1Content := "content of file one"
	file2Content := "content of file two with more data"
	if err := os.WriteFile(filepath.Join(repoDir, "file1.txt"), []byte(file1Content), 0644); err != nil {
		t.Fatalf("failed to write file1: %v", err)
	}
	if err := os.WriteFile(filepath.Join(repoDir, "file2.txt"), []byte(file2Content), 0644); err != nil {
		t.Fatalf("failed to write file2: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:    "file://" + repoDir,
				Branch: "main",
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir), WithLockDir(lockDir))

	_, err := client.Fetch("test-source")
	if err != nil {
		t.Fatalf("Fetch() error = %v", err)
	}

	lock, err := LoadLock(lockDir)
	if err != nil {
		t.Fatalf("LoadLock() error = %v", err)
	}

	entry, found := lock.FindSourceByName("test-source")
	if !found {
		t.Fatal("lockfile should contain entry for fetched source")
	}

	if len(entry.Files) == 0 {
		t.Fatal("lockfile entry Files should not be empty after Fetch")
	}

	if len(entry.Files) != 2 {
		t.Errorf("lockfile entry Files count = %d, want 2", len(entry.Files))
	}

	fileMap := make(map[string]FileLock)
	for _, f := range entry.Files {
		fileMap[f.Path] = f
	}

	if _, ok := fileMap["file1.txt"]; !ok {
		t.Error("lockfile Files should contain file1.txt")
	}
	if _, ok := fileMap["file2.txt"]; !ok {
		t.Error("lockfile Files should contain file2.txt")
	}
}

func TestClient_Fetch_FileLockHasValidSHA256(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(repoDir, 0755); err != nil {
		t.Fatalf("failed to create repo dir: %v", err)
	}

	fileContent := "test content for hash verification"
	if err := os.WriteFile(filepath.Join(repoDir, "hashtest.txt"), []byte(fileContent), 0644); err != nil {
		t.Fatalf("failed to write file: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:    "file://" + repoDir,
				Branch: "main",
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir), WithLockDir(lockDir))

	_, err := client.Fetch("test-source")
	if err != nil {
		t.Fatalf("Fetch() error = %v", err)
	}

	lock, err := LoadLock(lockDir)
	if err != nil {
		t.Fatalf("LoadLock() error = %v", err)
	}

	entry, found := lock.FindSourceByName("test-source")
	if !found {
		t.Fatal("lockfile should contain entry for fetched source")
	}

	if len(entry.Files) == 0 {
		t.Fatal("lockfile entry Files must not be empty to verify SHA256")
	}

	for _, f := range entry.Files {
		if f.SHA256 == "" {
			t.Errorf("FileLock for %q has empty SHA256", f.Path)
			continue
		}
		if len(f.SHA256) != 64 {
			t.Errorf("FileLock for %q SHA256 length = %d, want 64 (hex-encoded SHA256)", f.Path, len(f.SHA256))
		}
		for _, c := range f.SHA256 {
			if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')) {
				t.Errorf("FileLock for %q SHA256 contains invalid hex char: %q", f.Path, c)
				break
			}
		}
	}
}

func TestClient_Fetch_FileLockHasValidSize(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := t.TempDir()
	repoDir := filepath.Join(tmpDir, "test-repo")

	if err := os.MkdirAll(repoDir, 0755); err != nil {
		t.Fatalf("failed to create repo dir: %v", err)
	}

	fileContent := "exactly 27 bytes of content"
	if err := os.WriteFile(filepath.Join(repoDir, "sizetest.txt"), []byte(fileContent), 0644); err != nil {
		t.Fatalf("failed to write file: %v", err)
	}

	if err := initGitRepo(repoDir); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {
				Git:    "file://" + repoDir,
				Branch: "main",
			},
		},
	}
	client := NewClient(cfg, WithDataDir(tmpDir), WithLockDir(lockDir))

	_, err := client.Fetch("test-source")
	if err != nil {
		t.Fatalf("Fetch() error = %v", err)
	}

	lock, err := LoadLock(lockDir)
	if err != nil {
		t.Fatalf("LoadLock() error = %v", err)
	}

	entry, found := lock.FindSourceByName("test-source")
	if !found {
		t.Fatal("lockfile should contain entry for fetched source")
	}

	for _, f := range entry.Files {
		if f.Size <= 0 {
			t.Errorf("FileLock for %q has invalid Size = %d, want > 0", f.Path, f.Size)
		}
	}

	fileMap := make(map[string]FileLock)
	for _, f := range entry.Files {
		fileMap[f.Path] = f
	}

	if fileLock, ok := fileMap["sizetest.txt"]; ok {
		expectedSize := int64(len(fileContent))
		if fileLock.Size != expectedSize {
			t.Errorf("FileLock for sizetest.txt Size = %d, want %d", fileLock.Size, expectedSize)
		}
	}
}
