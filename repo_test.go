package phora

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/plumbing/object"
)

func TestRepoStructFields(t *testing.T) {
	repo := Repo{
		Name:      "test-repo",
		URL:       "https://github.com/owner/repo.git",
		LocalPath: "/tmp/repos/owner/repo",
		Ref:       "main",
	}

	if repo.Name != "test-repo" {
		t.Errorf("Name = %q, want %q", repo.Name, "test-repo")
	}
	if repo.URL != "https://github.com/owner/repo.git" {
		t.Errorf("URL = %q, want %q", repo.URL, "https://github.com/owner/repo.git")
	}
	if repo.LocalPath != "/tmp/repos/owner/repo" {
		t.Errorf("LocalPath = %q, want %q", repo.LocalPath, "/tmp/repos/owner/repo")
	}
	if repo.Ref != "main" {
		t.Errorf("Ref = %q, want %q", repo.Ref, "main")
	}
}

func TestCloneOrPull_ClonesNewRepo(t *testing.T) {
	tmpDir := t.TempDir()
	localPath := filepath.Join(tmpDir, "test-repo")

	repo := Repo{
		Name:      "go-git",
		URL:       "https://github.com/go-git/go-git.git",
		LocalPath: localPath,
		Ref:       "master",
	}

	err := CloneOrPull(repo)
	if err != nil {
		t.Fatalf("CloneOrPull() error = %v", err)
	}

	if _, err := os.Stat(filepath.Join(localPath, ".git")); os.IsNotExist(err) {
		t.Error("CloneOrPull() did not create .git directory")
	}
}

func TestCloneOrPull_PullsExistingRepo(t *testing.T) {
	tmpDir := t.TempDir()
	localPath := filepath.Join(tmpDir, "test-repo")

	_, err := git.PlainClone(localPath, false, &git.CloneOptions{
		URL:   "https://github.com/go-git/go-git.git",
		Depth: 1,
	})
	if err != nil {
		t.Fatalf("setup: PlainClone error = %v", err)
	}

	repo := Repo{
		Name:      "go-git",
		URL:       "https://github.com/go-git/go-git.git",
		LocalPath: localPath,
		Ref:       "master",
	}

	err = CloneOrPull(repo)
	if err != nil {
		t.Errorf("CloneOrPull() on existing repo error = %v", err)
	}
}

func TestRepo_CurrentCommit(t *testing.T) {
	tmpDir := t.TempDir()
	localPath := filepath.Join(tmpDir, "test-repo")

	gitRepo, err := git.PlainInit(localPath, false)
	if err != nil {
		t.Fatalf("setup: PlainInit error = %v", err)
	}

	testFile := filepath.Join(localPath, "test.txt")
	if err := os.WriteFile(testFile, []byte("test content"), 0644); err != nil {
		t.Fatalf("setup: WriteFile error = %v", err)
	}

	wt, err := gitRepo.Worktree()
	if err != nil {
		t.Fatalf("setup: Worktree error = %v", err)
	}
	if _, err := wt.Add("test.txt"); err != nil {
		t.Fatalf("setup: Add error = %v", err)
	}
	commitHash, err := wt.Commit("initial commit", &git.CommitOptions{
		Author: &object.Signature{Name: "test", Email: "test@test.com"},
	})
	if err != nil {
		t.Fatalf("setup: Commit error = %v", err)
	}

	repo := Repo{
		Name:      "test-repo",
		LocalPath: localPath,
	}

	hash, err := repo.CurrentCommit()
	if err != nil {
		t.Fatalf("CurrentCommit() error = %v", err)
	}

	if hash != commitHash.String() {
		t.Errorf("CurrentCommit() = %q, want %q", hash, commitHash.String())
	}
}

func TestRepo_CurrentCommit_NotARepo(t *testing.T) {
	tmpDir := t.TempDir()

	repo := Repo{
		Name:      "not-a-repo",
		LocalPath: tmpDir,
	}

	_, err := repo.CurrentCommit()
	if err == nil {
		t.Error("CurrentCommit() should error for non-git directory")
	}
}

func TestRepo_ListFiles(t *testing.T) {
	tmpDir := t.TempDir()
	localPath := filepath.Join(tmpDir, "test-repo")

	gitRepo, err := git.PlainInit(localPath, false)
	if err != nil {
		t.Fatalf("setup: PlainInit error = %v", err)
	}

	files := map[string]string{
		"README.md":        "# Test",
		"src/main.go":      "package main",
		"src/lib/util.go":  "package lib",
		"docs/guide.md":    "# Guide",
	}

	for path, content := range files {
		fullPath := filepath.Join(localPath, path)
		if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
			t.Fatalf("setup: MkdirAll error = %v", err)
		}
		if err := os.WriteFile(fullPath, []byte(content), 0644); err != nil {
			t.Fatalf("setup: WriteFile error = %v", err)
		}
	}

	wt, err := gitRepo.Worktree()
	if err != nil {
		t.Fatalf("setup: Worktree error = %v", err)
	}
	if _, err := wt.Add("."); err != nil {
		t.Fatalf("setup: Add error = %v", err)
	}
	if _, err := wt.Commit("initial", &git.CommitOptions{
		Author: &object.Signature{Name: "test", Email: "test@test.com"},
	}); err != nil {
		t.Fatalf("setup: Commit error = %v", err)
	}

	repo := Repo{
		Name:      "test-repo",
		LocalPath: localPath,
	}

	result, err := repo.ListFiles()
	if err != nil {
		t.Fatalf("ListFiles() error = %v", err)
	}

	if len(result) != 4 {
		t.Errorf("ListFiles() returned %d files, want 4", len(result))
	}

	expected := []string{"README.md", "docs/guide.md", "src/lib/util.go", "src/main.go"}
	for _, exp := range expected {
		found := false
		for _, got := range result {
			if got == exp {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("ListFiles() missing expected file %q", exp)
		}
	}
}

func TestRepo_ListFiles_NotARepo(t *testing.T) {
	tmpDir := t.TempDir()

	repo := Repo{
		Name:      "not-a-repo",
		LocalPath: tmpDir,
	}

	_, err := repo.ListFiles()
	if err == nil {
		t.Error("ListFiles() should error for non-git directory")
	}
}

func TestRepo_ListFiles_ReturnsRelativePaths(t *testing.T) {
	tmpDir := t.TempDir()
	localPath := filepath.Join(tmpDir, "test-repo")

	gitRepo, err := git.PlainInit(localPath, false)
	if err != nil {
		t.Fatalf("setup: PlainInit error = %v", err)
	}

	nestedPath := filepath.Join(localPath, "deep", "nested", "file.txt")
	if err := os.MkdirAll(filepath.Dir(nestedPath), 0755); err != nil {
		t.Fatalf("setup: MkdirAll error = %v", err)
	}
	if err := os.WriteFile(nestedPath, []byte("content"), 0644); err != nil {
		t.Fatalf("setup: WriteFile error = %v", err)
	}

	wt, err := gitRepo.Worktree()
	if err != nil {
		t.Fatalf("setup: Worktree error = %v", err)
	}
	if _, err := wt.Add("."); err != nil {
		t.Fatalf("setup: Add error = %v", err)
	}
	if _, err := wt.Commit("initial", &git.CommitOptions{
		Author: &object.Signature{Name: "test", Email: "test@test.com"},
	}); err != nil {
		t.Fatalf("setup: Commit error = %v", err)
	}

	repo := Repo{
		Name:      "test-repo",
		LocalPath: localPath,
	}

	result, err := repo.ListFiles()
	if err != nil {
		t.Fatalf("ListFiles() error = %v", err)
	}

	if len(result) != 1 {
		t.Fatalf("ListFiles() returned %d files, want 1", len(result))
	}

	if result[0] != "deep/nested/file.txt" {
		t.Errorf("ListFiles() = %q, want relative path %q", result[0], "deep/nested/file.txt")
	}

	if filepath.IsAbs(result[0]) {
		t.Errorf("ListFiles() returned absolute path %q, want relative", result[0])
	}
}
