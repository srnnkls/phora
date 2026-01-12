package transform

import (
	"bytes"
	"fmt"
	"text/template"

	"github.com/srnnkls/tropos/internal/artifact"
)

type Transformer struct {
	Variables map[string]string
	Mappings  map[string]string
}

func ExecuteTemplate(content string, vars map[string]string) (string, error) {
	tmpl, err := template.New("content").Parse(content)
	if err != nil {
		return "", fmt.Errorf("parse template: %w", err)
	}

	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, vars); err != nil {
		return "", fmt.Errorf("execute template: %w", err)
	}

	return buf.String(), nil
}

func ApplyMappings(fm map[string]any, mappings map[string]string) map[string]any {
	if mappings == nil {
		result := make(map[string]any)
		for k, v := range fm {
			result[k] = v
		}
		return result
	}

	result := make(map[string]any)
	for k, v := range fm {
		if newKey, ok := mappings[k]; ok {
			result[newKey] = v
		} else {
			result[k] = v
		}
	}

	return result
}

func (t *Transformer) Transform(art *artifact.Artifact) (*artifact.Artifact, error) {
	result := &artifact.Artifact{
		Name:        art.Name,
		Type:        art.Type,
		SourcePath:  art.SourcePath,
		IsDirectory: art.IsDirectory,
		Resources:   art.Resources,
		Frontmatter: make(map[string]any),
	}

	for k, v := range art.Frontmatter {
		if str, ok := v.(string); ok {
			transformed, err := ExecuteTemplate(str, t.Variables)
			if err != nil {
				return nil, fmt.Errorf("transform frontmatter %q: %w", k, err)
			}
			result.Frontmatter[k] = transformed
		} else {
			result.Frontmatter[k] = v
		}
	}

	result.Frontmatter = ApplyMappings(result.Frontmatter, t.Mappings)

	body, err := ExecuteTemplate(art.Body, t.Variables)
	if err != nil {
		return nil, fmt.Errorf("transform body: %w", err)
	}
	result.Body = body

	return result, nil
}
