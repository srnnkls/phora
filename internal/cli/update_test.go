package cli

import (
	"testing"
	"time"

	"github.com/srnnkls/phora"
)

func TestUpdateCommand_ResolvesRefToNewSHA(t *testing.T) {
	tmpDir := t.TempDir()
	lockDir := tmpDir

	cfg := &phora.Config{
		Version: 1,
		Sources: map[string]phora.Source{
			"shared": {
				Git:    "https://github.com/company/shared.git",
				Branch: "main",
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
				Repo:      "company/shared",
				Ref:       "main",
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
				Git:    "https://github.com/company/shared.git",
				Branch: "main",
			},
		},
	}

	oldDigest := "0000000000000000000000000000000000000000000000000000000000000000"
	oldLock := &phora.Lock{
		Version: 1,
		Sources: []phora.SourceLock{
			{
				Name:      "shared",
				Repo:      "company/shared",
				Ref:       "main",
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
				Git:    "https://github.com/org/repo-a.git",
				Branch: "main",
			},
			"source-b": {
				Git:    "https://github.com/org/repo-b.git",
				Branch: "main",
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
				Repo:      "org/repo-a",
				Ref:       "main",
				SHA:       oldSHA_A,
				Digest:    (&srcA).Digest(),
				FetchedAt: now,
			},
			{
				Name:      "source-b",
				Repo:      "org/repo-b",
				Ref:       "main",
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
				Git:    "https://github.com/org/repo-a.git",
				Branch: "main",
			},
			"source-b": {
				Git:    "https://github.com/org/repo-b.git",
				Branch: "develop",
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
				Repo:      "org/repo-a",
				Ref:       "main",
				SHA:       oldSHA_A,
				Digest:    (&srcA).Digest(),
				FetchedAt: now,
			},
			{
				Name:      "source-b",
				Repo:      "org/repo-b",
				Ref:       "develop",
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
				Git:    "https://github.com/org/repo.git",
				Branch: "main",
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
				Git:    "https://github.com/company/shared.git",
				Branch: "main",
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
				Repo:      "company/shared",
				Ref:       "main",
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
