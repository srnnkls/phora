package phora

import (
	"os"
	"path/filepath"
)

type DriftStatus int

const (
	DriftNone DriftStatus = iota
	DriftModified
	DriftMissing
)

type DriftResult struct {
	Path     string
	Expected string
	Actual   string
	Status   DriftStatus
}

func DetectDrift(lock *Lock, sourceName string, targetDir string) ([]DriftResult, error) {
	source, found := lock.FindSourceByName(sourceName)
	if !found {
		return nil, nil
	}

	var results []DriftResult
	for _, file := range source.Files {
		fullPath := filepath.Join(targetDir, file.Path)

		if _, err := os.Stat(fullPath); os.IsNotExist(err) {
			results = append(results, DriftResult{
				Path:     file.Path,
				Expected: file.SHA256,
				Actual:   "",
				Status:   DriftMissing,
			})
			continue
		}

		actualHash, _, err := ComputeFileHash(fullPath)
		if err != nil {
			return nil, err
		}

		if actualHash != file.SHA256 {
			results = append(results, DriftResult{
				Path:     file.Path,
				Expected: file.SHA256,
				Actual:   actualHash,
				Status:   DriftModified,
			})
		}
	}

	return results, nil
}
