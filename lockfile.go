package phora

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/pelletier/go-toml/v2"
)

const LockFileName = "phora.lock"

type Lock struct {
	Version int          `toml:"version,omitempty"`
	Sources []SourceLock `toml:"sources,omitempty"`
	Repos   []RepoEntry  `toml:"repos,omitempty"`
}

type SourceLock struct {
	Name      string     `toml:"name"`
	Repo      string     `toml:"repo"`
	Ref       string     `toml:"ref"`
	SHA       string     `toml:"sha"`
	Digest    string     `toml:"digest"`
	FetchedAt time.Time  `toml:"fetched_at"`
	Files     []FileLock `toml:"files"`
}

type FileLock struct {
	Path   string `toml:"path"`
	SHA256 string `toml:"sha256"`
	Size   int64  `toml:"size"`
}

type RepoEntry struct {
	Name      string    `toml:"name"`
	Repo      string    `toml:"repo"`
	Ref       string    `toml:"ref"`
	Commit    string    `toml:"commit"`
	FetchedAt time.Time `toml:"fetched_at"`
}

func LoadLock(dir string) (*Lock, error) {
	lockPath := filepath.Join(dir, LockFileName)
	data, err := os.ReadFile(lockPath)
	if os.IsNotExist(err) {
		return &Lock{}, nil
	}
	if err != nil {
		return nil, err
	}

	var lock Lock
	if err := toml.Unmarshal(data, &lock); err != nil {
		return nil, err
	}
	return &lock, nil
}

func (l *Lock) Save(dir string) error {
	lockPath := filepath.Join(dir, LockFileName)
	data, err := toml.Marshal(l)
	if err != nil {
		return err
	}
	return os.WriteFile(lockPath, data, 0644)
}

func (l *Lock) AddRepo(entry RepoEntry) {
	for i, r := range l.Repos {
		if r.Name == entry.Name {
			l.Repos[i] = entry
			return
		}
	}
	l.Repos = append(l.Repos, entry)
}

func (l *Lock) FindByName(name string) (RepoEntry, bool) {
	for _, r := range l.Repos {
		if r.Name == name {
			return r, true
		}
	}
	return RepoEntry{}, false
}

func (l *Lock) RemoveByName(name string) {
	var filtered []RepoEntry
	for _, r := range l.Repos {
		if r.Name != name {
			filtered = append(filtered, r)
		}
	}
	l.Repos = filtered
}

func (l *Lock) IsEmpty() bool {
	return len(l.Repos) == 0
}

func ComputeFileHash(path string) (string, int64, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", 0, err
	}
	if info.IsDir() {
		return "", 0, fmt.Errorf("cannot hash directory: %s", path)
	}

	f, err := os.Open(path)
	if err != nil {
		return "", 0, err
	}
	defer f.Close()

	h := sha256.New()
	size, err := io.Copy(h, f)
	if err != nil {
		return "", 0, err
	}

	return hex.EncodeToString(h.Sum(nil)), size, nil
}
