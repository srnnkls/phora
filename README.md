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

# Add artifacts from a repository
phora add owner/repo

# Add with options
phora add owner/repo --ref main --path skills --local

# Deploy artifacts to configured harnesses
phora deploy

# Deploy to specific harness
phora deploy --target claude

# Deploy with options
phora deploy --dry-run           # Preview changes
phora deploy --interactive       # Prompt for conflicts
phora deploy --skip              # Skip existing files
phora deploy --source ./custom   # Deploy from specific path
```

## Configuration

Phora uses `phora.toml` for configuration:

```toml
default_harnesses = ["claude"]
default_artifacts = ["skills", "commands", "agents"]

# Optional: Declare available artifacts in this repo
[manifest]
skills = ["code-review", "code-test"]
commands = ["build"]
agents = ["reviewer"]

# Optional: Configure artifact sources
[sources.mycompany]
repo = "mycompany/shared-skills"
ref = "main"
path = "artifacts"
# Artifacts become: mycompany.skill-name

[sources.personal]
repo = "username/dotfiles"
global = true  # Use bare names (no namespace prefix)

# Claude Code harness
[harness.claude]
path = "~/.claude"

[harness.claude.variables]
model_strong = "opus"
model_weak = "haiku"

# OpenCode harness
[harness.opencode]
path = "~/.config/opencode"
generate_commands_from_skills = true

# Map YAML keys (rename fields)
[harness.opencode.keys]
allowed_tools = "tools"

# Map tool names
[harness.opencode.tools]
bash = "Bash"
read = "Read"

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
- Tool name mappings (e.g., `bash` → `Bash`)
- Variable substitution (e.g., `{{model_strong}}`)
- Auto-generation of commands from user-invocable skills

## Namespaces

Phora uses namespaces to prevent conflicts when artifacts come from multiple sources:

- **Global sources** (with `global = true`) use bare artifact names: `skill-name`
- **Non-global sources** use namespaced names: `source-name.skill-name`

Example:
```toml
[sources.personal]
repo = "user/dotfiles"
global = true  # Artifacts: code-review, test-runner

[sources.company]
repo = "company/shared"
# Artifacts: company.deploy, company.review
```

When deployed, artifacts are stored with their full names:
```
~/.claude/skills/code-review/         # from global source
~/.claude/skills/company.deploy/      # from company source
```

## References

Phora transforms artifact references in backticks to match each harness's format:

| Symbol | Type | Source Format | Claude Code | OpenCode |
|--------|------|---------------|-------------|----------|
| `$` | Skill | `$skill-name` | `/skill-name` | `@skill-name` |
| `/` | Command | `/cmd-name` | `/cmd-name` | `/cmd-name` |
| `@` | Agent | `@agent-name` | `@agent-name` | `@agent-name` |
| `!` | Tool | `!tool-name` | `ToolName` | `tool_name` |

Example skill content:
```markdown
Use `$code-review` after writing code.
Run `/build` to compile the project.
Ask `@reviewer` for feedback.
Execute with `!bash` tool.
```

Transforms to (Claude Code):
```markdown
Use `/code-review` after writing code.
Run `/build` to compile the project.
Ask `@reviewer` for feedback.
Execute with `Bash` tool.
```
