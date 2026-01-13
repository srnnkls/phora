# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

A package manager and multiplexer for agent artifacts—skills, commands, and agents that work across AI coding assistants (Claude Code, OpenCode, Codex).

Phora carries your artifacts across harnesses. Install once, sync everywhere.

## Installation

```bash
go install github.com/srnnkls/phora/cmd/phora@latest
```

## Usage

```bash
# Initialize config in current directory
phora init

# Install artifacts from a repository
phora install owner/repo

# Sync artifacts to configured harnesses
phora sync

# Sync to specific harness
phora sync --target claude

# Dry run
phora sync --dry-run
```

## Configuration

Phora uses `phora.toml` for configuration:

```toml
default_harnesses = ["claude"]
default_artifacts = ["skills", "commands", "agents"]

[harness.claude]
path = "~/.claude"

[harness.opencode]
path = "~/.opencode"
generate_commands_from_skills = true

[harness.opencode.mappings]
allowed_tools = "tools"

[harness.opencode.variables]
model_strong = "anthropic/claude-sonnet-4-5"
model_weak = "anthropic/claude-haiku-4-5"
```

## Harnesses

Phora supports multiple AI coding assistant configurations:

- **Claude Code** — Anthropic's CLI
- **OpenCode** — Open-source alternative
- **Codex** — Custom harness support

Each harness can have:
- Custom output paths
- Key mappings (e.g., `allowed_tools` → `tools`)
- Variable substitution (e.g., `{{model_strong}}`)
- Auto-generation of commands from user-invocable skills
