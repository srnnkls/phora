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

// Resolution represents user's choice for a conflict
type Resolution int

const (
	ResolutionSkip Resolution = iota
	ResolutionOverwrite
)

// ResolutionMap maps conflict keys to resolution choices
type ResolutionMap map[string]Resolution

// DetectionResult contains pre-sync analysis for a target
type DetectionResult struct {
	Target    string
	Harness   config.Harness
	Artifacts []*artifact.Artifact
	Conflicts []Conflict
	Generated int
}

// ApplyOptions extends Options with resolution data
type ApplyOptions struct {
	Options
	Resolutions ResolutionMap
}

// ConflictKey generates a unique key for a conflict
func ConflictKey(target, artifactName string) string {
	return target + ":" + artifactName
}

// Detect analyzes sources and identifies conflicts without writing
func Detect(cfg *config.Config, opts Options) ([]DetectionResult, error) {
	var results []DetectionResult
	var errors []error

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
			errors = append(errors, err)
			continue
		}
		allArtifacts = append(allArtifacts, arts...)
	}

	if len(errors) > 0 {
		return nil, fmt.Errorf("discovery errors: %v", errors)
	}

	for _, targetName := range targetNames {
		harness, ok := cfg.Harness[targetName]
		if !ok {
			continue
		}

		// Filter artifacts based on include/exclude
		filtered := filterArtifacts(allArtifacts, harness)

		// Generate commands from user-invocable skills if configured
		var generated int
		if harness.GenerateCommandsFromSkills {
			genArts := generateCommands(filtered)
			filtered = append(filtered, genArts...)
			generated = len(genArts)
		}

		// Detect conflicts
		conflicts := DetectFileConflicts(filtered, targetName, harness)

		results = append(results, DetectionResult{
			Target:    targetName,
			Harness:   harness,
			Artifacts: filtered,
			Conflicts: conflicts,
			Generated: generated,
		})
	}

	return results, nil
}

// Apply writes artifacts based on detection results and resolutions
func Apply(cfg *config.Config, detection []DetectionResult, opts ApplyOptions) (*Result, error) {
	result := &Result{}

	for _, det := range detection {
		tgt := target.NewFromConfig(det.Target, det.Harness)
		tr := &transform.Transformer{
			Variables: det.Harness.Variables,
			Mappings:  det.Harness.Mappings,
		}

		result.Generated += det.Generated

		for _, art := range det.Artifacts {
			key := ConflictKey(det.Target, art.Name)

			// Check if this artifact has a conflict
			if hasConflict(det.Conflicts, art) {
				if opts.Force {
					// Force mode: always overwrite
				} else if resolution, hasRes := opts.Resolutions[key]; hasRes {
					if resolution == ResolutionSkip {
						result.Skipped++
						continue
					}
					// ResolutionOverwrite falls through to write
				} else {
					// No resolution provided for conflict, skip
					result.Skipped++
					continue
				}
			} else if !opts.Force {
				// No conflict, but check if file exists (for non-force mode)
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

	return result, nil
}

// Sync is the original API, uses Detect + Apply internally (backward compatible)
func Sync(cfg *config.Config, opts Options) (*Result, error) {
	detection, err := Detect(cfg, opts)
	if err != nil {
		return nil, err
	}

	// Original behavior: if any conflicts and file_exists="error", fail
	if !opts.Force && cfg.Conflict.FileExists == "error" {
		var allConflicts []Conflict
		for _, det := range detection {
			allConflicts = append(allConflicts, det.Conflicts...)
		}
		if len(allConflicts) > 0 {
			return &Result{Conflicts: allConflicts}, fmt.Errorf("conflicts detected: %d", len(allConflicts))
		}
	}

	// For backward compat: if force, overwrite all; otherwise skip conflicts
	resolutions := make(ResolutionMap)
	if opts.Force {
		for _, det := range detection {
			for _, c := range det.Conflicts {
				resolutions[ConflictKey(c.Target, c.Artifact.Name)] = ResolutionOverwrite
			}
		}
	}

	return Apply(cfg, detection, ApplyOptions{
		Options:     opts,
		Resolutions: resolutions,
	})
}

// filterArtifacts applies include/exclude rules from harness config
func filterArtifacts(arts []*artifact.Artifact, harness config.Harness) []*artifact.Artifact {
	var filtered []*artifact.Artifact

	for _, art := range arts {
		if !shouldSync(art.Name, harness) {
			continue
		}
		filtered = append(filtered, art)
	}

	return filtered
}

// shouldSync checks if an artifact should be synced based on include/exclude lists
func shouldSync(name string, harness config.Harness) bool {
	// If include list is set, artifact must be in it
	if len(harness.Include) > 0 {
		found := false
		for _, inc := range harness.Include {
			if inc == name {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}

	// Check exclude list
	for _, exc := range harness.Exclude {
		if exc == name {
			return false
		}
	}

	return true
}

func hasConflict(conflicts []Conflict, art *artifact.Artifact) bool {
	for _, c := range conflicts {
		if c.Artifact.Name == art.Name {
			return true
		}
	}
	return false
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
