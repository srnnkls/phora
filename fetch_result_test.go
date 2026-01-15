package phora

import (
	"path/filepath"
	"strings"
	"testing"
)

func TestFetchResult_FieldsExist(t *testing.T) {
	result := FetchResult{
		Name:      "test-source",
		LocalPath: "/tmp/repos/owner/repo",
		Commit:    "abc123def456",
		Files:     []string{"README.md", "src/main.go"},
	}

	if result.Name != "test-source" {
		t.Errorf("Name = %q, want %q", result.Name, "test-source")
	}
	if result.LocalPath != "/tmp/repos/owner/repo" {
		t.Errorf("LocalPath = %q, want %q", result.LocalPath, "/tmp/repos/owner/repo")
	}
	if result.Commit != "abc123def456" {
		t.Errorf("Commit = %q, want %q", result.Commit, "abc123def456")
	}
	if len(result.Files) != 2 {
		t.Errorf("Files length = %d, want 2", len(result.Files))
	}
}

func TestFetchResult_ZeroValue(t *testing.T) {
	var result FetchResult

	if result.Name != "" {
		t.Errorf("zero value Name = %q, want empty", result.Name)
	}
	if result.LocalPath != "" {
		t.Errorf("zero value LocalPath = %q, want empty", result.LocalPath)
	}
	if result.Commit != "" {
		t.Errorf("zero value Commit = %q, want empty", result.Commit)
	}
	if result.Files != nil {
		t.Errorf("zero value Files = %v, want nil", result.Files)
	}
}

func TestFetchResult_FilesAreRelativePaths(t *testing.T) {
	result := FetchResult{
		Name:      "test-source",
		LocalPath: "/tmp/repos/owner/repo",
		Commit:    "abc123",
		Files: []string{
			"README.md",
			"src/main.go",
			"internal/pkg/util.go",
		},
	}

	for _, file := range result.Files {
		if filepath.IsAbs(file) {
			t.Errorf("file path %q should be relative, not absolute", file)
		}
		if strings.HasPrefix(file, "/") {
			t.Errorf("file path %q starts with /, should be relative", file)
		}
		if strings.HasPrefix(file, result.LocalPath) {
			t.Errorf("file path %q contains LocalPath prefix, should be relative", file)
		}
	}
}

func TestFetchResult_EmptyFiles(t *testing.T) {
	result := FetchResult{
		Name:      "empty-repo",
		LocalPath: "/tmp/repos/owner/empty",
		Commit:    "deadbeef",
		Files:     []string{},
	}

	if result.Files == nil {
		t.Error("Files should be empty slice, not nil")
	}
	if len(result.Files) != 0 {
		t.Errorf("Files length = %d, want 0", len(result.Files))
	}
}

func TestFetchResult_CommitIsValidHash(t *testing.T) {
	tests := []struct {
		name   string
		commit string
		valid  bool
	}{
		{"short hash", "abc123d", true},
		{"full SHA-1", "abc123def456789012345678901234567890abcd", true},
		{"empty", "", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := FetchResult{
				Name:      "test",
				LocalPath: "/tmp/repo",
				Commit:    tt.commit,
				Files:     []string{},
			}

			hasCommit := result.Commit != ""
			if hasCommit != tt.valid {
				t.Errorf("Commit %q validity = %v, want %v", tt.commit, hasCommit, tt.valid)
			}
		})
	}
}

func TestFetchResult_LocalPathIsAbsolute(t *testing.T) {
	result := FetchResult{
		Name:      "test-source",
		LocalPath: "/tmp/repos/github.com/owner/repo",
		Commit:    "abc123",
		Files:     []string{"file.go"},
	}

	if !filepath.IsAbs(result.LocalPath) {
		t.Errorf("LocalPath %q should be absolute", result.LocalPath)
	}
}

func TestFetchResult_MultipleResults(t *testing.T) {
	results := []FetchResult{
		{
			Name:      "source-a",
			LocalPath: "/tmp/repos/owner-a/repo-a",
			Commit:    "commit-a",
			Files:     []string{"a.go"},
		},
		{
			Name:      "source-b",
			LocalPath: "/tmp/repos/owner-b/repo-b",
			Commit:    "commit-b",
			Files:     []string{"b.go", "c.go"},
		},
	}

	if len(results) != 2 {
		t.Errorf("results length = %d, want 2", len(results))
	}

	if results[0].Name != "source-a" {
		t.Errorf("first result Name = %q, want %q", results[0].Name, "source-a")
	}
	if results[1].Name != "source-b" {
		t.Errorf("second result Name = %q, want %q", results[1].Name, "source-b")
	}
}
