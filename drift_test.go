package phora

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestDetectDrift_NoDrift(t *testing.T) {
	dir := t.TempDir()

	fileContent := []byte("hello world\n")
	filePath := "skills/code-test/SKILL.md"
	fullPath := filepath.Join(dir, filePath)

	if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(fullPath, fileContent, 0644); err != nil {
		t.Fatal(err)
	}

	hash, _, err := ComputeFileHash(fullPath)
	if err != nil {
		t.Fatal(err)
	}

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Rev:       "v1.0",
				SHA:       "abc123",
				FetchedAt: time.Now(),
				Files: []FileLock{
					{Path: filePath, SHA256: hash, Size: int64(len(fileContent))},
				},
			},
		},
	}

	results, err := DetectDrift(lock, "shared", dir)
	if err != nil {
		t.Fatalf("DetectDrift() error = %v", err)
	}

	if len(results) != 0 {
		t.Errorf("DetectDrift() returned %d results, want 0 (no drift)", len(results))
	}
}

func TestDetectDrift_ModifiedFile(t *testing.T) {
	dir := t.TempDir()

	filePath := "skills/code-test/SKILL.md"
	fullPath := filepath.Join(dir, filePath)

	if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(fullPath, []byte("modified content\n"), 0644); err != nil {
		t.Fatal(err)
	}

	originalHash := "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Rev:       "v1.0",
				SHA:       "abc123",
				FetchedAt: time.Now(),
				Files: []FileLock{
					{Path: filePath, SHA256: originalHash, Size: 12},
				},
			},
		},
	}

	results, err := DetectDrift(lock, "shared", dir)
	if err != nil {
		t.Fatalf("DetectDrift() error = %v", err)
	}

	if len(results) != 1 {
		t.Fatalf("DetectDrift() returned %d results, want 1", len(results))
	}

	r := results[0]
	if r.Path != filePath {
		t.Errorf("Path = %q, want %q", r.Path, filePath)
	}
	if r.Expected != originalHash {
		t.Errorf("Expected = %q, want %q", r.Expected, originalHash)
	}
	if r.Actual == "" {
		t.Error("Actual should contain current hash, not empty")
	}
	if r.Actual == originalHash {
		t.Error("Actual should differ from Expected for modified file")
	}
	if r.Status != DriftModified {
		t.Errorf("Status = %v, want DriftModified", r.Status)
	}
}

func TestDetectDrift_MissingFile(t *testing.T) {
	dir := t.TempDir()

	filePath := "skills/code-test/SKILL.md"
	originalHash := "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Rev:       "v1.0",
				SHA:       "abc123",
				FetchedAt: time.Now(),
				Files: []FileLock{
					{Path: filePath, SHA256: originalHash, Size: 12},
				},
			},
		},
	}

	results, err := DetectDrift(lock, "shared", dir)
	if err != nil {
		t.Fatalf("DetectDrift() error = %v", err)
	}

	if len(results) != 1 {
		t.Fatalf("DetectDrift() returned %d results, want 1", len(results))
	}

	r := results[0]
	if r.Path != filePath {
		t.Errorf("Path = %q, want %q", r.Path, filePath)
	}
	if r.Expected != originalHash {
		t.Errorf("Expected = %q, want %q", r.Expected, originalHash)
	}
	if r.Actual != "" {
		t.Errorf("Actual = %q, want empty string for missing file", r.Actual)
	}
	if r.Status != DriftMissing {
		t.Errorf("Status = %v, want DriftMissing", r.Status)
	}
}

func TestDetectDrift_ExtraFiles(t *testing.T) {
	dir := t.TempDir()

	fileContent := []byte("hello world\n")
	trackedPath := "skills/code-test/SKILL.md"
	extraPath := "skills/extra/SKILL.md"

	for _, p := range []string{trackedPath, extraPath} {
		fullPath := filepath.Join(dir, p)
		if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(fullPath, fileContent, 0644); err != nil {
			t.Fatal(err)
		}
	}

	hash, _, err := ComputeFileHash(filepath.Join(dir, trackedPath))
	if err != nil {
		t.Fatal(err)
	}

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Rev:       "v1.0",
				SHA:       "abc123",
				FetchedAt: time.Now(),
				Files: []FileLock{
					{Path: trackedPath, SHA256: hash, Size: int64(len(fileContent))},
				},
			},
		},
	}

	results, err := DetectDrift(lock, "shared", dir)
	if err != nil {
		t.Fatalf("DetectDrift() error = %v", err)
	}

	if len(results) != 0 {
		t.Errorf("DetectDrift() returned %d results, want 0 (extra files should be ignored)", len(results))
	}
}

func TestDetectDrift_MultipleFiles(t *testing.T) {
	dir := t.TempDir()

	fileContent := []byte("hello world\n")
	modifiedContent := []byte("modified content\n")
	originalHash := "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"

	files := []struct {
		path    string
		content []byte
	}{
		{"skills/a/SKILL.md", fileContent},
		{"skills/b/SKILL.md", modifiedContent},
		{"skills/d/SKILL.md", fileContent},
	}

	for _, f := range files {
		fullPath := filepath.Join(dir, f.path)
		if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(fullPath, f.content, 0644); err != nil {
			t.Fatal(err)
		}
	}

	hash, _, err := ComputeFileHash(filepath.Join(dir, "skills/a/SKILL.md"))
	if err != nil {
		t.Fatal(err)
	}

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Rev:       "v1.0",
				SHA:       "abc123",
				FetchedAt: time.Now(),
				Files: []FileLock{
					{Path: "skills/a/SKILL.md", SHA256: hash, Size: int64(len(fileContent))},
					{Path: "skills/b/SKILL.md", SHA256: originalHash, Size: 12},
					{Path: "skills/c/SKILL.md", SHA256: originalHash, Size: 12},
					{Path: "skills/d/SKILL.md", SHA256: hash, Size: int64(len(fileContent))},
				},
			},
		},
	}

	results, err := DetectDrift(lock, "shared", dir)
	if err != nil {
		t.Fatalf("DetectDrift() error = %v", err)
	}

	if len(results) != 2 {
		t.Fatalf("DetectDrift() returned %d results, want 2 (b=modified, c=missing)", len(results))
	}

	resultsByPath := make(map[string]DriftResult)
	for _, r := range results {
		resultsByPath[r.Path] = r
	}

	if r, ok := resultsByPath["skills/b/SKILL.md"]; !ok {
		t.Error("expected skills/b/SKILL.md in results (modified)")
	} else if r.Status != DriftModified {
		t.Errorf("skills/b/SKILL.md Status = %v, want DriftModified", r.Status)
	}

	if r, ok := resultsByPath["skills/c/SKILL.md"]; !ok {
		t.Error("expected skills/c/SKILL.md in results (missing)")
	} else if r.Status != DriftMissing {
		t.Errorf("skills/c/SKILL.md Status = %v, want DriftMissing", r.Status)
	}
}

func TestDetectDrift_SourceNotInLock(t *testing.T) {
	dir := t.TempDir()

	filePath := "skills/code-test/SKILL.md"
	fullPath := filepath.Join(dir, filePath)

	if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(fullPath, []byte("content"), 0644); err != nil {
		t.Fatal(err)
	}

	lock := &Lock{
		Version: 1,
		Sources: []SourceLock{
			{
				Name:      "other-source",
				Repo:      "company/other",
				Rev:       "v1.0",
				SHA:       "abc123",
				FetchedAt: time.Now(),
				Files: []FileLock{
					{Path: filePath, SHA256: "somehash", Size: 100},
				},
			},
		},
	}

	results, err := DetectDrift(lock, "nonexistent-source", dir)
	if err != nil {
		t.Fatalf("DetectDrift() error = %v", err)
	}

	if len(results) != 0 {
		t.Errorf("DetectDrift() returned %d results, want 0 (source not in lock)", len(results))
	}
}
