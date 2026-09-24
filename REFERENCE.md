# phora reference

Every command, flag, configuration key, environment variable and exit code. For the model behind
them, read [GUIDE.md](GUIDE.md); for complete configs, read [USE-CASES.md](USE-CASES.md).

## Synopsis

```
phora [-C <DIRECTORY>] <COMMAND> [OPTIONS]
```

Phora works on the project in the current directory: `phora.toml`, an optional `phora.local.toml`
overlay, and their lock files. `phora help <COMMAND>` prints the built-in help for any command.

## Global options

| Option | Meaning |
| --- | --- |
| `-C`, `--directory <DIRECTORY>` | run in this project directory; relative paths and hooks resolve from it |
| `-h`, `--help` | print help |
| `-V`, `--version` | print the version |

## Commands

These commands read the merged view of `phora.toml` and `phora.local.toml`: `sync`, `update`,
`list`, `verify`, `preview`, `explain`, `rebuild-registry`, `bind`, and `target rm`. `add` reads
`[defaults] auto_target` from the merged view. `rm` and `source rm` read and edit both files. `add`,
`bind` and `unbind` check the merged view before they write. The others read `phora.toml` alone.

### phora sync

Deploy every target from the sources pinned in the lock.

```
phora sync [--prune] [--force] [--fast-forward] [--frozen] [--no-hooks]
           [--no-transitive-hooks] [--no-progress] [--json] [-j <N>]
```

A source missing from the lock, or whose URL, ref or offer changed, is resolved and pinned. Every
other source deploys at its locked commit. When nothing changed, a sync fetches nothing.

| Flag | Meaning |
| --- | --- |
| `--prune` | delete artifacts the configuration no longer selects, and managed file links whose source file is gone; skipped when a deploy failed |
| `--force` | overwrite locally modified and foreign files without asking; sources stay at their locked commits |
| `--fast-forward` | delete deployed artifacts that a moved pin no longer offers, instead of stopping |
| `--frozen` | fetch and resolve nothing; every source, nested dependencies included, must already be in the lock, except link sources; fails when a history binding's mirror holds only pinned commits; also runs against a read-only state root when nothing needs writing |
| `--no-hooks` | run no hooks at all |
| `--no-transitive-hooks` | run your own hooks, but none from imported dependencies |
| `--no-progress` | never draw live progress |
| `--json` | print one JSON record per event on stdout (NDJSON) |
| `-j`, `--jobs <N>` | number of resolver threads; must be at least 1 |

Without `--jobs`, phora starts one thread per source ref, capped at the larger of 50 and twice the
core count. With `--json`, the stream ends with one `summary` or `aborted` record, and
human-readable messages go to stderr.

When a modified or foreign file sits where an artifact should land, a terminal prompts
`[s]kip/[o]verwrite/[e]ject/[a]bort?`. Without a terminal, phora skips the file and says so.

```console
$ phora sync
sync complete
```

On a terminal, a live progress display and a summary replace `sync complete`.

See also: [phora update](#phora-update), [phora preview](#phora-preview), [hooks](#hooks),
[Environment](#environment).

### phora update

Move pins to the newest commit of each ref, then sync.

```
phora update [SOURCE] [--prune] [--fast-forward]
```

With no argument, every source is re-resolved. With a source name, that source and every
transitive dependency, from any import, are re-resolved; your other sources stay pinned.

| Flag | Meaning |
| --- | --- |
| `--prune` | delete artifacts no longer selected after the update, including generated links |
| `--fast-forward` | delete deployed artifacts that the new pin no longer offers, instead of stopping |

```sh
phora update dotfiles --fast-forward
```

See also: [phora sync](#phora-sync), [Files and state](#files-and-state).

### phora list

Show each target's artifacts and their state.

```
phora list [--orphans]
```

| Flag | Meaning |
| --- | --- |
| `--orphans` | list registry records whose target left the configuration, with their paths on disk |

| State | Meaning |
| --- | --- |
| `✓ clean` | deployed bytes match the record |
| `outdated` | the lock moved past the deployed commit, or `[vars]` changed since a template rendered |
| `modified` | files changed outside phora |
| `foreign` | a file phora did not deploy sits where the artifact belongs |
| `missing` | recorded, but gone from disk |
| `ejected` | no longer managed; files kept |
| `linked` | a live symlink into a working tree |

A history binding prefixes the state with `history,`.

```console
$ phora list
demo:
  phora/README.md  ✓ clean
```

See also: [phora verify](#phora-verify), [phora where](#phora-where).

### phora verify

Re-hash every deployed file against its recorded digest.

```
phora verify
```

Exits 1 on a content mismatch, a missing file, a path that now holds a different kind of entry, or
when a composed dependency has untrusted hooks that were stripped. Linked artifacts have no digests
and are skipped. A stale history overlay is reported without failing.

```console
$ phora verify
phora/README.md: README.md (content mismatch)
```

See also: [phora list](#phora-list), [phora trust](#phora-trust).

### phora where

Look up deployed artifacts in the registry.

```
phora where [--source <NAME>] [--artifact <NAME>] [--commit <SHA>] [--digest <DIGEST>]
```

Filters combine with AND. With none, every deployed artifact is listed.

| Flag | Meaning |
| --- | --- |
| `--source <NAME>` | artifacts from the [binding](#bindings) with this identity |
| `--artifact <NAME>` | the artifact with exactly this name |
| `--commit <SHA>` | artifacts deployed at this commit |
| `--digest <DIGEST>` | artifacts with this content digest |

```console
$ phora where --source phora
Artifact: phora/README.md (commit 2198be09, digest blake3:1314b48decbf57e8622c507b728eb178b1e26ee02c049b835b6a1f04c2944b6e)
  - demo
```

See also: [phora list](#phora-list).

### phora preview

Show what a sync would deploy, without touching the network.

```
phora preview [--target <NAME>] [--source <NAME>] [--files] [--json]
```

Commits come from the lock and trees from the cache mirror. A collapsed directory ends in `/`. A
source phora cannot plan offline is marked `not locked`, `needs sync` or `link working tree gone`,
and the command still exits 0.

| Flag | Meaning |
| --- | --- |
| `--target <NAME>` | only this target |
| `--source <NAME>` | only this source |
| `--files` | list the files inside each artifact |
| `--json` | print the plan as JSON |

```console
$ phora preview --files
demo -> ./out
  phora@2198be09 README.md -> ./out/README.md
    README.md
```

See also: [phora explain](#phora-explain), [phora sync](#phora-sync).

### phora explain

Show why a path is or isn't deployed to a target, offline.

```
phora explain <TARGET> <SOURCE> [PATH]
```

With a path, it names the `include` or `exclude` rule that decided the offer and what the binding's
`take` did with it. Without one, it summarizes the whole offer.

```console
$ phora explain demo phora README.md
phora under demo
  offer: `README.md` allowed by include `README.md`
  take: kept at `README.md`
```

See also: [phora check-match](#phora-check-match), [take](#take).

### phora check-match

Test one path against a source's `include` and `exclude` rules.

```
phora check-match --source <SOURCE> <PATH>
```

It ignores bindings, so it tells an offer problem apart from a `take` problem.

```console
$ phora check-match --source phora README.md
artifact `README.md`: allowed
path `README.md`: allowed
include: ["README.md"]
exclude: []
```

See also: [phora explain](#phora-explain), [sources](#sources).

### phora add

Add a source from a URL or path, and bind it to targets.

```
phora add <URL> [--to <TARGET>]... [--name <NAME>] [--branch <BRANCH> | --tag <TAG>]
          [--root <PATH>] [--include <GLOB>]... [--exclude <GLOB>]... [--as <IDENTITY>]
          [--history] [-y] [--local | --symlink]
```

| Flag | Meaning |
| --- | --- |
| `--to <TARGET>` | bind the source into this target; repeatable |
| `--name <NAME>` | source name; defaults to one derived from the URL |
| `--branch <BRANCH>` | pin to a branch |
| `--tag <TAG>` | pin to a tag |
| `--root <PATH>` | set the source's `root` |
| `--include <GLOB>` | add to the source's `include`; repeatable |
| `--exclude <GLOB>` | add to the source's `exclude`; repeatable |
| `--as <IDENTITY>` | binding identity; needs exactly one `--to` |
| `--history` | write `history = true` on each binding |
| `-y`, `--yes` | create a missing `--to` target without asking |
| `--local` | write the path as a source in `phora.local.toml`, and bind nothing |
| `--symlink` | like `--local`, and set `deploy = "link"` |

URL forms:

| Input | Written as |
| --- | --- |
| `owner/repo` | `host = "github"` and `repo` |
| `gitlab:owner/repo` | that forge's `host` and `repo` |
| `gitlab.com/owner/repo` | the forge whose domain matches, and `repo` |
| `https://…`, `http://…`, `ssh://…` | `git`, with `.git` appended |
| `…/owner/repo/tree/<ref>/<path>` | `git`, plus `branch = "<ref>"` and `root = "<path>"` |
| `git@host:path` | `git`, as written |
| an existing directory or absolute path | `path`, made absolute and canonical |

In the shorthand forms, segments after `owner/repo` become `root`; a deeper repository path belongs
in the config's `repo` key.

With `--local` or `--symlink`, `add` writes only the source into `phora.local.toml`. Otherwise,
without `--to`, the source is bound into `[targets.default]` (path `.`, flat layout), which is
created if missing; `[defaults] auto_target = false` turns that off. A `--to` target that does not
exist is created at `./<name>` with a flat layout when that directory already exists or `--yes` is
given. Otherwise a terminal asks `[Y/n]`, and a non-interactive run fails with a
`phora target add` hint.

`--history` conflicts with `--root`, `--include`, `--exclude`, `--local` and `--symlink`.
`--local` and `--symlink` reject `--to` and `--as`. A failed command leaves both files as they were.

```sh
phora add srnnkls/dotfiles --to neovim --as nvim --branch main
```

See also: [phora source](#phora-source), [phora bind](#phora-bind), [defaults](#defaults).

### phora rm

Remove a source and every binding to it.

```
phora rm <NAME>
```

Scrubs `phora.toml` and `phora.local.toml` together, so it takes no `--local`. Entries in a
target's `imports` are left for you to remove. Same as `phora source rm`.

```sh
phora rm dotfiles
```

See also: [phora unbind](#phora-unbind).

### phora source

Add, remove, list or show sources.

```
phora source add <URL> [--name <NAME>] [--branch <BRANCH> | --tag <TAG>] [--root <PATH>]
                 [--include <GLOB>]... [--exclude <GLOB>]... [--local | --symlink]
phora source rm <NAME>
phora source list
phora source show <NAME>
```

| Subcommand | Meaning |
| --- | --- |
| `add` | `phora add` without binding: it takes no `--to`, `--as`, `--history` or `--yes` |
| `rm` | same as [`phora rm`](#phora-rm) |
| `list` | each source's name, remote and ref |
| `show` | one source's settings and the targets that bind it |

```sh
phora source add https://github.com/junegunn/fzf.git --tag v0.55.0 --include bin
```

See also: [phora add](#phora-add), [sources](#sources).

### phora target

Add, remove, list or show targets.

```
phora target add <NAME> --path <PATH> [--layout <LAYOUT>] [--local]
phora target rm <NAME> [--local] [--force]
phora target list
phora target show <NAME>
```

| Flag | Meaning |
| --- | --- |
| `--path <PATH>` | deployment directory; required by `add` |
| `--layout <LAYOUT>` | `flat`, `by-source` or `prefixed`; defaults to `flat` |
| `--local` | edit `phora.local.toml` |
| `--force` | remove the target even while it still has deployed artifacts |

`list` prints each target's name, path and sources. `show` adds each artifact's state.

```sh
phora target add neovim --path ~/.config/nvim
```

See also: [targets](#targets), [layouts](#layouts).

### phora bind

Bind sources to one or more targets.

```
phora bind <SOURCE>... --to <TARGET> [--to <TARGET>]... [--as <IDENTITY>] [--take <ENTRY>]...
           [--branch <BRANCH> | --tag <TAG> | --rev <SHA>] [--root <PATH>] [--history] [--local]
```

A plain bind appends the source name to a flat `sources` list, or writes `name = {}` into a keyed
table. Any refinement writes a keyed entry.

| Flag | Meaning |
| --- | --- |
| `--to <TARGET>` | target to bind into; required, repeatable |
| `--as <IDENTITY>` | binding identity; one source only |
| `--take <ENTRY>` | a leaf, a glob, or `src=dest`; repeatable |
| `--branch <BRANCH>` | pin this binding to a branch |
| `--tag <TAG>` | pin this binding to a tag |
| `--rev <SHA>` | pin this binding to a full commit id |
| `--root <PATH>` | set `root` on each named source in the edited file |
| `--history` | set `history = true`; conflicts with `--root` and `--take` |
| `--local` | edit `phora.local.toml` |

Every source and target must already exist in the merged configuration.

```sh
phora bind dotfiles --to neovim --as nvim --take 'nvim/**'
phora bind references --to laptop --to desktop --history
```

See also: [phora unbind](#phora-unbind), [bindings](#bindings), [take](#take).

### phora unbind

Remove bindings from a target by identity.

```
phora unbind <IDENTITY>... --from <TARGET> [--local]
```

| Flag | Meaning |
| --- | --- |
| `--from <TARGET>` | target to edit; required |
| `--local` | edit `phora.local.toml` |

A target left with no bindings deploys nothing, and phora warns.

```sh
phora unbind nvim --from neovim
```

See also: [phora bind](#phora-bind), [phora rm](#phora-rm).

### phora eject

Stop managing an artifact and keep its files.

```
phora eject <ARTIFACT> --source <SOURCE> --target <TARGET>
```

Both flags are required. `--source` takes the [binding](#bindings) identity.

Ejecting a [history](#history) deployment turns it into a standalone clone, detached at the pin
with `origin` set to the upstream. `phora uneject` refuses while that clone's `.git` is there.

```sh
phora eject nvim/init.lua --source nvim --target neovim
```

See also: [phora uneject](#phora-uneject).

### phora uneject

Manage an ejected artifact again.

```
phora uneject <ARTIFACT> --source <SOURCE> --target <TARGET>
```

Both flags are required, and `--source` takes the binding identity. The next sync treats the
artifact like any other.

```sh
phora uneject nvim/init.lua --source nvim --target neovim
```

See also: [phora eject](#phora-eject).

### phora trust

Review and approve hooks from imported dependencies.

```
phora trust [SOURCE] [--list] [--revoke] [--show <PATH>]
```

With no flags, a terminal prompts once per hook; without a terminal it lists them. A source name
narrows everything to that dependency. Listings read the cache mirror. A commit or file the mirror lacks is fetched from the dependency's remote.

| Flag | Meaning |
| --- | --- |
| `--list` | list hooks without approving |
| `--revoke` | drop every approval for the named source; needs a source |
| `--show <PATH>` | print a file, or list a directory, of the dependency at its pinned commit; needs a source |

Approvals are stored in `phora.lock` and tied to the command and the dependency commit, so a
changed hook or a new commit asks again.

```sh
phora trust toolkit --show phora.toml
```

See also: [imports](#imports), [hooks](#hooks), [GUIDE: transitive dependencies](GUIDE.md#transitive-dependencies).

### phora rebuild-registry

Rebuild the registry from the lock and what is on disk.

```
phora rebuild-registry
```

It hashes each deployed artifact it finds and prints how many records it rebuilt, plus any modified
or foreign paths.

```sh
phora rebuild-registry
```

See also: [Files and state](#files-and-state), [Troubleshooting](#troubleshooting).

## Configuration

`phora.toml` sits in the project root. Unknown keys are an error in every table except `[vars]`,
which is free-form, and `[targets.<t>.take]` and `[targets.<t>.collapse]`, whose keys name imports
and are not checked. [phora.example.toml](phora.example.toml) shows every section below.

### Top-level keys

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `version` | integer | `1` | schema version; `1` is the only one |
| `protocol` | `"https"` \| `"ssh"` | `"https"` | which remote template forge sources use |

### paths

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `cache` | path | platform cache root | root for git mirrors, under `<cache>/git/` |
| `state` | path | platform state root | root for per-project state, under `<state>/projects/` |

A relative path resolves from the project root. The value is the root itself; no `phora` directory
is appended.

```toml
[paths]
cache = ".phora/cache"
state = ".phora/state"
```

### defaults

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `auto_target` | bool | `true` | `phora add` without `--to` binds into `[targets.default]`; `false` only declares the source |

```toml
[defaults]
auto_target = false
```

### vars

A flat table of strings for templates. A source file ending in `.tmpl` is rendered with
[minijinja](https://docs.rs/minijinja) and deployed without the suffix.

Rendering is strict: an undefined variable fails that artifact, and the others still deploy. A
binding's `template` key changes which files render. See [GUIDE: templating](GUIDE.md#templating).

```toml
[vars]
email = "me@example.com"
editor = "nvim"
```

### hosts

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `remote` | string \| `{ https, ssh }` | built in for shipped forges | URL templates; a string is the https template |

`github`, `gitlab`, `codeberg`, `sr.ht` and `bitbucket` are built in with https and ssh templates. A
`[hosts.<name>]` table adds a forge or replaces a built-in's templates.

| Placeholder | Value |
| --- | --- |
| `{path}` | the source's `repo`, as written |
| `{owner}` | the first `/` segment of `repo` |
| `{repo}` | everything after it |

```toml
[hosts.company]
remote = { https = "https://git.company.com/{path}.git", ssh = "git@git.company.com:{path}.git" }
```

### sources

Each `[sources.<name>]` sets one kind: forge (`repo`, optionally `host`), local (`path`),
git remote (`git`), or download (`url`).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `repo` | string | | `owner/repo` on a forge; deeper paths are fine |
| `host` | string | `"github"` | forge name from `[hosts]` or the built-ins |
| `path` | string | | local directory; `~` and `~/` expand to your home directory |
| `git` | string | | git remote: https, `ssh://`, or `git@host:path` |
| `url` | string | | https download imported as one snapshot |
| `digest` | string | | `sha256:<hex>` or `blake3:<hex>`, checked before a `url` is extracted |
| `protocol` | `"https"` \| `"ssh"` | top-level `protocol` | template for this forge source |
| `branch` | string | the remote's default branch | follow a branch |
| `tag` | string | | pin a tag |
| `rev` | string | | pin a full 40- or 64-hex commit id |
| `root` | path | source root | serve the offer from this subdirectory |
| `include` | array of globs | everything except `.git/` | paths to offer, gitignore syntax |
| `exclude` | array of globs | `[]` | paths to withhold; wins over `include`; no `!` patterns |
| `allow_symlinks` | bool | `false`; `true` under `history` | deploy symlinks found in the source |
| `preserve_executable` | bool | `true` | keep the executable bit |
| `deploy` | `"copy"` \| `"link"` | `"copy"` | copy content, or symlink into the local working tree |
| `transitive` | bool | `false` | read the source's own `phora.toml`; see [imports](#imports) |

Set at most one of `branch`, `tag` and `rev`. A `url` source rejects `branch`, `tag`, `rev` and
`root`; tar, tar.gz, tgz and zip archives are unpacked and a single top-level directory is
stripped. Any other file deploys under the URL's basename.

`deploy = "link"` needs a local `path` and works in either config file. Phora warns when
`phora.toml` declares the source and the linked path is absolute. `git = "<local path>"` and
`host` + `path` are deprecated spellings of `path` and `repo`, and phora warns when it reads them.

```toml
[sources.dotfiles]
repo = "me/dotfiles"
branch = "main"
root = "modules"
exclude = ["**/*.bak"]
```

### targets

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `path` | path | required | deployment directory; `~` expands |
| `sources` | array \| table | none | bindings; see [bindings](#bindings) |
| `layout` | string \| table | `"flat"` | see [layouts](#layouts) |
| `phase` | `"deploy"` \| `"prepare"` | `"deploy"` | `prepare` targets deploy first, before `post_prepare` |
| `imports` | array | none | transitive sources to compose here; see [imports](#imports) |
| `take` | table | none | per-import `take`, keyed by the imported source |
| `collapse` | table | none | per-import `collapse`, keyed by the imported source |
| `hooks` | table | none | see [hooks](#hooks) |

`sources` is an allow-list: an omitted key or `[]` deploys nothing. Prepare and deploy targets must
use separate directory trees.

```toml
[targets.neovim]
path = "~/.config/nvim"
sources = ["dotfiles"]
```

### bindings

A binding connects one source to one target. A flat list, `sources = ["a", "b"]`, binds each
source whole. A keyed table sets options per binding:

```toml
[targets.editor.sources]
nvim = { source = "dotfiles", take = ["nvim/**"] }
helix = { source = "dotfiles", take = ["helix/**"], collapse = false }
```

The key is the binding identity, one safe path component. Bindings are processed in identity
order.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `source` | string | the key | source this binding reads |
| `take` | array | the whole offer | which offered paths to deploy, and where; see [take](#take) |
| `collapse` | bool | automatic | `true` requires one directory artifact; `false` deploys each file separately |
| `template` | array of globs \| `false` | `.tmpl` files only | extra files to render, or `false` to render none |
| `history` | bool | `false` | deploy the whole source as a copy with its pinned git history attached |
| `branch` | string | the source's ref | follow a branch for this binding |
| `tag` | string | the source's ref | pin a tag for this binding |
| `rev` | string | the source's ref | pin a full commit id for this binding |

Each distinct binding ref gets its own lock entry. `root`, `include`, `exclude` and `map` are
rejected here; set the first three on the source and rename with `take`.

Restrictions:

- A `url` source binding accepts only `source` and `collapse`.
- A `link` source binding rejects `branch`, `tag` and `rev`.
- `history = true` rejects `take`, `template` and `collapse`, and its source may not be `url`,
  `link` or `transitive`, set `root`, `include` or `exclude`, or set `preserve_executable = false`.

See [GUIDE: bindings](GUIDE.md#bindings), [collapse](GUIDE.md#collapse) and
[history overlay](GUIDE.md#history-overlay).

### take

| Entry | Example | Deploys |
| --- | --- | --- |
| literal leaf | `"nvim/init.lua"` | that file, at the same path |
| glob | `"nvim/**"` | every offered file it matches |
| rename | `{ "nvim/init.lua" = "init.lua" }` | one offered file at a new path |
| subtree rename | `{ "claude/" = "." }` | every offered file under `claude/`, re-rooted at the destination |

- A literal or rename source that is not offered is an error. `take` never widens the offer.
- A glob that matches nothing prints a warning.
- In a subtree rename, `"."` is the target root. Literal and file renames take precedence over
  subtree renames, and the longest matching subtree prefix wins.
- Subtree-renamed files deploy one by one; `collapse = true` is rejected.
- `take = []` deploys nothing. An omitted `take` deploys the whole offer.

```toml
[targets.agents.sources]
dotfiles = { take = [{ "claude/" = "." }, { "codex/AGENTS.md" = "AGENTS.md" }] }
```

### layouts

A layout places artifact `a` from the binding with identity `i`.

| Layout | Path |
| --- | --- |
| `"flat"` | `a` |
| `"by-source"` | `i/a` |
| `"prefixed"` or `{ type = "prefixed", separator = "-" }` | `i-a`; `separator` defaults to `-` |

```toml
[targets.policies]
path = "~/.policies"
sources = ["loqui"]
layout = { type = "prefixed", separator = "/" }
```

### hooks

| Key | Runs | On failure |
| --- | --- | --- |
| `[hooks] pre_sync` | first, before any source is read | the remaining entries still run, then the sync stops and the lock is left alone |
| `[hooks] post_prepare` | after `prepare` targets deploy, before other sources resolve | commands after it are skipped, deploy targets stay as they were, and the sync exits 1 |
| `[hooks] post_sync` | after your `on_change` hooks, before an imported dependency's `on_change` | the sync exits 1 |
| `[hooks] when` | reserved; the only value is `"always"` | |
| `[targets.<t>.hooks] pre_deploy` | once per target, before any target of the same phase is written | see `pre_deploy_on_fail` |
| `[targets.<t>.hooks] pre_deploy_on_fail` | `"abort"` (default) stops the sync; `"skip"` skips that target | |
| `[targets.<t>.hooks] on_change` | after this target's artifacts were added or changed | the sync exits 1 and the hook runs again next time |

A sync runs in this order: `pre_sync`, compose imports, `prepare` targets with their own
`pre_deploy` and `on_change`, `post_prepare`, then for deploy targets: resolve, plan, `pre_deploy`,
apply, prune, your `on_change`, `post_sync`, and the trusted `on_change` hooks of imported
dependencies. A failed `pre_sync` or `post_prepare`, or an aborting `pre_deploy`, skips everything
after it, `post_sync` included.

A hook is a string run by `sh -c`, a table, or an array of them run in order with duplicates
dropped. `{ run = "...", shell = "bash -c" }` picks the shell; `{ cmd = ["prog", "arg"] }` runs the
program directly, with no shell.

| Hook | Environment |
| --- | --- |
| `pre_sync`, `post_prepare` | `PHORA_TARGETS`: every configured target name, space-separated |
| `pre_deploy` | `PHORA_TARGET`, `PHORA_TARGET_PATH` |
| `on_change` | `PHORA_TARGET`; `PHORA_CHANGED` and `PHORA_CHANGED_NAMES`, newline-separated deployed paths and artifact names |
| `on_change` from an import | `PHORA_TARGET`: the composed target path |

Hooks inherit phora's environment and run in the project directory. `on_change` records success,
so it skips a sync where nothing changed; removals alone don't trigger it. Hooks come only from
your own config files. An imported dependency's hooks run after [phora trust](#phora-trust).

```toml
[hooks]
pre_sync = "test -w ~/.config"

[targets.neovim.hooks]
on_change = "nvim --headless '+Lazy! sync' +qa"
pre_deploy = { cmd = ["test", "-d", "/Volumes/work"] }
pre_deploy_on_fail = "skip"
```

See [GUIDE: hooks](GUIDE.md#hooks) and [preparing inputs](GUIDE.md#preparing-inputs).

### history

`history = true` on a binding deploys the source as a verified copy with its pinned git history
attached. The deployed directory gets a `.git` file pointing at a worktree of the cache mirror, so
`git log` works there.

The source must be a whole-repository forge, `git` or `path` source (see the restrictions under
[bindings](#bindings)). Symlinks in the source are deployed unless `allow_symlinks = false`.
`phora verify` reports a stale overlay without failing, and the next sync repairs it.

Every other binding fetches only its pinned commits, without history, and only the file contents it
deploys. A history binding makes phora fetch the source's full history, and the mirror keeps it from
then on.

```toml
[targets.references.sources]
rust-book = { history = true }
```

See [GUIDE: history overlay](GUIDE.md#history-overlay).

### imports

A source with `transitive = true` is a package: its own `phora.toml` targets compose beneath the
importing target's path.

| Form | Meaning |
| --- | --- |
| `"name"` | import at the source's ref |
| `{ source = "name", branch = "..." }` | import at another branch; `tag` or `rev` also work, one at a time |

- A transitive source must be imported by some target and cannot also appear in that target's
  `sources`.
- A `url` package cannot select a ref.
- An imported package may use `deploy = "link"`; phora then reads its working tree and locks it as
  `resolved = "link"`. Sources inside a package cannot use link mode.
- Inside a package, `path = "."` names the package itself, read from the pinned commit.
- Nesting stops at depth 64.
- `[targets.<t>.take]` and `[targets.<t>.collapse]`, keyed by the imported source, override the
  package's own bindings.

```toml
[sources.toolkit]
repo = "me/toolkit"
transitive = true

[targets.toolkit]
path = "~/.config/toolkit"
imports = [{ source = "toolkit", tag = "v1.2.0" }]

[targets.toolkit.take]
toolkit = ["skills/**"]
```

See [GUIDE: transitive dependencies](GUIDE.md#transitive-dependencies) and
[local packages](GUIDE.md#local-packages).

### Local overlay

`phora.local.toml` has the same schema and overrides `phora.toml` for one machine. Keep it out of
version control. Sources it declares or overrides are locked in `phora.local.lock`.

| Table | Merge |
| --- | --- |
| top level, `[paths]`, `[defaults]`, `[vars]` | per key |
| `[hosts.<name>]` | per key |
| `[sources.<name>]` | per key; a new kind replaces the old one, and any ref key replaces all three |
| `[targets.<name>]` | `path` is required; `sources`, `layout`, `phase`, `imports`, `take`, `collapse` and `hooks` each replace the base value whole |
| `[hooks]` | replaces the base table whole |

An empty `take` or `collapse` table in the overlay clears the base one. Overriding a source with a
local `path` and `deploy = "link"` drops the base ref.

```toml
[sources.loqui]
path = "~/dev/loqui"
deploy = "link"

[vars]
email = "me@work.example"
```

`phora add --local <path>` and `phora add --symlink <path>` write this file for you. See
[phora.local.example.toml](phora.local.example.toml) and [GUIDE: link mode](GUIDE.md#link-mode).

## Environment

| Variable | Effect |
| --- | --- |
| `XDG_CACHE_HOME` | cache root, when absolute; `[paths] cache` wins |
| `XDG_STATE_HOME` | state root, when absolute; `[paths] state` wins |
| `HOME` | expands `~` in paths |
| `PHORA_NO_PROGRESS` | any non-empty value turns off live progress |
| `CI` | any non-empty value turns off live progress |
| `TERM=dumb` | turns off live progress |

Live progress also needs stderr to be a terminal. Hooks receive the `PHORA_*` variables listed
under [hooks](#hooks).

## Files and state

| Path | Holds |
| --- | --- |
| `phora.toml` | configuration |
| `phora.local.toml` | machine-local overlay |
| `phora.lock` | commit per source and ref, plus hook approvals; commit it |
| `phora.local.lock` | pins for overlay sources |
| `<cache>/git/` | one bare mirror per remote, shared by all projects; safe to delete |
| `<state>/projects/<project>/` | this project's registry, journal and `state.lock` |

| Root | Linux | macOS |
| --- | --- | --- |
| cache | `~/.cache/phora` | `~/Library/Caches/phora` |
| state | `~/.local/state/phora` | `~/Library/Application Support/phora` |

Precedence is `[paths]`, then `XDG_*`, then the platform default. A sync holds `state.lock` for
the whole run, and a second sync of the same project exits 75 instead of waiting.

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | an error; a failed hook or deploy; `verify` found a mismatch or stripped hooks |
| 2 | invalid command line |
| 75 | another phora process holds this project's state lock; retry |

## Troubleshooting

### What would a sync change?

Run `phora preview --files`. It works offline from the lock and the cache mirror.

### Why isn't this file deployed?

Run `phora explain <target> <source> <path>` to see which rule decided it. `phora check-match
--source <source> <path>` tests the source's `include` and `exclude` alone.

### Did something change a deployed file?

`phora verify` names each mismatch and exits 1. `phora list` shows the artifact as `modified`.

### Where did this file come from?

`phora where --artifact <path>` prints the source, commit, digest and targets.

### A sync stops at a file it didn't deploy

On a terminal, phora asks `[s]kip/[o]verwrite/[e]ject/[a]bort?`. Without one, it skips the file:

```console
$ phora sync
phora: skipping foreign content at <TARGET>/editor; use --force to overwrite
sync complete
```

Use `phora sync --force` to overwrite, or `phora eject` to keep the file and stop managing it.

### A sync refuses to delete something upstream removed

The pin moved and a deployed artifact is no longer offered. `phora sync --fast-forward` deletes it;
`phora eject` keeps it.

### The registry doesn't match the disk

Run `phora rebuild-registry`. `phora list --orphans` shows records for removed targets, and
`phora sync --prune` deletes them.

### Exit 75: another phora process is running

```console
$ phora sync
error: lock error: another phora process is running for this project (state.lock held)
```

Wait for the other sync, then retry. On NFS, SMB or CIFS the lock is advisory and may not stop a
sync on another machine; phora prints a one-line warning when the state root is on one. If a shared
home directory is mounted on several machines, don't sync the same project from two of them at
once, or set `[paths] state` to machine-local storage.

See [GUIDE: when something looks wrong](GUIDE.md#when-something-looks-wrong).
