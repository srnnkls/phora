package reference

import (
	"testing"
)

func TestParse_ReturnsEmptyForNoReferences(t *testing.T) {
	refs := Parse("plain text with no references")
	if len(refs) != 0 {
		t.Errorf("Parse() returned %d refs, want 0", len(refs))
	}
}

func TestParse_SkillReference(t *testing.T) {
	refs := Parse("Use `$my-skill` for this")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeSkill {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeSkill)
	}
	if refs[0].Name != "my-skill" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "my-skill")
	}
	if refs[0].Raw != "$my-skill" {
		t.Errorf("refs[0].Raw = %q, want %q", refs[0].Raw, "$my-skill")
	}
}

func TestParse_CommandReference(t *testing.T) {
	refs := Parse("Run `/deploy` now")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeCommand {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeCommand)
	}
	if refs[0].Name != "deploy" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "deploy")
	}
	if refs[0].Raw != "/deploy" {
		t.Errorf("refs[0].Raw = %q, want %q", refs[0].Raw, "/deploy")
	}
}

func TestParse_AgentReference(t *testing.T) {
	refs := Parse("Ask `@reviewer` for feedback")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeAgent {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeAgent)
	}
	if refs[0].Name != "reviewer" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "reviewer")
	}
	if refs[0].Raw != "@reviewer" {
		t.Errorf("refs[0].Raw = %q, want %q", refs[0].Raw, "@reviewer")
	}
}

func TestParse_FileReference(t *testing.T) {
	refs := Parse("See `#config.yaml` for details")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeFile {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeFile)
	}
	if refs[0].Name != "config.yaml" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "config.yaml")
	}
	if refs[0].Raw != "#config.yaml" {
		t.Errorf("refs[0].Raw = %q, want %q", refs[0].Raw, "#config.yaml")
	}
}

func TestParse_ToolReference(t *testing.T) {
	refs := Parse("Use `!grep` to search")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeTool {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeTool)
	}
	if refs[0].Name != "grep" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "grep")
	}
	if refs[0].Raw != "!grep" {
		t.Errorf("refs[0].Raw = %q, want %q", refs[0].Raw, "!grep")
	}
}

func TestParse_IgnoresReferencesOutsideBackticks(t *testing.T) {
	tests := []struct {
		name  string
		input string
	}{
		{"skill outside", "Use $my-skill directly"},
		{"command outside", "Run /deploy now"},
		{"agent outside", "Ask @reviewer for help"},
		{"file outside", "See #config.yaml"},
		{"tool outside", "Use !grep to search"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			refs := Parse(tt.input)
			if len(refs) != 0 {
				t.Errorf("Parse(%q) returned %d refs, want 0 (refs outside backticks should be ignored)", tt.input, len(refs))
			}
		})
	}
}

func TestParse_MultipleReferences(t *testing.T) {
	refs := Parse("Use `$skill-a` and `$skill-b` together with `/command`")

	if len(refs) != 3 {
		t.Fatalf("Parse() returned %d refs, want 3", len(refs))
	}

	if refs[0].Name != "skill-a" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "skill-a")
	}
	if refs[1].Name != "skill-b" {
		t.Errorf("refs[1].Name = %q, want %q", refs[1].Name, "skill-b")
	}
	if refs[2].Name != "command" {
		t.Errorf("refs[2].Name = %q, want %q", refs[2].Name, "command")
	}
}

func TestParse_MixedContent(t *testing.T) {
	input := "Start with `$setup`, outside $ignore this, then `/run` and @skip-this too"
	refs := Parse(input)

	if len(refs) != 2 {
		t.Fatalf("Parse() returned %d refs, want 2", len(refs))
	}
	if refs[0].Name != "setup" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "setup")
	}
	if refs[1].Name != "run" {
		t.Errorf("refs[1].Name = %q, want %q", refs[1].Name, "run")
	}
}

func TestParse_ReferenceWithDashes(t *testing.T) {
	refs := Parse("`$my-complex-skill-name`")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Name != "my-complex-skill-name" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "my-complex-skill-name")
	}
}

func TestParse_ReferenceWithUnderscores(t *testing.T) {
	refs := Parse("`$my_skill_name`")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Name != "my_skill_name" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "my_skill_name")
	}
}

func TestParse_ReferenceWithNumbers(t *testing.T) {
	refs := Parse("`$skill123`")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Name != "skill123" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "skill123")
	}
}

func TestParse_MalformedReference_EmptySigil(t *testing.T) {
	refs := Parse("Use `$` here")

	if len(refs) != 0 {
		t.Errorf("Parse() returned %d refs for malformed '$', want 0", len(refs))
	}
}

func TestParse_MalformedReference_SigilWithSpace(t *testing.T) {
	refs := Parse("Use `$ skill` here")

	if len(refs) != 0 {
		t.Errorf("Parse() returned %d refs for '$ skill', want 0", len(refs))
	}
}

func TestParse_BacktickWithNonReference(t *testing.T) {
	refs := Parse("Regular `code block` here")

	if len(refs) != 0 {
		t.Errorf("Parse() returned %d refs for non-reference backtick content, want 0", len(refs))
	}
}

func TestParse_AllSigilTypes(t *testing.T) {
	input := "`$skill` `/command` `@agent` `#file` `!tool`"
	refs := Parse(input)

	if len(refs) != 5 {
		t.Fatalf("Parse() returned %d refs, want 5", len(refs))
	}

	types := map[Type]bool{
		TypeSkill:   false,
		TypeCommand: false,
		TypeAgent:   false,
		TypeFile:    false,
		TypeTool:    false,
	}

	for _, ref := range refs {
		types[ref.Type] = true
	}

	for typ, found := range types {
		if !found {
			t.Errorf("Type %v not found in parsed references", typ)
		}
	}
}

func TestParse_NestedBackticks(t *testing.T) {
	refs := Parse("Use `$skill` in ``code`` blocks")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Name != "skill" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "skill")
	}
}

func TestParse_FileReferenceWithPath(t *testing.T) {
	refs := Parse("See `#internal/config/config.go` for details")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeFile {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeFile)
	}
	if refs[0].Name != "internal/config/config.go" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "internal/config/config.go")
	}
}

func TestParse_CommandWithSubcommand(t *testing.T) {
	refs := Parse("Run `/config.edit` to modify")

	if len(refs) != 1 {
		t.Fatalf("Parse() returned %d refs, want 1", len(refs))
	}
	if refs[0].Type != TypeCommand {
		t.Errorf("refs[0].Type = %v, want %v", refs[0].Type, TypeCommand)
	}
	if refs[0].Name != "config.edit" {
		t.Errorf("refs[0].Name = %q, want %q", refs[0].Name, "config.edit")
	}
}

func TestReferenceType_String(t *testing.T) {
	tests := []struct {
		typ  Type
		want string
	}{
		{TypeSkill, "skill"},
		{TypeCommand, "command"},
		{TypeAgent, "agent"},
		{TypeFile, "file"},
		{TypeTool, "tool"},
	}

	for _, tt := range tests {
		t.Run(tt.want, func(t *testing.T) {
			if got := tt.typ.String(); got != tt.want {
				t.Errorf("Type.String() = %q, want %q", got, tt.want)
			}
		})
	}
}
