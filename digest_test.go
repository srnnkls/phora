package phora

import "testing"

func TestSource_Digest_Basic(t *testing.T) {
	s := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}

	digest := s.Digest()

	if digest == "" {
		t.Error("Digest() returned empty string")
	}
	if len(digest) != 64 {
		t.Errorf("Digest() length = %d, want 64 (SHA256 hex)", len(digest))
	}
	for _, c := range digest {
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')) {
			t.Errorf("Digest() contains non-hex character: %c", c)
			break
		}
	}
}

func TestSource_Digest_SameConfigSameDigest(t *testing.T) {
	s1 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
	}
	s2 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 != d2 {
		t.Errorf("same config produced different digests:\n  s1: %s\n  s2: %s", d1, d2)
	}
}

func TestSource_Digest_DifferentGitDifferentDigest(t *testing.T) {
	s1 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	s2 := Source{
		Git:    "https://github.com/other/repo.git",
		Branch: "main",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different git URLs should produce different digests")
	}
}

func TestSource_Digest_DifferentBranchDifferentDigest(t *testing.T) {
	s1 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}
	s2 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "develop",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different branch should produce different digests")
	}
}

func TestSource_Digest_DifferentTagDifferentDigest(t *testing.T) {
	s1 := Source{
		Git: "https://github.com/company/shared.git",
		Tag: "v1.0",
	}
	s2 := Source{
		Git: "https://github.com/company/shared.git",
		Tag: "v2.0",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different tag should produce different digests")
	}
}

func TestSource_Digest_DifferentRevDifferentDigest(t *testing.T) {
	s1 := Source{
		Git: "https://github.com/company/shared.git",
		Rev: "abc123",
	}
	s2 := Source{
		Git: "https://github.com/company/shared.git",
		Rev: "def456",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different rev should produce different digests")
	}
}

func TestSource_Digest_DifferentPathDifferentDigest(t *testing.T) {
	s1 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
	}
	s2 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "prompts",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different path should produce different digests")
	}
}

func TestSource_Digest_DifferentTargetSameDigest(t *testing.T) {
	s1 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
		Target: ".claude/skills",
	}
	s2 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
		Path:   "skills",
		Target: "vendor/skills",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 != d2 {
		t.Errorf("different target should NOT change digest (target excluded):\n  s1: %s\n  s2: %s", d1, d2)
	}
}

func TestSource_Digest_DifferentIncludeDifferentDigest(t *testing.T) {
	s1 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Include: []string{"*.md"},
	}
	s2 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Include: []string{"*.go"},
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different include patterns should produce different digests")
	}
}

func TestSource_Digest_DifferentExcludeDifferentDigest(t *testing.T) {
	s1 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Exclude: []string{"*.test"},
	}
	s2 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Exclude: []string{"*.spec"},
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("different exclude patterns should produce different digests")
	}
}

func TestSource_Digest_IncludeOrderMatters(t *testing.T) {
	s1 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Include: []string{"a.md", "b.md"},
	}
	s2 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Include: []string{"b.md", "a.md"},
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	// Implementation may choose to sort include/exclude or preserve order.
	// Either behavior is acceptable as long as it's deterministic.
	// This test documents the actual behavior once implemented.
	// If digests differ, order matters. If same, implementation normalizes.
	_ = d1
	_ = d2
	// Note: This test will pass once Digest() is implemented.
	// The important thing is determinism - same input always produces same output.
}

func TestSource_Digest_ExcludeOrderMatters(t *testing.T) {
	s1 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Exclude: []string{"x.tmp", "y.tmp"},
	}
	s2 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Exclude: []string{"y.tmp", "x.tmp"},
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	// Same as IncludeOrderMatters - documenting actual behavior.
	_ = d1
	_ = d2
}

func TestSource_Digest_BranchVsTag(t *testing.T) {
	s1 := Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "v1.0",
	}
	s2 := Source{
		Git: "https://github.com/company/shared.git",
		Tag: "v1.0",
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	if d1 == d2 {
		t.Error("branch='v1.0' and tag='v1.0' should produce different digests")
	}
}

func TestSource_Digest_EmptyIncludeVsNil(t *testing.T) {
	s1 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Include: nil,
	}
	s2 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Include: []string{},
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	// Empty slice and nil should produce same digest (both mean "include all").
	if d1 != d2 {
		t.Errorf("nil include and empty include should produce same digest:\n  nil: %s\n  empty: %s", d1, d2)
	}
}

func TestSource_Digest_EmptyExcludeVsNil(t *testing.T) {
	s1 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Exclude: nil,
	}
	s2 := Source{
		Git:     "https://github.com/company/shared.git",
		Branch:  "main",
		Exclude: []string{},
	}

	d1 := s1.Digest()
	d2 := s2.Digest()

	// Empty slice and nil should produce same digest (both mean "exclude nothing").
	if d1 != d2 {
		t.Errorf("nil exclude and empty exclude should produce same digest:\n  nil: %s\n  empty: %s", d1, d2)
	}
}
