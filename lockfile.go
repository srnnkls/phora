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
	Version int          `toml:"version"`
	Sources []SourceLock `toml:"sources,omitempty"`
}

type SourceLock struct {
	Name      string     `toml:"name"`
	Repo      string     `toml:"repo"`
	Rev       string     `toml:"rev"`
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
	if err := lock.Validate(); err != nil {
		return nil, err
	}
	return &lock, nil
}

func (l *Lock) Validate() error {
	if l.Version != 1 {
		return fmt.Errorf("unsupported lock file version: %d (expected 1)", l.Version)
	}
	for _, src := range l.Sources {
		if err := src.Validate(); err != nil {
			return err
		}
	}
	return nil
}

func (s *SourceLock) Validate() error {
	if s.Name == "" {
		return fmt.Errorf("source lock missing required field: name")
	}
	if s.Repo == "" {
		return fmt.Errorf("source lock %q missing required field: repo", s.Name)
	}
	if !isValidSHA(s.SHA) {
		return fmt.Errorf("source lock %q has invalid SHA: must be 40-char hex string", s.Name)
	}
	if !isValidDigest(s.Digest) {
		return fmt.Errorf("source lock %q has invalid digest: must be 64-char hex string", s.Name)
	}
	return nil
}

func isValidSHA(s string) bool {
	if len(s) != 40 {
		return false
	}
	for _, c := range s {
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
			return false
		}
	}
	return true
}

func isValidDigest(s string) bool {
	if len(s) != 64 {
		return false
	}
	for _, c := range s {
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
			return false
		}
	}
	return true
}

func (l *Lock) Save(dir string) error {
	l.Version = 1
	lockPath := filepath.Join(dir, LockFileName)
	data, err := toml.Marshal(l)
	if err != nil {
		return err
	}
	return os.WriteFile(lockPath, data, 0644)
}

func (l *Lock) IsEmpty() bool {
	return len(l.Sources) == 0
}

func (l *Lock) FindSourceByName(name string) (SourceLock, bool) {
	for _, s := range l.Sources {
		if s.Name == name {
			return s, true
		}
	}
	return SourceLock{}, false
}

func (l *Lock) AddSource(source SourceLock) {
	for i, s := range l.Sources {
		if s.Name == source.Name {
			l.Sources[i] = source
			return
		}
	}
	l.Sources = append(l.Sources, source)
}

func (l *Lock) RemoveSource(name string) {
	var filtered []SourceLock
	for _, s := range l.Sources {
		if s.Name != name {
			filtered = append(filtered, s)
		}
	}
	l.Sources = filtered
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
