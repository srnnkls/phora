package cli

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/srnnkls/phora"
	"github.com/srnnkls/phora/internal/config"
)

func TestAddCommand_SourceNameCollision(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	existingConfig := &config.Config{
		Version: 1,
		Sources: map[string]config.Source{
			"dotfiles": {
				Git:    "https://github.com/srnnkls/dotfiles.git",
				Branch: "main",
			},
		},
	}
	if err := config.WriteFile(configPath, existingConfig); err != nil {
		t.Fatalf("failed to write config: %v", err)
	}

	err := checkSourceNameCollision(configPath, "dotfiles")
	if err == nil {
		t.Fatal("expected error for source name collision, got nil")
	}

	want := "source 'dotfiles' already exists"
	if err.Error() != want {
		t.Errorf("error = %q, want %q", err.Error(), want)
	}
}

func TestAddCommand_NoCollisionForNewSource(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	existingConfig := &config.Config{
		Version: 1,
		Sources: map[string]config.Source{
			"existing-source": {
				Git:    "https://github.com/owner/existing-source.git",
				Branch: "main",
			},
		},
	}
	if err := config.WriteFile(configPath, existingConfig); err != nil {
		t.Fatalf("failed to write config: %v", err)
	}

	err := checkSourceNameCollision(configPath, "new-source")
	if err != nil {
		t.Errorf("unexpected error for new source: %v", err)
	}
}

func TestAddCommand_CollisionCheckMissingFile(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "nonexistent", "phora.toml")

	err := checkSourceNameCollision(configPath, "any-source")
	if err != nil {
		t.Errorf("unexpected error for missing config: %v", err)
	}
}

func TestRepoNameFromGitURL(t *testing.T) {
	tests := []struct {
		gitURL   string
		wantRepo string
	}{
		{
			gitURL:   "https://github.com/srnnkls/dotfiles.git",
			wantRepo: "dotfiles",
		},
		{
			gitURL:   "https://gitlab.com/company/project.git",
			wantRepo: "project",
		},
	}

	for _, tt := range tests {
		t.Run(tt.gitURL, func(t *testing.T) {
			repo := repoNameFromGitURL(tt.gitURL)
			if repo != tt.wantRepo {
				t.Errorf("repo = %q, want %q", repo, tt.wantRepo)
			}
		})
	}
}

func TestParseURL_Integration(t *testing.T) {
	tests := []struct {
		name       string
		url        string
		wantRepo   string
		wantPath   string
		wantBranch string
	}{
		{
			name:       "owner/repo shorthand",
			url:        "srnnkls/dotfiles",
			wantRepo:   "dotfiles",
			wantPath:   "",
			wantBranch: "",
		},
		{
			name:       "GitHub URL with tree",
			url:        "https://github.com/owner/project/tree/main/src",
			wantRepo:   "project",
			wantPath:   "src",
			wantBranch: "main",
		},
		{
			name:       "GitLab shorthand",
			url:        "gitlab.com/company/configs",
			wantRepo:   "configs",
			wantPath:   "",
			wantBranch: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			parsed, err := phora.ParseURL(tt.url)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.url, err)
			}

			repo := repoNameFromGitURL(parsed.Git)
			if repo != tt.wantRepo {
				t.Errorf("repo = %q, want %q", repo, tt.wantRepo)
			}
			if parsed.Path != tt.wantPath {
				t.Errorf("path = %q, want %q", parsed.Path, tt.wantPath)
			}
			if parsed.Branch != tt.wantBranch {
				t.Errorf("ref = %q, want %q", parsed.Branch, tt.wantBranch)
			}
		})
	}
}

func TestAddSource_V1ConfigFormat(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	src := config.Source{
		Git:    "https://github.com/srnnkls/dotfiles.git",
		Branch: "main",
	}

	if err := config.AddSource(configPath, "dotfiles", src); err != nil {
		t.Fatalf("AddSource failed: %v", err)
	}

	content, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatalf("failed to read config: %v", err)
	}

	contentStr := string(content)

	if !strings.HasPrefix(contentStr, "version = 1\n") {
		t.Errorf("config should start with 'version = 1', got:\n%s", contentStr)
	}
}

func TestAddSource_GitURLFormat(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	src := config.Source{
		Git:    "https://github.com/srnnkls/dotfiles.git",
		Branch: "main",
	}

	if err := config.AddSource(configPath, "dotfiles", src); err != nil {
		t.Fatalf("AddSource failed: %v", err)
	}

	content, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatalf("failed to read config: %v", err)
	}

	contentStr := string(content)

	if !strings.Contains(contentStr, `git = "https://github.com/srnnkls/dotfiles.git"`) &&
		!strings.Contains(contentStr, `git = 'https://github.com/srnnkls/dotfiles.git'`) {
		t.Errorf("config should contain git URL format, got:\n%s", contentStr)
	}

	if strings.Contains(contentStr, `host = "github.com"`) {
		t.Errorf("config should NOT contain deprecated 'host' field, got:\n%s", contentStr)
	}

	if strings.Contains(contentStr, `owner = "srnnkls"`) {
		t.Errorf("config should NOT contain deprecated 'owner' field, got:\n%s", contentStr)
	}

	if strings.Contains(contentStr, `repo = "dotfiles"`) {
		t.Errorf("config should NOT contain deprecated 'repo' field, got:\n%s", contentStr)
	}
}

func TestAddSource_BranchRefFormat(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	src := config.Source{
		Git:    "https://github.com/company/shared.git",
		Branch: "main",
	}

	if err := config.AddSource(configPath, "shared", src); err != nil {
		t.Fatalf("AddSource failed: %v", err)
	}

	content, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatalf("failed to read config: %v", err)
	}

	contentStr := string(content)

	if !strings.Contains(contentStr, `branch = "main"`) &&
		!strings.Contains(contentStr, `branch = 'main'`) {
		t.Errorf("config should contain 'branch = \"main\"', got:\n%s", contentStr)
	}

	if strings.Contains(contentStr, `ref = "main"`) {
		t.Errorf("config should NOT contain deprecated 'ref' field, got:\n%s", contentStr)
	}
}

func TestBuildSourceFromParsedURL_UsesGitDirectly(t *testing.T) {
	parsed, err := phora.ParseURL("srnnkls/dotfiles")
	if err != nil {
		t.Fatalf("ParseURL failed: %v", err)
	}

	src := buildSourceFromParsedURL(parsed, "main", "")

	if src.Git != parsed.Git {
		t.Errorf("Source.Git = %q, want %q (from ParsedURL.Git)", src.Git, parsed.Git)
	}
}

func TestBuildSourceFromParsedURL_IncludesPathAndBranch(t *testing.T) {
	parsed, err := phora.ParseURL("https://github.com/company/project/tree/v2.0/configs")
	if err != nil {
		t.Fatalf("ParseURL failed: %v", err)
	}

	src := buildSourceFromParsedURL(parsed, "v2.0", "configs")

	if src.Git != "https://github.com/company/project.git" {
		t.Errorf("Source.Git = %q, want %q", src.Git, "https://github.com/company/project.git")
	}
	if src.Branch != "v2.0" {
		t.Errorf("Source.Branch = %q, want %q", src.Branch, "v2.0")
	}
	if src.Path != "configs" {
		t.Errorf("Source.Path = %q, want %q", src.Path, "configs")
	}
}

func TestBuildSourceFromParsedURL_GitLabURL(t *testing.T) {
	parsed, err := phora.ParseURL("gitlab.com/company/configs")
	if err != nil {
		t.Fatalf("ParseURL failed: %v", err)
	}

	src := buildSourceFromParsedURL(parsed, "main", "")

	if src.Git != "https://gitlab.com/company/configs.git" {
		t.Errorf("Source.Git = %q, want %q", src.Git, "https://gitlab.com/company/configs.git")
	}
}
