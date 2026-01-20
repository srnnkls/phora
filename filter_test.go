package phora

import (
	"reflect"
	"testing"
)

func TestFilter_EmptyIncludeIncludesAll(t *testing.T) {
	f := Filter{
		Include: []string{},
		Exclude: []string{},
	}

	paths := []string{"README.md", "src/main.go", "docs/guide.md"}
	for _, p := range paths {
		if !f.Match(p) {
			t.Errorf("Match(%q) = false, want true (empty include should include all)", p)
		}
	}
}

func TestFilter_EmptyExcludeExcludesNothing(t *testing.T) {
	f := Filter{
		Include: []string{"*.md"},
		Exclude: []string{},
	}

	if !f.Match("README.md") {
		t.Error("Match(README.md) = false, want true")
	}
}

func TestFilter_IncludePattern_SingleGlob(t *testing.T) {
	f := Filter{
		Include: []string{"*.md"},
		Exclude: []string{},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"README.md", true},
		{"CHANGELOG.md", true},
		{"docs/guide.md", false},
		{"skills/README.md", false},
		{"main.go", false},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v (*.md matches root only)", tt.path, got, tt.want)
		}
	}
}

func TestFilter_IncludePattern_DoubleGlob(t *testing.T) {
	f := Filter{
		Include: []string{"**/*.md"},
		Exclude: []string{},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"README.md", true},
		{"docs/guide.md", true},
		{"skills/internal/README.md", true},
		{"deep/nested/file.md", true},
		{"main.go", false},
		{"src/main.go", false},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v (**/*.md matches any depth)", tt.path, got, tt.want)
		}
	}
}

func TestFilter_ExcludePattern(t *testing.T) {
	f := Filter{
		Include: []string{},
		Exclude: []string{"*.test.go"},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"main.go", true},
		{"filter.go", true},
		{"filter_test.go", true},
		{"main.test.go", false},
		{"pkg.test.go", false},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v", tt.path, got, tt.want)
		}
	}
}

func TestFilter_IncludeThenExclude_Order(t *testing.T) {
	f := Filter{
		Include: []string{"**/*.md"},
		Exclude: []string{"drafts/*"},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"README.md", true},
		{"docs/guide.md", true},
		{"drafts/wip.md", false},
		{"drafts/draft.md", false},
		{"main.go", false},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v (include then exclude)", tt.path, got, tt.want)
		}
	}
}

func TestFilter_NoNegationSupport(t *testing.T) {
	f := Filter{
		Include: []string{},
		Exclude: []string{"!important.md"},
	}

	if !f.Match("important.md") {
		t.Error("Match(important.md) = false, want true (negation patterns not supported, so !important.md is a literal pattern)")
	}
}

func TestFilter_Apply_MultipleFiles(t *testing.T) {
	f := Filter{
		Include: []string{"**/*.md"},
		Exclude: []string{"drafts/*"},
	}

	paths := []string{
		"README.md",
		"docs/guide.md",
		"drafts/wip.md",
		"main.go",
		"src/app.go",
	}

	want := []string{
		"README.md",
		"docs/guide.md",
	}

	got := f.Apply(paths)
	if !reflect.DeepEqual(got, want) {
		t.Errorf("Apply() = %v, want %v", got, want)
	}
}

func TestFilter_Apply_EmptyInput(t *testing.T) {
	f := Filter{
		Include: []string{"*.md"},
		Exclude: []string{},
	}

	got := f.Apply([]string{})
	if len(got) != 0 {
		t.Errorf("Apply([]) = %v, want []", got)
	}
}

func TestFilter_Apply_AllFiltered(t *testing.T) {
	f := Filter{
		Include: []string{"*.md"},
		Exclude: []string{},
	}

	paths := []string{"main.go", "app.go", "test.go"}
	got := f.Apply(paths)
	if len(got) != 0 {
		t.Errorf("Apply() = %v, want [] (no files match)", got)
	}
}

func TestFilter_NilIncludeIncludesAll(t *testing.T) {
	f := Filter{
		Include: nil,
		Exclude: nil,
	}

	paths := []string{"README.md", "main.go", "deep/nested/file.txt"}
	for _, p := range paths {
		if !f.Match(p) {
			t.Errorf("Match(%q) = false, want true (nil include should include all)", p)
		}
	}
}

func TestFilter_MultipleIncludePatterns(t *testing.T) {
	f := Filter{
		Include: []string{"*.md", "*.txt"},
		Exclude: []string{},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"README.md", true},
		{"notes.txt", true},
		{"main.go", false},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v", tt.path, got, tt.want)
		}
	}
}

func TestFilter_MultipleExcludePatterns(t *testing.T) {
	f := Filter{
		Include: []string{},
		Exclude: []string{"*.tmp", "*.bak"},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"README.md", true},
		{"file.tmp", false},
		{"backup.bak", false},
		{"main.go", true},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v", tt.path, got, tt.want)
		}
	}
}

func TestFilter_DoubleGlobMiddle(t *testing.T) {
	f := Filter{
		Include: []string{"docs/**/*.md"},
		Exclude: []string{},
	}

	tests := []struct {
		path string
		want bool
	}{
		{"docs/guide.md", true},
		{"docs/api/reference.md", true},
		{"docs/deep/nested/file.md", true},
		{"README.md", false},
		{"src/docs/file.md", false},
	}

	for _, tt := range tests {
		got := f.Match(tt.path)
		if got != tt.want {
			t.Errorf("Match(%q) = %v, want %v", tt.path, got, tt.want)
		}
	}
}
