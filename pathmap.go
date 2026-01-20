package phora

import (
	"os"
	"path/filepath"

	toml "github.com/pelletier/go-toml/v2"
)

// PathMap provides path resolution through exports and rewrites.
type PathMap struct {
	Exports  map[string]string
	Rewrites map[string]string
}

// NewPathMap creates a PathMap with the given exports and rewrites.
func NewPathMap(exports, rewrites map[string]string) *PathMap {
	if exports == nil {
		exports = map[string]string{}
	}
	if rewrites == nil {
		rewrites = map[string]string{}
	}
	return &PathMap{
		Exports:  exports,
		Rewrites: rewrites,
	}
}

// ResolveExport resolves a path using only the exports mapping.
// Returns the original path if no mapping exists.
func (p *PathMap) ResolveExport(path string) string {
	if p.Exports == nil {
		return path
	}
	if resolved, ok := p.Exports[path]; ok {
		return resolved
	}
	return path
}

// Resolve resolves a path by applying exports first, then rewrites.
// Returns the original path if no mapping exists.
func (p *PathMap) Resolve(path string) string {
	resolved := p.ResolveExport(path)
	if p.Rewrites != nil {
		if rewritten, ok := p.Rewrites[resolved]; ok {
			return rewritten
		}
	}
	return resolved
}

// RepoManifest represents the manifest from a phora.toml file in a source repo.
type RepoManifest struct {
	Name    string            `toml:"name"`
	Exports map[string]string `toml:"exports"`
}

// LoadRepoManifest loads a manifest from phora.toml in the given repository path.
func LoadRepoManifest(repoPath string) (*RepoManifest, error) {
	configPath := filepath.Join(repoPath, "phora.toml")
	data, err := os.ReadFile(configPath)
	if err != nil {
		return nil, err
	}

	var m RepoManifest
	if err := toml.Unmarshal(data, &m); err != nil {
		return nil, err
	}

	return &m, nil
}
