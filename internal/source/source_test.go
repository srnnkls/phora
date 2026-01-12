package source

import (
	"os"
	"path/filepath"
	"testing"
)

func setupTestSource(t *testing.T) string {
	t.Helper()
	tmpDir := t.TempDir()

	// Directory-based skill
	skillDir := filepath.Join(tmpDir, "skills", "code-test")
	os.MkdirAll(skillDir, 0755)
	os.WriteFile(filepath.Join(skillDir, "SKILL.md"), []byte(`---
name: code-test
description: TDD workflow
---

# Code Test
`), 0644)
	os.MkdirAll(filepath.Join(skillDir, "reference"), 0755)
	os.WriteFile(filepath.Join(skillDir, "reference", "guide.md"), []byte("# Guide"), 0644)

	// Single-file skill
	os.WriteFile(filepath.Join(tmpDir, "skills", "simple.md"), []byte(`---
name: simple
---

# Simple
`), 0644)

	// Command
	cmdDir := filepath.Join(tmpDir, "commands", "spec.create")
	os.MkdirAll(cmdDir, 0755)
	os.WriteFile(filepath.Join(cmdDir, "COMMAND.md"), []byte(`---
name: spec.create
---

# Spec Create
`), 0644)

	// Agent
	agentsDir := filepath.Join(tmpDir, "agents")
	os.MkdirAll(agentsDir, 0755)
	os.WriteFile(filepath.Join(agentsDir, "tester.md"), []byte(`---
name: tester
---

# Tester
`), 0644)

	return tmpDir
}

func TestLocalSourceDiscover(t *testing.T) {
	srcDir := setupTestSource(t)

	src := NewLocal(srcDir, []string{"skills", "commands", "agents"})

	artifacts, err := src.Discover()
	if err != nil {
		t.Fatalf("Discover() error = %v", err)
	}

	if len(artifacts) != 4 {
		t.Errorf("Discover() = %d artifacts, want 4", len(artifacts))
		for _, a := range artifacts {
			t.Logf("  - %s (%s)", a.Name, a.Type)
		}
	}

	// Find code-test skill
	var codeTest *struct{ hasResources bool }
	for _, a := range artifacts {
		if a.Name == "code-test" {
			codeTest = &struct{ hasResources bool }{hasResources: len(a.Resources) > 0}
			break
		}
	}

	if codeTest == nil {
		t.Error("code-test skill not found")
	} else if !codeTest.hasResources {
		t.Error("code-test should have resources")
	}
}

func TestLocalSourceName(t *testing.T) {
	src := NewLocal("/path/to/source", nil)

	if src.Name() != "/path/to/source" {
		t.Errorf("Name() = %q", src.Name())
	}
}

func TestLocalSourceFilterByType(t *testing.T) {
	srcDir := setupTestSource(t)

	src := NewLocal(srcDir, []string{"skills"})

	artifacts, err := src.Discover()
	if err != nil {
		t.Fatalf("Discover() error = %v", err)
	}

	if len(artifacts) != 2 {
		t.Errorf("Discover() = %d artifacts, want 2 (skills only)", len(artifacts))
	}

	for _, a := range artifacts {
		if a.Type != "skill" {
			t.Errorf("found non-skill artifact: %s (%s)", a.Name, a.Type)
		}
	}
}

func TestLocalSourceEmptyDir(t *testing.T) {
	tmpDir := t.TempDir()

	src := NewLocal(tmpDir, []string{"skills", "commands"})

	artifacts, err := src.Discover()
	if err != nil {
		t.Fatalf("Discover() error = %v", err)
	}

	if len(artifacts) != 0 {
		t.Errorf("Discover() = %d, want 0 for empty source", len(artifacts))
	}
}

func TestRepoSourceParseRepoString(t *testing.T) {
	tests := []struct {
		input     string
		wantHost  string
		wantOwner string
		wantRepo  string
	}{
		{"srnnkls/tropos", "github.com", "srnnkls", "tropos"},
		{"github.com/org/repo", "github.com", "org", "repo"},
		{"gitlab.com/org/repo", "gitlab.com", "org", "repo"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			host, owner, repo := ParseRepoString(tt.input)
			if host != tt.wantHost || owner != tt.wantOwner || repo != tt.wantRepo {
				t.Errorf("ParseRepoString(%q) = %q, %q, %q; want %q, %q, %q",
					tt.input, host, owner, repo, tt.wantHost, tt.wantOwner, tt.wantRepo)
			}
		})
	}
}

func TestRepoSourceDataDir(t *testing.T) {
	src := &RepoSource{
		Host:    "github.com",
		Owner:   "srnnkls",
		Repo:    "tropos",
		DataDir: "/home/user/.local/share/tropos/repos",
	}

	expected := "/home/user/.local/share/tropos/repos/github.com/srnnkls/tropos"
	if src.LocalPath() != expected {
		t.Errorf("LocalPath() = %q, want %q", src.LocalPath(), expected)
	}
}
