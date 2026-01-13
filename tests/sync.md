# Phora Sync Tests

## Help

```scrut
$ phora --help
Phora syncs skills, commands, and agents across different AI coding assistant harnesses (Claude, OpenCode, Codex).

Usage:
  phora [command]

Available Commands:
  completion  Generate the autocompletion script for the specified shell
  help        Help about any command
  init        Initialize phora configuration
  install     Install artifacts from a repository
  sync        Sync artifacts to harnesses

Flags:
      --config string     Global config file (default "*") (glob)
      --data-dir string   Data directory for cloned repos (default "*") (glob)
  -h, --help              help for phora

Use "phora [command] --help" for more information about a command.
```

## Sync Help

```scrut
$ phora sync --help
Sync from sources to target harnesses with transformation

Usage:
  phora sync [flags]

Flags:
      --dry-run          Show what would be synced
      --force            Overwrite existing files
  -h, --help             help for sync
      --source strings   Source paths (default: current directory)
      --target strings   Target harnesses (default: all enabled)

Global Flags:
      --config string     Global config file (default "*") (glob)
      --data-dir string   Data directory for cloned repos (default "*") (glob)
```

## Sync Dry Run Shows Plan

```scrut
$ rm -rf /tmp/phora-test-claude /tmp/phora-test-opencode && cd "$TESTDIR/fixtures" && phora sync --dry-run --target claude 2>&1
Dry run - no files will be written
Would sync 4 artifact(s)
```

## Sync To Claude Target

```scrut
$ rm -rf /tmp/phora-test-claude && cd "$TESTDIR/fixtures" && phora sync --target claude 2>&1
Synced 4 artifact(s)
```

## List Claude Target Structure

```scrut
$ ls -R /tmp/phora-test-claude
agents
commands
skills

/tmp/phora-test-claude/agents:
tester

/tmp/phora-test-claude/agents/tester:
AGENT.md

/tmp/phora-test-claude/commands:
test.run

/tmp/phora-test-claude/commands/test.run:
COMMAND.md

/tmp/phora-test-claude/skills:
code-test
simple

/tmp/phora-test-claude/skills/code-test:
SKILL.md
reference

/tmp/phora-test-claude/skills/code-test/reference:
guide.md

/tmp/phora-test-claude/skills/simple:
SKILL.md
```

## Verify Claude Skill Output

```scrut
$ cat /tmp/phora-test-claude/skills/code-test/SKILL.md
---
allowed_tools:
  - read
  - write
  - bash
description: TDD workflow using opus
model: opus
name: code-test
user-invocable: true
---

# Test-Driven Development

Use opus for complex reasoning tasks.
Use haiku for simple validations.

## Workflow

1. Write failing test (RED)
2. Write minimal code (GREEN)
3. Refactor
```

## Verify Claude Resources Copied

```scrut
$ cat /tmp/phora-test-claude/skills/code-test/reference/guide.md
# TDD Best Practices

- Test behavior, not implementation
- One assertion per test
- Red, green, refactor
```

## Verify Claude Command Output

```scrut
$ cat /tmp/phora-test-claude/commands/test.run/COMMAND.md
---
description: Run tests with haiku
name: test.run
---

# Run Tests

Execute test suite using haiku for speed.
```

## Verify Claude Agent Output

```scrut
$ cat /tmp/phora-test-claude/agents/tester/AGENT.md
---
description: Test execution agent
model: haiku
name: tester
---

# Tester Agent

Runs tests and reports results.
```

## Verify Claude Simple Skill

```scrut
$ cat /tmp/phora-test-claude/skills/simple/SKILL.md
---
description: A simple skill without resources
name: simple
---

# Simple Skill

Just a basic skill.
```

## Sync To OpenCode Target

```scrut
$ rm -rf /tmp/phora-test-opencode && cd "$TESTDIR/fixtures" && phora sync --target opencode 2>&1
Synced 5 artifact(s)
Generated 1 command(s) from user-invocable skills
```

## List OpenCode Target Structure (Nested)

```scrut
$ ls -R /tmp/phora-test-opencode
agents
commands
skills

/tmp/phora-test-opencode/agents:
tester

/tmp/phora-test-opencode/agents/tester:
AGENT.md

/tmp/phora-test-opencode/commands:
code-test
test.run

/tmp/phora-test-opencode/commands/code-test:
COMMAND.md

/tmp/phora-test-opencode/commands/test.run:
COMMAND.md

/tmp/phora-test-opencode/skills:
code-test
simple

/tmp/phora-test-opencode/skills/code-test:
SKILL.md
reference

/tmp/phora-test-opencode/skills/code-test/reference:
guide.md

/tmp/phora-test-opencode/skills/simple:
SKILL.md
```

## Verify OpenCode Key Mapping

```scrut
$ cat /tmp/phora-test-opencode/skills/code-test/SKILL.md
---
description: TDD workflow using anthropic/claude-sonnet-4-5
model: anthropic/claude-sonnet-4-5
name: code-test
tools:
  - read
  - write
  - bash
user-invocable: true
---

# Test-Driven Development

Use anthropic/claude-sonnet-4-5 for complex reasoning tasks.
Use anthropic/claude-haiku-4-5 for simple validations.

## Workflow

1. Write failing test (RED)
2. Write minimal code (GREEN)
3. Refactor
```

## Verify Generated Command

```scrut
$ cat /tmp/phora-test-opencode/commands/code-test/COMMAND.md
---
description: TDD workflow using anthropic/claude-sonnet-4-5
name: code-test
---

Invoke skill: code-test
```

## Verify OpenCode Agent

```scrut
$ cat /tmp/phora-test-opencode/agents/tester/AGENT.md
---
description: Test execution agent
model: anthropic/claude-haiku-4-5
name: tester
---

# Tester Agent

Runs tests and reports results.
```

## Verify OpenCode Simple Skill

```scrut
$ cat /tmp/phora-test-opencode/skills/simple/SKILL.md
---
description: A simple skill without resources
name: simple
---

# Simple Skill

Just a basic skill.
```

## Verify OpenCode Command

```scrut
$ cat /tmp/phora-test-opencode/commands/test.run/COMMAND.md
---
description: Run tests with anthropic/claude-haiku-4-5
name: test.run
---

# Run Tests

Execute test suite using anthropic/claude-haiku-4-5 for speed.
```

## Verify OpenCode Resources Copied

```scrut
$ cat /tmp/phora-test-opencode/skills/code-test/reference/guide.md
# TDD Best Practices

- Test behavior, not implementation
- One assertion per test
- Red, green, refactor
```

## Force Overwrite

```scrut
$ cd "$TESTDIR/fixtures" && phora sync --target claude --force 2>&1
Synced 4 artifact(s)
```

## Init Creates Config

```scrut
$ TMPINIT=$(mktemp -d) && mkdir -p "$TMPINIT/skills/myskill" && printf '%s\n' '---' 'name: myskill' '---' '# My Skill' > "$TMPINIT/skills/myskill/SKILL.md" && cd "$TMPINIT" && phora init && rm -rf "$TMPINIT"
Created phora.toml
  Skills:   1
  Commands: 0
  Agents:   0
```
