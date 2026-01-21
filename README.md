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
# Add artifacts from a repository
phora add owner/repo

# Add with options
phora add owner/repo --ref v1.0 --path skills --name my-skills --target .claude/skills

# Add from full URL
phora add https://github.com/company/shared/tree/main/artifacts

# Sync sources (uses locked SHA, detects drift)
phora sync

# Force sync (overwrite drifted files)
phora sync --force

# Update refs to latest (re-resolve branch/tag to SHA)
phora update
phora update my-source  # Update specific source
```

## Configuration

Phora uses `phora.toml` for configuration:

```toml
version = 1

# Source definitions (Cargo-style inline tables)
[sources]
skills = { git = "https://github.com/company/shared.git", tag = "v1.0", path = "skills", target = ".claude/skills" }
prompts = { git = "https://github.com/company/prompts.git", branch = "main" }

# Ref types (mutually exclusive):
#   branch = "main"      - tracks a branch (use `phora update` to get latest)
#   tag = "v1.0"         - pinned to tag
#   rev = "abc123"       - pinned to specific commit

# Optional filtering
# filtered = { git = "...", branch = "main", include = ["**/*.md"], exclude = ["drafts/*"] }

# Export configuration (for this repo as a source)
[manifest]
artifacts = ["skills", "commands", "agents"]
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

## Lock File

Phora maintains a `phora.lock` file tracking synced sources:

```toml
version = 1

[[sources]]
name = "skills"
repo = "company/shared"
ref = "v1.0"
sha = "a1b2c3d4..."        # Resolved commit SHA
digest = "8f3a2b..."       # Config hash for lazy sync
fetched_at = 2026-01-18T10:00:00Z

[[sources.files]]          # Per-file integrity
path = "skills/code-review.md"
sha256 = "9e8d7c..."
```

- `phora sync` uses locked SHA (no network check if digest matches)
- `phora update` re-resolves refs and updates lock
- Drift detection compares local files against locked hashes

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
