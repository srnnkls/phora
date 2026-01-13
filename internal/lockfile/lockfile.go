package lockfile

import (
	"crypto/sha256"
	"encoding/hex"
	"io"
	"os"
	"path/filepath"
	"slices"

	"github.com/pelletier/go-toml/v2"
)

const FileName = ".tropos.lock"

type LockFile struct {
	Files []FileEntry `toml:"files"`
}

type FileEntry struct {
	Path     string `toml:"path"`
	Checksum string `toml:"checksum"`
	Artifact string `toml:"artifact"`
	Type     string `toml:"type"`
	Resource bool   `toml:"resource,omitempty"`
}

func Load(targetPath string) (*LockFile, error) {
	lockPath := filepath.Join(targetPath, FileName)
	data, err := os.ReadFile(lockPath)
	if os.IsNotExist(err) {
		return &LockFile{}, nil
	}
	if err != nil {
		return nil, err
	}

	var lf LockFile
	if err := toml.Unmarshal(data, &lf); err != nil {
		return nil, err
	}
	return &lf, nil
}

func (l *LockFile) Save(targetPath string) error {
	lockPath := filepath.Join(targetPath, FileName)

	// Only write if parsed content differs
	if existing, err := Load(targetPath); err == nil {
		if l.Equal(existing) {
			return nil
		}
	}

	data, err := toml.Marshal(l)
	if err != nil {
		return err
	}

	return os.WriteFile(lockPath, data, 0644)
}

func (l *LockFile) Equal(other *LockFile) bool {
	return slices.EqualFunc(l.Files, other.Files, func(a, b FileEntry) bool {
		return a == b
	})
}

func (l *LockFile) IsManaged(relativePath string) bool {
	for _, f := range l.Files {
		if f.Path == relativePath {
			return true
		}
	}
	return false
}

func (l *LockFile) IsEmpty() bool {
	return len(l.Files) == 0
}

func (l *LockFile) Add(entry FileEntry) {
	for i, f := range l.Files {
		if f.Path == entry.Path {
			l.Files[i] = entry
			return
		}
	}
	l.Files = append(l.Files, entry)
}

func (l *LockFile) Remove(relativePath string) {
	var filtered []FileEntry
	for _, f := range l.Files {
		if f.Path != relativePath {
			filtered = append(filtered, f)
		}
	}
	l.Files = filtered
}

func (l *LockFile) GetByArtifact(artifactName string) []FileEntry {
	var entries []FileEntry
	for _, f := range l.Files {
		if f.Artifact == artifactName {
			entries = append(entries, f)
		}
	}
	return entries
}

func ComputeChecksum(filePath string) (string, error) {
	f, err := os.Open(filePath)
	if err != nil {
		return "", err
	}
	defer f.Close()

	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}

	return "sha256:" + hex.EncodeToString(h.Sum(nil)), nil
}
