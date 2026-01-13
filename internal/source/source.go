package source

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/srnnkls/phora/internal/artifact"
	"github.com/srnnkls/phora/internal/config"
)

type Source interface {
	Name() string
	Discover() ([]*artifact.Artifact, error)
}

type LocalSource struct {
	path          string
	artifactTypes []string
}

type RepoSource struct {
	Host          string
	Owner         string
	Repo          string
	Ref           string
	DataDir       string
	HostConfig    *config.Host
	artifactTypes []string
}

// expandTemplate replaces {owner}, {repo}, {ref}, {path} placeholders in a URL template
func expandTemplate(template string, owner, repo, ref, path string) string {
	result := template
	result = strings.ReplaceAll(result, "{owner}", owner)
	result = strings.ReplaceAll(result, "{repo}", repo)
	result = strings.ReplaceAll(result, "{ref}", ref)
	result = strings.ReplaceAll(result, "{path}", path)
	return result
}

func NewLocal(path string, artifactTypes []string) *LocalSource {
	if len(artifactTypes) == 0 {
		artifactTypes = []string{"skills", "commands", "agents"}
	}
	return &LocalSource{
		path:          path,
		artifactTypes: artifactTypes,
	}
}

func (s *LocalSource) Name() string {
	return s.path
}

func (s *LocalSource) Discover() ([]*artifact.Artifact, error) {
	return artifact.Discover(s.path, s.artifactTypes)
}

// ParseRepoString parses a repository string into host, owner, and repo name
// Supports formats:
//   - "owner/repo" -> defaults to github.com
//   - "host/owner/repo" -> uses specified host
//   - "https://host/owner/repo" or "https://host/owner/repo.git" -> parses full URL
func ParseRepoString(repo string) (host, owner, name string) {
	// Remove git suffix if present
	repo = strings.TrimSuffix(repo, ".git")

	// Handle full URLs
	if strings.HasPrefix(repo, "https://") || strings.HasPrefix(repo, "http://") {
		repo = strings.TrimPrefix(repo, "https://")
		repo = strings.TrimPrefix(repo, "http://")
		parts := strings.Split(repo, "/")
		if len(parts) >= 3 {
			return parts[0], parts[1], parts[2]
		}
	}

	// Handle shorthand formats
	parts := strings.Split(repo, "/")
	switch len(parts) {
	case 3:
		return parts[0], parts[1], parts[2]
	case 2:
		return "github.com", parts[0], parts[1]
	default:
		return "github.com", "", repo
	}
}

func NewRepo(repoStr, ref, dataDir string, hostConfig *config.Host, artifactTypes []string) *RepoSource {
	host, owner, repo := ParseRepoString(repoStr)
	if ref == "" {
		ref = "main"
	}
	if len(artifactTypes) == 0 {
		artifactTypes = []string{"skills", "commands", "agents"}
	}
	return &RepoSource{
		Host:          host,
		Owner:         owner,
		Repo:          repo,
		Ref:           ref,
		DataDir:       dataDir,
		HostConfig:    hostConfig,
		artifactTypes: artifactTypes,
	}
}

func (s *RepoSource) Name() string {
	return fmt.Sprintf("%s/%s", s.Owner, s.Repo)
}

func (s *RepoSource) LocalPath() string {
	return filepath.Join(s.DataDir, s.Host, s.Owner, s.Repo)
}

func (s *RepoSource) RepoURL() string {
	// Use host config template if available
	if s.HostConfig != nil && s.HostConfig.GitURL != "" {
		return expandTemplate(s.HostConfig.GitURL, s.Owner, s.Repo, s.Ref, "")
	}
	// Fallback to standard git URL
	return fmt.Sprintf("https://%s/%s/%s.git", s.Host, s.Owner, s.Repo)
}

func (s *RepoSource) ConfigURL() string {
	// Use host config template if available
	if s.HostConfig != nil && s.HostConfig.RawURL != "" {
		return expandTemplate(s.HostConfig.RawURL, s.Owner, s.Repo, s.Ref, "phora.toml")
	}
	// No fallback for config URL - return empty string
	return ""
}

func (s *RepoSource) FetchConfig() ([]byte, error) {
	configURL := s.ConfigURL()
	if configURL == "" {
		return nil, fmt.Errorf("no host configuration for %s (direct config fetch not supported)", s.Host)
	}

	resp, err := http.Get(configURL)
	if err != nil {
		return nil, fmt.Errorf("fetch config: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("phora.toml not found: %s", resp.Status)
	}

	return io.ReadAll(resp.Body)
}

func (s *RepoSource) Discover() ([]*artifact.Artifact, error) {
	localPath := s.LocalPath()
	if _, err := os.Stat(localPath); os.IsNotExist(err) {
		return nil, fmt.Errorf("repo not cloned: %s", localPath)
	}
	return artifact.Discover(localPath, s.artifactTypes)
}

func (s *RepoSource) Clone() error {
	localPath := s.LocalPath()

	if _, err := os.Stat(localPath); err == nil {
		if err := s.Pull(); err != nil {
			if err := os.RemoveAll(localPath); err != nil {
				return fmt.Errorf("remove stale clone: %w", err)
			}
			return s.clone()
		}
		return nil
	}

	return s.clone()
}

func (s *RepoSource) clone() error {
	localPath := s.LocalPath()

	if err := os.MkdirAll(filepath.Dir(localPath), 0755); err != nil {
		return err
	}

	_, err := git.PlainClone(localPath, false, &git.CloneOptions{
		URL:           s.RepoURL(),
		ReferenceName: plumbing.NewBranchReferenceName(s.Ref),
		SingleBranch:  true,
		Depth:         1,
	})
	return err
}

func (s *RepoSource) Pull() error {
	localPath := s.LocalPath()

	repo, err := git.PlainOpen(localPath)
	if err != nil {
		return err
	}

	wt, err := repo.Worktree()
	if err != nil {
		return err
	}

	err = wt.Pull(&git.PullOptions{
		ReferenceName: plumbing.NewBranchReferenceName(s.Ref),
		SingleBranch:  true,
	})
	if err == git.NoErrAlreadyUpToDate {
		return nil
	}
	return err
}
