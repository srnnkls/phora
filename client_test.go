package phora

import (
	"os"
	"path/filepath"
	"testing"
)

func TestNewClient_CreatesClientWithConfig(t *testing.T) {
	cfg := &Config{
		Hosts: map[string]Host{
			"github": {GitURL: "https://github.com/{owner}/{repo}.git"},
		},
		Sources: map[string]Source{
			"test-source": {Repo: "owner/repo", Ref: "main"},
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

func TestClient_ResolveGitURL_WithDefaultHost(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {Repo: "owner/repo", Ref: "main"},
		},
	}
	client := NewClient(cfg)

	source := cfg.Sources["test"]
	url, err := client.ResolveGitURL(source)

	if err != nil {
		t.Errorf("ResolveGitURL() error = %v", err)
	}
	if url != "https://github.com/owner/repo.git" {
		t.Errorf("ResolveGitURL() = %q, want %q", url, "https://github.com/owner/repo.git")
	}
}

func TestClient_ResolveGitURL_WithCustomHost(t *testing.T) {
	cfg := &Config{
		Hosts: map[string]Host{
			"gitlab": {GitURL: "https://gitlab.example.com/{owner}/{repo}.git"},
		},
		Sources: map[string]Source{
			"test": {Repo: "myorg/myrepo", Ref: "main", Host: "gitlab"},
		},
	}
	client := NewClient(cfg)

	source := cfg.Sources["test"]
	url, err := client.ResolveGitURL(source)

	if err != nil {
		t.Errorf("ResolveGitURL() error = %v", err)
	}
	if url != "https://gitlab.example.com/myorg/myrepo.git" {
		t.Errorf("ResolveGitURL() = %q, want %q", url, "https://gitlab.example.com/myorg/myrepo.git")
	}
}

func TestClient_ResolveGitURL_UnknownHostReturnsError(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {Repo: "owner/repo", Ref: "main", Host: "nonexistent"},
		},
	}
	client := NewClient(cfg)

	source := cfg.Sources["test"]
	_, err := client.ResolveGitURL(source)

	if err == nil {
		t.Error("ResolveGitURL() should return error for unknown host")
	}
}

func TestClient_ResolveGitURL_InvalidRepoFormatReturnsError(t *testing.T) {
	cfg := &Config{}
	client := NewClient(cfg)

	source := Source{Repo: "invalid-repo-format"}
	_, err := client.ResolveGitURL(source)

	if err == nil {
		t.Error("ResolveGitURL() should return error for invalid repo format")
	}
}

func TestClient_Fetch_ReturnsFetchResult(t *testing.T) {
	tmpDir := t.TempDir()

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {Repo: "go-git/go-billy", Ref: "master"},
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
	tmpDir := t.TempDir()

	cfg := &Config{
		Sources: map[string]Source{
			"source-a": {Repo: "go-git/go-billy", Ref: "master"},
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
	tmpDir := t.TempDir()
	lockDir := t.TempDir()

	cfg := &Config{
		Sources: map[string]Source{
			"test-source": {Repo: "go-git/go-billy", Ref: "master"},
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

	entry, found := lock.FindByName("test-source")
	if !found {
		t.Error("lockfile should contain entry for fetched source")
	}
	if entry.Repo != "go-git/go-billy" {
		t.Errorf("lockfile entry Repo = %q, want %q", entry.Repo, "go-git/go-billy")
	}
	if entry.Commit == "" {
		t.Error("lockfile entry Commit should not be empty")
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
