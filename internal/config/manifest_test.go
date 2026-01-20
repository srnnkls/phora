package config

import (
	"testing"
)

func TestManifest_ArtifactsListParsing(t *testing.T) {
	tests := []struct {
		name     string
		toml     string
		wantLen  int
		wantErr  bool
	}{
		{
			name: "skills and commands",
			toml: `
[manifest]
artifacts = ["skills", "commands"]
`,
			wantLen: 2,
		},
		{
			name: "all artifact types",
			toml: `
[manifest]
artifacts = ["skills", "commands", "agents"]
`,
			wantLen: 3,
		},
		{
			name: "skills only",
			toml: `
[manifest]
artifacts = ["skills"]
`,
			wantLen: 1,
		},
		{
			name: "empty artifacts",
			toml: `
[manifest]
artifacts = []
`,
			wantLen: 0,
		},
		{
			name: "no manifest section",
			toml: `
[harness.claude]
path = "~/.claude"
`,
			wantLen: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg, err := ParseTOML([]byte(tt.toml))
			if tt.wantErr {
				if err == nil {
					t.Error("expected error, got nil")
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseTOML() error: %v", err)
			}

			if cfg.Manifest == nil && tt.wantLen > 0 {
				t.Fatalf("Manifest is nil, expected %d artifacts", tt.wantLen)
			}

			if cfg.Manifest != nil {
				if len(cfg.Manifest.Artifacts) != tt.wantLen {
					t.Errorf("len(Artifacts) = %d, want %d", len(cfg.Manifest.Artifacts), tt.wantLen)
				}
			}
		})
	}
}

func TestManifest_ContainsArtifact(t *testing.T) {
	m := &Manifest{
		Artifacts: []string{"skills", "commands"},
	}

	tests := []struct {
		artifact string
		want     bool
	}{
		{"skills", true},
		{"commands", true},
		{"agents", false},
		{"", false},
	}

	for _, tt := range tests {
		t.Run(tt.artifact, func(t *testing.T) {
			got := m.ContainsArtifact(tt.artifact)
			if got != tt.want {
				t.Errorf("ContainsArtifact(%q) = %v, want %v", tt.artifact, got, tt.want)
			}
		})
	}
}

func TestManifest_ContainsArtifact_EmptyManifest(t *testing.T) {
	m := &Manifest{}

	if m.ContainsArtifact("skills") {
		t.Error("empty manifest should not contain any artifact")
	}
}

func TestManifest_ValidatePath(t *testing.T) {
	tests := []struct {
		name      string
		artifacts []string
		path      string
		wantErr   bool
		errMsg    string
	}{
		{
			name:      "path in artifacts",
			artifacts: []string{"skills", "commands"},
			path:      "skills",
			wantErr:   false,
		},
		{
			name:      "path in artifacts - commands",
			artifacts: []string{"skills", "commands"},
			path:      "commands",
			wantErr:   false,
		},
		{
			name:      "path not in artifacts",
			artifacts: []string{"skills"},
			path:      "commands",
			wantErr:   true,
			errMsg:    "path 'commands' not in source artifacts",
		},
		{
			name:      "path not in artifacts - agents",
			artifacts: []string{"skills", "commands"},
			path:      "agents",
			wantErr:   true,
			errMsg:    "path 'agents' not in source artifacts",
		},
		{
			name:      "empty artifacts denies all",
			artifacts: []string{},
			path:      "anything",
			wantErr:   true,
		},
		{
			name:      "nil artifacts denies all",
			artifacts: nil,
			path:      "anything",
			wantErr:   true,
		},
		{
			name:      "path traversal rejected",
			artifacts: []string{"skills", "commands"},
			path:      "skills/../commands",
			wantErr:   true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			m := &Manifest{Artifacts: tt.artifacts}
			err := m.ValidatePath(tt.path)

			if tt.wantErr {
				if err == nil {
					t.Errorf("expected error for path %q with artifacts %v", tt.path, tt.artifacts)
					return
				}
				if tt.errMsg != "" && err.Error() != tt.errMsg {
					t.Errorf("error = %q, want %q", err.Error(), tt.errMsg)
				}
				return
			}

			if err != nil {
				t.Errorf("unexpected error: %v", err)
			}
		})
	}
}

func TestManifest_OldFieldsRemoved(t *testing.T) {
	toml := `
[manifest]
skills = ["skill1", "skill2"]
commands = ["cmd1"]
agents = ["agent1"]
`
	cfg, err := ParseTOML([]byte(toml))
	if err != nil {
		t.Skipf("TOML parsing accepts unknown fields, testing struct directly")
	}

	if cfg.Manifest != nil {
		if len(cfg.Manifest.Artifacts) > 0 {
			t.Error("old fields should not populate Artifacts")
		}
	}
}

func TestManifest_StructHasArtifactsField(t *testing.T) {
	m := Manifest{
		Artifacts: []string{"skills", "commands", "agents"},
	}

	if len(m.Artifacts) != 3 {
		t.Errorf("Artifacts length = %d, want 3", len(m.Artifacts))
	}
}

func TestManifest_ArtifactsZeroValue(t *testing.T) {
	var m Manifest

	if m.Artifacts != nil {
		t.Error("zero value Artifacts should be nil")
	}
}

func TestManifest_FilterDirectoriesByArtifacts(t *testing.T) {
	tests := []struct {
		name        string
		artifacts   []string
		dirs        []string
		wantExposed []string
	}{
		{
			name:        "filter skills and commands only",
			artifacts:   []string{"skills", "commands"},
			dirs:        []string{"skills", "commands", "agents", "other"},
			wantExposed: []string{"skills", "commands"},
		},
		{
			name:        "all artifacts exposed",
			artifacts:   []string{"skills", "commands", "agents"},
			dirs:        []string{"skills", "commands", "agents"},
			wantExposed: []string{"skills", "commands", "agents"},
		},
		{
			name:        "empty artifacts exposes nothing",
			artifacts:   []string{},
			dirs:        []string{"skills", "commands"},
			wantExposed: []string{},
		},
		{
			name:        "nil artifacts exposes nothing",
			artifacts:   nil,
			dirs:        []string{"skills", "commands"},
			wantExposed: []string{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			m := &Manifest{Artifacts: tt.artifacts}
			got := m.FilterDirectories(tt.dirs)

			if len(got) != len(tt.wantExposed) {
				t.Errorf("FilterDirectories() returned %d dirs, want %d", len(got), len(tt.wantExposed))
				return
			}

			for i, dir := range got {
				if dir != tt.wantExposed[i] {
					t.Errorf("FilterDirectories()[%d] = %q, want %q", i, dir, tt.wantExposed[i])
				}
			}
		})
	}
}

func TestManifest_FilterDirectories_DuplicateArtifacts(t *testing.T) {
	m := &Manifest{Artifacts: []string{"skills", "skills", "commands", "skills"}}
	dirs := []string{"skills", "commands", "agents"}

	got := m.FilterDirectories(dirs)

	want := []string{"skills", "commands"}
	if len(got) != len(want) {
		t.Errorf("FilterDirectories() with duplicates returned %d dirs, want %d", len(got), len(want))
		return
	}
	for i, dir := range got {
		if dir != want[i] {
			t.Errorf("FilterDirectories()[%d] = %q, want %q", i, dir, want[i])
		}
	}
}

func TestManifest_ValidatePath_NestedPath(t *testing.T) {
	m := &Manifest{Artifacts: []string{"skills", "commands"}}

	tests := []struct {
		name    string
		path    string
		wantErr bool
	}{
		{
			name:    "nested path under artifact should be valid",
			path:    "skills/subdir",
			wantErr: false,
		},
		{
			name:    "deeply nested path under artifact should be valid",
			path:    "skills/level1/level2/file.txt",
			wantErr: false,
		},
		{
			name:    "nested path under non-artifact should be invalid",
			path:    "agents/subdir",
			wantErr: true,
		},
		{
			name:    "path with artifact as suffix should be invalid",
			path:    "myskills",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := m.ValidatePath(tt.path)
			if tt.wantErr && err == nil {
				t.Errorf("ValidatePath(%q) = nil, want error", tt.path)
			}
			if !tt.wantErr && err != nil {
				t.Errorf("ValidatePath(%q) = %v, want nil", tt.path, err)
			}
		})
	}
}

func TestManifest_ValidatePath_ExactMatch(t *testing.T) {
	m := &Manifest{Artifacts: []string{"skills"}}

	err := m.ValidatePath("skills")
	if err != nil {
		t.Errorf("ValidatePath(\"skills\") = %v, want nil for exact match", err)
	}
}

func TestManifest_ParseFromConfig(t *testing.T) {
	toml := `
[hosts.github]
git_url = "https://github.com"
raw_url = "https://raw.githubusercontent.com"

[sources.dotfiles]
type = "github"
host = "github"
owner = "user"
repo = "dotfiles"

[manifest]
artifacts = ["skills", "commands", "agents"]

[harness.claude]
path = "~/.claude"
artifacts = ["skills", "commands"]
`
	cfg, err := ParseTOML([]byte(toml))
	if err != nil {
		t.Fatalf("ParseTOML() error: %v", err)
	}

	if cfg.Manifest == nil {
		t.Fatal("Manifest should not be nil")
	}

	wantArtifacts := []string{"skills", "commands", "agents"}
	if len(cfg.Manifest.Artifacts) != len(wantArtifacts) {
		t.Errorf("Manifest.Artifacts length = %d, want %d", len(cfg.Manifest.Artifacts), len(wantArtifacts))
	}

	for i, artifact := range cfg.Manifest.Artifacts {
		if artifact != wantArtifacts[i] {
			t.Errorf("Manifest.Artifacts[%d] = %q, want %q", i, artifact, wantArtifacts[i])
		}
	}

	if !cfg.Manifest.ContainsArtifact("skills") {
		t.Error("Manifest.ContainsArtifact(\"skills\") = false, want true")
	}
	if !cfg.Manifest.ContainsArtifact("commands") {
		t.Error("Manifest.ContainsArtifact(\"commands\") = false, want true")
	}
	if !cfg.Manifest.ContainsArtifact("agents") {
		t.Error("Manifest.ContainsArtifact(\"agents\") = false, want true")
	}
	if cfg.Manifest.ContainsArtifact("other") {
		t.Error("Manifest.ContainsArtifact(\"other\") = true, want false")
	}

	if err := cfg.Manifest.ValidatePath("skills"); err != nil {
		t.Errorf("ValidatePath(\"skills\") = %v, want nil", err)
	}
	if err := cfg.Manifest.ValidatePath("other"); err == nil {
		t.Error("ValidatePath(\"other\") = nil, want error")
	}

	dirs := []string{"skills", "commands", "agents", "other"}
	filtered := cfg.Manifest.FilterDirectories(dirs)
	wantFiltered := []string{"skills", "commands", "agents"}
	if len(filtered) != len(wantFiltered) {
		t.Errorf("FilterDirectories() returned %d dirs, want %d", len(filtered), len(wantFiltered))
	}
}
