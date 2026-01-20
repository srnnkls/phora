package cli

import (
	"path/filepath"
	"testing"

	"github.com/srnnkls/phora"
	"github.com/srnnkls/phora/internal/config"
)

func TestAddCommand_SourceNameCollision(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	existingConfig := &config.Config{
		Sources: map[string]config.Source{
			"dotfiles": {
				Host:  "github.com",
				Owner: "srnnkls",
				Repo:  "dotfiles",
				Ref:   "main",
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
		Sources: map[string]config.Source{
			"existing-source": {
				Host:  "github.com",
				Owner: "owner",
				Repo:  "existing-source",
				Ref:   "main",
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

func TestExtractHostOwnerRepoFromGitURL(t *testing.T) {
	tests := []struct {
		gitURL    string
		wantHost  string
		wantOwner string
		wantRepo  string
	}{
		{
			gitURL:    "https://github.com/srnnkls/dotfiles.git",
			wantHost:  "github.com",
			wantOwner: "srnnkls",
			wantRepo:  "dotfiles",
		},
		{
			gitURL:    "https://gitlab.com/company/project.git",
			wantHost:  "gitlab.com",
			wantOwner: "company",
			wantRepo:  "project",
		},
	}

	for _, tt := range tests {
		t.Run(tt.gitURL, func(t *testing.T) {
			host, owner, repo := extractHostOwnerRepo(tt.gitURL)
			if host != tt.wantHost {
				t.Errorf("host = %q, want %q", host, tt.wantHost)
			}
			if owner != tt.wantOwner {
				t.Errorf("owner = %q, want %q", owner, tt.wantOwner)
			}
			if repo != tt.wantRepo {
				t.Errorf("repo = %q, want %q", repo, tt.wantRepo)
			}
		})
	}
}

func TestParseURL_Integration(t *testing.T) {
	tests := []struct {
		name     string
		url      string
		wantRepo string
		wantPath string
		wantRef  string
	}{
		{
			name:     "owner/repo shorthand",
			url:      "srnnkls/dotfiles",
			wantRepo: "dotfiles",
			wantPath: "",
			wantRef:  "",
		},
		{
			name:     "GitHub URL with tree",
			url:      "https://github.com/owner/project/tree/main/src",
			wantRepo: "project",
			wantPath: "src",
			wantRef:  "main",
		},
		{
			name:     "GitLab shorthand",
			url:      "gitlab.com/company/configs",
			wantRepo: "configs",
			wantPath: "",
			wantRef:  "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			parsed, err := phora.ParseURL(tt.url)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.url, err)
			}

			_, _, repo := extractHostOwnerRepo(parsed.Git)
			if repo != tt.wantRepo {
				t.Errorf("repo = %q, want %q", repo, tt.wantRepo)
			}
			if parsed.Path != tt.wantPath {
				t.Errorf("path = %q, want %q", parsed.Path, tt.wantPath)
			}
			if parsed.Branch != tt.wantRef {
				t.Errorf("ref = %q, want %q", parsed.Branch, tt.wantRef)
			}
		})
	}
}
