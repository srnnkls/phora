package phora

import (
	"os"
	"path/filepath"
	"testing"
)

func TestConfig_FieldsExist(t *testing.T) {
	cfg := Config{
		Hosts: map[string]Host{
			"github": {GitURL: "https://github.com/{owner}/{repo}.git"},
		},
		Sources: map[string]Source{
			"company": {Repo: "company/shared-skills"},
		},
	}

	if cfg.Hosts == nil {
		t.Error("Hosts should not be nil")
	}
	if cfg.Sources == nil {
		t.Error("Sources should not be nil")
	}
}

func TestHost_GitURLField(t *testing.T) {
	host := Host{
		GitURL: "https://github.com/{owner}/{repo}.git",
	}

	if host.GitURL != "https://github.com/{owner}/{repo}.git" {
		t.Errorf("GitURL = %q, want %q", host.GitURL, "https://github.com/{owner}/{repo}.git")
	}
}

func TestSource_FieldsExist(t *testing.T) {
	source := Source{
		Repo:           "owner/repo-name",
		Ref:            "main",
		Path:           "subdirectory",
		IgnoreManifest: true,
		Paths: map[string]string{
			"my-skill": "custom/location/my-skill",
		},
	}

	if source.Repo != "owner/repo-name" {
		t.Errorf("Repo = %q, want %q", source.Repo, "owner/repo-name")
	}
	if source.Ref != "main" {
		t.Errorf("Ref = %q, want %q", source.Ref, "main")
	}
	if source.Path != "subdirectory" {
		t.Errorf("Path = %q, want %q", source.Path, "subdirectory")
	}
	if !source.IgnoreManifest {
		t.Error("IgnoreManifest should be true")
	}
	if source.Paths == nil {
		t.Error("Paths should not be nil")
	}
	if source.Paths["my-skill"] != "custom/location/my-skill" {
		t.Errorf("Paths[my-skill] = %q, want %q", source.Paths["my-skill"], "custom/location/my-skill")
	}
}

func TestSource_ParseRepo(t *testing.T) {
	tests := []struct {
		name      string
		repo      string
		wantOwner string
		wantRepo  string
		wantErr   bool
	}{
		{
			name:      "valid owner/repo",
			repo:      "owner/repo-name",
			wantOwner: "owner",
			wantRepo:  "repo-name",
			wantErr:   false,
		},
		{
			name:      "org with dashes",
			repo:      "my-company/shared-skills",
			wantOwner: "my-company",
			wantRepo:  "shared-skills",
			wantErr:   false,
		},
		{
			name:    "missing slash",
			repo:    "invalid-repo",
			wantErr: true,
		},
		{
			name:    "empty string",
			repo:    "",
			wantErr: true,
		},
		{
			name:    "too many slashes",
			repo:    "owner/repo/extra",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			source := Source{Repo: tt.repo}
			owner, repo, err := source.ParseRepo()

			if tt.wantErr {
				if err == nil {
					t.Errorf("ParseRepo() should return error for %q", tt.repo)
				}
				return
			}

			if err != nil {
				t.Errorf("ParseRepo() unexpected error: %v", err)
				return
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

func TestLoadConfig_FromTOMLFile(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"

[sources.company]
repo = "company/shared-skills"
ref = "main"
ignore_manifest = false

[sources.company.paths]
"my-skill" = "custom/location/my-skill"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	if cfg == nil {
		t.Fatal("LoadConfig() returned nil config")
	}

	host, ok := cfg.Hosts["github"]
	if !ok {
		t.Error("Hosts[github] not found")
	} else if host.GitURL != "https://github.com/{owner}/{repo}.git" {
		t.Errorf("Hosts[github].GitURL = %q, want %q", host.GitURL, "https://github.com/{owner}/{repo}.git")
	}

	source, ok := cfg.Sources["company"]
	if !ok {
		t.Error("Sources[company] not found")
	} else {
		if source.Repo != "company/shared-skills" {
			t.Errorf("Sources[company].Repo = %q, want %q", source.Repo, "company/shared-skills")
		}
		if source.Ref != "main" {
			t.Errorf("Sources[company].Ref = %q, want %q", source.Ref, "main")
		}
		if source.IgnoreManifest {
			t.Error("Sources[company].IgnoreManifest should be false")
		}
		if source.Paths["my-skill"] != "custom/location/my-skill" {
			t.Errorf("Sources[company].Paths[my-skill] = %q, want %q", source.Paths["my-skill"], "custom/location/my-skill")
		}
	}
}

func TestLoadConfig_FileNotFound(t *testing.T) {
	_, err := LoadConfig("/nonexistent/path/phora.toml")
	if err == nil {
		t.Error("LoadConfig() should return error for nonexistent file")
	}
}

func TestSource_DefaultRefIsMain(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `[sources.minimal]
repo = "owner/repo"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	source := cfg.Sources["minimal"]
	if source.Ref != "main" {
		t.Errorf("default Ref = %q, want %q", source.Ref, "main")
	}
}

func TestConfig_ValidateRepoFormat(t *testing.T) {
	tests := []struct {
		name    string
		repo    string
		wantErr bool
	}{
		{"valid", "owner/repo", false},
		{"valid with dashes", "my-org/my-repo", false},
		{"missing slash", "invalid", true},
		{"empty", "", true},
		{"extra slash", "owner/repo/extra", true},
		{"leading slash", "/owner/repo", true},
		{"trailing slash", "owner/repo/", true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := &Config{
				Sources: map[string]Source{
					"test": {Repo: tt.repo, Ref: "main"},
				},
			}
			err := cfg.Validate()
			if tt.wantErr && err == nil {
				t.Errorf("Validate() should return error for repo %q", tt.repo)
			}
			if !tt.wantErr && err != nil {
				t.Errorf("Validate() unexpected error for repo %q: %v", tt.repo, err)
			}
		})
	}
}

func TestConfig_ValidateHostExists(t *testing.T) {
	cfg := &Config{
		Hosts: map[string]Host{
			"github": {GitURL: "https://github.com/{owner}/{repo}.git"},
		},
		Sources: map[string]Source{
			"valid": {Repo: "owner/repo", Ref: "main", Host: "github"},
		},
	}

	if err := cfg.Validate(); err != nil {
		t.Errorf("Validate() should pass for valid host reference: %v", err)
	}

	cfg.Sources["invalid"] = Source{Repo: "owner/repo", Ref: "main", Host: "nonexistent"}
	if err := cfg.Validate(); err == nil {
		t.Error("Validate() should return error for nonexistent host reference")
	}
}

func TestConfig_ZeroValue(t *testing.T) {
	var cfg Config

	if cfg.Hosts != nil {
		t.Error("zero value Hosts should be nil")
	}
	if cfg.Sources != nil {
		t.Error("zero value Sources should be nil")
	}
}

func TestSource_ZeroValue(t *testing.T) {
	var source Source

	if source.Repo != "" {
		t.Error("zero value Repo should be empty")
	}
	if source.Ref != "" {
		t.Error("zero value Ref should be empty")
	}
	if source.Path != "" {
		t.Error("zero value Path should be empty")
	}
	if source.IgnoreManifest {
		t.Error("zero value IgnoreManifest should be false")
	}
	if source.Paths != nil {
		t.Error("zero value Paths should be nil")
	}
}

func TestHost_ZeroValue(t *testing.T) {
	var host Host

	if host.GitURL != "" {
		t.Error("zero value GitURL should be empty")
	}
}

func TestLoadConfig_MalformedTOML(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `[sources.broken
repo = "owner/repo"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	_, err := LoadConfig(configPath)
	if err == nil {
		t.Error("LoadConfig() should return error for malformed TOML")
	}
}

func TestSource_HostField(t *testing.T) {
	source := Source{
		Repo: "owner/repo",
		Host: "github",
	}

	if source.Host != "github" {
		t.Errorf("Host = %q, want %q", source.Host, "github")
	}
}
