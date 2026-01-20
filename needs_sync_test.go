package phora

import "testing"

func TestSource_NeedsSync_NewSource(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	lock := &Lock{}

	needsSync := source.NeedsSync("skills", lock)

	if !needsSync {
		t.Error("NeedsSync should return true for source not in lock")
	}
}

func TestSource_NeedsSync_DigestMatch(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:   "skills",
				Digest: source.Digest(),
			},
		},
	}

	needsSync := source.NeedsSync("skills", lock)

	if needsSync {
		t.Error("NeedsSync should return false when digest matches")
	}
}

func TestSource_NeedsSync_DigestMismatch(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:   "skills",
				Digest: "0000000000000000000000000000000000000000000000000000000000000000",
			},
		},
	}

	needsSync := source.NeedsSync("skills", lock)

	if !needsSync {
		t.Error("NeedsSync should return true when digest differs")
	}
}

func TestSource_NeedsSync_EmptyLock(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	lock := &Lock{
		Sources: []SourceLock{},
	}

	needsSync := source.NeedsSync("skills", lock)

	if !needsSync {
		t.Error("NeedsSync should return true when lock has no sources")
	}
}

func TestSource_NeedsSync_MultipleSourcesInLock(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:   "other",
				Digest: "1111111111111111111111111111111111111111111111111111111111111111",
			},
			{
				Name:   "skills",
				Digest: source.Digest(),
			},
			{
				Name:   "another",
				Digest: "2222222222222222222222222222222222222222222222222222222222222222",
			},
		},
	}

	needsSync := source.NeedsSync("skills", lock)

	if needsSync {
		t.Error("NeedsSync should find correct source by name and return false for matching digest")
	}
}

func TestSource_NeedsSync_MultipleSourcesNotFound(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:   "other",
				Digest: source.Digest(),
			},
			{
				Name:   "another",
				Digest: source.Digest(),
			},
		},
	}

	needsSync := source.NeedsSync("skills", lock)

	if !needsSync {
		t.Error("NeedsSync should return true when source name not found in lock (even if digest would match)")
	}
}

func TestSource_NeedsSync_NilLock(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}

	needsSync := source.NeedsSync("skills", nil)

	if !needsSync {
		t.Error("NeedsSync should return true when lock is nil")
	}
}

func TestSource_NeedsSync_ConfigChangeTriggersSync(t *testing.T) {
	originalSource := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
	}
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:   "skills",
				Digest: originalSource.Digest(),
			},
		},
	}

	changedSource := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "new-skills",
	}

	needsSync := changedSource.NeedsSync("skills", lock)

	if !needsSync {
		t.Error("NeedsSync should return true when config changed (path differs)")
	}
}

func TestSource_NeedsSync_TargetChangeDoesNotTriggerSync(t *testing.T) {
	originalSource := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
		Target: ".claude/skills",
	}
	lock := &Lock{
		Sources: []SourceLock{
			{
				Name:   "skills",
				Digest: originalSource.Digest(),
			},
		},
	}

	changedSource := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
		Target: "vendor/skills",
	}

	needsSync := changedSource.NeedsSync("skills", lock)

	if needsSync {
		t.Error("NeedsSync should return false when only target changed (target excluded from digest)")
	}
}
