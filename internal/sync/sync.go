package sync

import (
	"fmt"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
	"github.com/srnnkls/tropos/internal/source"
	"github.com/srnnkls/tropos/internal/target"
	"github.com/srnnkls/tropos/internal/transform"
)

type Options struct {
	SourcePaths []string
	Targets     []string
	DryRun      bool
	Force       bool
}

type Result struct {
	Synced    int
	Skipped   int
	Generated int
	Conflicts []Conflict
	Errors    []error
}

type Conflict struct {
	Artifact *artifact.Artifact
	Target   string
	Path     string
	Type     ConflictType
}

type ConflictType int

const (
	ConflictFileExists ConflictType = iota
	ConflictDuplicateSource
)

func Sync(cfg *config.Config, opts Options) (*Result, error) {
	result := &Result{}

	// Determine targets
	targetNames := opts.Targets
	if len(targetNames) == 0 {
		targetNames = cfg.DefaultHarnesses
	}

	// Collect all artifacts from sources
	var allArtifacts []*artifact.Artifact
	for _, srcPath := range opts.SourcePaths {
		src := source.NewLocal(srcPath, cfg.DefaultArtifacts)
		arts, err := src.Discover()
		if err != nil {
			result.Errors = append(result.Errors, err)
			continue
		}
		allArtifacts = append(allArtifacts, arts...)
	}

	// Process each target
	for _, targetName := range targetNames {
		harness, ok := cfg.Harness[targetName]
		if !ok {
			result.Errors = append(result.Errors, fmt.Errorf("unknown harness: %s", targetName))
			continue
		}

		// Generate commands from user-invocable skills if configured
		var generated []*artifact.Artifact
		if harness.GenerateCommandsFromSkills {
			generated = generateCommands(allArtifacts)
			result.Generated += len(generated)
		}

		// Combine artifacts
		artifacts := append(allArtifacts, generated...)

		// Detect conflicts
		if !opts.Force {
			conflicts := DetectFileConflicts(artifacts, targetName, harness)
			if len(conflicts) > 0 && cfg.Conflict.FileExists == "error" {
				result.Conflicts = append(result.Conflicts, conflicts...)
				continue
			}
		}

		// Create target writer
		tgt := target.NewFromConfig(targetName, harness)

		// Create transformer
		tr := &transform.Transformer{
			Variables: harness.Variables,
			Mappings:  harness.Mappings,
		}

		// Sync each artifact
		for _, art := range artifacts {
			// Check if exists and not forcing
			if !opts.Force {
				if exists, _ := tgt.Exists(art); exists {
					result.Skipped++
					continue
				}
			}

			// Transform
			transformed, err := tr.Transform(art)
			if err != nil {
				result.Errors = append(result.Errors, fmt.Errorf("transform %s: %w", art.Name, err))
				continue
			}

			// Write (unless dry-run)
			if !opts.DryRun {
				if err := tgt.Write(transformed); err != nil {
					result.Errors = append(result.Errors, fmt.Errorf("write %s: %w", art.Name, err))
					continue
				}
			}

			result.Synced++
		}
	}

	if len(result.Conflicts) > 0 {
		return result, fmt.Errorf("conflicts detected: %d", len(result.Conflicts))
	}

	return result, nil
}

func DetectFileConflicts(arts []*artifact.Artifact, targetName string, harness config.Harness) []Conflict {
	var conflicts []Conflict

	tgt := target.NewFromConfig(targetName, harness)

	for _, art := range arts {
		if exists, path := tgt.Exists(art); exists {
			conflicts = append(conflicts, Conflict{
				Artifact: art,
				Target:   targetName,
				Path:     path,
				Type:     ConflictFileExists,
			})
		}
	}

	return conflicts
}

func generateCommands(artifacts []*artifact.Artifact) []*artifact.Artifact {
	var commands []*artifact.Artifact

	for _, art := range artifacts {
		if art.Type != artifact.TypeSkill {
			continue
		}

		userInvocable, ok := art.Frontmatter["user-invocable"].(bool)
		if !ok || !userInvocable {
			continue
		}

		desc, _ := art.Frontmatter["description"].(string)

		cmd := &artifact.Artifact{
			Name: art.Name,
			Type: artifact.TypeCommand,
			Frontmatter: map[string]any{
				"name":        art.Name,
				"description": desc,
			},
			Body: fmt.Sprintf("Invoke skill: %s\n", art.Name),
		}

		commands = append(commands, cmd)
	}

	return commands
}
