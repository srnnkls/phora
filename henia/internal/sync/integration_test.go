package sync_test

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/srnnkls/henia"
	"github.com/srnnkls/henia/internal/sync"
	"github.com/srnnkls/phora"
)

// TestIntegrationHeniaSyncPhoraFetchDeploy tests the full sync workflow:
// 1. Fetcher (simulating phora.Client.FetchAll) returns local paths
// 2. Syncer discovers artifacts from those paths
// 3. Syncer transforms and deploys artifacts to harness targets

func TestIntegrationHeniaSyncPhoraFetchDeploy(t *testing.T) {
	tmpDir := t.TempDir()

	gitRepoDir := filepath.Join(tmpDir, "source-repo")
	targetDir := filepath.Join(tmpDir, "target")

	if err := os.MkdirAll(gitRepoDir, 0755); err != nil {
		t.Fatalf("MkdirAll gitRepoDir: %v", err)
	}
	if err := os.MkdirAll(targetDir, 0755); err != nil {
		t.Fatalf("MkdirAll targetDir: %v", err)
	}

	if err := initGitRepo(gitRepoDir); err != nil {
		t.Fatalf("initGitRepo: %v", err)
	}

	skillsDir := filepath.Join(gitRepoDir, "skills", "code-test")
	if err := os.MkdirAll(skillsDir, 0755); err != nil {
		t.Fatalf("MkdirAll skillsDir: %v", err)
	}

	skillContent := `---
name: code-test
description: TDD workflow skill
model: strong
---

# Code Test

Write tests first, then implementation.

## Workflow

1. RED - Write failing test
2. GREEN - Make it pass
3. REFACTOR - Clean up
`
	if err := os.WriteFile(filepath.Join(skillsDir, "SKILL.md"), []byte(skillContent), 0644); err != nil {
		t.Fatalf("WriteFile SKILL.md: %v", err)
	}

	commandsDir := filepath.Join(gitRepoDir, "commands", "test-run")
	if err := os.MkdirAll(commandsDir, 0755); err != nil {
		t.Fatalf("MkdirAll commandsDir: %v", err)
	}

	commandContent := `---
name: test-run
description: Run test suite
---

# Run Tests

Execute the test suite.
`
	if err := os.WriteFile(filepath.Join(commandsDir, "COMMAND.md"), []byte(commandContent), 0644); err != nil {
		t.Fatalf("WriteFile COMMAND.md: %v", err)
	}

	if err := commitGitRepo(gitRepoDir, "Add test artifacts"); err != nil {
		t.Fatalf("commitGitRepo: %v", err)
	}

	fetcher := &localFetcher{
		sources: map[string]string{
			"test-source": gitRepoDir,
		},
	}

	harnesses := map[string]henia.Harness{
		"claude": {
			Path:      targetDir,
			Artifacts: []string{"skills", "commands"},
			Keys: map[string]string{
				"model": "model_preference",
			},
			Values: map[string]map[string]string{
				"model_preference": {
					"strong": "opus",
				},
			},
		},
	}

	syncer := sync.NewSyncer(fetcher, harnesses)

	result, err := syncer.Sync()
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	if result.Synced != 2 {
		t.Errorf("Synced = %d, want 2 (1 skill + 1 command)", result.Synced)
	}

	if len(result.Errors) > 0 {
		t.Errorf("Unexpected errors: %v", result.Errors)
	}

	skillPath := filepath.Join(targetDir, "skills", "code-test", "SKILL.md")
	skillData, err := os.ReadFile(skillPath)
	if err != nil {
		t.Fatalf("Skill not deployed: %v", err)
	}

	skillStr := string(skillData)
	if !strings.Contains(skillStr, "model_preference: opus") {
		t.Errorf("Skill transformation not applied - expected model_preference: opus, got:\n%s", skillStr)
	}
	if !strings.Contains(skillStr, "name: code-test") {
		t.Errorf("Skill missing name field")
	}

	commandPath := filepath.Join(targetDir, "commands", "test-run", "COMMAND.md")
	commandData, err := os.ReadFile(commandPath)
	if err != nil {
		t.Fatalf("Command not deployed: %v", err)
	}

	if !strings.Contains(string(commandData), "name: test-run") {
		t.Errorf("Command missing name field")
	}
}

func TestIntegrationMultipleSourcesDeploy(t *testing.T) {
	tmpDir := t.TempDir()

	source1Dir := filepath.Join(tmpDir, "source1")
	source2Dir := filepath.Join(tmpDir, "source2")
	targetDir := filepath.Join(tmpDir, "target")

	for _, dir := range []string{source1Dir, source2Dir, targetDir} {
		if err := os.MkdirAll(dir, 0755); err != nil {
			t.Fatalf("MkdirAll %s: %v", dir, err)
		}
	}

	if err := initGitRepo(source1Dir); err != nil {
		t.Fatalf("initGitRepo source1: %v", err)
	}
	if err := initGitRepo(source2Dir); err != nil {
		t.Fatalf("initGitRepo source2: %v", err)
	}

	skillsDir1 := filepath.Join(source1Dir, "skills")
	if err := os.MkdirAll(skillsDir1, 0755); err != nil {
		t.Fatalf("MkdirAll skillsDir1: %v", err)
	}
	if err := os.WriteFile(filepath.Join(skillsDir1, "skill-a.md"), []byte(`---
name: skill-a
description: First skill from source 1
---

# Skill A
`), 0644); err != nil {
		t.Fatalf("WriteFile skill-a: %v", err)
	}

	skillsDir2 := filepath.Join(source2Dir, "skills")
	if err := os.MkdirAll(skillsDir2, 0755); err != nil {
		t.Fatalf("MkdirAll skillsDir2: %v", err)
	}
	if err := os.WriteFile(filepath.Join(skillsDir2, "skill-b.md"), []byte(`---
name: skill-b
description: Second skill from source 2
---

# Skill B
`), 0644); err != nil {
		t.Fatalf("WriteFile skill-b: %v", err)
	}

	if err := commitGitRepo(source1Dir, "Add skill-a"); err != nil {
		t.Fatalf("commitGitRepo source1: %v", err)
	}
	if err := commitGitRepo(source2Dir, "Add skill-b"); err != nil {
		t.Fatalf("commitGitRepo source2: %v", err)
	}

	fetcher := &localFetcher{
		sources: map[string]string{
			"source-1": source1Dir,
			"source-2": source2Dir,
		},
	}

	harnesses := map[string]henia.Harness{
		"claude": {
			Path:      targetDir,
			Artifacts: []string{"skills"},
		},
	}

	syncer := sync.NewSyncer(fetcher, harnesses)

	result, err := syncer.Sync()
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}

	if result.Synced != 2 {
		t.Errorf("Synced = %d, want 2 (2 skills from 2 sources)", result.Synced)
	}

	skillAPath := filepath.Join(targetDir, "skills", "skill-a", "SKILL.md")
	if _, err := os.Stat(skillAPath); err != nil {
		t.Errorf("skill-a not deployed: %v", err)
	}

	skillBPath := filepath.Join(targetDir, "skills", "skill-b", "SKILL.md")
	if _, err := os.Stat(skillBPath); err != nil {
		t.Errorf("skill-b not deployed: %v", err)
	}
}

type localFetcher struct {
	sources map[string]string
}

func (f *localFetcher) FetchAll() ([]phora.FetchResult, error) {
	results := make([]phora.FetchResult, 0, len(f.sources))
	for name, path := range f.sources {
		results = append(results, phora.FetchResult{
			Name:      name,
			LocalPath: path,
			Commit:    "test-commit",
			Files:     []string{},
		})
	}
	return results, nil
}

func initGitRepo(dir string) error {
	cmd := exec.Command("git", "init")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "config", "user.email", "test@test.com")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "config", "user.name", "Test")
	cmd.Dir = dir
	return cmd.Run()
}

func commitGitRepo(dir, message string) error {
	cmd := exec.Command("git", "add", ".")
	cmd.Dir = dir
	if err := cmd.Run(); err != nil {
		return err
	}

	cmd = exec.Command("git", "commit", "-m", message)
	cmd.Dir = dir
	return cmd.Run()
}
