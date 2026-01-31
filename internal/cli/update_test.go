package cli

import (
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/srnnkls/phora"
)

func TestUpdateCommand_ResolvesRefToNewSHA(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	// Use real public repo for testing
	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/go-git/go-billy.git",
				Branch: "master",
			},
		},
	}

	src := cfg.Sources["shared"]
	oldSHA := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	oldLock := &phora.Lock{
		Version: 1,
		Sources: []phora.SourceLock{
			{
				Name:      "shared",
				Repo:      "go-git/go-billy",
				Rev:       "master",
				SHA:       oldSHA,
				Digest:    (&src).Digest(),
				FetchedAt: time.Now().Add(-24 * time.Hour),
			},
		},
	}
	if err := oldLock.Save(lockDir); err != nil {
		t.Fatalf("failed to save initial lock: %v", err)
	}

	result, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	if result.SHA == oldSHA {
		t.Errorf("expected SHA to be re-resolved to new value, got same SHA: %s", result.SHA)
	}
	if result.SHA == "" {
		t.Error("expected non-empty SHA after update")
	}
}

func TestUpdateCommand_UpdatesLockDigest(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/go-git/go-billy.git",
				Branch: "master",
			},
		},
	}

	oldDigest := "0000000000000000000000000000000000000000000000000000000000000000"
	oldLock := &phora.Lock{
		Version: 1,
		Sources: []phora.SourceLock{
			{
				Name:      "shared",
				Repo:      "go-git/go-billy",
				Rev:       "master",
				SHA:       "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
				Digest:    oldDigest,
				FetchedAt: time.Now().Add(-24 * time.Hour),
			},
		},
	}
	if err := oldLock.Save(lockDir); err != nil {
		t.Fatalf("failed to save initial lock: %v", err)
	}

	_, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	lock, err := phora.LoadLock(lockDir)
	if err != nil {
		t.Fatalf("failed to load lock: %v", err)
	}

	sourceLock, found := lock.FindSourceByName("shared")
	if !found {
		t.Fatal("source 'shared' not found in lock after update")
	}

	src := cfg.Sources["shared"]
	expectedDigest := (&src).Digest()
	if sourceLock.Digest != expectedDigest {
		t.Errorf("digest = %q, want %q", sourceLock.Digest, expectedDigest)
	}
}

func TestUpdateCommand_SpecificSource(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"source-a": {
				Git:    "https://github.com/go-git/go-billy.git",
				Branch: "master",
			},
			"source-b": {
				Git:    "https://github.com/go-git/go-git.git",
				Branch: "master",
			},
		},
	}

	srcA := cfg.Sources["source-a"]
	srcB := cfg.Sources["source-b"]
	oldSHA_A := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	oldSHA_B := "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	now := time.Now().Add(-24 * time.Hour)

	oldLock := &phora.Lock{
		Version: 1,
		Sources: []phora.SourceLock{
			{
				Name:      "source-a",
				Repo:      "go-git/go-billy",
				Rev:       "master",
				SHA:       oldSHA_A,
				Digest:    (&srcA).Digest(),
				FetchedAt: now,
			},
			{
				Name:      "source-b",
				Repo:      "go-git/go-git",
				Rev:       "master",
				SHA:       oldSHA_B,
				Digest:    (&srcB).Digest(),
				FetchedAt: now,
			},
		},
	}
	if err := oldLock.Save(lockDir); err != nil {
		t.Fatalf("failed to save initial lock: %v", err)
	}

	_, err := updateSource(cfg, "source-a", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	lock, err := phora.LoadLock(lockDir)
	if err != nil {
		t.Fatalf("failed to load lock: %v", err)
	}

	sourceLockB, found := lock.FindSourceByName("source-b")
	if !found {
		t.Fatal("source-b not found in lock")
	}
	if sourceLockB.SHA != oldSHA_B {
		t.Errorf("source-b SHA was modified; got %s, want %s (should be unchanged)", sourceLockB.SHA, oldSHA_B)
	}
}

func TestUpdateCommand_AllSources(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"source-a": {
				Git:    "https://github.com/go-git/go-billy.git",
				Branch: "master",
			},
			"source-b": {
				Git:    "https://github.com/go-git/go-git.git",
				Branch: "master",
			},
		},
	}

	srcA := cfg.Sources["source-a"]
	srcB := cfg.Sources["source-b"]
	oldSHA_A := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	oldSHA_B := "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	now := time.Now().Add(-24 * time.Hour)

	oldLock := &phora.Lock{
		Version: 1,
		Sources: []phora.SourceLock{
			{
				Name:      "source-a",
				Repo:      "go-git/go-billy",
				Rev:       "master",
				SHA:       oldSHA_A,
				Digest:    (&srcA).Digest(),
				FetchedAt: now,
			},
			{
				Name:      "source-b",
				Repo:      "go-git/go-git",
				Rev:       "master",
				SHA:       oldSHA_B,
				Digest:    (&srcB).Digest(),
				FetchedAt: now,
			},
		},
	}
	if err := oldLock.Save(lockDir); err != nil {
		t.Fatalf("failed to save initial lock: %v", err)
	}

	results, err := updateAllSources(cfg, lockDir)
	if err != nil {
		t.Fatalf("updateAllSources failed: %v", err)
	}

	if len(results) != 2 {
		t.Errorf("expected 2 update results, got %d", len(results))
	}

	lock, err := phora.LoadLock(lockDir)
	if err != nil {
		t.Fatalf("failed to load lock: %v", err)
	}

	for _, name := range []string{"source-a", "source-b"} {
		sourceLock, found := lock.FindSourceByName(name)
		if !found {
			t.Errorf("source %q not found in lock after updateAll", name)
			continue
		}
		if sourceLock.FetchedAt.Before(now) || sourceLock.FetchedAt.Equal(now) {
			t.Errorf("source %q FetchedAt was not updated", name)
		}
	}
}

func TestUpdateCommand_SourceNotFound(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"existing": {
				Git:    "https://github.com/go-git/go-billy.git",
				Branch: "master",
			},
		},
	}

	_, err := updateSource(cfg, "nonexistent", lockDir)
	if err == nil {
		t.Fatal("expected error for nonexistent source, got nil")
	}

	want := "source 'nonexistent' not found in config"
	if err.Error() != want {
		t.Errorf("error = %q, want %q", err.Error(), want)
	}
}

func TestUpdateCommand_NoSources(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{},
	}

	results, err := updateAllSources(cfg, lockDir)
	if err != nil {
		t.Fatalf("updateAllSources failed: %v", err)
	}

	if len(results) != 0 {
		t.Errorf("expected 0 results for empty config, got %d", len(results))
	}
}

func TestUpdateCommand_UpdatesLockSHA(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/go-git/go-billy.git",
				Branch: "master",
			},
		},
	}

	src := cfg.Sources["shared"]
	oldSHA := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	oldLock := &phora.Lock{
		Version: 1,
		Sources: []phora.SourceLock{
			{
				Name:      "shared",
				Repo:      "go-git/go-billy",
				Rev:       "master",
				SHA:       oldSHA,
				Digest:    (&src).Digest(),
				FetchedAt: time.Now().Add(-24 * time.Hour),
			},
		},
	}
	if err := oldLock.Save(lockDir); err != nil {
		t.Fatalf("failed to save initial lock: %v", err)
	}

	result, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	lock, err := phora.LoadLock(lockDir)
	if err != nil {
		t.Fatalf("failed to load lock: %v", err)
	}

	sourceLock, found := lock.FindSourceByName("shared")
	if !found {
		t.Fatal("source 'shared' not found in lock after update")
	}

	if sourceLock.SHA != result.SHA {
		t.Errorf("lock SHA = %q, result SHA = %q; expected them to match", sourceLock.SHA, result.SHA)
	}
	if sourceLock.SHA == oldSHA {
		t.Errorf("lock SHA should be updated from old value %q", oldSHA)
	}
}

func TestUpdateSource_SHA_IsDeterministic(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/hashicorp/go-getter.git",
				Branch: "master",
			},
		},
	}

	result1, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("first updateSource failed: %v", err)
	}

	time.Sleep(10 * time.Millisecond)

	result2, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("second updateSource failed: %v", err)
	}

	if result1.SHA != result2.SHA {
		t.Errorf("SHA is not deterministic: first call returned %q, second call returned %q; "+
			"expected same SHA for same source+ref (SHA should not depend on timestamp)",
			result1.SHA, result2.SHA)
	}
}

func TestUpdateSource_SHA_IsValid40CharHex(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/hashicorp/go-getter.git",
				Branch: "master",
			},
		},
	}

	result, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	if len(result.SHA) != 40 {
		t.Errorf("SHA length = %d, want 40 (git commit SHAs are 40 hex characters)", len(result.SHA))
	}

	for i, c := range result.SHA {
		isHex := (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')
		if !isHex {
			t.Errorf("SHA contains invalid hex character %q at position %d", c, i)
		}
	}
}

func TestUpdateSource_SHA_MatchesGitLsRemote(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping network-dependent test in short mode")
	}

	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/hashicorp/go-getter.git",
				Branch: "master",
			},
		},
	}

	result, err := updateSource(cfg, "shared", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	expectedSHA := gitLsRemote(t, "https://github.com/hashicorp/go-getter.git", "refs/heads/master")

	if result.SHA != expectedSHA {
		t.Errorf("SHA = %q, want %q (from git ls-remote); "+
			"updateSource must resolve refs to real git commit SHAs, not generate fake ones",
			result.SHA, expectedSHA)
	}
}

func TestUpdateSource_SHA_ResolvesTagRef(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping network-dependent test in short mode")
	}

	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"getter": {
				Git: "https://github.com/hashicorp/go-getter.git",
				Tag: "v1.7.0",
			},
		},
	}

	result, err := updateSource(cfg, "getter", lockDir)
	if err != nil {
		t.Fatalf("updateSource failed: %v", err)
	}

	expectedSHA := gitLsRemote(t, "https://github.com/hashicorp/go-getter.git", "refs/tags/v1.7.0")

	if result.SHA != expectedSHA {
		t.Errorf("SHA = %q, want %q (from git ls-remote for tag v1.7.0); "+
			"updateSource must resolve tag refs to real git commit SHAs",
			result.SHA, expectedSHA)
	}
}

func gitLsRemote(t *testing.T, repoURL, ref string) string {
	t.Helper()

	cmd := exec.Command("git", "ls-remote", repoURL, ref)
	output, err := cmd.Output()
	if err != nil {
		t.Fatalf("git ls-remote failed: %v", err)
	}

	parts := strings.Fields(string(output))
	if len(parts) < 1 {
		t.Fatalf("git ls-remote returned unexpected output: %q", string(output))
	}

	return parts[0]
}
