package phora

import (
	"testing"
)

func TestParseURL_Shorthand(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  *ParsedURL
	}{
		{
			name:  "owner/repo",
			input: "srnnkls/dotfiles",
			want: &ParsedURL{
				Git:  "https://github.com/srnnkls/dotfiles.git",
				Path: "",
			},
		},
		{
			name:  "different owner/repo",
			input: "company/shared",
			want: &ParsedURL{
				Git:  "https://github.com/company/shared.git",
				Path: "",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseURL(tt.input)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.input, err)
			}
			if got.Git != tt.want.Git {
				t.Errorf("ParseURL(%q).Git = %q, want %q", tt.input, got.Git, tt.want.Git)
			}
			if got.Path != tt.want.Path {
				t.Errorf("ParseURL(%q).Path = %q, want %q", tt.input, got.Path, tt.want.Path)
			}
		})
	}
}

func TestParseURL_ShorthandWithPath(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  *ParsedURL
	}{
		{
			name:  "owner/repo/single-path",
			input: "srnnkls/dotfiles/skills",
			want: &ParsedURL{
				Git:  "https://github.com/srnnkls/dotfiles.git",
				Path: "skills",
			},
		},
		{
			name:  "owner/repo/nested-path",
			input: "srnnkls/dotfiles/.claude/skills",
			want: &ParsedURL{
				Git:  "https://github.com/srnnkls/dotfiles.git",
				Path: ".claude/skills",
			},
		},
		{
			name:  "owner/repo/deep-nested-path",
			input: "company/monorepo/packages/shared/src",
			want: &ParsedURL{
				Git:  "https://github.com/company/monorepo.git",
				Path: "packages/shared/src",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseURL(tt.input)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.input, err)
			}
			if got.Git != tt.want.Git {
				t.Errorf("ParseURL(%q).Git = %q, want %q", tt.input, got.Git, tt.want.Git)
			}
			if got.Path != tt.want.Path {
				t.Errorf("ParseURL(%q).Path = %q, want %q", tt.input, got.Path, tt.want.Path)
			}
		})
	}
}

func TestParseURL_GitHubTreeURL(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  *ParsedURL
	}{
		{
			name:  "tree URL with branch and path",
			input: "https://github.com/srnnkls/dotfiles/tree/main/.claude/skills",
			want: &ParsedURL{
				Git:    "https://github.com/srnnkls/dotfiles.git",
				Branch: "main",
				Path:   ".claude/skills",
			},
		},
		{
			name:  "tree URL with branch only",
			input: "https://github.com/owner/repo/tree/develop",
			want: &ParsedURL{
				Git:    "https://github.com/owner/repo.git",
				Branch: "develop",
				Path:   "",
			},
		},
		{
			name:  "tree URL with nested path",
			input: "https://github.com/company/monorepo/tree/main/packages/shared/src",
			want: &ParsedURL{
				Git:    "https://github.com/company/monorepo.git",
				Branch: "main",
				Path:   "packages/shared/src",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseURL(tt.input)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.input, err)
			}
			if got.Git != tt.want.Git {
				t.Errorf("ParseURL(%q).Git = %q, want %q", tt.input, got.Git, tt.want.Git)
			}
			if got.Branch != tt.want.Branch {
				t.Errorf("ParseURL(%q).Branch = %q, want %q", tt.input, got.Branch, tt.want.Branch)
			}
			if got.Path != tt.want.Path {
				t.Errorf("ParseURL(%q).Path = %q, want %q", tt.input, got.Path, tt.want.Path)
			}
		})
	}
}

func TestParseURL_GitHubBlobURL(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  *ParsedURL
	}{
		{
			name:  "blob URL with branch and file",
			input: "https://github.com/owner/repo/blob/main/README.md",
			want: &ParsedURL{
				Git:    "https://github.com/owner/repo.git",
				Branch: "main",
				Path:   "README.md",
			},
		},
		{
			name:  "blob URL with nested file path",
			input: "https://github.com/owner/repo/blob/develop/src/lib/utils.go",
			want: &ParsedURL{
				Git:    "https://github.com/owner/repo.git",
				Branch: "develop",
				Path:   "src/lib/utils.go",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseURL(tt.input)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.input, err)
			}
			if got.Git != tt.want.Git {
				t.Errorf("ParseURL(%q).Git = %q, want %q", tt.input, got.Git, tt.want.Git)
			}
			if got.Branch != tt.want.Branch {
				t.Errorf("ParseURL(%q).Branch = %q, want %q", tt.input, got.Branch, tt.want.Branch)
			}
			if got.Path != tt.want.Path {
				t.Errorf("ParseURL(%q).Path = %q, want %q", tt.input, got.Path, tt.want.Path)
			}
		})
	}
}

func TestParseURL_GitLabURL(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  *ParsedURL
	}{
		{
			name:  "GitLab shorthand with path",
			input: "gitlab.com/company/repo/artifacts",
			want: &ParsedURL{
				Git:  "https://gitlab.com/company/repo.git",
				Path: "artifacts",
			},
		},
		{
			name:  "GitLab shorthand without path",
			input: "gitlab.com/company/repo",
			want: &ParsedURL{
				Git:  "https://gitlab.com/company/repo.git",
				Path: "",
			},
		},
		{
			name:  "GitLab full URL with tree",
			input: "https://gitlab.com/company/repo/-/tree/main/src",
			want: &ParsedURL{
				Git:    "https://gitlab.com/company/repo.git",
				Branch: "main",
				Path:   "src",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseURL(tt.input)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.input, err)
			}
			if got.Git != tt.want.Git {
				t.Errorf("ParseURL(%q).Git = %q, want %q", tt.input, got.Git, tt.want.Git)
			}
			if got.Branch != tt.want.Branch {
				t.Errorf("ParseURL(%q).Branch = %q, want %q", tt.input, got.Branch, tt.want.Branch)
			}
			if got.Path != tt.want.Path {
				t.Errorf("ParseURL(%q).Path = %q, want %q", tt.input, got.Path, tt.want.Path)
			}
		})
	}
}

func TestParseURL_PlainGitURL(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  *ParsedURL
	}{
		{
			name:  "plain GitHub URL without .git",
			input: "https://github.com/owner/repo",
			want: &ParsedURL{
				Git:  "https://github.com/owner/repo.git",
				Path: "",
			},
		},
		{
			name:  "plain GitHub URL with .git suffix",
			input: "https://github.com/owner/repo.git",
			want: &ParsedURL{
				Git:  "https://github.com/owner/repo.git",
				Path: "",
			},
		},
		{
			name:  "plain GitLab URL",
			input: "https://gitlab.com/company/project",
			want: &ParsedURL{
				Git:  "https://gitlab.com/company/project.git",
				Path: "",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseURL(tt.input)
			if err != nil {
				t.Fatalf("ParseURL(%q) error = %v", tt.input, err)
			}
			if got.Git != tt.want.Git {
				t.Errorf("ParseURL(%q).Git = %q, want %q", tt.input, got.Git, tt.want.Git)
			}
			if got.Path != tt.want.Path {
				t.Errorf("ParseURL(%q).Path = %q, want %q", tt.input, got.Path, tt.want.Path)
			}
		})
	}
}

func TestParseURL_RefFieldsAreNotSet(t *testing.T) {
	input := "owner/repo"
	got, err := ParseURL(input)
	if err != nil {
		t.Fatalf("ParseURL(%q) error = %v", input, err)
	}

	if got.Branch != "" {
		t.Errorf("ParseURL(%q).Branch = %q, want empty (refs with slashes require --ref flag)", input, got.Branch)
	}
	if got.Tag != "" {
		t.Errorf("ParseURL(%q).Tag = %q, want empty", input, got.Tag)
	}
	if got.Rev != "" {
		t.Errorf("ParseURL(%q).Rev = %q, want empty", input, got.Rev)
	}
}

func TestParseURL_ErrorCases(t *testing.T) {
	tests := []struct {
		name  string
		input string
	}{
		{
			name:  "empty string",
			input: "",
		},
		{
			name:  "single word",
			input: "repo",
		},
		{
			name:  "invalid URL scheme",
			input: "ftp://github.com/owner/repo",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := ParseURL(tt.input)
			if err == nil {
				t.Errorf("ParseURL(%q) expected error, got nil", tt.input)
			}
		})
	}
}

func TestParsedURL_Fields(t *testing.T) {
	p := ParsedURL{
		Git:    "https://github.com/owner/repo.git",
		Branch: "main",
		Tag:    "v1.0",
		Rev:    "abc123",
		Path:   "src/lib",
	}

	if p.Git != "https://github.com/owner/repo.git" {
		t.Errorf("Git = %q, want %q", p.Git, "https://github.com/owner/repo.git")
	}
	if p.Branch != "main" {
		t.Errorf("Branch = %q, want %q", p.Branch, "main")
	}
	if p.Tag != "v1.0" {
		t.Errorf("Tag = %q, want %q", p.Tag, "v1.0")
	}
	if p.Rev != "abc123" {
		t.Errorf("Rev = %q, want %q", p.Rev, "abc123")
	}
	if p.Path != "src/lib" {
		t.Errorf("Path = %q, want %q", p.Path, "src/lib")
	}
}
