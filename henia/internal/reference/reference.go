package reference

import (
	"regexp"
)

type Type string

const (
	TypeSkill   Type = "skill"
	TypeCommand Type = "command"
	TypeAgent   Type = "agent"
	TypeFile    Type = "file"
	TypeTool    Type = "tool"
)

func (t Type) String() string {
	return string(t)
}

type Reference struct {
	Type Type
	Name string
	Raw  string
}

var backtickPattern = regexp.MustCompile("`([^`]+)`")

var referencePattern = regexp.MustCompile(`^([$/@#!])([a-zA-Z0-9._/-]+)$`)

func Parse(text string) []Reference {
	var refs []Reference

	matches := backtickPattern.FindAllStringSubmatch(text, -1)
	for _, match := range matches {
		if len(match) < 2 {
			continue
		}
		content := match[1]

		refMatch := referencePattern.FindStringSubmatch(content)
		if refMatch == nil {
			continue
		}

		sigil := refMatch[1]
		name := refMatch[2]

		var refType Type
		switch sigil {
		case "$":
			refType = TypeSkill
		case "/":
			refType = TypeCommand
		case "@":
			refType = TypeAgent
		case "#":
			refType = TypeFile
		case "!":
			refType = TypeTool
		default:
			continue
		}

		refs = append(refs, Reference{
			Type: refType,
			Name: name,
			Raw:  content,
		})
	}

	return refs
}
