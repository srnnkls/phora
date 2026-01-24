package phora

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
)

// SHA must be 40-character hex string per spec.md line 229
var validSHA = regexp.MustCompile(`^[a-f0-9]{40}$`)

// Digest must be 64-character hex string per spec.md line 230
var validDigest = regexp.MustCompile(`^[a-f0-9]{64}$`)

// File sha256 must be 64-character hex string per spec.md line 233
var validFileSHA256 = regexp.MustCompile(`^[a-f0-9]{64}$`)

// Spec-compliant test fixture values
const (
	// Valid 40-char SHA (git commit hash)
	FixtureSHA = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"
	// Alternative valid SHA for tests that need two different values
	FixtureSHAAlt = "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0a1"
	// Valid 64-char digest (SHA256 of source config)
	FixtureDigest = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0a1b2c3d4e5f6a7b8c9d0e1f2"
	// Valid 64-char file SHA256
	FixtureFileSHA256 = "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d"
)

func TestFixture_SHA_IsSpecCompliant(t *testing.T) {
	if !validSHA.MatchString(FixtureSHA) {
		t.Errorf("FixtureSHA %q is not a valid 40-char hex string", FixtureSHA)
	}
	if !validSHA.MatchString(FixtureSHAAlt) {
		t.Errorf("FixtureSHAAlt %q is not a valid 40-char hex string", FixtureSHAAlt)
	}
}

func TestFixture_Digest_IsSpecCompliant(t *testing.T) {
	if !validDigest.MatchString(FixtureDigest) {
		t.Errorf("FixtureDigest %q is not a valid 64-char hex string", FixtureDigest)
	}
}

func TestFixture_FileSHA256_IsSpecCompliant(t *testing.T) {
	if !validFileSHA256.MatchString(FixtureFileSHA256) {
		t.Errorf("FixtureFileSHA256 %q is not a valid 64-char hex string", FixtureFileSHA256)
	}
}

// TestSourceLock_SpecCompliance_SHA verifies that SourceLock SHA values
// use spec-compliant 40-char hex strings.
func TestSourceLock_SpecCompliance_SHA(t *testing.T) {
	// Create entry using the SAME values as lockfile_test.go TestSourceLock_Fields
	entry := SourceLock{
		Name:   "company",
		Repo:   "company/shared-skills",
		Rev:    "main",
		SHA:    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
		Digest: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
	}

	if !validSHA.MatchString(entry.SHA) {
		t.Errorf("SourceLock.SHA %q is not spec-compliant: must be 40-char hex string (got %d chars)",
			entry.SHA, len(entry.SHA))
	}
}

// TestSourceLock_SpecCompliance_Digest verifies that SourceLock Digest values
// use spec-compliant 64-char hex strings.
func TestSourceLock_SpecCompliance_Digest(t *testing.T) {
	entry := SourceLock{
		Name:   "company",
		Repo:   "company/shared-skills",
		Rev:    "main",
		SHA:    FixtureSHA,
		Digest: FixtureDigest,
	}

	if !validDigest.MatchString(entry.Digest) {
		t.Errorf("SourceLock.Digest %q is not spec-compliant: must be 64-char hex string (got %d chars)",
			entry.Digest, len(entry.Digest))
	}
}

// TestLoadLock_TOMLContent_SpecCompliance verifies that TOML fixtures use
// spec-compliant values.
func TestLoadLock_TOMLContent_SpecCompliance(t *testing.T) {
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

	if !validSHA.MatchString(s.SHA) {
		t.Errorf("Loaded SHA %q is not spec-compliant: must be 40-char hex string", s.SHA)
	}
	if !validDigest.MatchString(s.Digest) {
		t.Errorf("Loaded Digest %q is not spec-compliant: must be 64-char hex string", s.Digest)
	}
}

// TestSaveLock_SpecCompliance verifies that the SaveLock test uses spec-compliant Digest values.
func TestSaveLock_SpecCompliance(t *testing.T) {
	digest := FixtureDigest

	if !validDigest.MatchString(digest) {
		t.Errorf("TestSaveLock fixture Digest %q is not spec-compliant: must be 64-char hex string (got %d chars)",
			digest, len(digest))
	}
}

// TestAddSource_SpecCompliance verifies that AddSource test fixtures use
// spec-compliant SHA values.
func TestAddSource_SpecCompliance(t *testing.T) {
	newSHA := "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
	if !validSHA.MatchString(newSHA) {
		t.Errorf("TestLock_AddSource_New fixture SHA %q is not spec-compliant", newSHA)
	}

	oldSHA := "1111111111111111111111111111111111111111"
	if !validSHA.MatchString(oldSHA) {
		t.Errorf("TestLock_AddSource_UpdateExisting old SHA %q is not spec-compliant", oldSHA)
	}

	updatedSHA := "2222222222222222222222222222222222222222"
	if !validSHA.MatchString(updatedSHA) {
		t.Errorf("TestLock_AddSource_UpdateExisting new SHA %q is not spec-compliant", updatedSHA)
	}
}

// TestFindSourceByName_SpecCompliance verifies that FindSourceByName test fixtures
// use spec-compliant SHA values.
func TestFindSourceByName_SpecCompliance(t *testing.T) {
	shas := []string{
		"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
		"cccccccccccccccccccccccccccccccccccccccc",
	}

	for _, sha := range shas {
		if !validSHA.MatchString(sha) {
			t.Errorf("TestLock_FindSourceByName fixture SHA %q is not spec-compliant", sha)
		}
	}
}

// TestV1Schema_SpecCompliance verifies that lockfile_v1_schema_test.go fixtures
// use spec-compliant values.
func TestV1Schema_SpecCompliance(t *testing.T) {
	sha := "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
	digest := "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"

	if !validSHA.MatchString(sha) {
		t.Errorf("lockfile_v1_schema_test.go fixture SHA %q is not spec-compliant", sha)
	}
	if !validDigest.MatchString(digest) {
		t.Errorf("lockfile_v1_schema_test.go fixture Digest %q is not spec-compliant", digest)
	}
}

// TestSourceLock_WithCompliantFixtures demonstrates the CORRECT way to write tests
// using spec-compliant fixture values.
func TestSourceLock_WithCompliantFixtures(t *testing.T) {
	now := time.Now()
	entry := SourceLock{
		Name:      "company",
		Repo:      "company/shared-skills",
		Rev:       "main",
		SHA:       FixtureSHA,
		Digest:    FixtureDigest,
		FetchedAt: now,
		Files: []FileLock{
			{
				Path:   "skills/test.md",
				SHA256: FixtureFileSHA256,
				Size:   1024,
			},
		},
	}

	// All of these should PASS with compliant fixtures
	if !validSHA.MatchString(entry.SHA) {
		t.Errorf("SHA must be 40-char hex, got %q", entry.SHA)
	}
	if !validDigest.MatchString(entry.Digest) {
		t.Errorf("Digest must be 64-char hex, got %q", entry.Digest)
	}
	if len(entry.Files) > 0 && !validFileSHA256.MatchString(entry.Files[0].SHA256) {
		t.Errorf("File SHA256 must be 64-char hex, got %q", entry.Files[0].SHA256)
	}
}

// TestLoadLock_WithCompliantTOML demonstrates the CORRECT TOML format with
// spec-compliant values.
func TestLoadLock_WithCompliantTOML(t *testing.T) {
	dir := t.TempDir()
	lockPath := filepath.Join(dir, "phora.lock")

	// TOML with spec-compliant values
	content := `version = 1

[[sources]]
name = "company"
repo = "company/shared-skills"
rev = "main"
sha = "` + FixtureSHA + `"
digest = "` + FixtureDigest + `"
fetched_at = 2026-01-14T10:00:00Z

[[sources.files]]
path = "skills/test.md"
sha256 = "` + FixtureFileSHA256 + `"
size = 1024
`
	if err := os.WriteFile(lockPath, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	lock, err := LoadLock(dir)
	if err != nil {
		t.Fatalf("LoadLock failed: %v", err)
	}

	s := lock.Sources[0]
	if !validSHA.MatchString(s.SHA) {
		t.Errorf("SHA %q is not spec-compliant", s.SHA)
	}
	if !validDigest.MatchString(s.Digest) {
		t.Errorf("Digest %q is not spec-compliant", s.Digest)
	}
	if len(s.Files) > 0 && !validFileSHA256.MatchString(s.Files[0].SHA256) {
		t.Errorf("File SHA256 %q is not spec-compliant", s.Files[0].SHA256)
	}
}

// TestLockFile_V1Format_HasNoReposSection verifies lockfile uses [[sources]] not [[repos]]
func TestLockFile_V1Format_HasNoReposSection(t *testing.T) {
	dir := t.TempDir()
	fetchedAt := time.Date(2026, 1, 18, 10, 0, 0, 0, time.UTC)

	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:      "test",
				Repo:      "org/test",
				Rev:       "main",
				SHA:       FixtureSHA,
				Digest:    FixtureDigest,
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
		t.Errorf("lock file should not contain [[repos]] section (v0 format), got:\n%s", content)
	}
	if !strings.Contains(content, "[[sources]]") {
		t.Errorf("lock file should contain [[sources]] section (v1 format), got:\n%s", content)
	}
}
