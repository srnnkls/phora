package phora

import (
	"path/filepath"
	"strings"
)

// Filter applies include/exclude glob patterns to file paths.
type Filter struct {
	Include []string
	Exclude []string
}

// Match returns true if the path should be included after applying filters.
// Algorithm: include-then-exclude
// 1. If include is empty/nil, all paths are initially included
// 2. Otherwise, path must match at least one include pattern
// 3. If path matches any exclude pattern, it is excluded
func (f *Filter) Match(path string) bool {
	if !f.matchesInclude(path) {
		return false
	}
	return !f.matchesExclude(path)
}

func (f *Filter) matchesInclude(path string) bool {
	if len(f.Include) == 0 {
		return true
	}
	for _, pattern := range f.Include {
		if matchGlob(pattern, path) {
			return true
		}
	}
	return false
}

func (f *Filter) matchesExclude(path string) bool {
	for _, pattern := range f.Exclude {
		if matchGlob(pattern, path) {
			return true
		}
	}
	return false
}

// Apply filters a list of paths, returning only those that match.
func (f *Filter) Apply(paths []string) []string {
	var result []string
	for _, p := range paths {
		if f.Match(p) {
			result = append(result, p)
		}
	}
	return result
}

// matchGlob matches a pattern against a path, supporting ** for any depth.
func matchGlob(pattern, path string) bool {
	if strings.Contains(pattern, "**") {
		return matchDoubleGlob(pattern, path)
	}
	matched, _ := filepath.Match(pattern, path)
	return matched
}

// matchDoubleGlob handles patterns with ** which match any number of path segments.
func matchDoubleGlob(pattern, path string) bool {
	parts := strings.Split(pattern, "**")
	if len(parts) != 2 {
		return false
	}

	prefix := parts[0]
	suffix := parts[1]

	// Remove leading slash from suffix if present
	suffix = strings.TrimPrefix(suffix, "/")

	// Check prefix if present
	if prefix != "" {
		prefix = strings.TrimSuffix(prefix, "/")
		if !strings.HasPrefix(path, prefix+"/") && path != prefix {
			return false
		}
		// Remove prefix from path for suffix matching
		path = strings.TrimPrefix(path, prefix+"/")
	}

	// Match suffix against path and all subdirectories
	if suffix == "" {
		return true
	}

	// Try matching suffix at each directory level
	pathParts := strings.Split(path, "/")
	for i := range pathParts {
		subPath := strings.Join(pathParts[i:], "/")
		matched, _ := filepath.Match(suffix, subPath)
		if matched {
			return true
		}
		// Also try matching just the filename portion
		if i == len(pathParts)-1 {
			matched, _ = filepath.Match(suffix, pathParts[i])
			if matched {
				return true
			}
		}
	}
	return false
}
