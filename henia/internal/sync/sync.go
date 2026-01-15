package sync

import (
	"fmt"
	"strings"

	"github.com/srnnkls/henia"
	"github.com/srnnkls/henia/internal/artifact"
	"github.com/srnnkls/henia/internal/target"
	"github.com/srnnkls/henia/internal/transform"
	"github.com/srnnkls/phora"
)

type Fetcher interface {
	FetchAll() ([]phora.FetchResult, error)
}

type FetchedSource struct {
	Name      string
	LocalPath string
}

type Result struct {
	Synced  int
	Skipped int
	Errors  []error
}

type Syncer struct {
	Fetcher   Fetcher
	Harnesses map[string]henia.Harness
}

func NewSyncer(fetcher Fetcher, harnesses map[string]henia.Harness) *Syncer {
	return &Syncer{
		Fetcher:   fetcher,
		Harnesses: harnesses,
	}
}

func (s *Syncer) Sync() (*Result, error) {
	results, err := s.Fetcher.FetchAll()
	if err != nil {
		return nil, fmt.Errorf("fetch: %w", err)
	}

	sources := make([]FetchedSource, len(results))
	for i, r := range results {
		sources[i] = FetchedSource{
			Name:      r.Name,
			LocalPath: r.LocalPath,
		}
	}

	return s.Deploy(sources)
}

func (s *Syncer) Deploy(sources []FetchedSource) (*Result, error) {
	result := &Result{}

	var allArtifacts []*artifact.Artifact
	for _, src := range sources {
		arts, err := artifact.Discover(src.LocalPath, []string{"skills", "commands", "agents"})
		if err != nil {
			result.Errors = append(result.Errors, fmt.Errorf("discover %s: %w", src.Name, err))
			continue
		}
		allArtifacts = append(allArtifacts, arts...)
	}

	for harnessName, harness := range s.Harnesses {
		filtered := filterArtifacts(allArtifacts, harness)

		tgt := target.NewFromConfig(harnessName, harness)
		tr := &transform.Transformer{
			Variables:  harness.Variables,
			Keys:       harness.Keys,
			Values:     harness.Values,
			Tools:      harness.Tools,
			References: convertReferences(harness.References),
		}

		for _, art := range filtered {
			transformed, err := tr.Transform(art)
			if err != nil {
				result.Errors = append(result.Errors, fmt.Errorf("transform %s: %w", art.Name, err))
				continue
			}

			if err := tgt.Write(transformed); err != nil {
				result.Errors = append(result.Errors, fmt.Errorf("write %s: %w", art.Name, err))
				continue
			}

			result.Synced++
		}
	}

	return result, nil
}

func filterArtifacts(arts []*artifact.Artifact, harness henia.Harness) []*artifact.Artifact {
	var filtered []*artifact.Artifact

	allowedTypes := harness.Artifacts
	if len(allowedTypes) == 0 {
		allowedTypes = []string{"skills", "commands", "agents"}
	}

	typeSet := make(map[string]bool)
	for _, t := range allowedTypes {
		normalized := t
		if strings.HasSuffix(t, "s") {
			normalized = strings.TrimSuffix(t, "s")
		}
		typeSet[normalized] = true
	}

	for _, art := range arts {
		if !typeSet[string(art.Type)] {
			continue
		}
		if !shouldSync(art.Name, harness) {
			continue
		}
		filtered = append(filtered, art)
	}

	return filtered
}

func shouldSync(name string, harness henia.Harness) bool {
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

	for _, exc := range harness.Exclude {
		if exc == name {
			return false
		}
	}

	return true
}

func convertReferences(refs map[string]henia.ReferenceConfig) map[string]transform.ReferenceConfig {
	if refs == nil {
		return nil
	}
	result := make(map[string]transform.ReferenceConfig)
	for k, v := range refs {
		result[k] = transform.ReferenceConfig{Output: v.Output}
	}
	return result
}
