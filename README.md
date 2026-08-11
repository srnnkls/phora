# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

Phora keeps selected files from git repositories, local directories, and HTTPS downloads in sync
with the directories on your machine that consume them. You decide which files from each source go
to each destination; phora pins remote content to exact commits, verifies deployed files by content
hash, and recovers after an interruption.

Use phora when shared configuration, editor setups, prompt or skill bundles, or release assets live
in one or more repositories but must appear in the places other tools expect them.

Use this README for installation and the complete command and configuration reference. Read
[the guide](GUIDE.md) for the mental model and the reasoning behind the design, and
[the use cases](USE-CASES.md) for situation-based configurations.

## Installation

Choose one of the following installation methods.

### Shell (Linux, macOS)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/srnnkls/phora/releases/latest/download/phora-installer.sh | sh
```

### Homebrew

```sh
brew install srnnkls/phora/phora
```

### Cargo

```sh
cargo install phora
```

### Prebuilt binaries

Download an archive for your platform from the [releases page](https://github.com/srnnkls/phora/releases). Prebuilt targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Every release artifact ships with a SHA-256 checksum and an SLSA build-provenance attestation,
verifiable with `gh attestation verify <file> --repo srnnkls/phora`.

### From source

```sh
cargo install --path .
```

From a repository checkout, this requires a Rust toolchain with Rust 2024 edition support.

## Getting started

After installing phora, create a disposable project with a complete configuration:

```sh
mkdir phora-quickstart
cd phora-quickstart
cat > phora.toml <<'TOML'
version = 1

[sources.phora]
repo = "srnnkls/phora"
branch = "main"
include = ["README.md"]

[targets.demo]
path = "./out"
sources = ["phora"]
TOML
```

The source is a public repository and the target is the local `out` directory, so the example runs
unchanged:

```console
$ phora sync
sync complete

$ phora list
demo:
  phora/README.md  ✓ clean

$ phora verify
all verified
```

`phora sync` resolves `main` to an exact commit, records it in `phora.lock`, and deploys the
selected file into `out`. `phora list` reports what was deployed, and `phora verify` re-hashes the
deployed bytes against phora's record. Verification exits non-zero on a mismatch, so it also serves
as a CI check.

From here, `phora preview` shows what a sync would do before it does it, and `phora add` and
`phora bind` edit the configuration for you instead of by hand.

## Concepts

Phora moves content through a short pipeline: a source publishes an offer, a binding selects from
that offer, and phora deploys the resulting artifacts into a target.

| Term | Meaning |
| --- | --- |
| *source* | a git repository, local directory, or HTTPS download phora reads content from, pinned by `branch`, `tag`, or `rev` |
| *offer* | the paths a source makes available, after applying `root`, `include`, and `exclude` |
| *artifact* | one file or directory phora manages as a single deployment unit, named by its full offered path |
| *target* | the local directory artifacts are deployed into, drawing only from its explicit `sources` allow-list |
| *binding* | the connection from one source to one target |
| *take* | the subset and renaming rules one binding applies to the offer |
| *collapse* | whether a binding deploys the taken paths separately or as one directory |
| *layout* | the rule that places an artifact's path inside a target |
| *lock* | `phora.lock`, which records the exact commit resolved for each source |
| *registry* | the machine-local record of what phora deployed, where it landed, and which commit and content digest produced it |

`phora update` moves the commits recorded in the lock. The registry is what lets phora detect local
changes, conflicting files, and deployments the configuration no longer selects. See
[State & locations](#state--locations) for where each lives.

A source that carries its own `phora.toml` can be used as a *transitive dependency*. Set
`transitive = true` on the source and name it in a target's `imports`, and that source's own targets
are deployed beneath the importing target. See
[Transitive dependencies](GUIDE.md#transitive-dependencies) for composition and hook trust.

## Command reference

Every command reads `phora.toml` from the working directory, overlaid by `phora.local.toml` when
one is present. Commands that edit configuration write `phora.toml` unless `--local` sends them to
the overlay.

### Deployment

#### `phora sync`

Fetch every source, resolve its ref to a commit, and deploy its artifacts into their targets.
Repeated syncs are cheap: an unchanged lock refetches nothing.

| Flag | Meaning |
| --- | --- |
| `--prune` | delete artifacts the configuration no longer selects |
| `--force` | overwrite locally-modified files instead of prompting |
| `--no-hooks` | deploy without running any hook |
| `--no-transitive-hooks` | deploy composed dependencies but run none of their hooks; your own hooks still run |
| `--frozen` | refuse to fetch or re-resolve; every source, nested dependencies included, must already be pinned in the lock |
| `--fast-forward` | delete deployed artifacts that disappear when the pin moves, rather than erroring |
| `-j`, `--jobs <N>` | set the resolution worker count; when omitted, derive it from the work |

#### `phora update [SOURCE]`

Re-resolve to the latest commit and then sync. With no argument it bumps every source; with a
source name it bumps that one.

| Flag | Meaning |
| --- | --- |
| `--fast-forward` | delete deployed artifacts that disappear when the updated pin moves |

### Inspection

#### `phora list`

Report per-target deployment state, one line per artifact.

| Flag | Meaning |
| --- | --- |
| `--plan` | print a pointer to `phora sync`; use `phora preview` for a dry run |
| `--orphans` | report registry records whose target left the configuration, with their on-disk paths |

Each artifact has exactly one state: `✓ clean`; `outdated` when the configuration or variables moved
ahead of the deployed copy; `modified` when its files changed outside phora; `foreign` when a file
phora did not deploy sits where the artifact wants to land; `missing`; `ejected`; or `linked`.

#### `phora verify`

Re-hash every deployed file against its recorded digest. Exits non-zero on any mismatch. Linked
artifacts carry no per-file hashes and are skipped. Takes no flags.

#### `phora where`

Reverse-lookup over the registry. Every flag given is an AND constraint; with none, it lists
everything deployed.

| Flag | Meaning |
| --- | --- |
| `--source <NAME>` | show artifacts deployed from this source |
| `--artifact <NAME>` | show artifacts under this offered path |
| `--commit <SHA>` | show artifacts deployed at this commit |
| `--digest <DIGEST>` | show artifacts with this content digest |

#### `phora preview`

Show the offline dry run. Per target it reports each binding's identity, the artifacts it selects,
and where they would land, taking commits from the lock and trees from the mirror without touching
the network. A collapsed directory carries a trailing slash. An unsynced source is annotated
(`not locked`, `needs sync`, or `link working tree gone`) rather than fetched, and the command still
exits 0. Predicted flat-layout collisions render as warnings.

| Flag | Meaning |
| --- | --- |
| `--target <NAME>` | restrict to one target |
| `--source <NAME>` | restrict to one source |
| `--files` | expand each artifact to the files it would deploy |
| `--json` | emit the same plan as a machine-readable document |

#### `phora explain <TARGET> <SOURCE> [PATH]`

Attribute a deployment decision offline. With a path, it reports which `include` or `exclude` rule
offered that path and how the binding's `take` resolved it; without one, it summarizes the whole
offer. Takes no flags.

#### `phora check-match --source <SOURCE> <PATH>`

Probe one path against a source's `include` and `exclude` rules and print the verdict alongside the
rules themselves. Where `explain` accounts for the whole binding, `check-match` isolates the offer.

### Editing configuration

#### `phora add <URL>`

Parse a URL, add it as a source, and optionally bind it to targets. Shorthands persist as a forge
source (`host` + `repo`), not an expanded URL; scheme and scp-style URLs stay literal as `git`.

| Flag | Meaning |
| --- | --- |
| `--to <TARGET>` | bind the new source into this target; repeatable |
| `--name <NAME>` | set the source name; when omitted, derive it from the URL |
| `--branch <BRANCH>` | pin the source to a branch |
| `--tag <TAG>` | pin the source to a tag |
| `--root <PATH>` | re-anchor the new source's offer at this subdirectory |
| `--include <GLOB>` | keep only matching paths in the new source's offer; repeatable |
| `--exclude <GLOB>` | drop matching paths from the new source's offer; repeatable |
| `--as <IDENTITY>` | set the binding identity; requires exactly one `--to` |
| `--local` | write `phora.local.toml`, recording the path as a local source |
| `--symlink` | write `phora.local.toml` and set `deploy = "link"` to live-link the working tree |

```sh
phora add owner/repo --name myconfigs --branch main --root configs  # host = "github"
phora add github:srnnkls/tropos             # colon alias, any built-in forge
phora add github.com/me/dotfiles            # domain shorthand
phora add https://github.com/me/dotfiles.git  # stays literal: git = "…"
phora add git@github.com:me/dotfiles.git --tag v1.2
```

The colon alias caps at `owner/repo`; segments past that become `root`, so a deep GitLab subgroup
belongs in the config's `repo` key (`repo = "group/sub/proj"`) rather than the alias.

With no `--to`, `phora add` creates `[targets.default]` if needed (path `.`, flat layout) and binds
the source to it. Set `[defaults] auto_target = false` to make bare `add` declare the source without
deploying it. An explicit `--to` always uses only the named targets and never touches
`[targets.default]`. If a named target does not exist, phora offers to create it (flat layout, path
`./<name>`) when run interactively; in a non-interactive run it exits with a `phora target add`
suggestion. Configuration edits are atomic: if the command fails, it leaves the configuration
unchanged.

#### `phora rm <NAME>`

Remove a source and scrub it from every target's `sources`. An alias for `phora source rm`. Because
the scrub spans both config files, it takes no `--local`.

#### `phora source add <URL>`

Identical to top-level `add` minus the binding flags: it accepts `--name`, `--branch`, `--tag`,
`--root`, `--include`, `--exclude`, `--local`, and `--symlink`.

#### `phora source rm <NAME>`

As `phora rm`.

#### `phora source list`

List every source over the merged config: name, resolved remote, and selected branch, tag, or
commit.

#### `phora source show <NAME>`

Show one source's effective config and the targets that deploy it.

#### `phora target add <NAME> --path <PATH>`

Declare a target.

| Flag | Meaning |
| --- | --- |
| `--path <PATH>` | set the deployment directory; required |
| `--layout <LAYOUT>` | set the layout to `flat`, `by-source`, or `prefixed`; defaults to `flat` |
| `--local` | write `phora.local.toml` |

#### `phora target rm <NAME>`

Remove a target.

| Flag | Meaning |
| --- | --- |
| `--local` | write `phora.local.toml` |
| `--force` | remove the block even while the registry still has deployed artifacts |

#### `phora target list`

List every target over the merged config: name, path, bound sources.

#### `phora target show <NAME>`

Show one target's effective config, resolved sources, and deployment state.

#### `phora bind <SOURCE>... --to <TARGET>`

Add bindings to a target's `sources`. With no refinement it appends a bare source name to the
target's flat list, or writes `name = {}` if that target is already a keyed table; any refinement
writes a keyed table entry.

| Flag | Meaning |
| --- | --- |
| `--to <TARGET>` | choose the target for these bindings; required |
| `--as <IDENTITY>` | set the binding identity; valid for one source only |
| `--take <ENTRY>` | subset or rename the offer: a leaf, a glob, or `src=dest`; repeatable |
| `--branch <BRANCH>` | pin this binding to a branch, overriding the source's ref for this target |
| `--tag <TAG>` | pin this binding to a tag |
| `--rev <SHA>` | pin this binding to a full commit id |
| `--root <PATH>` | write `root` onto each named `[sources.<name>]`, since `root` is source-owned; error if a named source is not declared in the file being edited |
| `--local` | write `phora.local.toml` |

#### `phora unbind <IDENTITY>... --from <TARGET>`

Remove bindings by their identity. Emptying a target's list leaves it deploying nothing.

| Flag | Meaning |
| --- | --- |
| `--from <TARGET>` | choose the target from which to remove the bindings; required |
| `--local` | write `phora.local.toml` |

```sh
phora bind dotfiles --to neovim                            # bare binding, whole offer
phora bind dotfiles --to neovim --as nvim --take 'nvim/**' # one slice under identity `nvim`
phora unbind nvim --from neovim
```

### Managing individual artifacts

#### `phora eject <ARTIFACT> --source <SOURCE> --target <TARGET>`

Stop managing an artifact while keeping its files on disk. Both flags are required.

#### `phora uneject <ARTIFACT> --source <SOURCE> --target <TARGET>`

Resume managing a previously ejected artifact. Both flags are required.

### Transitive hooks

#### `phora trust [SOURCE]`

Inspect and approve hooks discovered in composed dependencies. With a source and no flags, it shows
that dependency's hooks and prompts per hook; with no source, it lists every discovered hook across
all sources.

| Flag | Meaning |
| --- | --- |
| `--list` | show the hooks without approving anything |
| `--revoke` | drop every approval recorded for the named source |
| `--show <PATH>` | print a dependency file, or list a dependency directory, at the pinned commit, offline; requires a source |

Every listing resolves offline from the cache mirror. Approval lives in your `phora.lock` and is
pinned to both the command and the exact dependency commit it came from, so a changed hook drops
back to needing approval. The guide explains the trust model in
[Transitive dependencies](GUIDE.md#transitive-dependencies).

### Maintenance

#### `phora rebuild-registry`

Reconstruct the registry from the lock and the on-disk targets. Takes no flags. Reach for it when
the registry is lost or inconsistent with what is actually deployed.

## Configuration

Phora reads `phora.toml` from the working directory, optionally overlaid by `phora.local.toml`
(same schema, local values win per key). Unknown keys are a parse error everywhere.
[`phora.example.toml`](phora.example.toml) and
[`phora.local.example.toml`](phora.local.example.toml) are complete annotated examples.

```toml
version = 1

[hosts.github]
auth = { type = "token", env = "GITHUB_TOKEN" }

[sources.dotfiles]
host = "github"          # forge remote: host + repo
repo = "me/dotfiles"
branch = "main"          # or tag / rev; omit all to follow the repo's default branch
root = "modules"         # re-anchor the offer at this subdirectory
include = ["editor"]     # source-owned offer: include − exclude, gitignore syntax
exclude = ["**/*.bak"]

[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]   # flat list: every source consumed at its whole offer
layout = "flat"

[targets.editor]
path = "~/.config/editor"

[targets.editor.sources]         # keyed table: the key is the binding identity
nvim = { source = "dotfiles", take = ["nvim/**"] }
```

### Top level

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `version` | integer | — | schema version; required |
| `protocol` | `"https"` \| `"ssh"` | `"https"` | which remote template forge sources resolve through; overridable per source |

### `[paths]`

Pins where phora keeps its two shared trees, overriding both the `XDG_*` variables and the platform
defaults. A relative value resolves under the project root; an absolute value is used as-is. The
configured path is itself the root, with no `phora` leaf appended.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `cache` | path | platform cache root | git mirrors live under `<root>/git/` |
| `state` | path | platform state root | registry, locks, and journal live under `<root>/projects/` |

### `[defaults]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `auto_target` | bool | `true` | a bare `phora add` (no `--to`) ensures `[targets.default]` and binds into it; `false` makes bare `add` declare-only |

### `[vars]`

A flat table of string values available to templates. Templating is the `.tmpl` suffix convention:
a source file named `*.tmpl` is rendered with [minijinja](https://docs.rs/minijinja) and lands with
the suffix stripped, while every other file copies byte for byte. Rendering is strict — referencing
an undefined variable aborts that artifact's export, and its siblings still deploy. A binding's
`template` key widens or disables the opt-in. `phora.local.toml` overrides vars per key, so each
machine fills in its own. The guide explains the two-digest model in
[the templating chapter](GUIDE.md).

```toml
[vars]
greeting = "hello"
editor = "nvim"
```

### `[hosts.<alias>]`

Hosts supply remote URL templates and auth. `github`, `gitlab`, `codeberg`, `sr.ht`, and
`bitbucket` ship built in with both https and ssh shapes, so no template is needed for them; a
`[hosts.X]` block adds a new forge or overrides a built-in's `remote` or `auth`. Changing a host's
`remote` re-points every source on that host with no per-source edit.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `remote` | string \| `{ https, ssh }` | built-in for shipped forges | URL templates; a bare string is the https template alone |
| `auth` | `{ type = "token", env }` \| `{ type = "ssh", key }` | none | token from an environment variable, or an ssh key path |

A template fills three placeholders:

| Placeholder | Value |
| ----------- | ----- |
| `{path}` | the source's `repo` (`owner/repo`), verbatim |
| `{owner}` | the first `/`-segment of `repo` |
| `{repo}` | the remainder, so `{owner}/{repo}` ≡ `{path}` at any depth |

```toml
[hosts.company]
remote = { https = "https://git.company.com/{path}.git", ssh = "git@git.company.com:{path}.git" }
auth = { type = "ssh", key = "~/.ssh/id_ed25519" }
```

Selecting `ssh` against a host whose `remote` has no `ssh` key is a config error. `protocol` is
ignored for literal `git` and local `path` sources.

### `[sources.<name>]`

A source declares its remote in exactly one kind — forge, local, literal, or url — and never more
than one.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `host` | string | `"github"` when `repo` is set | forge alias resolved against the host registry |
| `repo` | string | — | `owner/repo` on the forge; nested paths are fine |
| `path` | string | — | local filesystem path, used verbatim as the remote |
| `git` | string | — | literal remote: https, `ssh://`, or scp-style `git@host:path` |
| `url` | string | — | downloadable resource imported as a single snapshot |
| `digest` | string | none | `sha256:<hex>` or `blake3:<hex>`, verified before a url source is extracted |
| `protocol` | `"https"` \| `"ssh"` | top-level `protocol` | per-source template selection for a forge source |
| `branch` | string | repo's default branch | pin to a branch |
| `tag` | string | — | pin to a tag |
| `rev` | string | — | pin to a commit, written in full as 40 or 64 hex characters; an abbreviated sha is an error. Precedence within a source is `rev` > `tag` > `branch` |
| `root` | path | source root | re-anchor the offer at this subdirectory |
| `include` | array of glob | everything but `.git/` | gitignore-syntax patterns kept in the offer |
| `exclude` | array of glob | empty | gitignore-syntax patterns pruned from the offer; exclude wins, and there is no `!` re-inclusion |
| `allow_symlinks` | bool | `false` | export symlinks found in the source tree |
| `preserve_executable` | bool | `true` | carry the executable bit onto deployed files |
| `deploy` | `"copy"` \| `"link"` | `"copy"` | materialize a content-hashed copy, or symlink the source's live working tree |
| `transitive` | bool | `false` | recurse into the source's own `phora.toml` so it can be imported |

A url source is a single imported snapshot, so `branch`, `tag`, `rev`, and `root` are config errors
on one; `include` and `exclude` still select files. Archives in tar, tar.gz/tgz, and zip are
recognized by magic bytes and a lone top-level directory is stripped, so version-stamped release
tarballs need no per-version `root`; anything else becomes one file named from the URL basename.

A `link` source must be a local path — `deploy = "link"` on a remote is a config error naming the
source, and a relative path counts as local only if it already exists relative to the working
directory. Link mode is allowed in either file, but a committed link over an absolute path prints a
non-fatal portability warning.

`git = "/abs/local"` remains an accepted spelling for a local source, and `host` + `path` for a
forge one; both emit a deprecation warning pointing at `path` and `repo`. A bare
`path = "owner/repo"` with no host means a local path — the github shorthand is bare `repo`.

The guide covers the kinds, and link mode's trade-off, in [Sources](GUIDE.md#sources).

### `[targets.<name>]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `path` | path | — | deployment directory; required |
| `sources` | array of string \| table | none | the bindings this target deploys; see below |
| `layout` | string \| table | `"flat"` | how an artifact's path is composed; see below |
| `imports` | array of string | none | transitive sources whose own targets compose under this target's path |
| `take` | table | none | mount-level subset of an imported dependency, keyed by the composed anchor |
| `collapse` | table | none | mount-level collapse override for an imported dependency, keyed by the composed anchor |
| `hooks` | table | none | see [Hooks](#hooks) |

`sources` is an explicit allow-list: an omitted key or `[]` deploys nothing. It takes one of two
forms, never both at once. A flat list of bare names consumes each source at its whole offer. A
keyed table under `[targets.<t>.sources]` maps a binding identity to a table refining that one
binding; a bare entry inside a keyed target is written `name = {}`. Prefer `phora bind` and
`phora unbind` over editing the list by hand.

For `take` and `collapse`, an omitted table inherits, a present-but-empty one clears an inherited
table back to take-all, and a non-empty local table replaces the base table wholesale on overlay.

### Bindings: `[targets.<name>.sources]`

The table key is the binding identity. It defaults to the source name, and `source` is written only
when the two diverge. Phora uses the identity in registry records and in the `by-source` and
`prefixed` layouts. Because the identity is a TOML key, it may appear only once. Bindings are
processed alphabetically by identity, regardless of their order in a flat list.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `source` | string | the table key | the source this binding draws from |
| `take` | array | whole offer | subsets and renames the offer; `take = []` takes nothing |
| `collapse` | bool | algorithmic | `false` forces per-leaf artifacts, `true` demands one directory artifact |
| `template` | array of glob \| `false` | `.tmpl` suffix only | widen rendering to more paths, or disable it entirely |
| `branch` | string | the source's ref | pin this binding to a branch for this target alone |
| `tag` | string | the source's ref | pin this binding to a tag |
| `rev` | string | the source's ref | pin this binding to a commit, written in full as 40 or 64 hex characters; precedence is `rev` > `tag` > `branch` |

A `take` entry is a literal leaf kept verbatim, a gitignore glob that expands over the offer set
only, or a rename table `{ "src" = "dest" }` that consumes one offered leaf and emits it at `dest`
instead. A literal or rename `src` that is not offered is a hard error, since a take may not widen
the offer; a glob matching nothing warns without failing.

```toml
[targets.neovim.sources]
nvim = { source = "dotfiles", take = ["nvim/**", { "nvim/init.lua" = "init.lua" }] }
```

Each distinct binding ref gets its own lock entry at its own commit, while bindings that do not
override the ref share the source's single entry. Resolution still does one fetch per source,
covering every ref its bindings name.

`root`, `include`, `exclude`, and `map` are not valid binding keys, and setting one is a parse
error. Put `root`, `include`, and `exclude` under `[sources.<name>]`, and express a rename with the
`take` table form; the error names where the key belongs. A binding backed by a url source accepts
no refinement at all — there is no offer to subset and no ref to resolve — and a `deploy = "link"`
source rejects `branch`, `tag`, and `rev`, since it live-links a working tree rather than a pinned
commit.

The guide works through identity, renaming, and collapse in
[Bindings](GUIDE.md#bindings-per-target-selection).

### Layouts

A layout decides where an artifact `a` from a binding with identity `i` lands inside a target.

| Layout | Path | Notes |
| --- | --- | --- |
| `"flat"` | `a` | the default |
| `"by-source"` | `i/a` | |
| `{ type = "prefixed", separator = "-" }` | `i-a` | `separator` defaults to `-` |

The string forms `"flat"`, `"by-source"`, and `"prefixed"` are equivalent to the table form with
the default separator.

### Hooks

Hooks run shell commands around a sync, and are read only from `phora.toml` and `phora.local.toml`.
A synced source tree that happens to carry a hook-shaped `phora.toml` is inert content: it is never
read as config and never executes. Hooks from an imported transitive dependency are stripped until
you approve them with `phora trust`.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `[hooks] pre_sync` | hook | none | runs once after fetch and before any deploy; a non-zero exit aborts the sync |
| `[hooks] post_sync` | hook | none | runs after every sync |
| `[hooks] when` | `"always"` | `"always"` | controls whether `post_sync` runs again |
| `[targets.<t>.hooks] pre_deploy` | hook | none | gates one target's deploy; every gate runs before any target mutates |
| `[targets.<t>.hooks] pre_deploy_on_fail` | `"abort"` \| `"skip"` | `"abort"` | on a failed gate, halt the whole sync or skip only that target |
| `[targets.<t>.hooks] on_change` | hook | none | runs once after a sync that added or modified this target's artifacts |

A hook can be a command string run by `sh -c`, a table, or an array of strings and tables. Array
entries run in declared order, with duplicates removed. A table is either
`{ run = "...", shell = "..." }`, where `shell` defaults to `sh -c`, or `{ cmd = ["prog", "arg"] }`,
which runs the listed program and arguments directly, with no shell and no variable expansion.

```toml
[hooks]
pre_sync = "test -w ~/.config"
post_sync = "notify-send 'phora synced'"

[targets.neovim.hooks]
on_change = "nvim --headless +'Lazy! sync' +qa"

[targets.editors.hooks]
pre_deploy = { cmd = ["mise", "trust"] }
pre_deploy_on_fail = "skip"
on_change = [
  { run = "stylua .", shell = "bash -c" },
  "git -C ~/.config add -A",
]
```

Every hook inherits phora's full environment, plus the variables for that hook type:

| Hook | Variables |
| --- | --- |
| `pre_sync` | `PHORA_TARGETS` — the target names in this run |
| `pre_deploy` | `PHORA_TARGET`, `PHORA_TARGET_PATH` |
| `on_change` | `PHORA_TARGET`, `PHORA_CHANGED` (newline-separated deployed paths), `PHORA_CHANGED_NAMES` (newline-separated artifact names) |

Artifacts land on disk before `on_change` runs. Hook success is recorded, so a no-op sync runs no
`on_change`; a hook that exits non-zero is not recorded, makes `phora sync` exit non-zero, leaves
the deployed files in place, and re-fires on the next sync. A pure removal does not fire
`on_change` — that is what `post_sync` is for. The guide covers dispatch and recording in
[the hooks chapter](GUIDE.md).

### `phora.local.toml`

The overlay shares the schema and wins per key, and it gets its own companion `phora.local.lock`.
It is machine-local: keep it out of version control. A `sources` list in the overlay replaces the
base target's list wholesale rather than merging per binding, so an overriding target must restate
every binding it wants, `take` included.

```toml
version = 1

[sources.loqui]
path = "/home/me/dev/loqui"   # local checkout, live-linked
deploy = "link"

[vars]
greeting = "hi from this laptop"
```

`phora add --local <path>` writes that overlay for you, recording an absolute `path` for a local
source; `phora add --symlink <path>` does the same and adds `deploy = "link"`. The guide covers the
workflow in [the link-mode chapter](GUIDE.md).

## State & locations

Phora keeps its shared state in the platform's standard cache and state directories:

| Root | Holds | Override | Linux default | macOS default |
| ----- | ---- | -------- | ------------- | ------------- |
| Cache | git mirrors, regenerable | `XDG_CACHE_HOME` | `~/.cache/phora` | `~/Library/Caches/phora` |
| State | registry, locks, journal | `XDG_STATE_HOME` | `~/.local/state/phora` | `~/Library/Application Support/phora` |

A project may pin either root with a [`[paths]`](#paths) table, which makes self-contained project
setups possible without exporting the XDG variables. Resolution precedence is config, then the
`XDG_*` environment, then the platform default.

An `XDG_*` override is honored only when absolute, per the XDG spec; a relative value is ignored and
the platform default applies. macOS has no native state directory, so the state root falls back to
`~/Library/Application Support`. `XDG_DATA_HOME` and `XDG_CONFIG_HOME` are unused: phora has no
portable data payload — the registry is machine-local and mirrors are regenerable — and no global
config root, since config is the project-local `phora.toml`. Neither tree is migrated; mirrors
re-clone and the registry rebuilds on the next sync.

The forge and literal forms of one repository, and its https and ssh remotes alike, share a single
mirror under the cache root's `git/` subdirectory, so switching kind or protocol never re-clones.

## Locking

Each sync takes an exclusive OS lock on `state.lock` under the project's registry directory, so two
syncs of the same project on one machine serialize and never corrupt the registry or journal. A
contended lock exits 75 (`EX_TEMPFAIL`): busy, retry.

That lock is only reliable on a local filesystem. On a network filesystem — NFS, SMB, or CIFS —
file locks are advisory and best-effort: the kernel may not honor them across hosts, so two
machines syncing the same state root at once are not mutually excluded. Phora cannot build a
cross-host lock over these mounts, since there is no lock server; when it detects the state root on
a network filesystem it prints a one-line advisory and proceeds. Cross-machine safety there is your
responsibility.

This matters most for a shared `$HOME`, since the state root defaults under your home directory. If
the same home directory is mounted on several machines — a common lab or cluster setup — do not run
concurrent syncs of the same project from two machines. Serialize them, or give each machine its own
state root by pointing `[paths].state` at machine-local storage.

## Troubleshooting

### What would a sync deploy?

Run `phora preview` before syncing. It resolves commits from the lock and trees from the mirror
without touching the network, so it is safe to run anywhere. `phora preview --files` expands each
artifact into the files behind it.

```console
$ phora preview --files
demo -> ./out
  phora@61831974 README.md -> ./out/README.md
    README.md
```

### Why is this file not deploying?

`phora explain <target> <source> <path>` attributes the decision to a rule: which `include` or
`exclude` offered the path, and how the binding's `take` resolved it. Without a path it summarizes
the whole offer.

```console
$ phora explain demo phora README.md
phora under demo
  offer: `README.md` allowed by include `README.md`
  take: kept at `README.md`
```

### Is my `include` and `exclude` doing what I think?

`phora check-match --source <source> <path>` probes one path against the source's rules alone and
prints the rules alongside the verdict, which isolates an offer problem from a binding problem.

```console
$ phora check-match --source phora README.md
artifact `README.md`: allowed
path `README.md`: allowed
include: ["README.md"]
exclude: []
```

### Has anything changed on disk since phora deployed it?

`phora verify` re-hashes every deployed file, names each mismatch, and exits non-zero.

```console
$ phora verify
phora/README.md: README.md (content mismatch)
```

`phora list` then reports the same artifact as `modified`.

### Where did this file come from?

`phora where` queries the registry in reverse — by source, artifact, commit, or digest — and reports
the commit, the content digest, and every target the artifact reached.

```console
$ phora where --source phora
Artifact: phora/README.md (commit 61831974, digest blake3:a0bb899173da5d73300c6e3ba93b24872407f27969d794321b17efc892cae50c)
  - demo
```

### Sync keeps stopping on a file it did not deploy

That is the conflict prompt. When a sync finds a target file modified outside phora, or a foreign
file where an artifact wants to land, it asks interactively:

```
[s]kip / [o]verwrite / [e]ject / [a]bort
```

Non-interactive runs skip such files unless `--force` is given. To keep a file and stop managing it,
`phora eject` it.

### Sync refuses to delete an artifact that upstream removed

The pin moved, and an artifact that is still deployed is no longer offered. Re-run with
`phora sync --fast-forward` to follow the pin and delete it, or `phora eject` it first to keep the
files.

### The registry is out of step with what is on disk

`phora rebuild-registry` reconstructs the registry from the lock and the on-disk targets, hashing
what it finds and restoring `linked` markers without hashing. `phora list --orphans` reports records
whose target left the configuration, and `phora sync --prune` removes them.

The guide has a longer diagnostic walkthrough in [When something looks wrong](GUIDE.md).

## Further reading

- [The phora guide](GUIDE.md) — how phora works, the pipeline every source runs through, and what
  happens under the hood.
- [Use cases](USE-CASES.md) — situations, each with a working configuration.

## Development

```sh
mise run check     # clippy (pedantic, -D warnings) + rustfmt --check + tests
mise run test      # cargo test
mise run fmt       # cargo fmt
mise run build     # cargo build
```

### Testing

```sh
mise run test-integration   # integration suites under tests/scrut/ against a release build
```

The integration suites exercise the shipped binary end to end. CI also runs the narrated workflow in
[`tests/scrut/showcase.md`](tests/scrut/showcase.md), which adds a git source, deploys it, then
layers a machine-local symlink overlay.
