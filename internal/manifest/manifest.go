package manifest

import (
	"os"
	"path/filepath"

	"github.com/srnnkls/tropos/internal/artifact"
	"gopkg.in/yaml.v3"
)

const (
	Dir      = ".tropos"
	FileName = "manifest.yaml"
)

func FilePath(rootDir string) string {
	return filepath.Join(rootDir, Dir, FileName)
}

type Manifest struct {
	Skills   []string `yaml:"skills,omitempty"`
	Commands []string `yaml:"commands,omitempty"`
	Agents   []string `yaml:"agents,omitempty"`
}

func Generate(rootDir string, artifactTypes []string) (*Manifest, error) {
	artifacts, err := artifact.Discover(rootDir, artifactTypes)
	if err != nil {
		return nil, err
	}

	m := &Manifest{}

	for _, art := range artifacts {
		switch art.Type {
		case artifact.TypeSkill:
			m.Skills = append(m.Skills, art.Name)
		case artifact.TypeCommand:
			m.Commands = append(m.Commands, art.Name)
		case artifact.TypeAgent:
			m.Agents = append(m.Agents, art.Name)
		}
	}

	return m, nil
}

func (m *Manifest) Write(path string) error {
	data, err := yaml.Marshal(m)
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0644)
}

func Load(data []byte) (*Manifest, error) {
	var m Manifest
	if err := yaml.Unmarshal(data, &m); err != nil {
		return nil, err
	}
	return &m, nil
}

func LoadFile(path string) (*Manifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return Load(data)
}
