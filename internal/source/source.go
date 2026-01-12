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
	Host          string
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

func ParseRepoString(repo string) (host, owner, name string) {
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

func NewRepo(repoStr, ref, dataDir string, artifactTypes []string) *RepoSource {
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
	return fmt.Sprintf("https://%s/%s/%s.git", s.Host, s.Owner, s.Repo)
}

func (s *RepoSource) ManifestURL() string {
	if s.Host == "github.com" {
		return fmt.Sprintf("https://raw.githubusercontent.com/%s/%s/%s/.tropos/manifest.yaml",
			s.Owner, s.Repo, s.Ref)
	}
	return fmt.Sprintf("https://%s/%s/%s/-/raw/%s/.tropos/manifest.yaml",
		s.Host, s.Owner, s.Repo, s.Ref)
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
