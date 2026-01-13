package transform

import (
	"bytes"
	"fmt"
	"strings"
	"text/template"

	"github.com/srnnkls/phora/internal/artifact"
	"github.com/srnnkls/phora/internal/reference"
)

type ReferenceConfig struct {
	Output string
}

type Transformer struct {
	Variables  map[string]string
	Mappings   map[string]string
	Keys       map[string]string
	Values     map[string]map[string]string
	References map[string]ReferenceConfig
	Tools      map[string]string
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

func ApplyValueMappings(fm map[string]any, values map[string]map[string]string) map[string]any {
	if values == nil {
		result := make(map[string]any)
		for k, v := range fm {
			result[k] = v
		}
		return result
	}

	result := make(map[string]any)
	for k, v := range fm {
		if valueMap, ok := values[k]; ok {
			if str, isStr := v.(string); isStr {
				if mapped, found := valueMap[str]; found {
					result[k] = mapped
				} else {
					result[k] = v
				}
			} else {
				result[k] = v
			}
		} else {
			result[k] = v
		}
	}

	return result
}

func (t *Transformer) Transform(art *artifact.Artifact) (*artifact.Artifact, error) {
	result := &artifact.Artifact{
		Name:        art.Name,
		Namespace:   art.Namespace,
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

	keyMappings := t.Mappings
	if t.Keys != nil {
		keyMappings = t.Keys
	}
	result.Frontmatter = ApplyMappings(result.Frontmatter, keyMappings)

	result.Frontmatter = ApplyValueMappings(result.Frontmatter, t.Values)

	body, err := ExecuteTemplate(art.Body, t.Variables)
	if err != nil {
		return nil, fmt.Errorf("transform body: %w", err)
	}

	body = t.transformReferences(body)

	result.Body = body

	return result, nil
}

func (t *Transformer) transformReferences(body string) string {
	refs := reference.Parse(body)
	if len(refs) == 0 {
		return body
	}

	for _, ref := range refs {
		var replacement string

		if ref.Type == reference.TypeTool {
			if mapped, ok := t.Tools[ref.Name]; ok {
				replacement = "`" + mapped + "`"
			}
		} else {
			refConfig, ok := t.References[ref.Type.String()]
			if ok {
				output, err := t.executeReferenceTemplate(refConfig.Output, ref)
				if err == nil {
					replacement = wrapOutput(output)
				}
			}
		}

		if replacement != "" {
			body = strings.Replace(body, "`"+ref.Raw+"`", replacement, 1)
		}
	}

	return body
}

func wrapOutput(output string) string {
	if strings.ContainsAny(output, "*[]") {
		return output
	}
	return "`" + output + "`"
}

func (t *Transformer) executeReferenceTemplate(tmpl string, ref reference.Reference) (string, error) {
	data := map[string]string{
		"Name": ref.Name,
		"Type": ref.Type.String(),
		"Raw":  ref.Raw,
	}

	parsed, err := template.New("ref").Parse(tmpl)
	if err != nil {
		return "", err
	}

	var buf bytes.Buffer
	if err := parsed.Execute(&buf, data); err != nil {
		return "", err
	}

	return buf.String(), nil
}
