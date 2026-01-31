package cli

import (
	"fmt"
	"os/exec"
	"strings"
	"time"

	"github.com/srnnkls/phora"
)

type UpdateResult struct {
	SourceName string
	OldSHA     string
	NewSHA     string
	SHA        string
	Digest     string
}

func updateSource(cfg *phora.Config, sourceName string, lockDir string) (*UpdateResult, error) {
	source, ok := cfg.Sources[sourceName]
	if !ok {
		return nil, fmt.Errorf("source '%s' not found in config", sourceName)
	}

	lock, err := phora.LoadLock(lockDir)
	if err != nil {
		return nil, fmt.Errorf("loading lock: %w", err)
	}

	var oldSHA string
	oldLock, found := lock.FindSourceByName(sourceName)
	if found {
		oldSHA = oldLock.SHA
	}

	ref := source.ResolveRev()
	if ref == "" {
		ref = "main"
	}

	newSHA, err := resolveRefToSHA(source.Git, ref)
	if err != nil {
		return nil, fmt.Errorf("resolving ref %q for source %q: %w", ref, sourceName, err)
	}
	newDigest := source.Digest()

	newSourceLock := phora.SourceLock{
		Name:      sourceName,
		Repo:      extractRepoFromGit(source.Git),
		Rev:       ref,
		SHA:       newSHA,
		Digest:    newDigest,
		FetchedAt: time.Now(),
	}

	lock.AddSource(newSourceLock)
	if err := lock.Save(lockDir); err != nil {
		return nil, fmt.Errorf("saving lock: %w", err)
	}

	return &UpdateResult{
		SourceName: sourceName,
		OldSHA:     oldSHA,
		NewSHA:     newSHA,
		SHA:        newSHA,
		Digest:     newDigest,
	}, nil
}

func updateAllSources(cfg *phora.Config, lockDir string) ([]UpdateResult, error) {
	var results []UpdateResult
	for name := range cfg.Sources {
		result, err := updateSource(cfg, name, lockDir)
		if err != nil {
			return nil, err
		}
		results = append(results, *result)
	}
	return results, nil
}

func resolveRefToSHA(gitURL, ref string) (string, error) {
	if gitURL == "" {
		return "", fmt.Errorf("git URL is empty")
	}

	cmd := exec.Command("git", "ls-remote", gitURL, ref)
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("git ls-remote failed: %w", err)
	}

	lines := strings.TrimSpace(string(output))
	if lines == "" {
		return "", fmt.Errorf("ref %q not found in %s", ref, gitURL)
	}

	parts := strings.Fields(lines)
	if len(parts) < 1 {
		return "", fmt.Errorf("unexpected git ls-remote output: %q", lines)
	}

	sha := parts[0]
	if len(sha) != 40 {
		return "", fmt.Errorf("invalid SHA length %d: %q", len(sha), sha)
	}

	return sha, nil
}

func extractRepoFromGit(gitURL string) string {
	if gitURL == "" {
		return ""
	}
	url := gitURL
	if len(url) > 4 && url[len(url)-4:] == ".git" {
		url = url[:len(url)-4]
	}
	if idx := lastIndex(url, "/"); idx >= 0 {
		owner := ""
		if prevIdx := lastIndex(url[:idx], "/"); prevIdx >= 0 {
			owner = url[prevIdx+1 : idx]
		}
		repo := url[idx+1:]
		if owner != "" {
			return owner + "/" + repo
		}
		return repo
	}
	return url
}

func lastIndex(s string, substr string) int {
	for i := len(s) - 1; i >= 0; i-- {
		if s[i] == substr[0] {
			return i
		}
	}
	return -1
}
