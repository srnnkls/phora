package phora

// FetchResult represents the outcome of fetching a source.
type FetchResult struct {
	Name      string
	LocalPath string
	Commit    string
	Files     []string
}
