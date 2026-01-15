package phora

import (
	"os"
	"path/filepath"

	"github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/go-git/go-git/v5/plumbing/object"
)

type Repo struct {
	Name      string
	URL       string
	LocalPath string
	Ref       string
}

func CloneOrPull(repo Repo) error {
	if _, err := os.Stat(repo.LocalPath); os.IsNotExist(err) {
		if err := os.MkdirAll(filepath.Dir(repo.LocalPath), 0755); err != nil {
			return err
		}
		_, err := git.PlainClone(repo.LocalPath, false, &git.CloneOptions{
			URL:           repo.URL,
			ReferenceName: plumbing.NewBranchReferenceName(repo.Ref),
			SingleBranch:  true,
			Depth:         1,
		})
		return err
	}

	gitRepo, err := git.PlainOpen(repo.LocalPath)
	if err != nil {
		return err
	}

	wt, err := gitRepo.Worktree()
	if err != nil {
		return err
	}

	err = wt.Pull(&git.PullOptions{})
	if err == git.NoErrAlreadyUpToDate {
		return nil
	}
	return err
}

func (r *Repo) CurrentCommit() (string, error) {
	gitRepo, err := git.PlainOpen(r.LocalPath)
	if err != nil {
		return "", err
	}

	head, err := gitRepo.Head()
	if err != nil {
		return "", err
	}

	return head.Hash().String(), nil
}

func (r *Repo) ListFiles() ([]string, error) {
	gitRepo, err := git.PlainOpen(r.LocalPath)
	if err != nil {
		return nil, err
	}

	head, err := gitRepo.Head()
	if err != nil {
		return nil, err
	}

	commit, err := gitRepo.CommitObject(head.Hash())
	if err != nil {
		return nil, err
	}

	tree, err := commit.Tree()
	if err != nil {
		return nil, err
	}

	var files []string
	err = tree.Files().ForEach(func(f *object.File) error {
		files = append(files, f.Name)
		return nil
	})
	if err != nil {
		return nil, err
	}

	return files, nil
}
