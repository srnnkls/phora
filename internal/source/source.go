package source

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/srnnkls/tropos/internal/artifact"
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
	Owner         string
	Repo          string
	Ref           string
	DataDir       string
	artifactTypes []string
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

func ParseRepoString(repo string) (owner, name string) {
	parts := strings.SplitN(repo, "/", 2)
	if len(parts) != 2 {
		return "", repo
	}
	return parts[0], parts[1]
}

func NewRepo(repoStr, ref, dataDir string, artifactTypes []string) *RepoSource {
	owner, repo := ParseRepoString(repoStr)
	if ref == "" {
		ref = "main"
	}
	if len(artifactTypes) == 0 {
		artifactTypes = []string{"skills", "commands", "agents"}
	}
	return &RepoSource{
		Owner:         owner,
		Repo:          repo,
		Ref:           ref,
		DataDir:       dataDir,
		artifactTypes: artifactTypes,
	}
}

func (s *RepoSource) Name() string {
	return fmt.Sprintf("%s/%s", s.Owner, s.Repo)
}

func (s *RepoSource) LocalPath() string {
	return filepath.Join(s.DataDir, s.Owner, s.Repo)
}

func (s *RepoSource) ManifestURL() string {
	return fmt.Sprintf("https://raw.githubusercontent.com/%s/%s/%s/tropos.toml",
		s.Owner, s.Repo, s.Ref)
}

func (s *RepoSource) FetchManifest() ([]byte, error) {
	resp, err := http.Get(s.ManifestURL())
	if err != nil {
		return nil, fmt.Errorf("fetch manifest: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("manifest not found: %s", resp.Status)
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
		return s.Pull()
	}

	if err := os.MkdirAll(filepath.Dir(localPath), 0755); err != nil {
		return err
	}

	// Use git sparse-checkout for efficiency
	// For now, simple clone
	repoURL := fmt.Sprintf("https://github.com/%s/%s.git", s.Owner, s.Repo)

	// This is a placeholder - actual implementation would use go-git
	_ = repoURL
	return fmt.Errorf("clone not implemented - use go-git")
}

func (s *RepoSource) Pull() error {
	return fmt.Errorf("pull not implemented - use go-git")
}
