package target

import (
	"io"
	"os"
	"path/filepath"

	"github.com/srnnkls/tropos/internal/artifact"
	"github.com/srnnkls/tropos/internal/config"
)

type Target interface {
	Name() string
	Path() string
	TargetPath(art *artifact.Artifact) string
	Exists(art *artifact.Artifact) (bool, string)
	Write(art *artifact.Artifact) error
}

type HarnessTarget struct {
	name     string
	basePath string
}

func New(basePath string) *HarnessTarget {
	return &HarnessTarget{basePath: basePath}
}

func NewFromConfig(name string, h config.Harness) Target {
	path := config.ExpandPath(h.Path)
	return &HarnessTarget{name: name, basePath: path}
}

func (t *HarnessTarget) Name() string { return t.name }
func (t *HarnessTarget) Path() string { return t.basePath }

func (t *HarnessTarget) TargetPath(art *artifact.Artifact) string {
	typeDir := artifact.TypeDirName(art.Type)
	mainFile := artifact.MainFileName(art.Type)
	return filepath.Join(t.basePath, typeDir, art.Name, mainFile)
}

func (t *HarnessTarget) Exists(art *artifact.Artifact) (bool, string) {
	path := t.TargetPath(art)
	_, err := os.Stat(path)
	return err == nil, path
}

func (t *HarnessTarget) Write(art *artifact.Artifact) error {
	targetPath := t.TargetPath(art)
	targetDir := filepath.Dir(targetPath)

	if err := os.MkdirAll(targetDir, 0755); err != nil {
		return err
	}

	content := art.Render()
	if err := os.WriteFile(targetPath, []byte(content), 0644); err != nil {
		return err
	}

	if art.IsDirectory && len(art.Resources) > 0 {
		if err := copyResources(art.SourcePath, targetDir, art.Resources); err != nil {
			return err
		}
	}

	return nil
}

func copyResources(srcDir, dstDir string, resources []string) error {
	for _, res := range resources {
		srcPath := filepath.Join(srcDir, res)
		dstPath := filepath.Join(dstDir, res)

		info, err := os.Stat(srcPath)
		if err != nil {
			continue
		}

		if info.IsDir() {
			if err := copyDir(srcPath, dstPath); err != nil {
				return err
			}
		} else {
			if err := copyFile(srcPath, dstPath); err != nil {
				return err
			}
		}
	}
	return nil
}

func copyDir(src, dst string) error {
	if err := os.MkdirAll(dst, 0755); err != nil {
		return err
	}

	entries, err := os.ReadDir(src)
	if err != nil {
		return err
	}

	for _, entry := range entries {
		srcPath := filepath.Join(src, entry.Name())
		dstPath := filepath.Join(dst, entry.Name())

		if entry.IsDir() {
			if err := copyDir(srcPath, dstPath); err != nil {
				return err
			}
		} else {
			if err := copyFile(srcPath, dstPath); err != nil {
				return err
			}
		}
	}

	return nil
}

func copyFile(src, dst string) error {
	if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
		return err
	}

	srcFile, err := os.Open(src)
	if err != nil {
		return err
	}
	defer srcFile.Close()

	srcInfo, err := srcFile.Stat()
	if err != nil {
		return err
	}

	dstFile, err := os.OpenFile(dst, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, srcInfo.Mode())
	if err != nil {
		return err
	}
	defer dstFile.Close()

	_, err = io.Copy(dstFile, srcFile)
	return err
}
