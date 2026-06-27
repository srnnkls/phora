# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

Phora is a git-based artifact package manager and multiplexer. It mirrors git repositories — or
imports plain-https resources (tarballs, zips, single files) as content-addressed
synthetic commits — picks out the paths you want (its *offer*) and projects them
into local *target* directories, pinned to exact commits, verifiable by content
hash, and recoverable after interruption.

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

- *Source* — provenance: where bytes come from, pinned by `branch`, `tag`, or
  `rev`. A source owns its *offer*: `root` re-anchors the slice and
  `include`/`exclude` (gitignore syntax) compose `include − exclude` into the leaf
  set the source publishes. Declare its remote as a forge (`host` + `repo`), a local
  path (`path = "/dir"`), a literal git URL (`git = "…"`), or a downloadable resource
  (`url = "https://…"`).
- *Offer* — the leaf set a source publishes, named relative to its `root`. With no
  `include` the offer is everything in the source minus VCS metadata (`.git/`); an
  `include` narrows it and `exclude` prunes it (exclude wins; no `!` re-inclusion).
  Dotfiles match like any other path.
- *Artifact* — one offered leaf, identified by its full offered path. The unit a
  target takes, renames, and deploys.
- *Target* — a local directory artifacts are projected into, with a chosen
  layout. A target draws from its explicit `sources` allow-list.
- *Binding* — a target's link to a source. The source owns the offer; the binding
  owns the *take*: its `take` subsets and renames the offer for that target alone,
  and `collapse` controls how the taken set materializes. See [Bindings](#bindings).
- *Transitive dependency* — a source that is itself a phora project. Mark it
  `transitive = true`, import it into a target with `imports = [...]`, and its own
  `phora.toml` targets compose into your workspace under that target's path. See
  [Transitive dependencies](#transitive-dependencies).
- *Lock* — `phora.lock` pins each source to a resolved commit so syncs are
  reproducible (`phora.local.toml` gets a companion `phora.local.lock`).
  `phora update` bumps it.
- *Registry* — per-project state under the state root (`XDG_STATE_HOME` or, by
  default, `~/.local/state/phora` on Linux and `~/Library/Application Support/phora`
  on macOS) recording what was deployed where (commit + content digest), so phora can
  detect drift, conflicts, and orphans. Bare mirrors live under the cache root
  (`XDG_CACHE_HOME` or, by default, `~/.cache/phora` on Linux and
  `~/Library/Caches/phora` on macOS), in its `git/` subdirectory. See
  [State & locations](#state--locations).

The model splits cleanly into who-owns-what:

| Term     | Owner  | What it does                                                        |
| -------- | ------ | ------------------------------------------------------------------- |
| offer    | source | the published leaf set: `include − exclude` (gitignore), under `root`; no `include` ⇒ everything minus `.git/` |
| take     | target | subsets and renames the offer per binding (literal / glob / `{ src = dest }`) |
| artifact | —      | one leaf, identified by its full offered path                       |
| collapse | target | how a taken set materializes: per-leaf, or one dir symlink/subtree  |

### State & locations

Phora keeps its shared state in two XDG-rooted trees:

| Root  | Holds                              | Override         | Linux default          | macOS default                         |
| ----- | ---------------------------------- | ---------------- | ---------------------- | ------------------------------------- |
| Cache | git mirrors (regenerable)          | `XDG_CACHE_HOME` | `~/.cache/phora`       | `~/Library/Caches/phora`              |
| State | registry (deploy journal, locks)   | `XDG_STATE_HOME` | `~/.local/state/phora` | `~/Library/Application Support/phora` |

A project may pin either root in `phora.toml` with a `[paths]` table — `cache` and
`state` are optional and independent — which makes project-local installs and
hermetic tests possible without exporting the XDG vars:

```toml
[paths]
cache = ".phora/cache"   # git mirrors live under <root>/git/
state = ".phora/state"   # registry, locks, and journal live under <root>/projects/
```

A configured path is itself the root: relative values resolve under the project
root, absolute values are used as-is, and no `phora` leaf is appended. Resolution
precedence is config, then the `XDG_*` env, then the platform default.

An `XDG_*` override is honored only when absolute (per the XDG spec); a relative
value is ignored and the platform default applies. macOS has no native state
directory, so the state root falls back to `~/Library/Application Support`.
`XDG_DATA_HOME` and `XDG_CONFIG_HOME` are intentionally unused: phora has no portable
data payload (the registry is machine-local, mirrors are regenerable) and no global
config root (config is project-local `phora.toml`). Neither tree is migrated — a
legacy `~/.phora` is abandoned; mirrors re-clone and the registry rebuilds on the
next sync.

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

# Bind sources to a target; --take subsets/renames the offer for that target
phora bind dotfiles --to neovim                          # bare binding, takes the whole offer
phora bind dotfiles --to neovim --as nvim --take nvim/**  # take just nvim/** under identity `nvim`
phora unbind nvim --from neovim                          # remove a binding by identity
# --root/--include/--exclude on `add` shape the SOURCE offer (source-owned), not a binding.
phora add me/dotfiles --to neovim --as nvim --root nvim

# Fetch sources, resolve commits, project artifacts into targets
phora sync
phora sync --prune          # also remove artifacts no longer selected
phora sync --force          # overwrite locally-modified files without prompting
phora sync --frozen         # refuse to fetch or re-resolve — every source must be pinned in the lock
phora sync --no-transitive-hooks  # deploy composed deps, but run none of their hooks

# Transitive (composed-dependency) hooks — inspect and approve before they run
phora trust                       # list every discovered composed-dep hook across all sources
phora trust tropos                # inspect tropos's hooks, approve interactively
phora trust tropos --list         # show tropos's hooks without approving anything
phora trust tropos --show <path>  # print a tropos dep file (or list a dir) at the pinned commit
phora trust tropos --revoke       # drop every approval recorded for tropos

# Re-resolve to the latest commit, then sync
phora update                # all sources
phora update myconfigs      # one source

# Inspect state
phora list                  # per-target deployment status
phora verify                # re-hash deployed files, exit non-zero on mismatch
phora where --source loqui  # reverse-lookup registry (by source/artifact/commit/digest)
phora preview               # dry-run: the full tree a sync would project (offline, from the lock)
phora preview --target home # one target;  --source <s> limits to one source
phora preview --files       # expand each artifact to its files
phora preview --json        # machine-readable plan

# Stop managing an artifact but keep its files on disk
phora eject <artifact> --source <source> --target <target>
phora uneject <artifact> --source <source> --target <target>

# Maintenance / debugging
phora rebuild-registry      # reconstruct registry from lock + on-disk targets
phora check-match --source <source> <path>   # debug include/exclude matching
phora explain <target> <source> [path]       # offline: which include/exclude offered a path, and how `take` resolves it
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

### Preview

`phora preview` is the dry-run projection view: per target, it shows each binding's
identity (the `[targets.<t>.sources]` table key, defaulting to the source name), the artifacts it selects,
and the destinations they'd land at under the target's layout — without writing
anything. Commits come from the lock and the tree from the mirror, with no network.
An unsynced source is annotated (`not locked`, `needs sync`, or `link working tree
gone`) rather than fetched, and the command still exits 0. Predicted flat-layout
collisions render as warnings. Where `check-match` is a single-path probe, preview
is the whole-tree view.

```
home
  dotfiles@a1b2c3d4 editor -> /home/me/deploy/editor
  dotfiles@a1b2c3d4 lint -> /home/me/deploy/lint
```

`--files` expands each artifact to the files it would deploy; `--json` emits the
same plan as a machine-readable document.

### Conflicts

When `sync` finds a target file that was modified outside phora, or a foreign file
where an artifact wants to land, it prompts (on a TTY):

```
[s]kip / [o]verwrite / [e]ject / [a]bort
```

Non-interactive runs skip such files unless `--force` is given.

### Hooks

Hooks run shell commands after a sync. A target's `on_change` fires once after a
sync that added or modified that target's artifacts (pure removals don't
trigger it — that's what the global `post_sync` escape hatch is for); the global
`[hooks] post_sync` runs after every sync. Hooks are declared only in
`phora.toml` / `phora.local.toml`.

```toml
[targets.neovim.hooks]
# a bare string runs under `sh -c`
on_change = "nvim --headless +'Lazy! sync' +qa"

[targets.editors.hooks]
# a table picks the shell; an array runs several in declared order (deduped)
on_change = [
  { run = "stylua .", shell = "bash -c" },
  "git -C ~/.config add -A",
]

[hooks]
post_sync = "notify-send 'phora synced'"   # runs every sync (when = "always")
```

A hook value is a command string, a `{ run = "...", shell = "..." }` table
(`shell` optional, default `sh -c`), or an array mixing both. Each hook sees
phora's full environment plus, for `on_change`:

| Variable              | Value                                             |
| --------------------- | ------------------------------------------------- |
| `PHORA_TARGET`        | the target name                                   |
| `PHORA_CHANGED`       | newline-separated deployed paths of changed artifacts |
| `PHORA_CHANGED_NAMES` | newline-separated artifact names                  |

Artifacts land on disk before the hook runs. Hook success is recorded, so a
no-op sync runs no `on_change`; a hook that exits non-zero is not recorded, makes
`phora sync` exit non-zero, leaves the deployed files in place, and re-fires on
the next sync. `phora sync --no-hooks` deploys without running any hook.

Each hook that ran is reported with its scope and status:

```
hook neovim#nvim --headless +'Lazy! sync' +qa#sh -c [on_change] `nvim --headless +'Lazy! sync' +qa` ok
sync complete
```

Trust boundary. Hooks come only from the consumer's config. A synced source
tree that happens to carry a hook-shaped `phora.toml` is inert content — it is
never read as config and never executes.

### Templating

Files can be rendered per-machine with [minijinja](https://docs.rs/minijinja)
before they deploy. A source file named `*.tmpl` is rendered and lands with the
suffix stripped (`motd.tmpl` → `motd`); every other file copies byte-for-byte.
Variables come from a flat `[vars]` table:

```toml
[vars]
greeting = "hello"
editor = "nvim"
```

```jinja
{# editor/motd.tmpl → deploys as editor/motd #}
{{ greeting }} from {{ editor }}
```

The `.tmpl` suffix is the opt-in by default; a refined binding can widen it to
arbitrary globs or turn it off:

```toml
[targets.editor.sources]
# render these paths too, in addition to *.tmpl:
wide = { source = "dotfiles", template = ["*.conf", "config/*"] }
# render nothing, even .tmpl files:
plain = { source = "dotfiles", template = false }
```

Rendering is strict: referencing an undefined variable aborts that artifact's
export — its sibling artifacts still deploy. `phora.local.toml` overrides vars
per key — keys it omits keep their base value — so each machine fills in its own:

```toml
# phora.local.toml — overlays phora.toml, never committed
[vars]
greeting = "hi from this laptop"
```

Integrity. Phora hashes the *rendered* bytes, so `phora verify` checks the
deployed output, not the template. The lock records *source* bytes only: two
machines with different vars produce byte-identical locks, keeping the integrity
check machine-independent. Editing a variable marks the affected artifacts
outdated, and the next `phora sync` re-renders and redeploys them — no new commit
needed. `phora preview --files` shows the deployed name and flags what renders:

```
home
  dotfiles@a1b2c3d4 editor -> /home/me/.config/editor
    motd (templated)
    static.txt
```

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
branch = "main"          # or tag = "...", or rev = "<sha>"; omit all to follow the repo's default branch
root = "modules"         # re-anchor the offer at this subdirectory
include = ["editor"]     # source-owned offer: include − exclude (gitignore)
exclude = ["**/*.bak"]

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]   # all-bare: a flat list of the sources this target deploys
layout = "flat"          # "flat" | "by-source" | { type = "prefixed", separator = "-" }

# a second target using the keyed-table form (key = identity, defaults to source name):
[targets.editor]
path = "~/.config/editor"

[targets.editor.sources]
nvim = { source = "dotfiles", take = ["nvim/**"] }
```

Target sources are an explicit allow-list: `["a", "b"]` deploys those two,
`[]` (or an omitted `sources` key) deploys nothing. Edit the list with `phora
bind`/`phora unbind` rather than by hand; `bind` onto a target with no `sources`
key creates it.

`[defaults] auto_target` (bool, default `true`) controls bare-`add` DX: when
on, `phora add <url>` without `--to` ensures `[targets.default]` (path `.`, flat)
and binds the source into it; set it `false` to make bare `add` declare-only.
`--to` is unaffected — it always routes to the named target(s).

Hosts supply remote templates and auth. `github`, `gitlab`, `codeberg`,
`sr.ht`, and `bitbucket` ship built in (both https and ssh); a `[hosts.X]` block
adds a new forge or overrides a built-in's `remote`/`auth`. Auth is either
`{ type = "token", env = "VAR" }` or `{ type = "ssh", key = "~/.ssh/key" }`.

Source flags: `allow_symlinks` (default off), `preserve_executable` (default on),
`deploy` (`"copy"` | `"link"`, default `"copy"`; `"link"` is local-overlay-only — see
[Link mode](#link-mode-local-development)).

Layouts decide how an artifact `a` from a binding `i` (its identity — the
`[targets.<t>.sources]` table key, defaulting to the source name) is placed in a target:

| Layout                          | Path        |
| ------------------------------- | ----------- |
| `flat` (default)                | `a`         |
| `by-source`                     | `i/a`       |
| `{ type = "prefixed", sep="-" }`| `i-a`       |

### Bindings

A target's `sources` says which sources it consumes — and, per source, how. Each
entry is a binding: the edge from a target to a source. The source owns the offer
(`root`/`include`/`exclude`); the binding owns the take — `take` subsets and renames
that offer for one target, without touching the source or any other target.

A target's `sources` takes one of two forms — never both at once:

- Flat list of bare names — `sources = ["dotfiles", "loqui"]`. Every source is
  consumed at its whole offer. This is the all-bare, zero-settings form (each
  element is equivalent to `name = {}`, which omits `take`).
- Keyed table — `[targets.<t>.sources]`, a map whose key is the binding
  identity and whose value is always a table refining that one binding. The
  key defaults to the source name; `source` is written only on divergence,
  when the identity differs from the source name. A bare entry inside a refined
  (keyed) target is `name = {}`. A binding may set `take`, `collapse`, `template`,
  and a per-target ref (`branch`/`tag`/`rev`); the offer scope itself
  (`root`/`include`/`exclude`) is not a binding key.

Take subsets and renames the offer. A binding's `take` is a list whose entries are:

- a literal leaf (a plain offered path, e.g. `"nvim/init.lua"`) — kept verbatim;
- a gitignore glob (any entry with `*`, `?`, `[`, `]`, or a trailing `/`, e.g.
  `"nvim/**"`) — expands over the offer set only, never widening it;
- a rename table `{ "src" = "dest" }` — the offered leaf `src` is consumed and
  emitted at `dest` instead (destructive: it does not also land at `src`).

A literal or rename `src` that is not in the offer is a hard error (a `take` may not
widen the offer; the diagnostic suggests the closest offered leaf). A glob that
matches nothing warns but does not fail. An omitted `take` takes the whole offer;
`take = []` takes nothing.

```toml
[targets.neovim.sources]
# take just the editor's tree, then rename one leaf as it lands:
nvim = { source = "dotfiles", take = ["nvim/**", { "nvim/init.lua" = "init.lua" }] }
```

Restriction. `take` (or any other refinement) on a binding backed by a `url` source
is a config error — a url source has no offer to subset. `branch`/`tag`/`rev`
on a binding backed by a `url` source or a `deploy = "link"` source are config
errors too — a url has no ref to resolve, and a link source live-links a working
tree rather than a pinned commit.

Binding scope is rejected. `root`, `include`, `exclude`, and `map` are no longer
binding keys: setting any of them on a `[targets.<t>.sources]` entry is a hard parse
error with a did-you-mean diagnostic — it redirects `root`/`include`/`exclude` to the
source offer (`[sources.<name>]`) and `map` to the target `take` rename form. This is
pre-alpha; there is no migration shim.

Identity (the table key). A binding's identity is the
`[targets.<t>.sources]` table key; it defaults to the source name and you write
`source` only when the identity diverges. The identity keys the registry artifact
and the `by-source` and `prefixed` layout labels, and is structurally unique because
TOML keys are unique. To feed one source into one target as two slices, give each a
distinct key, each `take`-ing a different subtree of the same `source`. A genuine
destination clash between bindings is caught at sync as a collision. Bindings resolve
in identity (key) order, sorted alphabetically — independent of how a flat `sources`
list is written. The slices below take distinct keys for legible `by-source` labels
(`nvim/…`, `helix/…`):

```toml
[targets.editors]
path = "~/.config"
layout = "by-source"     # labels each slice by its identity: nvim/… and helix/…
[targets.editors.sources]
nvim  = { source = "dotfiles", take = ["nvim/**"] }
helix = { source = "dotfiles", take = ["helix/**"] }
```

Per-target version (`branch`/`tag`/`rev`). A binding may also set its own ref —
exactly as it sets its own `take`. The source's ref is the default;
a binding's ref overrides it for that target alone; a bare binding inherits the
source's ref. As on a source, set at most one ref per binding (precedence within a
binding is `rev` > `tag` > `branch`).

Each distinct ref gets its own lock entry, at its own resolved commit. Bindings
that don't override the ref collapse to the source's ref and share one lock entry, so
a config that names no binding refs locks byte-for-byte as before; a ref-overriding
binding records its own entry. Resolution still does one fetch per source —
that single fetch covers every ref the source's bindings name.

To bind one source at two versions, give each binding a distinct key, each naming the
same `source` and pinning its own ref:

```toml
[targets.tools]
path = "~/.local/tools"
layout = "by-source"     # stable/… and canary/… resolve to different commits
[targets.tools.sources]
stable = { source = "fzf", tag = "v0.55.0" }
canary = { source = "fzf", tag = "v0.56.0" }
```

CLI. `phora bind <source>… --to <target>` adds bindings; `--as`, `--take <entry>…`,
and `--branch`/`--tag`/`--rev` refine the binding. A `--take` entry is a leaf, a glob,
or a `src=dest` rename (the `=` form writes the `{ src = dest }` rename table). Any
binding refinement writes a keyed table entry; with no refinement it appends a bare
source name to the target's flat list (or, if the target is already a keyed table,
writes `name = {}`). `--branch`/`--tag`/`--rev` write a table binding pinning that ref
for the target. Because `--as` sets a single binding identity, it cannot apply to
multiple sources. `--root` is source-owned, not a binding key: `bind --root` writes
`root` onto each named `[sources.<name>]` table (and errors if a named source is not
declared in the file being edited). `phora unbind <identity>… --from <target>`
removes bindings by their identity.

`phora add <url>` and `phora source add <url>` carry the source-owned offer flags
`--root`, `--include <glob>…`, and `--exclude <glob>…`, which shape the NEW source's
offer (they land on `[sources.<name>]`, never on a binding). With `--to <target>`,
`phora add` also accepts `--as` to set the binding identity (it requires exactly one
`--to` target, erroring with multiple `--to` or none). The ref flags stay
source-level on `add`: `phora add`'s `--branch`/`--tag` add a source at a version, so
per-target ref overrides are a `bind` concern only (`bind --branch/--tag/--rev`).
Local/symlink overlays (`--local`/`--symlink`) accept neither `--to` nor binding
refinement flags.

### Renaming leaves (`take` rename)

There is no separate `map` construct — renaming is the `{ "src" = "dest" }` form of
a binding's `take`. Where the rest of phora keeps an offered leaf at its own path, a
rename entry consumes one offered leaf and emits it at a chosen destination instead.
The canonical case is a single shared file fanned out under the names different tools
expect:

```toml
[targets.agents]
path = "~/myproject"
[targets.agents.sources]
dotfiles = { take = [{ "AGENTS.md" = "AGENTS.md" }] }
claude   = { source = "dotfiles", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
codex    = { source = "dotfiles", take = [{ "AGENTS.md" = "codex.md" }] }
```

One `AGENTS.md` in the source now lands three times, under three names, with no
copies in the source tree. A rename entry is `{ "<offered-leaf>" = "<dest>" }`: the
key is a path the source offers, the value the path it deploys as under the target's
layout.

- Destructive. The renamed leaf is emitted only at `dest`, not also at its
  original path. A `src` already covered by a glob in the same `take` is consumed out
  of that glob, so it is not double-emitted.
- The `src` must be offered. A rename whose `src` is not in the offer is a hard
  error (a `take` may not widen the offer); the diagnostic suggests the closest
  offered leaf. A leaf named both as a literal `take` and as a rename `src` is also
  rejected.
- Portable `dest`. A `dest` must be a forward-slashed relative path inside the
  target root: an absolute path, a `..` escape, or a backslash is rejected. Nested
  dests are allowed.
- No within-binding clash. Two entries resolving to the same destination (case-
  insensitively, NFC-folded) are a config error; one `src` renamed to two different
  dests is rejected too.
- Fan-out without duplication. The same source leaf renamed to different dests
  across bindings and targets is each its own binding under its own table key, naming
  the same `source`; their dests differ, so they never clash. The source is fetched
  once.
- Copy and link both work. Default `deploy = "copy"` materializes the leaf;
  `deploy = "link"` (local-path only — see [Link mode](#link-mode-local-development))
  makes the dest a symlink to the source leaf in the working tree.

Overlay. A binding's `take` lives on the binding, and a `phora.local.toml` `sources`
list replaces the base target's list wholesale — it does not merge per binding. A
local override of a target's `sources` must therefore restate every binding it
wants, including their `take`; base takes it omits are dropped for that target.

### Collapse (`collapse`)

A binding's `collapse` controls how a taken set materializes: per-leaf artifacts, or
one directory artifact (a directory symlink under `deploy = "link"`, a subtree copy
under `deploy = "copy"`). It is a binding-level opt (and has a mount-parity table —
see [Transitive dependencies](#transitive-dependencies)), exempt from the
binding-scope rejection alongside `template` and `take`.

- Omitted — algorithmic default. A directory collapses to one artifact exactly
  when every offered leaf under it is taken at its identity and no per-leaf rename
  targets it; collapse is maximal, taking the topmost clean directory. Under `link`,
  a within-dir exclude blocks collapse and the directory falls back per-leaf with a
  warning; under `copy`, an excluded child is simply pruned from the subtree and the
  directory still collapses.
- `collapse = false` — force per-leaf. Every kept leaf stays its own artifact even
  on a wholly-taken directory (snapshot semantics).
- `collapse = true` — demand the directory artifact. It is a hard error, naming the
  directory, if a within-dir exclude (under `link`) or a per-leaf rename makes
  whole-directory collapse impossible. This is the analogue of dotter's `recurse`:
  request the directory symlink/subtree, and fail loudly when it cannot be honored.

```toml
[targets.editors.sources]
# force a per-leaf snapshot even though the whole tree is taken:
nvim = { source = "dotfiles", take = ["nvim/**"], collapse = false }
```

### Source kinds

A source declares its remote in exactly one kind — never more than one:

- *Forge:* `host = "<alias>"` + `repo = "<owner/repo>"`, resolved at sync time
  from the host's `remote` template. `host` may be omitted when `repo` is set, in
  which case it defaults to `github` (`repo = "owner/repo"` is github shorthand).
- *Local:* `path = "<dir-or-file>"`, a filesystem path used verbatim as the
  remote — exactly like a `git = "/abs/local"` URL.
- *Literal:* `git = "<url>"`, any https, `ssh://`, or scp-style (`git@host:path`)
  remote.
- *Url:* `url = "https://…"`, a downloadable resource (see below).

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

Back-compat aliases. `git = "/abs/local"` still declares a local source.
`host` + `path` (forge owner/repo) is a deprecated alias for `host` + `repo`.

> Breaking change: a bare `path = "owner/repo"` (no host) now means a LOCAL
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

Built-in forges. `github`, `gitlab`, `codeberg`, `sr.ht`, and `bitbucket`
ship as `remote` tables with both https and ssh shapes, so no template is needed
for them. A `[hosts.X]` block of the same name overrides the built-in's `remote`
or adds `auth`; changing a host's `remote` re-points every source on that host
with no per-source edits.

Protocol. `protocol = "https" | "ssh"` selects which template key a forge
source resolves through. It defaults to `https`, can be set globally at the top
level, and is overridable per source. Selecting `ssh` against a host whose
`remote` has no `ssh` key is a config error. (`protocol` is ignored for literal
`git` and local `path` sources.)

The forge and literal forms of one repo — and its https and ssh remotes —
share a single mirror under the cache root's `git/` subdirectory (see
[State & locations](#state--locations)), so switching kind or protocol never
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

Formats. tar, tar.gz/tgz, and zip, detected by content (magic bytes); a
non-archive URL becomes a single file named from the URL basename.

Auto-strip. When an archive has exactly one top-level directory it is stripped
automatically, so version-stamped release tarballs (`fzf-0.55.0/…`) need no
per-version `root` — and `root` re-selection is unavailable on url sources anyway.
Only `include`/`exclude` apply; `root`/`branch`/`tag`/`rev` are config errors.

Integrity. An optional `digest = "<algo>:<hex>"` (`sha256:` or `blake3:`, 64
hex chars) is verified before extraction; a mismatch errors, naming the source
with expected vs actual.

Determinism. Content is imported as a content-addressed synthetic git commit
(fixed identity, fixed time, constant message), so identical bytes yield an
identical commit and no lock churn. The synthetic commit's time is fixed at epoch+1
(1 second), not epoch 0, since some filesystems (FAT32, HFS+) clamp a 0 mtime —
which would otherwise make `phora verify` report every url-sourced file as modified.
`phora sync` of unchanged content is a no-op; `phora update` (or `--force`)
re-downloads, and the lock advances only if the content changed. `phora verify`
re-hashes deployed files with the same guarantees as git sources.

Out of scope (for now). Auth for private assets and forge release-tag
resolution (latest tag → asset URL) are future work; v1 targets public URLs.

### Link mode (local development)

By default `deploy = "copy"` materializes a reflink-style copy of each artifact
from the committed git ODB — point-in-time, content-hashed, verifiable. For a
tight dev loop, `deploy = "link"` instead symlinks the artifact destination
at the source's live working tree (`<source path>/<root>/<artifact>`, absolute).
Uncommitted edits in the checkout are visible through the target immediately, with
no re-sync.

Two guardrails apply:

- Local path only. A link source must be a local source: `path = "/dir"` (or
  the `git = "/dir"` alias), a local filesystem path. `deploy = "link"` on a remote
  URL is a config error that names the source. A relative path counts as local only
  if it exists relative to the working directory; a relative path that does not yet
  exist is rejected as "not local".
- Portable paths in shared config. `deploy = "link"` is allowed in either
  `phora.toml` or `phora.local.toml`. A committed (`phora.toml`) link source over an
  absolute path syncs but prints a non-fatal stderr warning that names the source:
  an absolute checkout path is machine-specific and rarely portable across machines.
  A committed link over a relative (portable) path warns nothing. Machine-specific
  checkouts still belong in `phora.local.toml`, which never warns.

Linked artifacts sit outside the integrity model: their registry record carries
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

## Transitive dependencies

So far a source has been a flat bag of artifacts. A *transitive dependency* is a
source that is itself a phora project — it ships its own `phora.toml` with its own
`[sources]` and `[targets]`. Instead of re-typing that project's whole layout into
your config, you import it and phora composes its targets straight into your
workspace.

Take [`srnnkls/tropos`](https://github.com/srnnkls/tropos), a toolkit of
agent-harness artifacts — skills, commands, agents, workflows. Its `loqui` skill
hands the agent language-specific coding guidelines, but those live in a separate
repo, [`srnnkls/loqui`](https://github.com/srnnkls/loqui), and the skill expects them
vendored underneath it at `skills/loqui/reference/loqui/`. So tropos declares loqui
as one of its own sources and lets phora compose it into that spot. Mark tropos
`transitive = true`, import it, and phora follows that edge.

```toml
# your phora.toml
[sources.tropos]
host = "github"
repo = "srnnkls/tropos"
branch = "main"
transitive = true

[targets.claude]
path = "~/.claude"
imports = ["tropos"]      # mount tropos's own targets under ~/.claude
```

```toml
# inside srnnkls/tropos, its own phora.toml — the slice that matters here:
[sources.loqui]
host = "github"
repo = "srnnkls/loqui"    # the language guidelines the loqui skill leans on

[targets.loqui]
path = "skills/loqui/reference/loqui"   # relative — composes UNDER the importing anchor
sources = ["loqui"]
```

A `phora sync` now fetches tropos, parses its manifest, resolves its `loqui`
source, and deploys loqui's artifacts (its `languages/` and `resources/` trees) at
`~/.claude/skills/loqui/reference/loqui/…`, exactly where the skill looks for them.
One `imports` line, and tropos's dependency rode along. A target can import several at
once — `imports = ["tropos", "work-config"]` — each composing under the same anchor.

### How composition works

- The importing target's `path` is the anchor. Each dep target's own `path` is
  taken as relative and joined under it — tropos's `loqui` target at `path =
  "skills/loqui/reference/loqui"`, imported into your target at `~/.claude`, deploys
  to `~/.claude/skills/loqui/reference/loqui`.
- The dep's own layout governs its artifacts, not yours. If that target declares
  `layout = "by-source"`, loqui's trees nest one level deeper under the source
  identity rather than landing flat — and that's tropos's call, not yours, even when
  your `claude` target is `prefixed`. The anchor's layout is never re-applied to a
  mounted subtree.
- Nothing silently merges. A dep's sources are namespaced per dep instance. If
  both you and tropos define a source named `loqui` pointing at different repos, your
  `loqui` serves your targets and tropos's is a distinct instance serving its own.
  Import a second config that pulls its own `loqui` and the two stay separate too.
  And a dep that imports its own deps composes recursively, with a cycle guard so a
  diamond collapses to a single fetch instead of looping forever.
- Real collisions are hard errors. If two composed dep targets resolve to the
  same destination, the sync stops and names the path (`composed targets resolve to
  the same destination`) rather than letting one quietly clobber the other.

### Subsetting a mounted dep

A consumer subsets what a mounted dep contributes with target-owned `[take]` and
`[collapse]` tables, keyed by the imported dep's anchor (the composed destination
the dep target lands at). This is the mount-level analogue of a binding's `take` and
`collapse`: it is the consumer's own slice of a composed subtree, the dep cannot
override it.

```toml
[targets.claude]
path = "~/.claude"
imports = ["tropos"]
# keep only the gestalt skill out of tropos's skills tree, and rename one leaf:
[targets.claude.take]
"skills" = ["skills/gestalt/**", { "skills/gestalt/SKILL.md" = "skills/gestalt/skill.md" }]
# force the loqui reference tree to land per-leaf rather than as one dir artifact:
[targets.claude.collapse]
"skills/loqui/reference/loqui" = false
```

An omitted table inherits (no subsetting); a present-but-empty `[take]`/`[collapse]`
clears any inherited table back to take-all; a non-empty local table replaces the
base table wholesale on overlay.

### Confinement

A dep's `phora.toml` is untrusted input, so a composed dep can only ever write
inside its anchor. phora rejects, at compose or write time:

- a dep target path that escapes the anchor with `..`, is absolute, or carries an
  unsafe path component;
- a write whose anchor ancestor is a symlink (no following a planted link out of
  the tree);
- writes into protected paths — your `phora.toml`/`phora.lock`, `.git`, and phora's
  own state and cache roots;
- `deploy = "link"` on a transitive source (a link would point at an unconfinable
  mirror path); your own link sources are unaffected.

A dep's inner sources resolve their remotes against your host registry, so the
dep records intent (`host` + `repo`) and your config decides the protocol and the
forge URL. An inner source with an absolute-path or `file://` remote is rejected:
a dep cannot reach back onto the consumer's local filesystem.

### Hook trust

Here is the sharp edge. A tropos target might carry a hook — say `on_change = "mise
trust && mise install"` to provision the toolchain it just laid down — a shell
command its author wants run after the files land. That command would run on your
machine, from a repo you don't control. So phora never trusts a dep's hooks
implicitly. On the first sync, discovered dep hooks are stripped — recorded, but
not run — and the sync tells you so. You approve them deliberately:

```bash
phora trust tropos --list   # show each hook: its command, its commit-pinned preimage,
                            # and the dep surface around it (see below)
phora trust tropos          # same, then prompt [y/N] per hook; a yes is recorded
```

What `--list` shows around each hook depends on whether you have trusted it before.
A hook you have approved at an earlier commit renders the file-level diff between
that trusted commit and the current candidate commit, so you can see what moved in
the dep before re-trusting. A hook with no prior trusted commit instead lists the
dependency-repo-relative files the consumer composes from the dep at the candidate
commit — the actual surface the hook will run against, honoring the binding's
include/exclude. Both are resolved offline from the cache mirror; if the candidate
commit is unresolved or absent from the mirror, the listing degrades to a
`run phora sync first` notice rather than guessing.

To read the surrounding tree directly, `phora trust tropos --show <path>` prints a
dep file at the pinned candidate commit, also offline. A UTF-8 file prints its
contents; a directory lists its direct entries ls-style, with subdirectories
slash-suffixed; an absent path errors naming the path; binary (non-UTF-8) content is
refused rather than dumped; and a commit not yet in the mirror points you at
`phora sync`. `--show` requires a source and refuses to guess when one source has
several distinct pinned dep commits — it names them so you can disambiguate.

Approval is consumer-owned and lives in your `phora.lock` (a `[[trusted_hooks]]`
entry pinned to the hook's command and the exact dep commit it came from);
discovered-but-unapproved hooks sit under `[[candidate_hooks]]`, which carries no
trust at all. A trusted hook runs on the next sync without a prompt — but the moment
the dep changes that hook or the files around it, the preimage stops matching and it
drops back to needing approval. There is deliberately no "trust on first sight."

When hooks are stripped, an interactive sync exits non-zero so a human acts on it; a
non-interactive run (CI) surfaces the same notice but stays green, because the files
are deployed and only the post-processing was skipped. `phora sync
--no-transitive-hooks` skips composed-dep hooks entirely (your own hooks still run),
and `phora trust tropos --revoke` drops every approval for a dep.

### Reproducibility

`phora sync --frozen` refuses to fetch or re-resolve anything: every source — root,
imported dep, and nested dep alike — must already be pinned in the lock. A miss
hard-errors, naming the source (and, for a nested dep, its depth) so a drifted or
dropped pin can't pass silently. It is the offline, "the lock is the law" mode for
CI and reproducible checkouts. A `phora.local.toml` overlay can flip a source to
`transitive = true` for a single machine, exactly like any other per-machine
override.

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

### Testing

```bash
mise run test-integration   # scrut suites under tests/scrut/ against a release build
```

The scrut suites drive the shipped binary end to end and double as runnable usage
docs. [`tests/scrut/showcase.md`](tests/scrut/showcase.md) is a narrated
walkthrough — adding a git source, projecting it, then layering a machine-local
symlink overlay — whose assertions CI keeps honest.
