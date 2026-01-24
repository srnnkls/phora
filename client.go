package phora

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// extractRepoFromGit extracts "owner/repo" from a git URL.
// Handles both https:// and git@ formats.
func extractRepoFromGit(gitURL string) string {
	if gitURL == "" {
		return ""
	}
	url := gitURL
	url = strings.TrimSuffix(url, ".git")

	// Handle git@host:owner/repo format (SSH URLs)
	if strings.HasPrefix(url, "git@") {
		if idx := strings.Index(url, ":"); idx >= 0 {
			return url[idx+1:]
		}
	}

	// Handle https://host/owner/repo format
	// Find the last two path segments
	parts := strings.Split(url, "/")
	if len(parts) >= 2 {
		return parts[len(parts)-2] + "/" + parts[len(parts)-1]
	}
	return url
}

type Client struct {
	Config  *Config
	DataDir string
	LockDir string
}

type ClientOption func(*Client)

func NewClient(cfg *Config, opts ...ClientOption) *Client {
	c := &Client{
		Config: cfg,
	}

	for _, opt := range opts {
		opt(c)
	}

	if c.DataDir == "" {
		home, err := os.UserHomeDir()
		if err == nil {
			c.DataDir = filepath.Join(home, ".local", "share", "phora")
		}
	}

	return c
}

func WithDataDir(dir string) ClientOption {
	return func(c *Client) {
		c.DataDir = dir
	}
}

func WithLockDir(dir string) ClientOption {
	return func(c *Client) {
		c.LockDir = dir
	}
}

func (c *Client) Fetch(sourceName string) (*FetchResult, error) {
	if c.Config.Sources == nil {
		return nil, fmt.Errorf("unknown source %q", sourceName)
	}
	source, ok := c.Config.Sources[sourceName]
	if !ok {
		return nil, fmt.Errorf("unknown source %q", sourceName)
	}

	localPath := filepath.Join(c.DataDir, sourceName)

	repo := Repo{
		Name:      sourceName,
		URL:       source.Git,
		LocalPath: localPath,
		Ref:       source.ResolveRev(),
	}

	if err := CloneOrPull(repo); err != nil {
		return nil, err
	}

	// Validate path against producer's manifest
	if source.Path != "" && !source.IgnoreManifest {
		producerCfg, err := LoadConfig(filepath.Join(localPath, "phora.toml"))
		if err == nil && producerCfg.Manifest != nil {
			if err := producerCfg.Manifest.ValidatePath(source.Path); err != nil {
				return nil, fmt.Errorf("path validation: %w", err)
			}
		}
	}

	commit, err := repo.CurrentCommit()
	if err != nil {
		return nil, err
	}

	files, err := repo.ListFiles()
	if err != nil {
		return nil, err
	}

	result := &FetchResult{
		Name:      sourceName,
		LocalPath: localPath,
		Commit:    commit,
		Files:     files,
	}

	if c.LockDir != "" {
		lock, err := LoadLock(c.LockDir)
		if err != nil {
			return nil, err
		}

		var fileLocks []FileLock
		for _, path := range files {
			fullPath := filepath.Join(localPath, path)
			hash, size, err := ComputeFileHash(fullPath)
			if err != nil {
				return nil, fmt.Errorf("computing hash for %s: %w", path, err)
			}
			fileLocks = append(fileLocks, FileLock{
				Path:   path,
				SHA256: hash,
				Size:   size,
			})
		}

		lock.AddSource(SourceLock{
			Name:      sourceName,
			Repo:      extractRepoFromGit(source.Git),
			Rev:       source.ResolveRev(),
			SHA:       commit,
			Digest:    source.Digest(),
			FetchedAt: time.Now(),
			Files:     fileLocks,
		})

		if err := lock.Save(c.LockDir); err != nil {
			return nil, err
		}
	}

	return result, nil
}

func (c *Client) FetchAll() ([]FetchResult, error) {
	results := make([]FetchResult, 0)

	if c.Config.Sources == nil {
		return results, nil
	}

	for name := range c.Config.Sources {
		result, err := c.Fetch(name)
		if err != nil {
			return nil, err
		}
		results = append(results, *result)
	}

	return results, nil
}
