package phora

import (
	"os"
	"path/filepath"
	"strings"
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

	content := `version = 1

[hosts.github]
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

	content := `version = 1

[sources.minimal]
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

func TestSource_NewCargoStyleFields(t *testing.T) {
	source := Source{
		Git:     "https://github.com/company/shared.git",
		Tag:     "v1.0",
		Path:    "skills",
		Target:  ".claude/skills",
		Include: []string{"*.md", "*.txt"},
		Exclude: []string{"test/*"},
	}

	if source.Git != "https://github.com/company/shared.git" {
		t.Errorf("Git = %q, want %q", source.Git, "https://github.com/company/shared.git")
	}
	if source.Tag != "v1.0" {
		t.Errorf("Tag = %q, want %q", source.Tag, "v1.0")
	}
	if source.Path != "skills" {
		t.Errorf("Path = %q, want %q", source.Path, "skills")
	}
	if source.Target != ".claude/skills" {
		t.Errorf("Target = %q, want %q", source.Target, ".claude/skills")
	}
	if len(source.Include) != 2 || source.Include[0] != "*.md" {
		t.Errorf("Include = %v, want [*.md, *.txt]", source.Include)
	}
	if len(source.Exclude) != 1 || source.Exclude[0] != "test/*" {
		t.Errorf("Exclude = %v, want [test/*]", source.Exclude)
	}
}

func TestSource_NewCargoStyleFields_Branch(t *testing.T) {
	source := Source{
		Git:    "https://github.com/company/prompts.git",
		Branch: "main",
	}

	if source.Git != "https://github.com/company/prompts.git" {
		t.Errorf("Git = %q, want %q", source.Git, "https://github.com/company/prompts.git")
	}
	if source.Branch != "main" {
		t.Errorf("Branch = %q, want %q", source.Branch, "main")
	}
}

func TestSource_NewCargoStyleFields_Rev(t *testing.T) {
	source := Source{
		Git: "https://github.com/company/tools.git",
		Rev: "a1b2c3d",
	}

	if source.Git != "https://github.com/company/tools.git" {
		t.Errorf("Git = %q, want %q", source.Git, "https://github.com/company/tools.git")
	}
	if source.Rev != "a1b2c3d" {
		t.Errorf("Rev = %q, want %q", source.Rev, "a1b2c3d")
	}
}

func TestLoadConfig_CargoStyleInlineTable(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[sources]
skills = { git = "https://github.com/company/shared.git", tag = "v1.0", path = "skills", target = ".claude/skills" }
prompts = { git = "https://github.com/company/prompts.git", branch = "main" }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	skills, ok := cfg.Sources["skills"]
	if !ok {
		t.Fatal("Sources[skills] not found")
	}
	if skills.Git != "https://github.com/company/shared.git" {
		t.Errorf("skills.Git = %q, want %q", skills.Git, "https://github.com/company/shared.git")
	}
	if skills.Tag != "v1.0" {
		t.Errorf("skills.Tag = %q, want %q", skills.Tag, "v1.0")
	}
	if skills.Path != "skills" {
		t.Errorf("skills.Path = %q, want %q", skills.Path, "skills")
	}
	if skills.Target != ".claude/skills" {
		t.Errorf("skills.Target = %q, want %q", skills.Target, ".claude/skills")
	}

	prompts, ok := cfg.Sources["prompts"]
	if !ok {
		t.Fatal("Sources[prompts] not found")
	}
	if prompts.Git != "https://github.com/company/prompts.git" {
		t.Errorf("prompts.Git = %q, want %q", prompts.Git, "https://github.com/company/prompts.git")
	}
	if prompts.Branch != "main" {
		t.Errorf("prompts.Branch = %q, want %q", prompts.Branch, "main")
	}
}

func TestLoadConfig_CargoStyleWithRev(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[sources]
pinned = { git = "https://github.com/company/tools.git", rev = "a1b2c3d" }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	pinned, ok := cfg.Sources["pinned"]
	if !ok {
		t.Fatal("Sources[pinned] not found")
	}
	if pinned.Git != "https://github.com/company/tools.git" {
		t.Errorf("pinned.Git = %q, want %q", pinned.Git, "https://github.com/company/tools.git")
	}
	if pinned.Rev != "a1b2c3d" {
		t.Errorf("pinned.Rev = %q, want %q", pinned.Rev, "a1b2c3d")
	}
}

func TestLoadConfig_CargoStyleWithIncludeExclude(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[sources]
filtered = { git = "https://github.com/company/docs.git", branch = "main", include = ["*.md", "*.txt"], exclude = ["drafts/*"] }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	filtered, ok := cfg.Sources["filtered"]
	if !ok {
		t.Fatal("Sources[filtered] not found")
	}
	if len(filtered.Include) != 2 {
		t.Errorf("filtered.Include length = %d, want 2", len(filtered.Include))
	}
	if len(filtered.Include) >= 1 && filtered.Include[0] != "*.md" {
		t.Errorf("filtered.Include[0] = %q, want %q", filtered.Include[0], "*.md")
	}
	if len(filtered.Include) >= 2 && filtered.Include[1] != "*.txt" {
		t.Errorf("filtered.Include[1] = %q, want %q", filtered.Include[1], "*.txt")
	}
	if len(filtered.Exclude) != 1 {
		t.Errorf("filtered.Exclude length = %d, want 1", len(filtered.Exclude))
	}
	if len(filtered.Exclude) >= 1 && filtered.Exclude[0] != "drafts/*" {
		t.Errorf("filtered.Exclude[0] = %q, want %q", filtered.Exclude[0], "drafts/*")
	}
}

func TestLoadConfig_CargoStyleTargetDefaultsToSourceKey(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[sources]
myskills = { git = "https://github.com/company/shared.git", branch = "main" }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	myskills, ok := cfg.Sources["myskills"]
	if !ok {
		t.Fatal("Sources[myskills] not found")
	}

	if myskills.Target != "myskills" {
		t.Errorf("myskills.Target = %q, want %q (should default to source key name)", myskills.Target, "myskills")
	}
}

func TestConfig_VersionValidation_MissingVersion(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `[sources]
skills = { git = "https://github.com/company/shared.git", branch = "main" }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	_, err := LoadConfig(configPath)
	if err == nil {
		t.Error("LoadConfig() should return error when version field is missing")
	}
}

func TestConfig_VersionValidation_UnsupportedVersion(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 2

[sources]
skills = { git = "https://github.com/company/shared.git", branch = "main" }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	_, err := LoadConfig(configPath)
	if err == nil {
		t.Error("LoadConfig() should return error for unsupported config version")
	}
	if err != nil && !strings.Contains(err.Error(), "unsupported config version") {
		t.Errorf("error message should contain 'unsupported config version', got: %v", err)
	}
}

func TestConfig_VersionValidation_ValidVersion(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[sources]
skills = { git = "https://github.com/company/shared.git", branch = "main" }
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() should not return error for valid version: %v", err)
	}
	if cfg.Version != 1 {
		t.Errorf("cfg.Version = %d, want 1", cfg.Version)
	}
}

func TestConfig_ValidateRefMutualExclusivity_BranchAndTag(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo:   "company/repo",
				Branch: "main",
				Tag:    "v1.0",
			},
		},
	}
	err := cfg.Validate()
	if err == nil {
		t.Error("Validate() should return error when both branch and tag are specified")
	}
}

func TestConfig_ValidateRefMutualExclusivity_BranchAndRev(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo:   "company/repo",
				Branch: "main",
				Rev:    "abc123",
			},
		},
	}
	err := cfg.Validate()
	if err == nil {
		t.Error("Validate() should return error when both branch and rev are specified")
	}
}

func TestConfig_ValidateRefMutualExclusivity_TagAndRev(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo: "company/repo",
				Tag:  "v1.0",
				Rev:  "abc123",
			},
		},
	}
	err := cfg.Validate()
	if err == nil {
		t.Error("Validate() should return error when both tag and rev are specified")
	}
}

func TestConfig_ValidateRefMutualExclusivity_AllThree(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo:   "company/repo",
				Branch: "main",
				Tag:    "v1.0",
				Rev:    "abc123",
			},
		},
	}
	err := cfg.Validate()
	if err == nil {
		t.Error("Validate() should return error when branch, tag, and rev are all specified")
	}
}

func TestConfig_ValidateRefMutualExclusivity_OnlyBranch(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo:   "company/repo",
				Branch: "main",
			},
		},
	}
	err := cfg.Validate()
	if err != nil {
		t.Errorf("Validate() should not return error when only branch is specified: %v", err)
	}
}

func TestConfig_ValidateRefMutualExclusivity_OnlyTag(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo: "company/repo",
				Tag:  "v1.0",
			},
		},
	}
	err := cfg.Validate()
	if err != nil {
		t.Errorf("Validate() should not return error when only tag is specified: %v", err)
	}
}

func TestConfig_ValidateRefMutualExclusivity_OnlyRev(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo: "company/repo",
				Rev:  "abc123",
			},
		},
	}
	err := cfg.Validate()
	if err != nil {
		t.Errorf("Validate() should not return error when only rev is specified: %v", err)
	}
}

func TestConfig_ValidateRefMutualExclusivity_None(t *testing.T) {
	cfg := &Config{
		Sources: map[string]Source{
			"test": {
				Repo: "company/repo",
			},
		},
	}
	err := cfg.Validate()
	if err != nil {
		t.Errorf("Validate() should not return error when no ref fields are specified: %v", err)
	}
}

func TestConfig_HasManifestField(t *testing.T) {
	cfg := Config{
		Manifest: &Manifest{
			Artifacts: []string{"skills", "prompts"},
		},
	}

	if cfg.Manifest == nil {
		t.Fatal("Config.Manifest should not be nil after assignment")
	}
	if len(cfg.Manifest.Artifacts) != 2 {
		t.Errorf("Manifest.Artifacts length = %d, want 2", len(cfg.Manifest.Artifacts))
	}
	if cfg.Manifest.Artifacts[0] != "skills" {
		t.Errorf("Manifest.Artifacts[0] = %q, want %q", cfg.Manifest.Artifacts[0], "skills")
	}
}

func TestConfig_HasHarnessField(t *testing.T) {
	cfg := Config{
		Harness: map[string]Harness{
			"claude": {
				Path:      ".claude",
				Structure: "nested",
				Artifacts: []string{"skills", "commands"},
			},
		},
	}

	if cfg.Harness == nil {
		t.Fatal("Config.Harness should not be nil after assignment")
	}
	harness, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("Config.Harness[claude] not found")
	}
	if harness.Path != ".claude" {
		t.Errorf("Harness.Path = %q, want %q", harness.Path, ".claude")
	}
	if harness.Structure != "nested" {
		t.Errorf("Harness.Structure = %q, want %q", harness.Structure, "nested")
	}
}

func TestConfig_HasArtifactsField(t *testing.T) {
	cfg := Config{
		Artifacts: []string{"skills", "prompts", "commands"},
	}

	if cfg.Artifacts == nil {
		t.Fatal("Config.Artifacts should not be nil after assignment")
	}
	if len(cfg.Artifacts) != 3 {
		t.Errorf("Config.Artifacts length = %d, want 3", len(cfg.Artifacts))
	}
}

func TestManifest_ContainsArtifact(t *testing.T) {
	m := Manifest{
		Artifacts: []string{"skills", "prompts"},
	}

	if !m.ContainsArtifact("skills") {
		t.Error("ContainsArtifact(skills) = false, want true")
	}
	if !m.ContainsArtifact("prompts") {
		t.Error("ContainsArtifact(prompts) = false, want true")
	}
	if m.ContainsArtifact("commands") {
		t.Error("ContainsArtifact(commands) = true, want false")
	}
}

func TestManifest_ContainsArtifact_EmptyManifest(t *testing.T) {
	m := Manifest{}

	if m.ContainsArtifact("skills") {
		t.Error("ContainsArtifact on empty manifest should return false")
	}
}

func TestManifest_ValidatePath(t *testing.T) {
	m := Manifest{
		Artifacts: []string{"skills", "prompts"},
	}

	if err := m.ValidatePath("skills"); err != nil {
		t.Errorf("ValidatePath(skills) error: %v", err)
	}
	if err := m.ValidatePath("skills/my-skill"); err != nil {
		t.Errorf("ValidatePath(skills/my-skill) error: %v", err)
	}
	if err := m.ValidatePath("commands"); err == nil {
		t.Error("ValidatePath(commands) should return error for path not in artifacts")
	}
}

func TestManifest_ValidatePath_EmptyManifest(t *testing.T) {
	m := Manifest{}

	if err := m.ValidatePath("anything"); err == nil {
		t.Error("ValidatePath on empty manifest should return error (deny-by-default per spec)")
	}
}

func TestManifest_ValidatePath_PathTraversal(t *testing.T) {
	m := Manifest{Artifacts: []string{"skills", "commands"}}

	if err := m.ValidatePath("skills/../commands"); err == nil {
		t.Error("ValidatePath should reject path traversal")
	}
	if err := m.ValidatePath("../outside"); err == nil {
		t.Error("ValidatePath should reject path starting with ..")
	}
}

func TestManifest_FilterDirectories(t *testing.T) {
	m := Manifest{
		Artifacts: []string{"skills", "prompts"},
	}

	dirs := []string{"skills", "prompts", "commands", "other"}
	filtered := m.FilterDirectories(dirs)

	if len(filtered) != 2 {
		t.Errorf("FilterDirectories length = %d, want 2", len(filtered))
	}

	found := make(map[string]bool)
	for _, d := range filtered {
		found[d] = true
	}
	if !found["skills"] {
		t.Error("FilterDirectories should include skills")
	}
	if !found["prompts"] {
		t.Error("FilterDirectories should include prompts")
	}
	if found["commands"] {
		t.Error("FilterDirectories should not include commands")
	}
}

func TestManifest_FilterDirectories_EmptyManifest(t *testing.T) {
	m := Manifest{}

	dirs := []string{"skills", "prompts"}
	filtered := m.FilterDirectories(dirs)

	if len(filtered) != 0 {
		t.Errorf("FilterDirectories on empty manifest should return empty slice, got %d items", len(filtered))
	}
}

func TestHarness_FieldsExist(t *testing.T) {
	h := Harness{
		Path:                       ".claude",
		Structure:                  "nested",
		GenerateCommandsFromSkills: true,
		Artifacts:                  []string{"skills"},
		Keys:                       map[string]string{"key1": "value1"},
		Values:                     map[string]map[string]string{"section": {"k": "v"}},
		Variables:                  map[string]string{"var1": "val1"},
		Tools:                      map[string]string{"tool1": "path1"},
		Include:                    []string{"*.md"},
		Exclude:                    []string{"test/*"},
	}

	if h.Path != ".claude" {
		t.Errorf("Path = %q, want %q", h.Path, ".claude")
	}
	if h.Structure != "nested" {
		t.Errorf("Structure = %q, want %q", h.Structure, "nested")
	}
	if !h.GenerateCommandsFromSkills {
		t.Error("GenerateCommandsFromSkills should be true")
	}
	if len(h.Artifacts) != 1 {
		t.Errorf("Artifacts length = %d, want 1", len(h.Artifacts))
	}
	if h.Keys["key1"] != "value1" {
		t.Errorf("Keys[key1] = %q, want %q", h.Keys["key1"], "value1")
	}
	if h.Values["section"]["k"] != "v" {
		t.Errorf("Values[section][k] = %q, want %q", h.Values["section"]["k"], "v")
	}
	if h.Variables["var1"] != "val1" {
		t.Errorf("Variables[var1] = %q, want %q", h.Variables["var1"], "val1")
	}
	if h.Tools["tool1"] != "path1" {
		t.Errorf("Tools[tool1] = %q, want %q", h.Tools["tool1"], "path1")
	}
	if len(h.Include) != 1 || h.Include[0] != "*.md" {
		t.Errorf("Include = %v, want [*.md]", h.Include)
	}
	if len(h.Exclude) != 1 || h.Exclude[0] != "test/*" {
		t.Errorf("Exclude = %v, want [test/*]", h.Exclude)
	}
}

func TestHarness_ArtifactMappingsField(t *testing.T) {
	h := Harness{
		ArtifactMappings: map[string]ArtifactMapping{
			"skills": {
				Keys:   map[string]string{"name": "skill_name"},
				Values: map[string]map[string]string{"types": {"command": "cmd"}},
			},
		},
	}

	if h.ArtifactMappings == nil {
		t.Fatal("ArtifactMappings should not be nil")
	}
	mapping, ok := h.ArtifactMappings["skills"]
	if !ok {
		t.Fatal("ArtifactMappings[skills] not found")
	}
	if mapping.Keys["name"] != "skill_name" {
		t.Errorf("mapping.Keys[name] = %q, want %q", mapping.Keys["name"], "skill_name")
	}
}

func TestHarness_ReferencesField(t *testing.T) {
	h := Harness{
		References: map[string]ReferenceConfig{
			"api": {Output: "docs/api.md"},
		},
	}

	if h.References == nil {
		t.Fatal("References should not be nil")
	}
	ref, ok := h.References["api"]
	if !ok {
		t.Fatal("References[api] not found")
	}
	if ref.Output != "docs/api.md" {
		t.Errorf("ref.Output = %q, want %q", ref.Output, "docs/api.md")
	}
}

func TestLoadConfig_WithManifestSection(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[manifest]
artifacts = ["skills", "prompts"]

[sources.company]
repo = "company/shared-skills"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	if cfg.Manifest == nil {
		t.Fatal("LoadConfig() should parse [manifest] section")
	}
	if len(cfg.Manifest.Artifacts) != 2 {
		t.Errorf("Manifest.Artifacts length = %d, want 2", len(cfg.Manifest.Artifacts))
	}
	if cfg.Manifest.Artifacts[0] != "skills" {
		t.Errorf("Manifest.Artifacts[0] = %q, want %q", cfg.Manifest.Artifacts[0], "skills")
	}
}

func TestLoadConfig_WithHarnessSection(t *testing.T) {
	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "phora.toml")

	content := `version = 1

[harness.claude]
path = ".claude"
structure = "nested"
artifacts = ["skills", "commands"]

[sources.company]
repo = "company/shared-skills"
`
	if err := os.WriteFile(configPath, []byte(content), 0644); err != nil {
		t.Fatalf("failed to write test config: %v", err)
	}

	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}

	if cfg.Harness == nil {
		t.Fatal("LoadConfig() should parse [harness] section")
	}
	harness, ok := cfg.Harness["claude"]
	if !ok {
		t.Fatal("Harness[claude] not found")
	}
	if harness.Path != ".claude" {
		t.Errorf("harness.Path = %q, want %q", harness.Path, ".claude")
	}
	if harness.Structure != "nested" {
		t.Errorf("harness.Structure = %q, want %q", harness.Structure, "nested")
	}
}

func TestMergeConfigs(t *testing.T) {
	global := &Config{
		Sources: map[string]Source{
			"global-src": {Repo: "company/global", Ref: "main"},
		},
		Harness: map[string]Harness{
			"claude": {Path: ".claude", Structure: "flat"},
		},
	}

	project := &Config{
		Sources: map[string]Source{
			"project-src": {Repo: "company/project", Ref: "main"},
		},
		Harness: map[string]Harness{
			"claude": {Structure: "nested"},
		},
	}

	merged := Merge(global, project)

	if merged == nil {
		t.Fatal("Merge() returned nil")
	}

	if _, ok := merged.Sources["global-src"]; !ok {
		t.Error("Merge should include global sources")
	}
	if _, ok := merged.Sources["project-src"]; !ok {
		t.Error("Merge should include project sources")
	}

	harness, ok := merged.Harness["claude"]
	if !ok {
		t.Fatal("Merged harness[claude] not found")
	}
	if harness.Structure != "nested" {
		t.Errorf("Project harness should override global: Structure = %q, want %q", harness.Structure, "nested")
	}
	if harness.Path != ".claude" {
		t.Errorf("Non-overridden fields should be preserved: Path = %q, want %q", harness.Path, ".claude")
	}
}

func TestLoad_GlobalAndProjectConfigs(t *testing.T) {
	globalDir := t.TempDir()
	projectDir := t.TempDir()

	globalConfigPath := filepath.Join(globalDir, "phora.toml")
	globalContent := `[sources.global]
repo = "company/global"
`
	if err := os.WriteFile(globalConfigPath, []byte(globalContent), 0644); err != nil {
		t.Fatalf("failed to write global config: %v", err)
	}

	projectConfigPath := filepath.Join(projectDir, "phora.toml")
	projectContent := `[sources.project]
repo = "company/project"
`
	if err := os.WriteFile(projectConfigPath, []byte(projectContent), 0644); err != nil {
		t.Fatalf("failed to write project config: %v", err)
	}

	cfg, err := Load(projectDir, globalConfigPath)
	if err != nil {
		t.Fatalf("Load() error: %v", err)
	}

	if cfg == nil {
		t.Fatal("Load() returned nil")
	}
	if _, ok := cfg.Sources["global"]; !ok {
		t.Error("Load should include global sources")
	}
	if _, ok := cfg.Sources["project"]; !ok {
		t.Error("Load should include project sources")
	}
}
