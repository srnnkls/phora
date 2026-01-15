package cli

import (
	"bytes"
	"os"
	"strings"
	"testing"
)

func TestDeployCommand_PrintsDeprecationWarning(t *testing.T) {
	var stderr bytes.Buffer
	rootCmd.SetErr(&stderr)
	rootCmd.SetOut(&bytes.Buffer{})
	rootCmd.SetArgs([]string{"deploy", "--dry-run"})

	// Command will fail (no config), but PreRunE should still print warning
	_ = rootCmd.Execute()

	got := stderr.String()
	want := "Warning: 'phora deploy' is deprecated. Use 'henia deploy' instead."
	if !strings.Contains(got, want) {
		t.Errorf("stderr = %q, want to contain %q", got, want)
	}
}

func TestInitCommand_PrintsDeprecationWarning(t *testing.T) {
	// Change to temp directory to avoid modifying actual files
	tmpDir := t.TempDir()
	originalDir, _ := os.Getwd()
	os.Chdir(tmpDir)
	defer os.Chdir(originalDir)

	var stderr bytes.Buffer
	rootCmd.SetErr(&stderr)
	rootCmd.SetOut(&bytes.Buffer{})
	rootCmd.SetArgs([]string{"init"})

	_ = rootCmd.Execute()

	got := stderr.String()
	want := "Warning: 'phora init' is deprecated. Use 'henia init' instead."
	if !strings.Contains(got, want) {
		t.Errorf("stderr = %q, want to contain %q", got, want)
	}
}
