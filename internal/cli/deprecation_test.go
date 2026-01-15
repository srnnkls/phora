package cli

import (
	"bytes"
	"strings"
	"testing"
)

func TestDeployCommand_PrintsDeprecationWarning(t *testing.T) {
	var stderr bytes.Buffer
	rootCmd.SetErr(&stderr)
	rootCmd.SetOut(&bytes.Buffer{})
	rootCmd.SetArgs([]string{"deploy", "--help"})

	_ = rootCmd.Execute()

	got := stderr.String()
	want := "Warning: 'phora deploy' is deprecated. Use 'henia deploy' instead."
	if !strings.Contains(got, want) {
		t.Errorf("stderr = %q, want to contain %q", got, want)
	}
}

func TestInitCommand_PrintsDeprecationWarning(t *testing.T) {
	var stderr bytes.Buffer
	rootCmd.SetErr(&stderr)
	rootCmd.SetOut(&bytes.Buffer{})
	rootCmd.SetArgs([]string{"init", "--help"})

	_ = rootCmd.Execute()

	got := stderr.String()
	want := "Warning: 'phora init' is deprecated. Use 'henia init' instead."
	if !strings.Contains(got, want) {
		t.Errorf("stderr = %q, want to contain %q", got, want)
	}
}
