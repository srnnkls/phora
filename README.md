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

# Worktree includes (per-linked-worktree files)
phora worktree apply                          # materialize includes in the current worktree
phora worktree import-legacy .worktreeinclude # migrate a git-worktreeinclude manifest

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
`preserve_executable` (default on), `deploy` (`"copy"` | `"link"`, default
`"copy"`; `"link"` is local-overlay-only — see [Link mode](#link-mode-local-development)).

**Layouts** decide how an artifact `a` from source `s` is placed in a target:

| Layout                          | Path        |
| ------------------------------- | ----------- |
| `flat` (default)                | `a`         |
| `by-source`                     | `s/a`       |
| `{ type = "prefixed", sep="-" }`| `s-a`       |

### Link mode (local development)

By default `deploy = "copy"` materializes a reflink-style copy of each artifact
from the committed git ODB — point-in-time, content-hashed, verifiable. For a
tight dev loop, `deploy = "link"` instead **symlinks** the artifact destination
at the source's live working tree (`<source path>/<root>/<artifact>`, absolute).
Uncommitted edits in the checkout are visible through the target immediately, with
no re-sync.

Two guardrails apply:

- **Local overlay only.** `deploy = "link"` is honored only in `phora.local.toml`.
  Setting it in the committed `phora.toml` is a config error that names the source.
  Keep it out of shared config.
- **Local path only.** A link source's `git` must be a local filesystem path
  (absolute, or an existing relative path). `deploy = "link"` on a remote URL is a
  config error.

Linked artifacts sit **outside the integrity model**: their registry record carries
a `linked` marker and no per-file hashes. `phora verify` skips them, drift detection
never reports them modified or foreign, `phora list` shows them as `linked`, and
`phora rebuild-registry` reconstructs the marker without hashing. `--prune` removes
an orphaned linked artifact by deleting the symlink only. If the working-tree target
is deleted or renamed the link reads as missing and is redeployed on the next sync.
Switching a source between `link` and `copy` replaces the destination on the next
sync (symlink ⇄ materialized copy, with full integrity restored on `copy`). If a
symlink cannot be created (e.g. on Windows without the privilege), phora warns,
skips that artifact, and continues the rest of the sync.

```toml
# phora.local.toml — overlays phora.toml, never committed.
# Override the `loqui` source onto a local checkout and live-link it.
[sources.loqui]
git = "/home/me/dev/loqui"   # local path; the live working tree
deploy = "link"
```

## Worktree includes

Some files belong in *every* `git worktree` of a repo but aren't committed —
`.envrc`, local tool config, secret overlays, large vendored submodules. The
`[worktree]` section lists what each newly checked-out worktree should carry over
from the primary worktree.

```toml
version = 1

[[worktree.includes]]
path = ".envrc"               # mode omitted -> "symlink" (default)

[[worktree.includes]]
path = "secrets/local.env"
mode = "copy"                 # an independent copy per worktree

[[worktree.includes]]
path = "vendor/lib"           # a gitlink (submodule)
mode = "submodule-walk"
```

`path` is a worktree-relative path (no absolute, `..`, or `.` components).
`mode` is one of:

| Mode             | Effect                                                          |
| ---------------- | -------------------------------------------------------------- |
| `symlink` (default) | a symlink pointing at the primary worktree's copy           |
| `copy`           | an independent copy taken from the primary worktree            |
| `submodule-walk` | per-leaf symlinks into the primary's submodule worktree        |

The manifest lives in the committed `phora.toml`, optionally overlaid by an
uncommitted `phora.local.toml` (same merge rules as the rest of the config —
local replaces the base `includes` array wholesale). Both are committed/placed
alongside the repo, so a freshly checked-out worktree has the manifest available
at apply time.

**Submodules.** When `path` is a gitlink:

- `symlink` (or the default) places a single directory symlink at the primary
  worktree's checked-out submodule.
- `submodule-walk` symlinks each leaf inside the submodule individually,
  skipping `.git`, which keeps the nested worktree usable in tools that refuse
  to descend through a symlinked submodule root.
- `copy` on a gitlink is unsupported and the include is skipped.

### `phora worktree apply`

Run inside a linked worktree, `phora worktree apply` materializes the configured
includes from the primary worktree. It is meant to run automatically from the
repo's `post-checkout` hook. If you are migrating from `git-worktreeinclude`,
swap the hook command and drop the old manifest:

```diff
 # .git/hooks/post-checkout
-git-worktreeinclude apply
+phora worktree apply
```

```bash
rm .worktreeinclude   # the legacy manifest is replaced by [worktree] in phora.toml
```

Behavior:

- **Tracked paths are refused** — an include that names a path git already
  tracks is rejected, so apply never shadows committed content.
- **Missing primary sources and placement failures warn and continue** — a
  missing source in the primary, or a placement that fails (e.g. a symlink that
  cannot be created on Windows without the privilege), is reported and skipped;
  the remaining includes are still applied.
- **No-op in the primary** — running apply in the primary worktree does nothing.

### `phora worktree import-legacy`

`phora worktree import-legacy <.worktreeinclude>` is a one-shot migration aid: it
reads a legacy `git-worktreeinclude` manifest and prints the equivalent
`[worktree]` config to stdout. Lines that can't map to an explicit literal
include — globs, negations (`!`), unsafe paths, or a `submodule-walk` without
`symlink` — are reported on stderr and left out, so the printed config always
re-parses cleanly.

The output is a standalone `version = 1` + `[worktree]` snippet — review it, then
merge the `[worktree]` section into your existing `phora.toml` by hand. Do not
append it blindly, or you will duplicate `version` and other keys.

```bash
phora worktree import-legacy .worktreeinclude > worktree-includes.toml
```

## Development

```bash
mise run check     # clippy (pedantic, -D warnings) + rustfmt --check + tests
mise run test      # cargo test
mise run fmt       # cargo fmt
```
