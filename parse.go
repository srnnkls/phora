package phora

import (
	"errors"
	"net/url"
	"strings"
)

// ParsedURL contains components extracted from a repository URL.
type ParsedURL struct {
	Git    string // Full git URL (e.g., "https://github.com/owner/repo.git")
	Branch string // Branch name if extracted from URL
	Tag    string // Tag name (not extracted from URL parsing)
	Rev    string // Commit rev (not extracted from URL parsing)
	Path   string // Subdirectory path within repo
}

// ParseURL parses various URL formats into normalized components.
// Supported formats:
//   - owner/repo (GitHub shorthand)
//   - owner/repo/path/to/dir (GitHub shorthand with path)
//   - https://github.com/owner/repo
//   - https://github.com/owner/repo/tree/ref/path
//   - https://github.com/owner/repo/blob/ref/path
//   - gitlab.com/owner/repo/path
//   - https://gitlab.com/owner/repo/-/tree/ref/path
func ParseURL(input string) (*ParsedURL, error) {
	if input == "" {
		return nil, errors.New("empty URL")
	}

	if !strings.Contains(input, "/") {
		return nil, errors.New("invalid URL: must contain owner/repo")
	}

	// Check if it's a full URL (has scheme)
	if strings.HasPrefix(input, "https://") || strings.HasPrefix(input, "http://") {
		return parseFullURL(input)
	}

	// Check for invalid scheme
	if strings.Contains(input, "://") {
		return nil, errors.New("unsupported URL scheme")
	}

	// Check if it starts with a known host (e.g., gitlab.com/...)
	if strings.HasPrefix(input, "gitlab.com/") {
		return parseGitLabShorthand(input)
	}

	// Treat as GitHub shorthand: owner/repo or owner/repo/path
	return parseGitHubShorthand(input)
}

func parseFullURL(input string) (*ParsedURL, error) {
	u, err := url.Parse(input)
	if err != nil {
		return nil, err
	}

	if u.Scheme != "https" && u.Scheme != "http" {
		return nil, errors.New("unsupported URL scheme")
	}

	path := strings.TrimPrefix(u.Path, "/")
	parts := strings.Split(path, "/")

	if len(parts) < 2 {
		return nil, errors.New("invalid URL: must contain owner/repo")
	}

	owner := parts[0]
	repo := strings.TrimSuffix(parts[1], ".git")

	result := &ParsedURL{
		Git: "https://" + u.Host + "/" + owner + "/" + repo + ".git",
	}

	// Handle GitLab URLs with /-/ pattern
	if u.Host == "gitlab.com" && len(parts) >= 5 && parts[2] == "-" {
		// gitlab.com/owner/repo/-/tree/ref/path or /-/blob/ref/path
		if parts[3] == "tree" || parts[3] == "blob" {
			result.Branch = parts[4]
			if len(parts) > 5 {
				result.Path = strings.Join(parts[5:], "/")
			}
		}
		return result, nil
	}

	// Handle GitHub tree/blob URLs
	if len(parts) >= 4 && (parts[2] == "tree" || parts[2] == "blob") {
		result.Branch = parts[3]
		if len(parts) > 4 {
			result.Path = strings.Join(parts[4:], "/")
		}
		return result, nil
	}

	// Plain URL (just owner/repo)
	return result, nil
}

func parseGitHubShorthand(input string) (*ParsedURL, error) {
	parts := strings.Split(input, "/")
	if len(parts) < 2 {
		return nil, errors.New("invalid URL: must contain owner/repo")
	}

	owner := parts[0]
	repo := parts[1]

	result := &ParsedURL{
		Git: "https://github.com/" + owner + "/" + repo + ".git",
	}

	if len(parts) > 2 {
		result.Path = strings.Join(parts[2:], "/")
	}

	return result, nil
}

func parseGitLabShorthand(input string) (*ParsedURL, error) {
	// Remove gitlab.com/ prefix
	path := strings.TrimPrefix(input, "gitlab.com/")
	parts := strings.Split(path, "/")

	if len(parts) < 2 {
		return nil, errors.New("invalid URL: must contain owner/repo")
	}

	owner := parts[0]
	repo := parts[1]

	result := &ParsedURL{
		Git: "https://gitlab.com/" + owner + "/" + repo + ".git",
	}

	if len(parts) > 2 {
		result.Path = strings.Join(parts[2:], "/")
	}

	return result, nil
}
