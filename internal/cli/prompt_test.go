package cli

import (
	"bytes"
	"strings"
	"testing"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/sync"
)

func TestPrompter_ResolveConflicts_Skip(t *testing.T) {
	input := strings.NewReader("s\n")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{
			Artifact: &artifact.Artifact{Name: "skill1"},
			Target:   "claude",
			Path:     "/path/to/skill1",
		},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	key := sync.ConflictKey("claude", "skill1")
	if result.Resolutions[key] != sync.ResolutionSkip {
		t.Errorf("Resolution = %v, want ResolutionSkip", result.Resolutions[key])
	}
}

func TestPrompter_ResolveConflicts_Overwrite(t *testing.T) {
	input := strings.NewReader("o\n")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{
			Artifact: &artifact.Artifact{Name: "skill1"},
			Target:   "claude",
			Path:     "/path/to/skill1",
		},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	key := sync.ConflictKey("claude", "skill1")
	if result.Resolutions[key] != sync.ResolutionOverwrite {
		t.Errorf("Resolution = %v, want ResolutionOverwrite", result.Resolutions[key])
	}
}

func TestPrompter_ResolveConflicts_SkipAll(t *testing.T) {
	input := strings.NewReader("a\n")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{Artifact: &artifact.Artifact{Name: "skill1"}, Target: "claude", Path: "/a"},
		{Artifact: &artifact.Artifact{Name: "skill2"}, Target: "claude", Path: "/b"},
		{Artifact: &artifact.Artifact{Name: "skill3"}, Target: "claude", Path: "/c"},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	for _, c := range conflicts {
		key := sync.ConflictKey(c.Target, c.Artifact.Name)
		if result.Resolutions[key] != sync.ResolutionSkip {
			t.Errorf("Resolution for %s = %v, want ResolutionSkip", c.Artifact.Name, result.Resolutions[key])
		}
	}
}

func TestPrompter_ResolveConflicts_OverwriteAll(t *testing.T) {
	input := strings.NewReader("l\n")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{Artifact: &artifact.Artifact{Name: "skill1"}, Target: "claude", Path: "/a"},
		{Artifact: &artifact.Artifact{Name: "skill2"}, Target: "claude", Path: "/b"},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	for _, c := range conflicts {
		key := sync.ConflictKey(c.Target, c.Artifact.Name)
		if result.Resolutions[key] != sync.ResolutionOverwrite {
			t.Errorf("Resolution for %s = %v, want ResolutionOverwrite", c.Artifact.Name, result.Resolutions[key])
		}
	}
}

func TestPrompter_ResolveConflicts_Quit(t *testing.T) {
	input := strings.NewReader("q\n")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{Artifact: &artifact.Artifact{Name: "skill1"}, Target: "claude", Path: "/a"},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	if !result.Aborted {
		t.Error("Expected Aborted = true")
	}
}

func TestPrompter_ResolveConflicts_Mixed(t *testing.T) {
	input := strings.NewReader("s\no\ns\n")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{Artifact: &artifact.Artifact{Name: "skill1"}, Target: "claude", Path: "/a"},
		{Artifact: &artifact.Artifact{Name: "skill2"}, Target: "claude", Path: "/b"},
		{Artifact: &artifact.Artifact{Name: "skill3"}, Target: "opencode", Path: "/c"},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	if result.Resolutions[sync.ConflictKey("claude", "skill1")] != sync.ResolutionSkip {
		t.Error("skill1 should be Skip")
	}
	if result.Resolutions[sync.ConflictKey("claude", "skill2")] != sync.ResolutionOverwrite {
		t.Error("skill2 should be Overwrite")
	}
	if result.Resolutions[sync.ConflictKey("opencode", "skill3")] != sync.ResolutionSkip {
		t.Error("skill3 should be Skip")
	}
}

func TestPrompter_ResolveConflicts_DefaultIsSkip(t *testing.T) {
	input := strings.NewReader("\n") // Empty input (just Enter)
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	conflicts := []sync.Conflict{
		{Artifact: &artifact.Artifact{Name: "skill1"}, Target: "claude", Path: "/a"},
	}

	result, err := p.ResolveConflicts(conflicts)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	key := sync.ConflictKey("claude", "skill1")
	if result.Resolutions[key] != sync.ResolutionSkip {
		t.Errorf("Default resolution = %v, want ResolutionSkip", result.Resolutions[key])
	}
}

func TestPrompter_ResolveConflicts_Empty(t *testing.T) {
	input := strings.NewReader("")
	output := &bytes.Buffer{}

	p := &Prompter{in: input, out: output}

	result, err := p.ResolveConflicts(nil)
	if err != nil {
		t.Fatalf("ResolveConflicts() error = %v", err)
	}

	if len(result.Resolutions) != 0 {
		t.Error("Expected empty resolutions for no conflicts")
	}
}
