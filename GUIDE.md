# The phora guide

This is the long-form companion to the [README](README.md). The README is the
map — terse, every flag in one place. This guide is the walkthrough: it starts
with a working setup, explains how phora works, and then goes under the hood
into how phora actually stores, fetches, and verifies things. Read it top to
bottom the first time; after that, jump to the section you need.

If you just want the command, it's in the README. If you want to understand why
phora did what it did — or what to do when it didn't — you're in the right place.

## Contents

- [How phora works](#how-phora-works)
- [Your first sync](#your-first-sync)
- [Sources](#sources)
  - [Git sources](#git-sources)
  - [URL sources](#url-sources)
  - [Which to reach for](#which-to-reach-for)
- [Choosing what ships](#choosing-what-ships)
- [Targets](#targets)
- [Bindings: per-target selection](#bindings-per-target-selection)
  - [Identity, and one source as several slices](#identity-and-one-source-as-several-slices)
  - [Per-target versions: one source, many refs](#per-target-versions-one-source-many-refs)
  - [Binding scope is rejected](#binding-scope-is-rejected)
- [Where artifacts land: layouts](#where-artifacts-land-layouts)
- [Renaming leaves: one file under another name](#renaming-leaves-one-file-under-another-name)
- [Collapse: how a taken set materializes](#collapse-how-a-taken-set-materializes)
- [Staying in sync](#staying-in-sync)
- [Hooks: running commands after a sync](#hooks-running-commands-after-a-sync)
- [Templating: per-machine values](#templating-per-machine-values)
- [The local dev loop: link mode](#the-local-dev-loop-link-mode)
- [Transitive dependencies](#transitive-dependencies)
  - [How a dependency composes](#how-a-dependency-composes)
  - [Subsetting a mounted dependency](#subsetting-a-mounted-dependency)
  - [Confinement](#confinement)
  - [Trusting a dependency's hooks](#trusting-a-dependencys-hooks)
  - [Reproducibility](#reproducibility)
- [Under the hood](#under-the-hood)
  - [One store for everything](#one-store-for-everything)
  - [No working tree: the object store is the substrate](#no-working-tree-the-object-store-is-the-substrate)
  - [Fetching a git source](#fetching-a-git-source)
  - [Fetching and importing a URL source](#fetching-and-importing-a-url-source)
  - [Why a URL import is deterministic](#why-a-url-import-is-deterministic)
  - [Projection and the content digest](#projection-and-the-content-digest)
  - [The lock and content identity](#the-lock-and-content-identity)
  - [Templating and the two digests](#templating-and-the-two-digests)
  - [Versioning, the git way](#versioning-the-git-way)
  - [Verification](#verification)
  - [Execution model: parallel reads, one serial writer](#execution-model-parallel-reads-one-serial-writer)
  - [Hook dispatch and recording](#hook-dispatch-and-recording)
  - [Composing a dependency graph](#composing-a-dependency-graph)
  - [Integrity boundaries](#integrity-boundaries)
- [When something looks wrong](#when-something-looks-wrong)
- [Where to look next](#where-to-look-next)

## How phora works

phora moves directory-shaped payloads from where they live (a git repo, or a URL)
into the places on disk that consume them — and keeps a record precise enough to
prove, later, that nothing drifted.

The vocabulary splits cleanly by who owns what. A *source* owns its *offer* — the
set of paths it publishes. A *target binding* owns its *take* — the subset of that
offer it actually wants, possibly renamed. Nothing in between merges silently:

- A *source* is where content comes from. It is one of four kinds, and exactly
  one: a forge (`host` + `repo`), a local path (`path = "/dir"`), a literal git
  remote (`git = "…"`), or a downloadable resource (`url = "https://…"`). A source
  also *owns its offer*: `root` re-anchors the slice it draws from, and
  `include`/`exclude` (gitignore syntax) compose into `include − exclude` — the leaf
  set the source publishes.
- The *offer* is that published leaf set, named relative to the source's `root`.
  With no `include` the offer is everything in the source minus VCS metadata
  (`.git/`); an `include` narrows it and an `exclude` prunes it (exclude wins; there
  is no `!` re-inclusion). Dotfiles match like any other path.
- An *artifact* is one offered leaf, identified by its full offered path — not a
  top-level directory. It is the unit a target takes, renames, and deploys. A
  wholly-taken directory may *collapse* back into a single directory artifact (see
  [Collapse](#collapse-how-a-taken-set-materializes)).
- A *target* is a local directory you project artifacts into, arranged by a
  *layout*, drawing from the bindings it declares. See [Targets](#targets).
- A *binding* is the edge from a target to a source — an entry in a target's
  `sources`, written either as a bare name in a flat list or as a keyed entry in a
  `[targets.<t>.sources]` table. The source decides provenance and the offer; the
  binding *owns the take*: its `take` subsets and renames the offer for that one
  target, without touching the source or any other target. See
  [Bindings](#bindings-per-target-selection).
- The *lock* (`phora.lock`) pins each source to one exact commit, so two machines
  syncing the same config get byte-identical results.
- The *registry* (under the state root — `XDG_STATE_HOME`, see [One store for
  everything](#one-store-for-everything)) remembers what landed where — which commit,
  which content hash, which files — so phora can later detect drift, conflicts,
  and orphans.

| Term     | Owner  | What it does                                                            |
| -------- | ------ | ----------------------------------------------------------------------- |
| offer    | source | the published leaf set: `include − exclude` (gitignore) under `root`; no `include` ⇒ everything minus `.git/` |
| take     | target | subsets and renames the offer per binding (literal / glob / `{ src = dest }`) |
| artifact | —      | one offered leaf, identified by its full offered path                   |
| collapse | target | how a taken set materializes: per-leaf, or one directory artifact       |

Everything phora does is one pipeline, and every source runs through it the same
way:

1. Fetch or import the bytes into a local store.
2. Resolve the source to one commit and write it to the lock.
3. Project each binding's taken artifacts into their targets.
4. Record what was deployed, and verify it on demand.

The one idea worth holding onto: the store is git, and *everything becomes a git
tree* — a cloned repo or a downloaded tarball alike. That is why a URL source
deploys, locks, and verifies exactly like a git source. Step 1 differs; steps 2–4
are shared code.

## Your first sync

The fastest start is a single command — `phora add` writes the config for you:

```bash
phora add me/dotfiles      # add the source and bind it into [targets.default] (deploys into ".")
phora sync
```

That records `[sources.dotfiles]`, ensures a `[targets.default]` (path `.`, flat
layout), and projects the source's artifacts into the project directory. `--to
<name>` routes to a named target instead — creating it, on a prompt, if it does
not exist — and `[defaults] auto_target = false` turns the default off so a bare
`add` only declares the source.

For control over *where* things land — a specific target path, the slice of the
source a target *takes*, a layout — use the `bind` and `add` flags, or write
`phora.toml` directly; the flags just edit the file for you. By hand the same setup
is:

```toml
version = 1

[sources.dotfiles]
host = "github"          # resolves to https://github.com/me/dotfiles.git
repo = "me/dotfiles"     # forge owner/repo
branch = "main"

[targets.nvim]
path = "~/.config/nvim"
layout = "flat"

[targets.nvim.sources]
dotfiles = { take = ["nvim/**"] }   # this target takes the repo's nvim/ subtree
```

Then sync:

```bash
phora sync
```

Here is what that one command did, in order:

1. Read `phora.toml` (and `phora.local.toml` if present, overlaid per-key).
2. Mirrored `github.com/me/dotfiles` into a bare repo under the cache root's `git/`
   subdirectory, resolved `main` to a concrete commit, and wrote it to `phora.lock`.
3. Resolved the binding's `take` (`nvim/**`) against the source's offer, and
   materialized the taken leaves into `~/.config/nvim` using the `flat` layout —
   collapsing the wholly-taken `nvim/` tree into a single directory artifact.
4. Recorded each deployed file's content hash in the registry.

Confirm what happened:

```bash
phora list        # per-target status: which artifacts are deployed, and their state
phora verify      # re-hash every deployed file against the record; exit 0 if all match
```

`phora verify` is the payoff for all the bookkeeping: it is the difference between
"the files are there" and "the files are exactly what phora put there." It exits
non-zero on the first mismatch, so it drops cleanly into a pre-commit hook or CI.

Later, to pick up upstream changes:

```bash
phora update      # re-resolve sources to their latest commit, then sync
```

`sync` honors the lock; `update` advances it. That distinction runs through the
whole tool, and it is worth internalizing early: a plain `sync` is reproducible
and offline-friendly, an `update` reaches for new commits.

## Sources

### Git sources

A git source names a repository and a point in its history. Pin exactly one of
`branch`, `tag`, or `rev`; setting two is a config error.

You can declare the remote two ways. Literally:

```toml
[sources.tool]
git = "https://github.com/me/tool.git"   # or ssh://…, or git@host:owner/repo
tag = "v1.4.0"
```

or symbolically, against a host alias:

```toml
[sources.tropos]
host = "github"          # built-in; may be omitted, in which case it defaults to github
repo = "srnnkls/tropos"
branch = "main"
```

The symbolic form exists so your config records intent (`github` + `owner/repo`)
rather than a baked-in URL, which means you can switch protocol or re-point a
whole forge without editing every source. `github`, `gitlab`, `codeberg`,
`sr.ht`, and `bitbucket` are built in with both https and ssh shapes. Add your own
with a `[hosts.X]` block whose `remote` template fills `{path}`, `{owner}`, and
`{repo}`:

```toml
[hosts.company]
remote = { https = "https://git.company.com/{path}.git", ssh = "git@git.company.com:{path}.git" }
```

A third form names a local checkout directly:

```toml
[sources.scratch]
path = "~/dev/scratch"   # a local filesystem path, used verbatim as the remote
branch = "main"
```

A bare `path = "owner/repo"` (no `host`) is a *local* path, not a forge shorthand —
the github shorthand is the bare `repo = "owner/repo"`. A local source is what
[link mode](#the-local-dev-loop-link-mode) live-links against.

A useful property falls out of how the store is keyed: the literal and symbolic
forms of one repo, and its https and ssh remotes, all share a single mirror.
Switching between them never re-clones. (The [internals](#fetching-a-git-source)
explain why.)

### URL sources

A URL source points at a downloadable resource — a release tarball, a zip, or a
single file:

```toml
[sources.fzf-bin]
url = "https://github.com/junegunn/fzf/releases/download/0.55.0/fzf-0.55.0-linux_amd64.tar.gz"
digest = "sha256:0123…"   # optional; verified before extraction
include = ["fzf"]
```

phora downloads it, optionally checks the digest, extracts it, and imports the
contents as a source — after which it discovers, exports, deploys, and verifies
exactly like a git source. A few things are worth knowing up front:

- Formats: tar, tar.gz/tgz, and zip, detected by content (the magic bytes), not
  the file extension. Anything that is not a recognized archive becomes a single
  file, named from the URL's basename.
- Auto-strip: if an archive has exactly one top-level directory — the
  `fzf-0.55.0/` that release tarballs love — phora strips it, so a version bump
  doesn't reshuffle your paths or your lock.
- No refs, no re-rooting: a URL source is a single imported snapshot, not a
  repository, so it has no history to point into and no subtree to descend into —
  `branch`, `tag`, `rev`, and `root` are all config errors on a URL source.
  `include`/`exclude` still shape its offer, filtering the imported tree.
- Integrity: an optional `digest = "sha256:…"` or `blake3:…` (64 hex chars) is
  verified against the downloaded bytes *before* anything is extracted. A mismatch
  errors, naming the source and showing expected vs actual. This is a stronger
  story than `curl | tar`: nothing touches your disk tree until the bytes check
  out.
- Determinism: identical bytes always import to the identical commit, so an
  unchanged URL is a true no-op on the next sync, and a changed one advances the
  lock. The [internals](#why-a-url-import-is-deterministic) cover how.

### Which to reach for

Use a git source when the upstream is a repository and you want a moving target
(`branch`) or a pinned one (`tag`/`rev`) with full history available to the
mirror. Use a URL source when the upstream publishes built artifacts — release
binaries, vendored bundles, a single script — that have no meaningful git history
you care about. Both end up in the same store with the same guarantees; the choice
is about where the bytes naturally live.

## Choosing what ships

Selection has two sides, and they belong to different owners. A *source* publishes
an *offer* — the leaf set it makes available. A *target binding* states a *take* —
the subset of that offer it actually wants. This section is the source side, the
offer; the take has its [own section](#bindings-per-target-selection).

On a source, `root` re-anchors the slice phora draws from (git sources only — URL
archives are already stripped to their root), and the offer is named relative to it.
`include` and `exclude` are *gitignore-syntax* lists composed into `include −
exclude`: an `include` narrows the offer and an `exclude` prunes it, with exclude
winning and no `!` re-inclusion. With no `include` at all, the offer is everything
in the source minus VCS metadata (`.git/`). Dotfiles match like any other path —
there is no special-casing.

```toml
[sources.dotfiles]
host = "github"
repo = "me/dotfiles"
root = "modules"
include = ["editor", "shell/**"]
exclude = ["**/*.bak", "**/.DS_Store"]
```

The offer is the *artifact set itself*: each offered leaf is an artifact, identified
by its full offered path (`editor/init.lua`, not just `editor`). The offer is what
every target sees; a binding cannot widen it, only take a subset (see
[Bindings](#bindings-per-target-selection)). Scope lives on the source so that one
edit changes what *all* consumers can see, while a binding's take stays a private,
per-target slice.

When a file does or doesn't ship and you can't see why, don't guess — ask:

```bash
phora check-match --source dotfiles path/in/the/source
```

It tells you whether a given path passes the include/exclude rules for that
source — both whether the path is offered and whether its top-level artifact is —
which is almost always faster than reading globs by eye. For the consumer side —
which include offered a path, and how a binding's `take` then resolves it — reach
for [`phora explain`](#when-something-looks-wrong).

## Targets

A *target* is a local directory phora projects artifacts into. It has three parts:
a `path` (where on disk), a `layout` (how artifacts are arranged inside it), and its
`sources` — the bindings it deploys.

```toml
[targets.nvim]
path = "~/.config/nvim"
layout = "flat"
sources = ["dotfiles"]
```

`path` is where the target lives. A leading `~/` expands to your home directory; a
relative path resolves against the project root — the directory `phora` runs in —
which is what lets a repository deploy into itself. Only `~/` is expanded (no `$VAR`,
no bare `~`). phora creates the directory, and any missing parents, on the first
sync that lands a file there.

`layout` arranges artifacts inside the target — `flat` (the default), `by-source`,
or `prefixed`. It only starts to matter once a target holds more than one binding,
so the detail lives in its own section: [Where artifacts
land](#where-artifacts-land-layouts).

`sources` is the target's set of bindings — each entry an edge to a source, with its
own `take` for this target. The keyed `[targets.<t>.sources]` table is the general
form; the flat list of bare names is an ergonomic shorthand for the case where every
binding takes the whole offer. A target deploys exactly its bindings and nothing else,
so a target with no `sources` key — or an empty `sources = []` — deploys nothing at
all. The [next section](#bindings-per-target-selection) is entirely about that edge.
You can edit the set with `phora bind <source> --to <target>` and `phora unbind
<identity> --from <target>`, or write the table by hand.

A config can declare as many targets as you like, each with its own path, layout,
and bindings. One source can fan out across several targets — a `dotfiles` source
feeding `~/.config/nvim`, `~/.config/git`, and a project tree — and one target can
compose several sources. `phora add` ensures a `[targets.default]` (path `.`, flat
layout) unless you route elsewhere with `--to` or disable it with `[defaults]
auto_target = false` (see [Your first sync](#your-first-sync)).

## Bindings: per-target selection

A target's `sources` is not just a list of source names — each entry is a
*binding*: the edge from this one target to a source, and where a single
consumer's *take* lives. The source says *what is on offer*; the binding says *how
much of it this target takes, and under what names*.

A target's `sources` takes one of two forms — never both at once:

- A *flat list of bare names* — `sources = ["dotfiles", "loqui"]` — takes each
  source's *whole offer*. This is the all-bare, zero-take form, and each element is
  equivalent to `name = {}` (no `take`, i.e. take everything).
- A *keyed table* — `[targets.<t>.sources]`, a map whose *key is the binding
  identity* and whose value is *always a table* refining that one binding. The key
  *defaults to the source name*; you write `source` *only when the identity
  diverges* from it. A bare entry inside a keyed target is `name = {}`. A binding
  may carry `take`, `collapse`, `template`, and a per-target ref
  (`branch`/`tag`/`rev`); the *offer scope* (`root`/`include`/`exclude`) is not a
  binding key — it belongs to the source.

A binding's `take` is a list that *subsets and renames the offer*. Each entry is one
of three things:

- a *literal leaf* — a plain offered path like `"nvim/init.lua"`, kept verbatim;
- a *gitignore glob* — any entry with `*`, `?`, `[`, `]`, or a trailing `/`, like
  `"nvim/**"`, which expands over the offer set only, never widening it;
- a *rename* `{ "src" = "dest" }` — the offered leaf `src` is consumed and emitted
  at `dest` instead (destructive: it does not also land at `src`).

Omitting `take` takes the whole offer; `take = []` takes nothing. A literal or
rename `src` that the offer does not expose is a hard error — a take may not widen
the offer, and the diagnostic suggests the nearest offered leaf. A glob that matches
nothing only warns.

```toml
[targets.neovim]
path = "~/.config/nvim"

[targets.neovim.sources]
nvim = { source = "dotfiles", take = ["nvim/**"] }
```

Here the `nvim` binding takes just the `nvim/` subtree of `dotfiles` for this target
alone. The source's offer is unchanged, and another target binding `dotfiles` bare
still takes the whole offer.

### Identity, and one source as several slices

Every binding has an *identity*. In a flat list it is the source name; in a keyed
table it *is* the table key, which defaults to the source name. Identity is the
binding's name in three places: it keys the registry artifact record, and it is the
label the `by-source` and `prefixed` layouts use (see
[layouts](#where-artifacts-land-layouts)). Identity is structurally unique because
TOML table keys are unique — there is no way to write the same identity twice.
Bindings resolve in identity order, sorted alphabetically, independent of how a flat
`sources` list is written.

Because it is the *identity* that keys a binding and not the source, the same
source can appear in one target more than once under distinct keys, each taking an
independent slice:

```toml
[targets.editors]
path = "~/.config"
layout = "by-source"     # ~/.config/nvim/… and ~/.config/helix/…

[targets.editors.sources]
nvim  = { source = "dotfiles", take = ["nvim/**"] }
helix = { source = "dotfiles", take = ["helix/**"] }
```

One source, one mirror — but two bindings taking two subtrees into the same target,
each labelled by its identity. This is the headline thing per-binding `take` buys
you: a target composes slices, not whole sources.

### Per-target versions: one source, many refs

Selection is not the only thing a binding can override. A binding may also set its
own *ref* — `branch`/`tag`/`rev` — and pin this one target at its own version. The
rule mirrors selection exactly: the source's ref is the default, a binding's ref
wins for that target alone, and a bare binding inherits the source's ref. As on a
source, at most one ref per binding (precedence within a binding is `rev` > `tag` >
`branch`); naming two is a config error.

This is what lets one source live at two versions inside a *single* target. Give
each binding a distinct table key, name the same `source` on each, and pin a
different ref:

```toml
[targets.tools]
path = "~/.local/tools"
layout = "by-source"     # ~/.local/tools/stable/… and ~/.local/tools/canary/…

[targets.tools.sources]
stable = { source = "fzf", tag = "v0.55.0" }
canary = { source = "fzf", tag = "v0.56.0" }
```

The two bindings share one mirror and one fetch, but resolve to two different
commits and project independently. Under the hood each distinct ref gets its own
lock entry; bindings that don't override the ref collapse onto the source's ref and
share a single entry, so a config that names no binding refs locks byte-for-byte as
it did before this existed (see [the lock](#the-lock-and-content-identity)).

### Binding scope is rejected

`root`, `include`, `exclude`, and `map` are *not* binding keys — they were, before
the offer/take split, and setting any of them on a `[targets.<t>.sources]` entry is
now a hard parse error with a did-you-mean redirect. `root`/`include`/`exclude`
redirect to the source offer (`[sources.<name>]`); `map` redirects to the `take`
rename form. This is pre-alpha — there is no migration shim, the error just points
you at the new home:

```
error: config error: selection: include — binding-level scope is removed
matched against: binding `dotfiles` of target `home`
remedy: move `include` to the source offer on `[sources.dotfiles]`; scope is owned by the source, not the binding
to debug: phora explain home dotfiles
```

### The URL restriction

`take` (or any other refinement) on a binding backed by a `url` source is a config
error: a URL source's content was stripped to a single root at import, so there is
no offer to subset. `branch`/`tag`/`rev` on a binding backed by a `url` source —
which has no ref — or a `deploy = "link"` source — which live-links a working tree
rather than resolving a pinned commit — are config errors too. Bind a URL source
bare.

### Editing bindings

The CLI can edit bindings for you, if you'd rather not touch the table by hand:

```bash
phora bind dotfiles --to neovim                          # bare binding, takes the whole offer
phora bind dotfiles --to neovim --as nvim --take nvim/** # a taken slice, identity `nvim`
phora unbind nvim --from neovim                          # remove a binding by its identity
```

`phora bind <source>… --to <target>` adds bindings; the binding flags `--as`,
`--take <entry>…`, and `--branch`/`--tag`/`--rev` scope to the binding. A `--take`
entry is a leaf, a glob, or a `src=dest` rename (the `=` form writes the
`{ src = dest }` rename table). Passing any binding flag writes a keyed
`[targets.<t>.sources]` table entry; passing none appends a bare source name to the
target's flat list, or writes `name = {}` if the target is already keyed (the writer
promotes a flat list to a keyed table on the first refinement, and never
auto-demotes). `--branch`/`--tag`/`--rev` pin that target's version
(`bind fzf --to tools --as canary --tag v0.56.0`). Because `--as` sets one identity,
it cannot apply to several sources at once. `phora unbind <identity>… --from
<target>` removes bindings *by identity* — so you unbind a slice by the name it was
bound under (`nvim`), not by its source.

`--root` is the exception: it is source-owned, not a binding key, so `bind --root`
writes `root` onto each named `[sources.<name>]` table (and errors if a named source
is not declared in the file). Likewise `phora add <url>`'s offer flags `--root`,
`--include <glob>…`, and `--exclude <glob>…` shape the *new source's* offer — they
land on `[sources.<name>]`, never on a binding. With `--to <target>`, `phora add`
also accepts `--as` to set the binding identity (requiring exactly one `--to`
target). The ref flags stay source-level on `add` — a source is added *at* a
version, so per-target ref overrides are a `bind` concern alone. The
`--local`/`--symlink` overlay forms accept neither `--to` nor binding flags.

## Where artifacts land: layouts

A layout decides the path an artifact `a` from a binding of identity `i` takes
inside a target. The label is the binding's *identity* — the `[targets.<t>.sources]`
table key, defaulting to the source name — not the underlying source:

| Layout                              | Path on disk |
| ----------------------------------- | ------------ |
| `flat` (default)                    | `a`          |
| `by-source`                         | `i/a`        |
| `{ type = "prefixed", separator = "-" }` | `i-a`   |

`flat` is what you want when one binding owns a target. The moment two bindings
project into the same target, `flat` risks collisions — two bindings each
shipping an `editor` artifact would fight over the same destination. That is what
`by-source` and `prefixed` are for, and because they label by identity, two
slices of the *same* source land cleanly side by side:

```toml
[targets.editors]
path = "~/.config"
sources = ["work-dotfiles", "personal-dotfiles"]
layout = "by-source"      # ~/.config/work-dotfiles/editor, ~/.config/personal-dotfiles/editor
```

## Renaming leaves: one file under another name

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
copies in the source tree. Because identity is the table key and keys are unique,
each binding takes a distinct key (the same rule that lets one source appear several
times), and the three destinations coexist.

A few rules follow from what a rename *is*, and phora rejects each violation:

- The `src` must be offered. A rename whose `src` is not in the offer is a hard
  error — a take may not widen the offer — and the diagnostic suggests the nearest
  offered leaf. A leaf named both as a literal take and as a rename `src` is rejected
  too.
- The `dest` is a portable relative path inside the deploy root: an absolute path, a
  `..` escape, or a backslash is rejected. Nested dests are allowed (`"a/b.md"`).
- Renaming is *destructive*: the leaf is emitted only at `dest`, never also at `src`.
  A `src` already covered by a glob in the same take is consumed out of that glob, so
  it is not double-emitted.
- No within-binding clash: two entries resolving to the same destination, or one
  `src` renamed to two dests, are config errors.
- A URL source has no offer to point a `src` into, so a rename on a URL binding is
  rejected too — bind a URL source bare.

Two bindings that resolve to the *same* destination collide — `phora sync` stops with
the structured selection diagnostic, naming the contested destination, rather than
letting one silently overwrite the other. Across distinct bindings and targets the
same source leaf renamed to *different* dests never clashes, and the source is
fetched once.

A renamed leaf is otherwise an ordinary artifact. It is keyed in the registry by
`<identity>/<dest>`, `phora verify` re-hashes it, `--prune` reclaims it by identity
when you drop the binding, and `deploy = "link"` links it to the source leaf for the
editing loop. One subtlety worth knowing: the content digest frames the
*destination* path into the hash, so the same source bytes under two names produce
two distinct digests — each rename is its own artifact, not an alias of the other.

## Collapse: how a taken set materializes

An artifact is an offered *leaf*, so a binding that takes a whole subtree would, by
the letter of that, deploy a flat pile of per-file artifacts. `collapse` is what
folds a wholly-taken directory back into a single *directory artifact* — one record,
one destination — which is both tidier in `phora list` and what link mode needs to
hang a single directory symlink. It is a binding-level opt, exempt from the
binding-scope rejection alongside `take` and `template`.

- *Omitted — the algorithmic default.* A directory collapses to one artifact exactly
  when every offered leaf under it is taken at its identity and no per-leaf rename
  targets it; collapse is maximal, taking the topmost clean directory. Under `link`,
  a within-directory exclude blocks collapse and the directory falls back to per-leaf
  with a warning; under `copy`, an excluded child is simply pruned from the subtree
  and the directory still collapses.
- `collapse = false` — *force per-leaf.* Every kept leaf stays its own artifact even
  on a wholly-taken directory (snapshot semantics).
- `collapse = true` — *demand the directory artifact.* A hard error, naming the
  directory, if a within-directory exclude (under `link`) or a per-leaf rename makes
  whole-directory collapse impossible. This is the analogue of dotter's `recurse`:
  request the directory symlink/subtree, and fail loudly when it cannot be honored.

```toml
[targets.editors.sources]
# force a per-leaf snapshot even though the whole tree is taken:
nvim = { source = "dotfiles", take = ["nvim/**"], collapse = false }
```

`phora preview` marks a collapsed directory with a trailing slash (`nvim/`), so the
plan shows at a glance which artifacts are whole directories and which are loose
leaves.

## Staying in sync

`phora sync` is the workhorse. It resolves each source against the lock, projects
artifacts, and reconciles what is on disk with what should be there. Two flags
change its disposition:

- `--prune` also removes artifacts that the registry tracks but the config no
  longer selects — the way you clean up after dropping a binding or narrowing a
  `take`. The prune is leaf-granular: narrowing one binding's `take` reclaims only
  the now-unselected leaves, never its siblings.
- `--force` overwrites locally modified or foreign files instead of stopping to
  ask.

About that asking: when sync finds a file that was changed outside phora, or a
foreign file sitting where an artifact wants to land, it prompts on a TTY —

```
[s]kip / [o]verwrite / [e]ject / [a]bort
```

— and on a non-interactive run it skips such files unless you passed `--force`.
`eject` is the interesting choice: it tells phora to stop managing that artifact
but leave its files in place (see `phora eject` / `phora uneject` for doing this
deliberately).

`phora update` is the only command that reaches for new commits. Without it, every
`sync` reproduces the locked state, which is exactly what you want on a fresh
checkout or in CI. With it, sources re-resolve to their latest commit and the lock
advances — for a URL source, only if the downloaded content actually changed.

## Hooks: running commands after a sync

Sometimes deploying the files is only half of it — a font cache wants rebuilding, a
plugin manager wants a sync, an index wants refreshing. A hook is a shell command
phora runs *after* it has written the files. Hooks live only in your config
(`phora.toml` or `phora.local.toml`); a synced source tree that happens to carry its
own `phora.toml` is inert content, read as files and never executed.

There are two kinds. A target's `on_change` fires once after a sync that *added or
changed* that target's artifacts — a pure no-op sync runs nothing, and pure removals
don't count either. The global `[hooks] post_sync` runs after *every* sync, change
or not:

```toml
[targets.neovim.hooks]
on_change = "nvim --headless +'Lazy! sync' +qa"

[hooks]
post_sync = "git -C ~/.config add -A"
```

A hook value is a command string, a `{ run = "…", shell = "…" }` table (the shell
defaults to `sh -c`), or an array of either, run in declared order and deduplicated.
An `on_change` hook is handed what changed — `$PHORA_CHANGED` (deployed paths),
`$PHORA_CHANGED_NAMES` (artifact names), and `$PHORA_TARGET` — and it runs only
after the files are on disk, so it can read them.

The recording rules are what make hooks idempotent without you tracking anything:

- A hook that succeeds is recorded, so the next no-op sync does not re-run it.
- A hook that exits non-zero is *not* recorded: `phora sync` exits non-zero too, the
  deployed files stay in place, and the hook re-fires on the next sync. Fix the
  cause, sync again, and it runs — even though no content changed.
- `phora sync --no-hooks` deploys without running any hook.

Each hook that ran is reported with its scope and status, so a sync that triggers
one reads:

```
hook neovim#nvim --headless +'Lazy! sync' +qa#sh -c [on_change] `nvim --headless +'Lazy! sync' +qa` ok
sync complete
```

## Templating: per-machine values

Most configuration is identical on every machine; a few values — an email, a
hostname, a path — are not. Rather than fork a file per machine, render it. A source
file named `*.tmpl` is run through [minijinja](https://docs.rs/minijinja) and
deployed with the suffix stripped (`config.tmpl` → `config`); every other file
copies byte-for-byte. Values come from a flat `[vars]` table, and `phora.local.toml`
overrides them per key — the keys it omits keep their base value — so the committed
config carries the shape and each machine fills in its own:

```toml
# phora.toml — committed
[vars]
email = "me@example.com"

# git/config.tmpl, deployed as git/config:
#   email = {{ email }}
```

```toml
# phora.local.toml — never committed
[vars]
email = "me@this-laptop.example"
```

The `.tmpl` suffix is the opt-in; a binding can widen it to arbitrary globs
(`template = ["*.conf"]`, rendered *in addition to* `*.tmpl`) or switch it off
entirely (`template = false`). Rendering is strict: a reference to an undefined
variable aborts that one artifact's export — its siblings still deploy — so a typo
fails loudly instead of shipping a half-rendered file.

The integrity story is the subtle part, and it is deliberate. phora hashes the
*rendered* bytes into the registry, so `phora verify` checks the output you actually
deployed, not the template. But the *lock* records *source* bytes only — so two
machines rendering the same template with different vars produce byte-identical
locks, and reproducibility stays machine-independent. Editing a var moves no commit:
it marks the affected artifacts outdated, and the next `phora sync` re-renders and
redeploys them with the lock untouched. `phora preview --files` shows each deployed
name and flags what renders (`config (templated)`).

## The local dev loop: link mode

The default deploy mode, `copy`, materializes each artifact from the committed git
object store: a point-in-time, content-hashed, verifiable copy (a copy-on-write
reflink where the filesystem supports it — see [projection](#projection-and-the-content-digest)).
That is the right default, but it is the wrong loop when you are actively editing
the source — you do
not want to commit and re-sync after every keystroke.

`deploy = "link"` swaps the copy for a symlink pointing at the source's live
working tree. Edits show up through the target immediately, no re-sync:

```toml
# phora.local.toml — overlays phora.toml, never committed
[sources.loqui]
git = "/home/me/dev/loqui"   # a local checkout
deploy = "link"
```

Two rules apply:

- The source must be a local filesystem path. A `path = "/dir"` source (or the
  `git = "/dir"` alias) qualifies; linking a remote URL is a config error that
  names the source. A relative path counts as local only if it resolves against the
  working directory — a relative path that does not yet exist is rejected as "not
  local".
- Link mode is allowed in either `phora.toml` or `phora.local.toml`, but a
  committed link over an *absolute* path is rarely portable — an absolute checkout
  path means something different on every machine. So a committed link over an
  absolute path syncs but prints a non-fatal warning naming the source; a committed
  link over a *relative* (portable) path warns nothing; and a link in
  `phora.local.toml` — where machine-specific checkouts belong — never warns. The
  earlier hard rejection of committed link mode is gone: the warning nudges you
  toward portability without blocking a deliberate choice.

One consequence to keep in mind: a linked artifact sits *outside* the integrity
model. Its registry record carries a `linked` marker and no per-file hashes, so
`phora verify` skips it, drift detection never flags it, and `phora list` shows it
as `linked`. That is the deal you are making — live edits in exchange for the
content guarantee. Switch back to `copy` and the next sync replaces the symlink
with a materialized, fully verifiable copy.

## Transitive dependencies

Everything so far treats a source as a flat bag of artifacts: phora reaches in,
takes the leaves you selected from its offer, and projects them. A *transitive
dependency* turns that inside out. It is a source that is itself a phora project —
one that ships its own `phora.toml`, with its own sources and its own targets. When
you import it, phora reads that manifest and composes the dependency's targets into
your workspace, so a single import can carry a whole sub-configuration.

Take [`srnnkls/tropos`](https://github.com/srnnkls/tropos), a toolkit of
agent-harness artifacts — skills, commands, agents, workflows. One of its skills,
`loqui`, hands the agent language-specific coding guidelines, and those guidelines
are not tropos's to maintain: they live in a separate repo,
[`srnnkls/loqui`](https://github.com/srnnkls/loqui), and the skill expects them
vendored underneath it at `skills/loqui/reference/loqui/`. In local development
that spot is a symlink to a loqui checkout; for a real install, tropos declares loqui
as one of its own sources and lets phora compose it into exactly that place. Mark
tropos `transitive = true`, import it, and phora follows that edge:

```toml
# your phora.toml
[sources.tropos]
host = "github"
repo = "srnnkls/tropos"
branch = "main"
transitive = true

[targets.claude]
path = "~/.claude"
imports = ["tropos"]
```

The dependency carries its own manifest. The slice that matters here is that tropos
declares loqui as a source and lands it under the skill that needs it — a relative
path, deep in tropos's own tree:

```toml
# inside srnnkls/tropos, its own phora.toml
[sources.loqui]
host = "github"
repo = "srnnkls/loqui"

[targets.loqui]
path = "skills/loqui/reference/loqui"
sources = ["loqui"]
```

A `phora sync` fetches tropos, parses its manifest, resolves its loqui source, and
deploys loqui's artifacts — its `languages/` and `resources/` trees — at
`~/.claude/skills/loqui/reference/loqui/…`, exactly where the skill looks for them.
You imported one repo and its dependency was wired into place for you. A target may
import several at once — `imports = ["tropos", "work-config"]` — each composing under
the same anchor.

### How a dependency composes

The importing target's `path` is the *anchor*. Each of the dependency's own targets
carries a relative `path`, and phora joins it under the anchor: tropos's `loqui`
target at `path = "skills/loqui/reference/loqui"`, imported into a target at
`~/.claude`, lands at `~/.claude/skills/loqui/reference/loqui`. The
dependency's own layout governs its artifacts — declare `by-source` there and loqui's
trees nest one level deeper under the source identity — and the anchor's layout is
never re-applied to the mounted subtree. The dependency decides its own shape; you
decide only where the whole thing roots.

Nothing silently merges, because every fetched dependency is a distinct *instance*
and its sources are namespaced under it. If both you and tropos define a source
named `loqui` pointing at different repos, your `loqui` serves your targets and
tropos's is a separate instance serving its own — no collision, no overwrite. Two
different dependencies that each pull a source called `loqui` stay separate the same
way. A dependency that imports its own dependencies composes recursively, and a
cycle guard collapses a diamond to a single fetch instead of looping. The one thing
that is an error rather than a merge is a genuine destination clash: if two composed
targets resolve to the same path, the sync stops and names it (`composed targets
resolve to the same destination`) rather than letting one clobber the other.

### Subsetting a mounted dependency

A binding's `take` slices a single source; the mount-level equivalent slices a whole
composed dependency. A consumer subsets what a mounted dependency contributes with
target-owned `[targets.<t>.take]` and `[targets.<t>.collapse]` tables, *keyed by the
imported dependency's anchor* — the composed destination the dependency target lands
at. It is the consumer's own slice of the composed subtree, and the dependency cannot
override it:

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
base table wholesale on overlay — the same wholesale-replace rule a target's
`sources` list follows.

### Confinement

A dependency's `phora.toml` is input you did not write, so phora treats it as
untrusted and boxes it in. A composed dependency can only ever write *inside* its
anchor. phora rejects, at compose or write time, a dependency target path that
escapes the anchor with `..`, is absolute, or carries an unsafe component; a write
whose anchor ancestor is a symlink (so a planted link cannot redirect the write out
of the tree); and any write into a protected path — your `phora.toml`/`phora.lock`,
`.git`, and phora's own cache and state roots. A transitive source may not use
`deploy = "link"` either, since a link would point at an unconfinable mirror path;
your own link sources are unaffected.

A dependency's inner sources resolve their remotes against *your* host registry, so
the dependency records intent (`host` + `repo`) and your config decides the protocol
and the forge URL. The flip side is an escape guard: an inner source with an
absolute-path or `file://` remote is rejected, so a dependency cannot reach back onto
your local filesystem.

### Trusting a dependency's hooks

This is the sharp edge. A dependency's target can carry a hook — an `on_change`
shell command its author wants run after the files land. That command would run on
your machine, from a repo you do not control, so phora never trusts it implicitly.
On the first sync, a discovered dependency hook is *stripped*: recorded, but not run.
The sync tells you so, and you approve it deliberately:

```bash
phora trust tropos --list   # each hook: its command, its commit-pinned preimage,
                            # and which dependency files changed since you last trusted it
phora trust tropos          # the same, then prompt [y/N] per hook; a yes is recorded
```

Approval is consumer-owned and lives in your `phora.lock`, as a `[[trusted_hooks]]`
entry pinned to the hook's command and the exact dependency commit it came from;
discovered-but-unapproved hooks sit under `[[candidate_hooks]]`, which carries no
trust. A trusted hook runs on the next sync without a prompt — but the moment the
dependency changes that hook or the files around it, the preimage stops matching and
it drops back to needing approval. There is deliberately no trust on first sight.

When hooks are stripped, an interactive sync exits non-zero so a human acts on it; a
non-interactive run stays green, because the files are deployed and only the
post-processing was skipped. `phora sync --no-transitive-hooks` skips composed-dep
hooks entirely (your own hooks still run), and `phora trust tropos --revoke` drops
every approval for a dependency.

### Reproducibility

`phora sync --frozen` refuses to fetch or re-resolve anything: every source — root,
imported dependency, and nested dependency alike — must already be pinned in the
lock. A miss hard-errors, naming the source and, for a nested dependency, its depth,
so a drifted or dropped pin cannot pass silently. It is the offline, the-lock-is-the-law
mode for CI and reproducible checkouts. As with any other field, a `phora.local.toml`
overlay can flip a source to `transitive = true` for one machine alone.

## Under the hood

Everything above is the contract. This is the machinery behind it. None of it is
required reading to use phora, but it is what lets you reason about edge cases — and
it is the honest answer to "what is this thing actually doing to my disk?"

### One store for everything

phora keeps its state in two XDG-rooted trees, split by who owns the bytes — a
*cache root* for regenerable git mirrors and a *state root* for the machine-local
records that cannot be regenerated:

- `<cache>/git/<MirrorKey>.git` — bare git mirrors, one per source, under the cache
  root (`XDG_CACHE_HOME`, or by default `~/.cache/phora` on Linux and
  `~/Library/Caches/phora` on macOS). The `MirrorKey` is the first 16 hex characters
  of `blake3` over a *normalized* form of the remote URL. Normalization strips a
  trailing `.git`, rewrites scp-style `git@host:owner/repo` to `host/owner/repo`,
  drops the scheme and any userinfo, and lowercases the host. That is the trick
  behind "https and ssh share a mirror": both normalize to the same string, so both
  hash to the same key.
- Per-project state, under the state root (`XDG_STATE_HOME`, or by default
  `~/.local/state/phora` on Linux and `~/Library/Application Support/phora` on
  macOS), keyed by a `ProjectId` — the first 16 hex characters of `blake3` over the
  canonical project root path. This holds the deploy journal and the lock that
  serializes phora runs, plus the registry records describing what is deployed. A
  record lives at
  `…/targets/<target>/artifacts/<identity>/<artifact>.toml` — keyed by the
  *binding identity*, not the source, which is exactly what lets two slices of one
  source coexist under one target. The record's own `source` field carries the
  underlying source name, so `phora where --source <s>` can still find every slice
  that draws from `s`.

The split is deliberate: the cache root is disposable — delete it and the next sync
re-clones — while the state root is the authoritative record of what is on disk. An
`XDG_*` override is honored only when *absolute* (per the XDG spec); a relative value
is ignored and the platform default applies. macOS has no native state directory, so
the state root falls back to `~/Library/Application Support`. `XDG_DATA_HOME` and
`XDG_CONFIG_HOME` go unused on purpose: there is no portable data payload (the
registry is machine-local, the mirrors regenerable) and no global config (config is
the project-local `phora.toml`).

A git source's mirror has ordinary refs (`refs/heads/*`). A URL source's mirror is
synthetic: phora writes the downloaded content into the same bare-repo object store
as a single commit and points `refs/heads/phora` at it. From the object store's
perspective there is no difference between the two — both are just commits with
trees and blobs — which is precisely why the projection and verification code does
not branch on source kind.

### No working tree: the object store is the substrate

The single design choice that the rest of the machinery falls out of: phora never
materializes a working tree for a source. There is no `git checkout`, no `git
worktree add`, no index, no second copy of the files on disk. A mirror is *bare* —
nothing but the `.git` object database and its refs — and that is the only form a
source ever takes on disk. Everything downstream reads out of that object store
directly.

That reframes the three operations you might expect a package manager to perform:

- Resolving a ref is a pure lookup, not a checkout. A `branch`/`tag`/`rev` *peels*
  to a commit object id; nothing is written to a working directory. The 40-hex
  commit is the entire answer.
- Reading a source's files is a tree walk, not a filesystem read. phora opens the
  commit, descends its tree objects, and pulls blob bytes straight from the object
  database — touching only the entries a target actually takes, never the whole
  tree. The mirror is still a full bare clone (all of history, fetched once); what a
  take avoids is checking that history out — you take one file by reading one blob,
  not by laying the whole tree down on disk first.
- Materializing writes those blob bytes into a staging directory and then into your
  target. The bytes flow object-store → staging → target without git ever owning a
  checked-out copy in between.

This is what makes a per-binding ref override cheap, and it is worth seeing why.
Two bindings of one source at different refs — the `stable`/`canary` pair — are two
commit ids resolved against one bare mirror. Git's object store is
content-addressed and immutable, so the two commits share every unchanged blob and
tree and differ only in what genuinely changed between the refs. There is no second
clone, no per-ref worktree, no duplicated history — the override adds a *ref to
resolve*, not a copy on disk. A worktree-based design would have to check out each
ref into its own directory and reconcile them; phora just reads two trees out of the
same packed store. The cost of holding ten versions side by side is ten commit ids
and whatever blobs actually differ between them.

The same property underwrites the rest of "Under the hood." A URL import being a
synthetic commit, projection touching only taken paths, the slice digest being a
hash over a tree walk, reflink placement out of staging — all of it assumes the
source lives as objects, not as files. The object store is not an implementation
detail behind the model; it is the model.

### Fetching a git source

The git backend uses `gix` (gitoxide) directly, no shelling out. On the first
fetch it clones the remote as a bare repository with a mirror refspec
(`+refs/heads/*:refs/heads/*`), so the local mirror's `refs/heads/*` track the
remote's heads exactly. On subsequent fetches it opens the existing mirror and
updates those refs in place. Fetches honor the interrupt flag, so a Ctrl-C during
a network operation unwinds cleanly rather than corrupting the mirror.

Resolving is then a local lookup: a `branch` peels `refs/heads/<name>` to a
commit, a `tag` peels `refs/tags/<name>`, and a `rev` is parsed as an object id
directly. The result is the 40-hex commit that goes into the lock.

### Fetching and importing a URL source

The URL backend runs four steps, and the order is the security property:

1. Download. An `ureq` client (rustls TLS, certificate verification on) streams
   the response body to a temporary file beside the mirror. It follows redirects
   (release assets love to 302 to a CDN) but strips auth headers across them, and
   it sets connect and body timeouts so a stalled server can't wedge the process.
   A non-2xx status or a transport failure is a clear error; a partially written
   temp file is cleaned up on any failure.
2. Verify. If the source declares a `digest`, phora hashes the downloaded bytes
   with the matching algorithm (`sha256` or `blake3`) and compares. A mismatch
   stops here — before extraction — naming the source with expected and actual
   hex. Nothing has touched your tree yet.
3. Extract. phora sniffs the magic bytes to pick tar, gzip-then-tar, zip, or
   raw-single-file, and unpacks into an in-memory list of entries. Three defenses
   apply during this step: every entry path is validated segment by segment
   (rejecting `..`, absolute paths, drive roots, backslashes, NUL bytes, and
   non-UTF-8 names), so a malicious archive cannot escape; a single common
   top-level directory is stripped; and a cumulative size cap (1 GiB of actual
   decompressed bytes, not the attacker-controlled header) guards against
   decompression bombs. Executable bits and symlinks carry through to the right
   git entry kind.
4. Import. The entries become git objects in the mirror, and `refs/heads/phora`
   moves to the new commit (see the next section for why this is deterministic).

Resolving a URL source afterward is trivial: read `refs/heads/phora`. The backend
ignores any refspec it is handed, which matters because the rest of the system
defaults a source's refspec to `branch = "main"` — a default that would otherwise
send a URL source looking for a branch that does not exist.

### Why a URL import is deterministic

The goal is that identical bytes produce an identical commit id, every time, on
any machine — so that re-downloading unchanged content does not churn the lock,
and so that two people importing the same tarball get the same result.

A git commit id is a hash of its content plus its metadata, so determinism means
nailing down everything that is not content:

- Fixed author and committer identity, a constant commit message, and no parents.
- A fixed timestamp of one second past the epoch. Not zero — some filesystems
  (FAT32, HFS+) clamp a zero mtime, which would make `phora verify` report every
  URL-sourced file as modified forever after. One second sidesteps that while
  staying constant.
- Tree entries written in true git tree order. Git sorts entries by name with a
  subtlety — a directory sorts as if its name had a trailing slash — and the
  object writer assumes that order. phora sorts every level explicitly before
  writing, so the input order of the extracted entries cannot leak into the commit
  id.

With all of that fixed, the commit id is a pure function of the file contents and
their paths. Re-importing the same bytes recomputes the same id and force-updates
the ref to the same place (a no-op in effect); importing changed bytes yields a
new id and the lock advances. You can sanity-check the result yourself: the
synthetic commits pass `git fsck --strict`.

### Projection and the content digest

Projection does not write straight into your target. It first materializes the
selected files into a staging directory, computing a single `blake3` digest over
the artifact as it goes — framing each entry (its relative path length and bytes, a
type tag for file/executable/symlink, and its content length and bytes) into the
hash. The framing matters: without length-prefixing, two different tree shapes
could collide by smearing a path into the next file's content. With it, the digest
is an honest fingerprint of the projected tree.

Moving the staged artifact into the target is an atomic directory swap: phora
journals its intent, renames staging into place, then records the result, so a
crash mid-swap is replayable rather than leaving a half-written target (the same
write-ahead journal the [execution model](#execution-model-parallel-reads-one-serial-writer)
relies on). Within a swap each file is placed with a reflink — a copy-on-write
clone, so on filesystems that support it (APFS, Btrfs, XFS) the bytes are shared
with the staged copy and the placement is near-free — falling back to a plain byte
copy elsewhere or across devices. Either way the commit-time mtime is set
explicitly, since a reflink does not carry mtime; that is what makes a
re-projection of unchanged content produce byte-identical metadata.

An `ExportPolicy` governs the edges: symlinks and submodules are refused unless
explicitly allowed (`allow_symlinks`, `allow_submodules`, both off by default), and
the executable bit is preserved by default (`preserve_executable`). Drift detection
is symlink-aware in the other direction too — it stats without following links, so
a recorded regular file later swapped for a symlink reads as modified rather than as
its target's contents.

### The lock and content identity

The lock is keyed per *(source, resolved commit)* — one entry for each distinct ref
a source's bindings resolve to. The common case, where no binding overrides the ref,
collapses to one entry per source, exactly as before: the lock is *take-neutral*
toward bindings, so however many targets bind a source and however each subsets it
with `take`, they all share the single entry at the source's own ref.
A ref-overriding binding is the one thing that splits it — each distinct
`branch`/`tag`/`rev` resolves to its own commit and records its own entry. Each entry
carries an optional ref discriminator that is *present only on an override* and absent
on the default, so a config that names no binding refs serializes byte-for-byte as it
did before per-target versions existed.

Each locked entry records its name, the remote (or URL), a resolved field, the commit,
the artifact digest, and a config digest. That last one — a `blake3` over the
export-affecting settings — is how phora notices that you changed *what* ships even
when the upstream commit did not move; it is computed from the *source's* offer —
its `include`/`exclude`/`root` (a URL source's digest covers the full archive) —
never from a binding's `take`, and is therefore shared across every split of a
source. The artifact digest, by contrast, is recomputed per entry at that entry's own
commit.

The split that follows is worth holding onto. The lock answers "which commit?", and a
binding's *take* has nothing to do with that — so a change that narrows one target's
`take` moves no commit and leaves `phora.lock` untouched; it shows up only as a
different per-artifact digest in the registry, and that is what tells sync to replace
the deployed slice. A *ref* override is the exception: it is the one binding-level
change that does answer "which commit?", so it adds (or moves) an entry. The offer
that lives on the source still flows into the config digest and can advance the
lock's reuse decision; the take that lives on a binding re-projects through the
registry alone.

Deciding whether a locked entry can be reused is where the two source kinds differ,
and it is worth being precise:

- A git or host source matches when its resolved remote identity (normalized, so
  https/ssh/literal forms agree), the binding's *effective* refspec, and its config
  digest all match the lock — the effective ref being the binding's override, or the
  source's ref when the binding does not override.
- A URL source has no meaningful remote-vs-refspec story, so it matches on its URL
  identity plus its config digest. Its lock carries the literal URL and a `"url"`
  sentinel in the resolved field. The synthetic commit is content-addressed, so the
  URL plus the config digest is a complete identity.

The consequence is the no-op model you feel at the command line: a plain `sync`
finds the lock matches and does not re-download; only `update` (or `--force`)
re-fetches, and even then identical bytes reproduce the same commit and the lock
does not move.

### Templating and the two digests

[Templating](#templating-per-machine-values) renders `*.tmpl` files at stage time —
in the staging directory, before the atomic swap — so the artifact materialized into
the target is already the rendered output. Two digests then part ways, and the split
is the whole trick:

- The *manifest* hashes the *rendered* bytes. That is what `phora verify` and drift
  detection compare against, so they check the file you actually deployed.
- The *lock's* artifact digest is computed over *source* bytes, independent of any
  vars. So two machines with different vars resolve to the same commit and write
  byte-identical locks — the reproducibility check never depends on a machine's local
  values.

To know *when* to re-render, phora records a per-artifact digest of the effective
(base overlaid by local) vars. Change a var and that digest changes, so the next
sync re-stages and redeploys the artifact even though no commit moved; leave the vars
alone and the artifact is a no-op. `phora rebuild-registry` reconciles against the
same merged vars, so a templated artifact comes back clean rather than reading as
modified. Rendering is strict and per-artifact: an undefined variable aborts just
that artifact's export, leaving its siblings deployed.

### Versioning, the git way

phora has no version solver, no semver ranges, and no package index. A *version*
is just a git commit, and the machinery for handling versions is the machinery you
already have for handling commits.

- For a git source the version is whatever `branch`/`tag`/`rev` resolves to in the
  mirror; for a URL source it is the content-addressed synthetic commit of the
  downloaded bytes. Either way the lock records one 40-hex commit, and that pin is
  the version — reproducible across machines, advanced only by `update`.
- Moving a version is editing a ref or a URL: bump `tag = "v0.56.0"` (or point a
  URL at the new tarball) and re-lock. There is nothing to resolve against a
  registry — the ref is the request and the commit is the answer.
- Keeping several versions costs almost nothing. The mirror is git's object store:
  immutable, content-addressed, deduplicated. `v0.55` and `v0.56` are two commits
  that share every unchanged blob and tree; importing the second writes only what
  actually differs and overwrites nothing.

Which version a deployment uses is a function of the *(project, binding)* pair, not
of the source's name. Across locations: each is its own `ProjectId` with its own
`phora.lock` and registry partition, so the same source name can carry different
versions in different checkouts with no special handling — `fzf` pinned to `v0.55`
here and `v0.56` there are just two locks selecting two commits out of the one shared
mirror. Within a *single* location: a binding's `branch`/`tag`/`rev` override pins its
target independently, so two bindings of one source — distinct table key, distinct
ref — hold both versions side by side in the same project (the `stable`/`canary` pair
above), each its own lock entry over the one mirror. You never spell the version into
the source name — the lock does that, per binding.

This is where phora diverges most from a conventional package manager. There is no
central index to publish to, no resolver to satisfy a version range, and no
per-project install tree to deduplicate after the fact. Git's content-addressed
object store is the cache, its commit graph is the version history, and an install
is a reflink out of that store. Everything phora adds on top — the lock, the
registry, identity — is bookkeeping about which commit goes where; the storage and
the versioning fall out of git for free.

### Verification

The registry stores, per deployed artifact — keyed by binding identity within its
target — the underlying source, the commit, the artifact digest, the layout and
policy in effect, and a manifest of every file with its size, mtime, and `blake3`
hash. `phora verify` re-hashes the files on disk and compares them against that
manifest, so it catches both content edits and missing files. `phora list` reads
the same records to show per-target status, and `phora where` queries them in
reverse — given a source, artifact, commit, or digest, it tells you where things
came from. Because every record carries its underlying source, a `phora where
--source <s>` finds all of `s`'s slices even when they were bound under different
identities (table keys).

If the registry and the on-disk reality fall out of step — say you hand-edited the
state root, or restored an old backup — `phora rebuild-registry` reconstructs the
records from the lock plus what is actually deployed.

### Execution model: parallel reads, one serial writer

The read-and-resolve half of a sync runs in parallel; the half that *writes* runs
serially behind a lock. phora fetches, resolves, and digests its sources across a
rayon thread pool — sized to one thread per resolution unit, capped at twice the
core count (from `available_parallelism`, fallback 8) and overridable with `phora
sync --jobs N` / `-j N` — then deploys the results one at a time. The parallelism is
purely a throughput win — the deploy loop and the recorded state are byte-identical
to the old serial path, so nothing about a lock or a registry record depends on how
many threads ran.

Fetch is network-bound — resolving a source is mostly I/O wait — so overlapping
those waits across the pool is the available throughput win. The concurrency phora
takes seriously is mutual exclusion and crash safety:

- One writer per project. A state-mutating command (`sync`, `eject`, `uneject`,
  `rebuild-registry`) takes an exclusive OS lock on a `state.lock` file in the
  project's state root, held for the whole run. A second phora invoked against the
  same project while the first runs fails fast with "another phora process is
  running" and exits `75` (`EX_TEMPFAIL`), rather than racing it into a corrupt
  registry.
- One writer per mirror. The git mirror cache lives under the *cache* root, which
  is shared across every project — so the per-project state lock alone would not
  stop two phora runs in *different* projects from fetching the same bare mirror at
  once. Each fetch therefore takes a blocking advisory flock on
  `<MirrorKey>.git.lock` before touching the mirror. Parallel fetches to *distinct*
  mirrors run concurrently; two fetches of the *same* mirror serialize on this lock.
  Git-mode fetch is idempotent and dedupes to one fetch per mirror; a url-mode source
  still fetches per source even when its key coincides, because each validates its own
  integrity digest and deduping would silently bypass the second pin. Mirror creation
  is atomic — built in a temp directory and renamed into place — so a crashed clone
  never leaves a half-written mirror for the next run to trip over.
- Crash safety via a journal. Before mutating a target, phora records the intended
  operation in a deploy journal. If a run is interrupted — a crash, a Ctrl-C, a
  killed terminal — the next run's recovery sweep reads the journal and cleans up
  the partial state before doing anything else. This is why an interrupted sync
  leaves you recoverable rather than half-deployed.
- Interrupt-aware fetches. The git network operations check an interrupt flag, so
  cancelling mid-fetch unwinds without leaving a broken mirror.

Deploy stays sequential on purpose: it is the write side, governed by the one-writer
lock and the journal, where ordering and crash safety matter more than overlap. The
parallelism lives where the waiting is.

### Hook dispatch and recording

[Hooks](#hooks-running-commands-after-a-sync) run after the deploy commits, never
during it — the files are on disk and the journal settled before a hook sees them,
which is why an `on_change` hook can read what it just received. Each hook has a
stable id derived from its scope, shell, and command (the shell is part of the id,
so the same command under two shells is two hooks, and the same command listed twice
under one shell is deduplicated to a single run).

"Recorded on success" has a concrete shape: when a hook succeeds, phora stores,
per hook, the set of artifact content digests that were live at that success. The
next sync recomputes the current digest-set for the hook's scope and skips the
`on_change` when it matches — that is what makes a no-op sync quiet. A non-zero exit
records nothing, so the set never matches and the hook re-fires until it succeeds.
The global `post_sync` carries `when = "always"`, which bypasses the digest-set
check and runs every time.

The trust boundary is structural, not a scan: hooks are only ever read from the
consumer's own `phora.toml`/`phora.local.toml`. A synced source tree is projected as
content and never parsed as configuration, so a `phora.toml` that rides along inside
a source can declare any hook it likes and none of it will ever run.

### Composing a dependency graph

A transitive dependency's `phora.toml` is parsed exactly once, into a manifest DTO
that keeps only its declarative `[sources]` and `[targets]`. Its trust-control and
global `[hooks]` fields are dropped on the way in — hooks are retained out of band as
an uninterpreted value for the admission gate, never merged into your config — so no
trust state can ride in from the dependency itself.

Two keys hold the graph together. A *fetch node* is the triple of a normalized
remote URL, its ref, and its resolved commit; it is the dedup key, so a diamond that
reaches the same triple collapses to one fetch and equivalent URL spellings share a
node. An *instance* is `(parent, source name, anchor target, fetch node)` — the same
fetched node mounted at two anchors is two instances. The instance's `stable_key` is
a length-prefixed blake3 over those fields, truncated to 16 hex; that key is what
namespaces a dependency's sources and hooks so two dependencies with a same-named
inner source never collapse into one node. Confinement is enforced by resolving the
anchor and checking every composed destination against it segment by segment,
rejecting `..`, absolute paths, unsafe components, and symlinked ancestors, plus a
fixed set of protected paths (your config, lock, `.git`, and phora's cache/state
roots).

Hook trust is a content gate, not a name check. Each composed hook is reduced to a
*preimage* — a commit-bound blake3 over its command — and the gate admits it only
when that preimage matches a `[[trusted_hooks]]` entry in your lock. Every discovered
hook is also recorded as a `[[candidate_hooks]]` entry carrying its command and
resolved commit, which is what `phora trust --list` reads and what the inspect-before-trust
diff (last trusted commit → candidate commit) is computed from. Because trust is keyed
on the preimage, any change to the command — or to the dependency commit it rode in
on — invalidates the match, and the hook reverts to a stripped candidate until you
re-approve. The lock keeps `trusted_hooks` and `candidate_hooks` skip-serialized when
empty, so a config with no transitive hooks serializes byte-for-byte as it did before
any of this existed. Under `--frozen`, the resolver consults the lock and refuses to
fetch, erroring on the first source — at any depth — that is not already pinned.

### Integrity boundaries

It is worth naming the one place the content guarantee deliberately stops: linked
artifacts (`deploy = "link"`). Because a symlink points at a live working tree
whose bytes change underfoot, hashing it would be meaningless, so phora doesn't.
Linked records carry a `linked` marker and no manifest; verify and drift detection
skip them; `--prune` removes an orphaned link by deleting the symlink only;
`rebuild-registry` reconstructs the marker without hashing. Everything else —
every `copy` artifact from a git or URL source — is inside the integrity model.

## When something looks wrong

Most confusion maps to one question, and each question maps to one command.

- "What would a sync actually deploy, before I run it?" Run `phora preview`. It
  prints the whole projection tree — per target, each binding's identity, the
  artifacts it selects, and where each would land under the layout — without writing
  anything or touching the network. A collapsed directory shows a trailing slash
  (`nvim/`), a rename shows `src -> dest -> destination`, and a `take` glob that
  matched nothing surfaces as a warning with a nearest-leaf suggestion. Commits come
  from the lock and trees from the mirror, so an unsynced source is *annotated* (`not
  locked`, `needs sync`, `link working tree gone`) rather than fetched, and the
  command still exits 0; predicted flat-layout collisions show up as warnings.
  `--files` expands each artifact to its files, `--json` emits the plan as a document
  (carrying `rename`, `collapsed`, and per-target `warnings` fields), and
  `--target`/`--source` narrow the view. Where `check-match` answers one path,
  `preview` shows the whole tree.
- "Did my `include`/`exclude` actually match this path?" Run `phora check-match
  --source <source> <path>`. It answers yes/no for that exact path against that
  source's offer rules, which beats re-reading globs.
- "Which include offered this path, and how did the binding's `take` resolve it?" Run
  `phora explain <target> <source> [path]`. Offline, from the lock, it attributes a
  single path to the include that offered it and shows the take outcome (kept,
  renamed, collapsed, or dropped); with no path it lists every offered leaf and its
  fate. A path the offer does not expose reports `outside the offer` with a
  nearest-leaf `did you mean:`. The structured selection diagnostics — a rejected
  binding scope, a take that would widen the offer, a rename escaping the deploy root
  — all point here.
- "Is what's on disk really what phora put there?" Run `phora verify`. It re-hashes
  every tracked file and exits non-zero on the first mismatch. A file you edited by
  hand will show up here; so will a truncated or replaced one.
- "Where did this file come from?" Run `phora where --source <source>` (or query by
  artifact, commit, or digest). It reverse-looks-up the registry. When nothing
  matches it says so — naming the active filter and pointing you at `phora
  sync`/`phora preview` — rather than printing blank; `phora list` likewise marks an
  undeployed target `(nothing deployed — run \`phora sync\`)` instead of a bare
  header.
- "phora won't run — it says another process is running." That is the `state.lock`
  doing its job. Either another phora really is running for this project, or a
  previous one died without releasing the lock; once you're sure none is running,
  the stale lock clears on the next clean run.
- "Every URL-sourced file shows as modified." Your filesystem is mangling
  timestamps — some clamp or round old mtimes. phora's epoch+1 import time dodges
  the common case; if you still hit it, that's a bug worth filing.
- "My registry looks wrong after I touched the state root." Run `phora
  rebuild-registry` to reconstruct it from the lock plus the deployed files.
- "Sync keeps stopping on a file it didn't deploy." That is the conflict prompt
  protecting a foreign or hand-modified file. Decide per file (skip / overwrite /
  eject / abort), or pass `--force` to overwrite, or `eject` to keep the file and
  stop managing that artifact.

## Where to look next

- The [README](README.md) is the flag-level reference and the fastest path to a
  specific option.
- [`phora.example.toml`](phora.example.toml) is a complete, annotated config you
  can crib from.
- The [scrut suites](tests/scrut/) drive the shipped binary end to end against real
  upstreams and double as runnable, CI-verified usage docs — this guide with
  assertions instead of prose. [`showcase.md`](tests/scrut/showcase.md) pins two of
  Anthropic's public Claude Code skills and walks the link-mode editing loop;
  [`release-assets.md`](tests/scrut/release-assets.md),
  [`versions.md`](tests/scrut/versions.md), and [`drift.md`](tests/scrut/drift.md)
  cover digest-checked tarballs, one source at two tags, and what happens when a
  deployed file is edited behind phora's back;
  [`mapped.md`](tests/scrut/mapped.md) fans one `AGENTS.md` out to the names every
  agent tool wants with `take` renames, [`hooks.md`](tests/scrut/hooks.md) runs
  commands after a sync, [`templates.md`](tests/scrut/templates.md) fills one config
  in per machine, and [`transitive.md`](tests/scrut/transitive.md) imports a
  dependency that carries its own — composing it under the anchor, isolating storage
  with `[paths]`, and stripping its untrusted hook.
- The source is organized along the pipeline: `config.rs` (parsing and modes),
  `config/source.rs` and `config/target.rs` (the offer and the take/collapse DTOs),
  `config/transitive.rs` (the dependency manifest DTO and graph keys),
  `kernel/selection.rs` (the offer compiler), `kernel/take.rs` (take resolution),
  `kernel/collapse.rs` (directory collapse), `diagnostic.rs` (structured selection
  diagnostics and `did_you_mean`), `source.rs` (the git backend, the
  synthetic-commit import, projection), `http.rs` (download and digest), `archive.rs`
  (extraction and the path guard), `backend.rs` (the source-mode router), `sync.rs`
  (the orchestration), `sync/transitive.rs` and `sync/confine.rs` (composition and
  destination confinement), `lock.rs` and `registry.rs` (identity and records).
