package cli

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/srnnkls/tropos/internal/sync"
)

// Prompter handles interactive conflict resolution
type Prompter struct {
	in  io.Reader
	out io.Writer
}

// NewPrompter creates a prompter using stdin/stdout
func NewPrompter() *Prompter {
	return &Prompter{in: os.Stdin, out: os.Stdout}
}

// PromptResult contains user's choices
type PromptResult struct {
	Resolutions sync.ResolutionMap
	Aborted     bool
}

// ResolveConflicts prompts user for each conflict
func (p *Prompter) ResolveConflicts(conflicts []sync.Conflict) (*PromptResult, error) {
	result := &PromptResult{
		Resolutions: make(sync.ResolutionMap),
	}

	if len(conflicts) == 0 {
		return result, nil
	}

	reader := bufio.NewReader(p.in)

	fmt.Fprintf(p.out, "\n%d conflict(s) found:\n\n", len(conflicts))

	for i := 0; i < len(conflicts); i++ {
		c := conflicts[i]
		fmt.Fprintf(p.out, "[%d/%d] %s (%s)\n", i+1, len(conflicts), c.Artifact.Name, c.Target)
		fmt.Fprintf(p.out, "  Existing: %s\n", c.Path)
		fmt.Fprintf(p.out, "  [s]kip, [o]verwrite, [d]iff, skip [a]ll, overwrite a[l]l, [q]uit: ")

		input, err := reader.ReadString('\n')
		if err != nil {
			if err == io.EOF {
				result.Aborted = true
				return result, nil
			}
			return nil, err
		}

		input = strings.TrimSpace(strings.ToLower(input))
		key := sync.ConflictKey(c.Target, c.Artifact.Name)

		switch input {
		case "s", "":
			result.Resolutions[key] = sync.ResolutionSkip
			fmt.Fprintf(p.out, "  → Skipping (will add to exclude list)\n\n")

		case "o":
			result.Resolutions[key] = sync.ResolutionOverwrite
			fmt.Fprintf(p.out, "  → Overwriting\n\n")

		case "d":
			if err := p.showDiff(c); err != nil {
				fmt.Fprintf(p.out, "  (diff failed: %v)\n", err)
			}
			i-- // Retry this conflict
			continue

		case "a":
			// Skip all remaining
			for j := i; j < len(conflicts); j++ {
				k := sync.ConflictKey(conflicts[j].Target, conflicts[j].Artifact.Name)
				result.Resolutions[k] = sync.ResolutionSkip
			}
			fmt.Fprintf(p.out, "  → Skipping all remaining\n\n")
			return result, nil

		case "l":
			// Overwrite all remaining
			for j := i; j < len(conflicts); j++ {
				k := sync.ConflictKey(conflicts[j].Target, conflicts[j].Artifact.Name)
				result.Resolutions[k] = sync.ResolutionOverwrite
			}
			fmt.Fprintf(p.out, "  → Overwriting all remaining\n\n")
			return result, nil

		case "q":
			result.Aborted = true
			return result, nil

		default:
			fmt.Fprintf(p.out, "  Invalid choice, try again.\n")
			i-- // Retry
			continue
		}
	}

	return result, nil
}

func (p *Prompter) showDiff(c sync.Conflict) error {
	existing, err := os.ReadFile(c.Path)
	if err != nil {
		return err
	}

	newContent := c.Artifact.Render()

	fmt.Fprintf(p.out, "\n--- existing: %s\n", c.Path)
	fmt.Fprintf(p.out, "+++ new: %s\n\n", c.Artifact.Name)

	existingLines := strings.Split(string(existing), "\n")
	newLines := strings.Split(newContent, "\n")

	p.printSimpleDiff(existingLines, newLines)

	fmt.Fprintln(p.out)
	return nil
}

func (p *Prompter) printSimpleDiff(old, new []string) {
	maxLines := 30

	fmt.Fprintf(p.out, "EXISTING (%d lines):\n", len(old))
	for i, line := range old {
		if i >= maxLines {
			fmt.Fprintf(p.out, "  ... (%d more lines)\n", len(old)-maxLines)
			break
		}
		fmt.Fprintf(p.out, "  %s\n", line)
	}

	fmt.Fprintf(p.out, "\nNEW (%d lines):\n", len(new))
	for i, line := range new {
		if i >= maxLines {
			fmt.Fprintf(p.out, "  ... (%d more lines)\n", len(new)-maxLines)
			break
		}
		fmt.Fprintf(p.out, "  %s\n", line)
	}
}

// isTerminal checks if stdin is a terminal (for auto-detecting interactive mode)
func isTerminal() bool {
	stat, err := os.Stdin.Stat()
	if err != nil {
		return false
	}
	return (stat.Mode() & os.ModeCharDevice) != 0
}
