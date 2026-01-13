package source

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/srnnkls/phora/internal/config"
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
		{"srnnkls/phora", "github.com", "srnnkls", "phora"},
		{"github.com/org/repo", "github.com", "org", "repo"},
		{"gitlab.com/org/repo", "gitlab.com", "org", "repo"},
		{"https://github.com/owner/repo", "github.com", "owner", "repo"},
		{"https://github.com/owner/repo.git", "github.com", "owner", "repo"},
		{"https://gitlab.com/owner/repo", "gitlab.com", "owner", "repo"},
		{"http://custom.git/owner/repo", "custom.git", "owner", "repo"},
		{"bitbucket.org/team/project", "bitbucket.org", "team", "project"},
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
		Repo:    "phora",
		DataDir: "/home/user/.local/share/phora/repos",
	}

	expected := "/home/user/.local/share/phora/repos/github.com/srnnkls/phora"
	if src.LocalPath() != expected {
		t.Errorf("LocalPath() = %q, want %q", src.LocalPath(), expected)
	}
}

func TestExpandTemplate(t *testing.T) {
	tests := []struct {
		name     string
		template string
		owner    string
		repo     string
		ref      string
		path     string
		want     string
	}{
		{
			name:     "github git URL",
			template: "https://github.com/{owner}/{repo}.git",
			owner:    "srnnkls",
			repo:     "phora",
			ref:      "main",
			path:     "",
			want:     "https://github.com/srnnkls/phora.git",
		},
		{
			name:     "github raw URL",
			template: "https://raw.githubusercontent.com/{owner}/{repo}/{ref}/{path}",
			owner:    "srnnkls",
			repo:     "phora",
			ref:      "main",
			path:     "phora.toml",
			want:     "https://raw.githubusercontent.com/srnnkls/phora/main/phora.toml",
		},
		{
			name:     "gitlab raw URL",
			template: "https://gitlab.com/{owner}/{repo}/-/raw/{ref}/{path}",
			owner:    "myorg",
			repo:     "myproject",
			ref:      "develop",
			path:     "config.toml",
			want:     "https://gitlab.com/myorg/myproject/-/raw/develop/config.toml",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := expandTemplate(tt.template, tt.owner, tt.repo, tt.ref, tt.path)
			if got != tt.want {
				t.Errorf("expandTemplate() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestRepoSourceURLsWithHostConfig(t *testing.T) {
	hostConfig := &config.Host{
		GitURL: "https://github.com/{owner}/{repo}.git",
		RawURL: "https://raw.githubusercontent.com/{owner}/{repo}/{ref}/{path}",
	}

	src := &RepoSource{
		Host:       "github.com",
		Owner:      "srnnkls",
		Repo:       "phora",
		Ref:        "main",
		HostConfig: hostConfig,
	}

	t.Run("RepoURL with config", func(t *testing.T) {
		got := src.RepoURL()
		want := "https://github.com/srnnkls/phora.git"
		if got != want {
			t.Errorf("RepoURL() = %q, want %q", got, want)
		}
	})

	t.Run("ConfigURL with config", func(t *testing.T) {
		got := src.ConfigURL()
		want := "https://raw.githubusercontent.com/srnnkls/phora/main/phora.toml"
		if got != want {
			t.Errorf("ConfigURL() = %q, want %q", got, want)
		}
	})
}

func TestRepoSourceURLsWithoutHostConfig(t *testing.T) {
	src := &RepoSource{
		Host:       "custom-git.company.com",
		Owner:      "team",
		Repo:       "project",
		Ref:        "main",
		HostConfig: nil,
	}

	t.Run("RepoURL fallback", func(t *testing.T) {
		got := src.RepoURL()
		want := "https://custom-git.company.com/team/project.git"
		if got != want {
			t.Errorf("RepoURL() = %q, want %q (fallback)", got, want)
		}
	})

	t.Run("ConfigURL no fallback", func(t *testing.T) {
		got := src.ConfigURL()
		if got != "" {
			t.Errorf("ConfigURL() = %q, want empty string (no host config)", got)
		}
	})
}

func TestRepoSourceFetchConfigWithoutHostConfig(t *testing.T) {
	src := &RepoSource{
		Host:       "unknown-host.com",
		Owner:      "owner",
		Repo:       "repo",
		Ref:        "main",
		HostConfig: nil,
	}

	_, err := src.FetchConfig()
	if err == nil {
		t.Error("FetchConfig() should return error when host config is missing")
	}
	if err != nil && err.Error() != "no host configuration for unknown-host.com (direct config fetch not supported)" {
		t.Errorf("FetchConfig() error = %q, want host config error", err.Error())
	}
}
