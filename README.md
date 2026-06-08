# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

Phora is a git-based artifact package manager. It mirrors git repositories, picks
out the top-level directories you want (**artifacts**), and projects them into
local **target** directories — pinned to exact commits, verifiable by content hash,
and recoverable after interruption.

Use it to distribute shared config, editor setups, prompt/skill bundles, or any
directory-shaped payload from one or more repos into the places on disk that
consume them.

## Installation

```bash
cargo install --path .
# or, during development:
mise run build      # cargo build
```

Requires a Rust toolchain (edition 2024).

## Concepts

- **Source** — a git repository, pinned by `branch`, `tag`, or `rev`. The
  top-level directories under its `root` are its artifacts.
- **Artifact** — one top-level directory in a source (dotfiles are skipped). Glob
  `include`/`exclude` rules select which artifacts and which files within them ship.
- **Target** — a local directory artifacts are projected into, with a chosen
  layout. A target may draw from all sources or a named subset.
- **Lock** — `phora.lock` pins each source to a resolved commit so syncs are
  reproducible (`phora.local.toml` gets a companion `phora.local.lock`).
  `phora update` bumps it.
- **Registry** — per-project state under `~/.phora` recording what was deployed
  where (commit + content digest), so phora can detect drift, conflicts, and
  orphans. Bare mirrors live under `~/.phora/git`.

## Usage

```bash
# Add a source (shorthand expands via host templates)
phora add owner/repo --name myconfigs --branch main --root configs
phora add https://github.com/me/dotfiles.git
phora add git@github.com:me/dotfiles.git --tag v1.2

# Fetch sources, resolve commits, project artifacts into targets
phora sync
phora sync --prune          # also remove artifacts no longer selected
phora sync --force          # overwrite locally-modified files without prompting

# Re-resolve to the latest commit, then sync
phora update                # all sources
phora update myconfigs      # one source

# Inspect state
phora list                  # per-target deployment status
phora verify                # re-hash deployed files, exit non-zero on mismatch
phora where --source loqui  # reverse-lookup registry (by source/artifact/commit/digest)

# Stop managing an artifact but keep its files on disk
phora eject <artifact> --source <source> --target <target>
phora uneject <artifact> --source <source> --target <target>

# Maintenance / debugging
phora rebuild-registry      # reconstruct registry from lock + on-disk targets
phora check-match --source <source> <path>   # debug include/exclude matching
```

### Conflicts

When `sync` finds a target file that was modified outside phora, or a foreign file
where an artifact wants to land, it prompts (on a TTY):

```
[s]kip / [o]verwrite / [e]ject / [a]bort
```

Non-interactive runs skip such files unless `--force` is given.

## Configuration

Phora reads `phora.toml` from the working directory, optionally overlaid by
`phora.local.toml` (same schema; local values win per-key). See
[`phora.example.toml`](phora.example.toml) for a complete example.

```toml
version = 1

[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"
auth = { type = "token", env = "GITHUB_TOKEN" }

[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"          # or tag = "...", or rev = "<sha>" (pick one)
root = "modules"         # repo subdirectory to treat as the artifact root
include = ["editor"]     # optional artifact/path globs
exclude = ["**/*.bak"]

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]   # omit to draw from every source
layout = "flat"          # "flat" | "by-source" | { type = "prefixed", separator = "-" }
```

**Hosts** provide URL templates (`{owner}`, `{repo}`) for `add` shorthands and
auth. `github` and `gitlab` are built in. Auth is either
`{ type = "token", env = "VAR" }` or `{ type = "ssh", key = "~/.ssh/key" }`.

**Source flags:** `allow_symlinks`, `allow_submodules` (both default off),
`preserve_executable` (default on).

**Layouts** decide how an artifact `a` from source `s` is placed in a target:

| Layout                          | Path        |
| ------------------------------- | ----------- |
| `flat` (default)                | `a`         |
| `by-source`                     | `s/a`       |
| `{ type = "prefixed", sep="-" }`| `s-a`       |

## Development

```bash
mise run check     # clippy (pedantic, -D warnings) + rustfmt --check + tests
mise run test      # cargo test
mise run fmt       # cargo fmt
```
