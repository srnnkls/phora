# Phora Tests

## Help

```scrut
$ phora --help
Phora syncs skills, commands, and agents across different AI coding assistant harnesses (Claude, OpenCode, Codex).

Usage:
  phora [command]

Available Commands:
  add         Add artifacts from a repository
  completion  Generate the autocompletion script for the specified shell
  config      Manage phora configuration
  deploy      Deploy artifacts to harnesses
  help        Help about any command
  init        Initialize phora config with manifest
  list        List configured sources
  sync        Fetch all configured sources
  update      Fetch all configured sources (alias for sync)

Flags:
      --config string     Global config file (default "*") (glob)
      --data-dir string   Data directory for cloned repos (default "*") (glob)
  -h, --help              help for phora

Use "phora [command] --help" for more information about a command.
```

## Sync Help

```scrut
$ phora sync --help
Fetch all sources defined in phora.toml

Usage:
  phora sync [flags]

Flags:
  -h, --help   help for sync

Global Flags:
      --config string     Global config file (default "*") (glob)
      --data-dir string   Data directory for cloned repos (default "*") (glob)
```

## Update Help

```scrut
$ phora update --help
Fetch all sources defined in phora.toml

Usage:
  phora update [flags]

Flags:
  -h, --help   help for update

Global Flags:
      --config string     Global config file (default "*") (glob)
      --data-dir string   Data directory for cloned repos (default "*") (glob)
```

## List Shows No Sources

```scrut
$ cd "$TESTDIR/fixtures" && phora list
No sources configured
```

## Config Without Version Fails

```scrut
$ TMPDIR=$(mktemp -d) && echo '[manifest]' > "$TMPDIR/phora.toml" && echo 'artifacts = ["skills"]' >> "$TMPDIR/phora.toml" && cd "$TMPDIR" && phora sync 2>&1 | head -1; rm -rf "$TMPDIR"
Error: load config: missing required field: version
```

## Config With Version 1 Valid

```scrut
$ cd "$TESTDIR/fixtures" && phora sync 2>&1
No sources configured
```

## List Shows Configured Sources

```scrut
$ TMPDIR=$(mktemp -d) && printf 'version = 1\n\n[sources]\nskills = { git = "https://github.com/company/shared.git", tag = "v1.0", target = ".claude/skills" }\nprompts = { git = "https://github.com/company/prompts.git", branch = "main" }\n' > "$TMPDIR/phora.toml" && cd "$TMPDIR" && phora list 2>&1; rm -rf "$TMPDIR"
Sources:
  skills:  (ref: main)
  prompts:  (ref: main)
```

## Add Help

```scrut
$ phora add --help
Add artifacts from a repository to your project.

Supported URL formats:
  - owner/repo              GitHub shorthand
  - owner/repo/path         GitHub shorthand with subdirectory
  - https://github.com/owner/repo/tree/ref/path
  - gitlab.com/owner/repo/path

The command parses the URL, clones the repository, and syncs artifacts
to the configured harnesses. The source is saved to phora.toml.

Flags:
  --ref      Branch, tag, or commit (required for refs containing "/")
  --path     Subdirectory within repo containing artifacts
  --harness  Target harnesses (default: all enabled)
  --global   Save source to global config instead of local phora.toml
  --force    Overwrite existing unmanaged files

Usage:
  phora add <url> [flags]

Examples:
  # Add from GitHub shorthand
  phora add srnnkls/dotfiles/.claude/skills

  # Add with explicit ref (required for feature/xyz branches)
  phora add srnnkls/dotfiles --ref feature/new-skills --path .claude/skills

  # Add from full GitHub URL
  phora add https://github.com/company/shared/tree/v1.0/artifacts/skills

  # Save to global config
  phora add company/shared --global

Flags:
  -f, --force             Overwrite existing unmanaged files
  -g, --global            Save source to global config instead of local phora.toml
      --harness strings   Target harnesses (default: all enabled)
  -h, --help              help for add
      --path string       Subdirectory within repo containing artifacts
      --ref string        Branch, tag, or commit (default "main")

Global Flags:
      --config string     Global config file (default "*") (glob)
      --data-dir string   Data directory for cloned repos (default "*") (glob)
```

## Sync Without Sources Shows Message

```scrut
$ TMPDIR=$(mktemp -d) && printf 'version = 1\n' > "$TMPDIR/phora.toml" && cd "$TMPDIR" && phora sync 2>&1; rm -rf "$TMPDIR"
No sources configured
```

## Sync With Invalid Sources Shows Error

```scrut
$ TMPDIR=$(mktemp -d) && printf 'version = 1\n\n[sources]\ntest = { git = "https://github.com/nonexistent/repo.git", branch = "main" }\n' > "$TMPDIR/phora.toml" && cd "$TMPDIR" && phora sync 2>&1 | head -1; rm -rf "$TMPDIR"
Error: fetch sources: repo is empty
```

## Config Validates Sources Have Git

```scrut
$ TMPDIR=$(mktemp -d) && printf 'version = 1\n\n[sources]\ntest = { branch = "main" }\n' > "$TMPDIR/phora.toml" && cd "$TMPDIR" && phora sync 2>&1 | head -1; rm -rf "$TMPDIR"
Error: fetch sources: repo is empty
```
