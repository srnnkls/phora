package phora

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

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

const defaultGitURLTemplate = "https://github.com/{owner}/{repo}.git"

func (c *Client) ResolveGitURL(source Source) (string, error) {
	owner, repo, err := source.ParseRepo()
	if err != nil {
		return "", err
	}

	template := defaultGitURLTemplate

	if source.Host != "" {
		if c.Config.Hosts == nil {
			return "", fmt.Errorf("unknown host %q", source.Host)
		}
		host, ok := c.Config.Hosts[source.Host]
		if !ok {
			return "", fmt.Errorf("unknown host %q", source.Host)
		}
		template = host.GitURL
	}

	url := strings.ReplaceAll(template, "{owner}", owner)
	url = strings.ReplaceAll(url, "{repo}", repo)

	return url, nil
}

func (c *Client) Fetch(sourceName string) (*FetchResult, error) {
	if c.Config.Sources == nil {
		return nil, fmt.Errorf("unknown source %q", sourceName)
	}
	source, ok := c.Config.Sources[sourceName]
	if !ok {
		return nil, fmt.Errorf("unknown source %q", sourceName)
	}

	url, err := c.ResolveGitURL(source)
	if err != nil {
		return nil, err
	}

	localPath := filepath.Join(c.DataDir, sourceName)

	repo := Repo{
		Name:      sourceName,
		URL:       url,
		LocalPath: localPath,
		Ref:       source.Ref,
	}

	if err := CloneOrPull(repo); err != nil {
		return nil, err
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

		lock.AddRepo(RepoEntry{
			Name:      sourceName,
			Repo:      source.Repo,
			Ref:       source.Ref,
			Commit:    commit,
			FetchedAt: time.Now(),
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
