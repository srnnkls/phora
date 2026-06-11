# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

Phora is a git-based artifact package manager. It mirrors git repositories — or
imports plain-https resources (tarballs, zips, single files) as content-addressed
synthetic commits — picks out the top-level directories you want (**artifacts**),
and projects them into local **target** directories, pinned to exact commits,
verifiable by content hash, and recoverable after interruption.

Use it to distribute shared config, editor setups, prompt/skill bundles, or release
assets from one or more repos (or URLs) into the places on disk that consume them.

## Installation

```bash
cargo install --path .
# or, during development:
mise run build      # cargo build
```

Requires a Rust toolchain (edition 2024).

## Concepts

- **Source** — a git repository, pinned by `branch`, `tag`, or `rev`. The
  top-level directories under its `root` are its artifacts. Declare its remote as
  a forge (`host` + `repo`), a local path (`path = "/dir"`), a literal git URL
  (`git = "…"`), or a downloadable resource (`url = "https://…"`).
- **Artifact** — one top-level directory in a source (dotfiles are skipped). Glob
  `include`/`exclude` rules select which artifacts and which files within them ship.
- **Target** — a local directory artifacts are projected into, with a chosen
  layout. A target draws from its explicit `sources` allow-list.
- **Lock** — `phora.lock` pins each source to a resolved commit so syncs are
  reproducible (`phora.local.toml` gets a companion `phora.local.lock`).
  `phora update` bumps it.
- **Registry** — per-project state under `~/.phora` recording what was deployed
  where (commit + content digest), so phora can detect drift, conflicts, and
  orphans. Bare mirrors live under `~/.phora/git`.

## Usage

```bash
# Add a source. Shorthands persist as a forge source (host + repo), not an expanded URL.
phora add owner/repo --name myconfigs --branch main --root configs  # -> host = "github"
phora add github:srnnkls/tropos             # colon alias -> host = "github"
phora add gitlab:group/repo                 # any built-in forge (alias caps at owner/repo)
phora add github.com/me/dotfiles            # domain shorthand -> host = "github"
phora add https://github.com/me/dotfiles.git  # scheme/scp URLs stay literal (git = "…")
phora add git@github.com:me/dotfiles.git --tag v1.2
# Deep GitLab subgroups go in the config `repo` field (repo = "group/sub/proj"),
# not the colon alias (segments past owner/repo become `root`).

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

### Sources, targets, and bindings

`source` and `target` group the registry commands; `bind`/`unbind` edit which
sources a target deploys. `add`/`rm` are top-level sugar over the source namespace.

```bash
# Sources (`add` is identical to `source add`)
phora source add owner/repo --name myconfigs --branch main
phora source list                  # name, resolved remote, refspec
phora source show myconfigs        # effective config + targets that deploy it
phora source rm myconfigs          # also scrubs it from every target's `sources`
phora rm myconfigs                 # alias for `source rm`

# Targets (--path required; --layout takes flat | by-source | prefixed)
phora target add neovim --path ~/.config/nvim --layout by-source
phora target list                  # name, path, source-resolution mode
phora target show neovim           # effective config + resolved sources + state
phora target rm neovim             # warns if the registry still has deployed artifacts

# Bindings — edit a target's `sources` list
phora bind dotfiles loqui --to neovim     # add sources to neovim's list
phora unbind loqui --from neovim          # remove; emptying it deploys nothing
```

`--to`/`--from` name the target an edge attaches to. `phora add <url> --to T1 --to T2`
adds the source then binds it to each target atomically — the whole desugar is
applied to one config-text string and written once, so a failure leaves nothing
behind. A `--to` target that does not exist prompts to create it (flat layout,
path `./T`) on an interactive terminal, and errors with a `phora target add` hint
off a TTY. `--local` on a mutating command writes `phora.local.toml` instead of
`phora.toml`; `source rm`/`rm` take no `--local`, since their scrub spans both
files.

A bare `phora add <url>` (no `--to`) deploys into the project: it ensures
`[targets.default]` (path `.`, flat layout) and binds the source into it. Set
`[defaults] auto_target = false` to opt out — then a bare `add` only declares the
source, and it deploys nowhere until bound. `--to` always routes to exactly the
named target(s) and never touches `[targets.default]`.

`bind` onto a target with no `sources` key creates the list with the bound
source(s); the target deploys exactly its listed sources (nothing until bound).

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
# protocol = "ssh"         # global default for forge sources (default https)

# [defaults]
# auto_target = true       # bare `add` populates [targets.default] (default true)

[hosts.github]
auth = { type = "token", env = "GITHUB_TOKEN" }   # remote is built in; just add auth

[sources.dotfiles]
host = "github"          # forge remote: host + repo (or use git = "…" for a literal URL)
repo = "me/dotfiles"
branch = "main"          # or tag = "...", or rev = "<sha>" (pick one)
root = "modules"         # repo subdirectory to treat as the artifact root
include = ["editor"]     # optional artifact/path globs
exclude = ["**/*.bak"]

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]   # the allow-list of sources this target deploys
layout = "flat"          # "flat" | "by-source" | { type = "prefixed", separator = "-" }
```

**Target sources** are an explicit allow-list: `["a", "b"]` deploys those two,
`[]` (or an omitted `sources` key) deploys nothing. Edit the list with `phora
bind`/`phora unbind` rather than by hand; `bind` onto a target with no `sources`
key creates it.

**`[defaults] auto_target`** (bool, default `true`) controls bare-`add` DX: when
on, `phora add <url>` without `--to` ensures `[targets.default]` (path `.`, flat)
and binds the source into it; set it `false` to make bare `add` declare-only.
`--to` is unaffected — it always routes to the named target(s).

**Hosts** supply remote templates and auth. `github`, `gitlab`, `codeberg`,
`sr.ht`, and `bitbucket` ship built in (both https and ssh); a `[hosts.X]` block
adds a new forge or overrides a built-in's `remote`/`auth`. Auth is either
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

### Source kinds

A source declares its remote in exactly one kind — never more than one:

- **Forge:** `host = "<alias>"` + `repo = "<owner/repo>"`, resolved at sync time
  from the host's `remote` template. `host` may be omitted when `repo` is set, in
  which case it defaults to `github` (`repo = "owner/repo"` is github shorthand).
- **Local:** `path = "<dir-or-file>"`, a filesystem path used verbatim as the
  remote — exactly like a `git = "/abs/local"` URL.
- **Literal:** `git = "<url>"`, any https, `ssh://`, or scp-style (`git@host:path`)
  remote.
- **Url:** `url = "https://…"`, a downloadable resource (see below).

```toml
[sources.tropos]
host = "github"          # built in; omit to default to github
repo = "srnnkls/tropos"
branch = "main"

[sources.internal]
host = "company"         # defined in [hosts.company]
repo = "team/sub/proj"   # nested paths are fine
protocol = "ssh"         # per-source override (default is https)

[sources.scratch]
path = "~/dev/scratch"   # local checkout, used verbatim as the remote
branch = "main"
```

**Back-compat aliases.** `git = "/abs/local"` still declares a local source.
`host` + `path` (forge owner/repo) is a deprecated alias for `host` + `repo`.

> **Breaking change:** a bare `path = "owner/repo"` (no host) now means a LOCAL
> path, not a github forge source. The github shorthand moved to bare
> `repo = "owner/repo"`.

A host's `remote` is either a single template string (https) or a
`{ https = "…", ssh = "…" }` table. Templates fill three placeholders:

| Placeholder | Value                                              |
| ----------- | -------------------------------------------------- |
| `{path}`    | the source's `repo` (owner/repo), verbatim         |
| `{owner}`   | the first `/`-segment of `repo`                     |
| `{repo}`    | the remainder (so `{owner}/{repo}` ≡ `{path}` at any depth — GitLab subgroups) |

```toml
[hosts.company]
remote = { https = "https://git.company.com/{path}.git", ssh = "git@git.company.com:{path}.git" }
```

**Built-in forges.** `github`, `gitlab`, `codeberg`, `sr.ht`, and `bitbucket`
ship as `remote` tables with both https and ssh shapes, so no template is needed
for them. A `[hosts.X]` block of the same name overrides the built-in's `remote`
or adds `auth`; changing a host's `remote` re-points every source on that host
with no per-source edits.

**Protocol.** `protocol = "https" | "ssh"` selects which template key a forge
source resolves through. It defaults to `https`, can be set globally at the top
level, and is overridable per source. Selecting `ssh` against a host whose
`remote` has no `ssh` key is a config error. (`protocol` is ignored for literal
`git` and local `path` sources.)

The forge and literal forms of one repo — and its https and ssh remotes —
share a single `~/.phora/git` mirror, so switching kind or protocol never
re-clones or refetches.

### Url sources

A `url = "https://…"` source is one of the four kinds (forge XOR local XOR git
XOR url). It downloads a resource and imports its contents as a source, then
discovers/exports/deploys exactly like a git source. `branch`, `tag`, `rev`, and
`root` have no meaning for a static resource and are config errors on a url source;
`include`/`exclude` still select files.

```toml
[sources.fzf-bin]
url = "https://github.com/junegunn/fzf/releases/download/0.55.0/fzf-0.55.0-linux_amd64.tar.gz"
digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
include = ["fzf"]
```

**Formats.** tar, tar.gz/tgz, and zip, detected by content (magic bytes); a
non-archive URL becomes a single file named from the URL basename.

**Auto-strip.** When an archive has exactly one top-level directory it is stripped
automatically, so version-stamped release tarballs (`fzf-0.55.0/…`) need no
per-version `root` — and `root` re-selection is unavailable on url sources anyway.
Only `include`/`exclude` apply; `root`/`branch`/`tag`/`rev` are config errors.

**Integrity.** An optional `digest = "<algo>:<hex>"` (`sha256:` or `blake3:`, 64
hex chars) is verified **before** extraction; a mismatch errors, naming the source
with expected vs actual.

**Determinism.** Content is imported as a content-addressed synthetic git commit
(fixed identity, fixed time, constant message), so identical bytes yield an
identical commit and no lock churn. The synthetic commit's time is fixed at epoch+1
(1 second), not epoch 0, since some filesystems (FAT32, HFS+) clamp a 0 mtime —
which would otherwise make `phora verify` report every url-sourced file as modified.
`phora sync` of unchanged content is a no-op; `phora update` (or `--force`)
re-downloads, and the lock advances only if the content changed. `phora verify`
re-hashes deployed files with the same guarantees as git sources.

**Out of scope (for now).** Auth for private assets and forge release-tag
resolution (latest tag → asset URL) are future work; v1 targets public URLs.

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
- **Local path only.** A link source must be a local source: `path = "/dir"` (or
  the `git = "/dir"` alias), a local filesystem path. `deploy = "link"` on a remote
  URL is a config error.

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
path = "/home/me/dev/loqui"  # local source; the live working tree
deploy = "link"
```

`phora add --local <path>` writes that overlay for you: it records
`path = "<abspath>"` for a local source in `phora.local.toml` (never `phora.toml`).
`phora add --symlink <path>` does the same and adds `deploy = "link"` to live-link it.

## Worktrees

A worktree is just a directory you run `phora sync` from; sync builds the managed
state there. It is cheap to re-run: an unchanged lock means no refetch.

Carrying ignored or local files (`.env`, editor settings, submodules) across
worktrees is out of scope — use [`git-worktreeinclude`](https://github.com/srnnkls/git-worktreeinclude)
for that. Migration: move any `[worktree].includes` entries into a
`.worktreeinclude` and drive them with `git-worktreeinclude`.

## Development

```bash
mise run check     # clippy (pedantic, -D warnings) + rustfmt --check + tests
mise run test      # cargo test
mise run fmt       # cargo fmt
```
