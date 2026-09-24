# The phora guide

Every flag and config key is listed in [REFERENCE.md](REFERENCE.md); the mechanisms behind a sync are in
[docs/architecture.md](docs/architecture.md).

## Contents

- [How phora works](#how-phora-works)
- [Your first sync](#your-first-sync)
- [Sources](#sources)
- [Choosing what ships](#choosing-what-ships)
- [Targets](#targets)
- [Bindings](#bindings)
- [Layouts](#layouts)
- [Renaming](#renaming)
- [Collapse](#collapse)
- [Staying in sync](#staying-in-sync)
- [Hooks](#hooks)
- [Templating](#templating)
- [Link mode](#link-mode)
- [History overlay](#history-overlay)
- [Preparing inputs](#preparing-inputs)
- [Transitive dependencies](#transitive-dependencies)
- [What phora keeps on disk](#what-phora-keeps-on-disk)
- [When something looks wrong](#when-something-looks-wrong)
- [Where to look next](#where-to-look-next)

## How phora works

phora copies files from where they are published into the directories that use
them, and remembers what it put where. A handful of terms carry the whole model:

- A *source* is where content comes from: a git repository, a local directory, or
  a file to download.
- The *offer* is the set of files a source publishes. The source shapes it with
  `root`, `include` and `exclude`.
- A *target* is a directory on your machine that phora deploys into.
- A *binding* connects one target to one source. Its *take* picks the part of the
  offer that this target wants, and can rename files on the way.
- An *artifact* is one file or directory that phora deploys as a unit, named by
  its path in the offer.
- The *lock* (`phora.lock`) pins every source to one commit. The *registry*
  records what landed where, with a hash for every file.

A `phora sync` runs these steps in order:

1. Run the global `pre_sync` hook.
2. Read the manifests of any [transitive dependencies](#transitive-dependencies).
3. Deploy [prepare-phase targets](#preparing-inputs), with their own
   `pre_deploy` and `on_change` hooks, then run `post_prepare`, if you have any.
   The steps below cover the deploy-phase targets.
4. Resolve every source to one commit, fetching what the cache lacks.
5. Work out which artifacts each target should hold.
6. Compare that plan with the disk and the registry, and ask about conflicts.
7. Run each deploy-phase target's `pre_deploy` gate, then look at the disk again.
8. Write the artifacts.
9. Remove what you asked to prune.
10. Run your `on_change` hooks, then `post_sync`, then the approved `on_change`
    hooks of transitive dependencies, and write `phora.lock`.

Everything phora reads becomes a git commit in a local store, including a
downloaded tarball. That is why a URL source locks, deploys and verifies the same
way a repository does.

## Your first sync

Start with an empty directory and a `phora.toml` that deploys phora's own README:

```toml
version = 1

[sources.phora]
repo = "srnnkls/phora"
branch = "main"
include = ["README.md"]

[targets.demo]
path = "./out"
sources = ["phora"]
```

Run a sync, then ask phora what it did:

```bash
phora sync
```

```console
$ phora list
demo:
  phora/README.md  ✓ clean

$ phora verify
all verified
```

Here is what the sync did:

1. It read `phora.toml`, plus `phora.local.toml` if one exists.
2. It cloned `github.com/srnnkls/phora` into its cache, resolved `main` to one
   commit, and wrote that commit to `phora.lock`.
3. It worked out the offer: just `README.md`.
4. It copied `README.md` into `./out` and recorded the file's hash.

On a terminal, `sync` shows progress and ends with a one-line summary. In a script
or CI it prints `sync complete`.

`phora list` shows each artifact as `<binding>/<artifact>` with its state.
`phora verify` hashes the deployed files again and compares them with the record.
It exits non-zero on any mismatch, so it works as a CI check.

Run `phora sync` again tomorrow and you get the same commit. To move to the
latest one, run:

```bash
phora update
```

`sync` honors the lock and `update` advances it. A plain sync is reproducible
and works offline once the cache holds the commit.

You don't have to write `phora.toml` by hand. `phora add` declares a source and
binds it, and `phora bind` edits bindings. See [phora add](REFERENCE.md#phora-add)
and [phora bind](REFERENCE.md#phora-bind).

## Sources

A source is one of four kinds: a forge repository (`repo`), a git remote (`git`),
a local path (`path`), or a download (`url`).

### Repositories

The shortest form names a repository on a forge:

```toml
[sources.phora]
repo = "srnnkls/phora"      # host defaults to github
branch = "main"
```

`host` picks the forge. `github`, `gitlab`, `codeberg`, `sr.ht` and `bitbucket`
are built in, and a `[hosts.X]` table adds your own. Your config records the
forge and the repository; the host decides the URL, so you can move from https
to ssh with one `protocol = "ssh"` line.

For any other remote, write it out:

```toml
[sources.bat]
git = "https://github.com/sharkdp/bat.git"
tag = "v0.24.0"
```

Pin at most one of `branch`, `tag` or `rev`. With none, phora follows the
remote's default branch. A `rev` is a full commit id: 40 hex characters, or 64
for a SHA-256 repository. phora rejects an abbreviated one, because a short id is
unique only until the repository grows another commit that starts the same way.

All spellings of one repository share a single cached clone, so switching
between https, ssh and the `host` form never clones again.

### Local directories

`path` names a git repository on your machine:

```toml
[sources.notes]
path = "~/projects/notes"
branch = "main"
```

A leading `~` or `~/` expands to your home directory. A relative path resolves
against the project root. `path = "owner/repo"` is a local path; the forge
shorthand is `repo = "owner/repo"`.

A local source still deploys from committed history. To deploy the files you are
editing, use [link mode](#link-mode).

### URLs

A URL source downloads one file or archive:

```toml
[sources.fzf-bin]
url = "https://github.com/junegunn/fzf/releases/download/v0.55.0/fzf-0.55.0-linux_amd64.tar.gz"
digest = "sha256:4df2393776942780ddab2cea713ddaac06cd5c3886cd23bc9119a6d3aa1e02bd"
include = ["fzf"]
```

phora downloads the file, checks the `digest` if you set one, unpacks it, and
imports the contents as a single commit. From then on it behaves like any other
source.

- tar, tar.gz and zip are recognized by their contents. Anything else becomes a
  single file named after the URL.
- An archive with one top-level directory, such as `fzf-0.55.0/`, loses that
  directory, so a version bump keeps your paths stable.
- The digest (`sha256:` or `blake3:`) is checked before anything is unpacked.
- `branch`, `tag`, `rev` and `root` are errors on a URL source. `include` and
  `exclude` still work.

The same bytes always import to the same commit, on any machine. An unchanged
download therefore leaves the lock alone, and a changed one moves it. Edit the
`url` and the next plain `phora sync` downloads the new file.

Reach for a URL source when the upstream publishes built files, such as release
binaries or a vendored bundle, and its git history is of no use to you.

## Choosing what ships

The offer belongs to the source. Grow the first example so the source publishes
the changelog and the `docs` directory too:

```toml
[sources.phora]
repo = "srnnkls/phora"
branch = "main"
include = ["README.md", "CHANGELOG.md", "docs"]
exclude = ["docs/RELEASING.md"]
```

`include` and `exclude` use gitignore syntax. The offer is everything `include`
matches minus everything `exclude` matches. Exclude always wins, and there is no
`!` to add a file back. Without `include`, the offer is the whole repository
except `.git/`. Dotfiles match like any other name.

`root` moves the starting point. With `root = "docs"`, the offer is named
relative to `docs/`, and `include` patterns are too. URL sources have no `root`.

Every target that binds this source sees the same offer. A binding can take less
of it and never more, so one edit to the source changes what every consumer can
reach.

When a file ships and you expected it not to, or the other way round, ask:

```bash
phora check-match --source phora docs/RELEASING.md
```

The answer has two verdicts: one for the top-level entry that holds the path
(`docs`), and one for the path itself. It also prints the `include` and
`exclude` lists it judged against.

## Targets

A target is a directory phora deploys into:

```toml
[targets.demo]
path = "./out"
layout = "flat"
sources = ["phora"]
```

- `path` is the directory. `~` and `~/` expand to your home directory, and a
  relative path resolves against the project root, so a repository can deploy
  into itself. phora creates the directory on the first sync that writes there.
- `layout` arranges artifacts when a target draws from several bindings. See
  [Layouts](#layouts).
- `sources` lists the bindings. A target deploys its bindings and nothing else,
  so a target without `sources` deploys nothing.

Declare as many targets as you need. One source can feed several targets, and one
target can draw from several sources. Here the changelog goes somewhere else:

```toml
[targets.notes]
path = "~/notes/phora"
sources = ["phora"]
```

`phora add` without `--to` binds the new source to `[targets.default]`, which
deploys into the current directory. Set `[defaults] auto_target = false` to make
a bare `add` only declare the source.

Removing a target block leaves its files on disk. `phora target rm <name>`
refuses while the target still has deployed artifacts, and tells you to unbind
them and run `phora sync --prune`. See [Orphans](#orphans) for what happens when
you delete the block by hand.

## Bindings

Right now both targets take the whole offer. A binding with a `take` narrows it
for one target:

```toml
[targets.demo]
path = "./out"

[targets.demo.sources]
phora = { take = ["README.md"] }

[targets.notes]
path = "~/notes/phora"

[targets.notes.sources]
phora = { take = ["CHANGELOG.md", "docs/**"] }
```

`demo` gets the README, and `notes` gets the changelog and the docs. The source
and its offer did not change.

`sources` comes in two forms. A flat list, `sources = ["phora"]`, takes every
source whole. A keyed table maps each binding to its settings; `phora = {}` also
takes everything. Use the table whenever one binding needs a `take` or any other
setting.

A `take` entry is one of three things:

- a literal path, such as `"README.md"`, which must be in the offer;
- a gitignore glob, such as `"docs/**"`, which matches offered paths only;
- a rename, `{ "README.md" = "PHORA.md" }`, covered in [Renaming](#renaming).

Leave `take` out to take everything. `take = []` takes nothing. A literal path
that the offer does not contain is an error, and phora suggests the nearest
offered path. A glob that matches nothing only warns.

The lock ignores `take`. Changing a take moves no commit and leaves `phora.lock`
alone. The next sync deploys what the take newly selects. Files it no longer
selects stay on disk until you sync with `--prune`. Two machines that take different slices of the same source
still share one lock entry.

### One source, several slices

The table key is the binding's *identity*. It defaults to the source name, and
you add `source = "…"` when they differ. Identity names the binding in the
registry, in `by-source` and `prefixed` layouts, and in `phora where --source`.

Because the key is the identity, one source can appear in a target more than once:

```toml
[targets.docs]
path = "~/docs"
layout = "by-source"

[targets.docs.sources]
readme    = { source = "phora", take = ["README.md"] }
changelog = { source = "phora", take = ["CHANGELOG.md"] }
```

This deploys `~/docs/readme/README.md` and `~/docs/changelog/CHANGELOG.md`. The
repository is still fetched once.

### One source, several versions

A binding can set its own `branch`, `tag` or `rev`. It wins for that binding;
bindings without one follow the source.

```toml
[targets.tools]
path = "~/.local/tools"
layout = "by-source"

[targets.tools.sources]
stable = { source = "bat", tag = "v0.24.0" }
canary = { source = "bat", tag = "v0.25.0" }
```

Both bindings read from one clone and resolve to two commits. Each distinct ref
gets its own lock entry. A config where no binding sets a ref keeps one entry per
source.

### Rules

- `root`, `include` and `exclude` belong to the source. A binding that sets them
  is an error that points you at `[sources.<name>]`.
- A binding to a URL source cannot set `take`, `template` or a ref. Shape the
  download with the source's `include` and `exclude` instead.
- A binding to a link source cannot set a ref.
- A binding sets at most one ref.
- `phora bind <source> --to <target>` adds a binding, and `--as` and `--take`
  refine it. `phora unbind <identity> --from <target>` removes one by identity.
  Flags that change the offer, such as `--root`, edit the source.

[phora bind](REFERENCE.md#phora-bind) lists every combination.

## Layouts

A layout decides where artifact `a` of the binding with identity `i` lands inside
the target:

| Layout | Path |
| --- | --- |
| `flat` (default) | `a` |
| `by-source` | `i/a` |
| `prefixed` | `i-a` (set `separator` to change `-`) |

`flat` suits a target with one binding. Add a second source to `demo` and two
READMEs would want the same path:

```toml
[sources.loqui]
repo = "srnnkls/loqui"
include = ["README.md"]

[targets.demo]
path = "./out"
layout = "by-source"
sources = ["phora", "loqui"]
```

Now `./out/phora/README.md` and `./out/loqui/README.md` sit side by side.
Layouts label by identity, so two slices of one source separate cleanly too.

When two bindings still resolve to the same path, `phora sync` stops and names the
contested destination.

## Renaming

A rename in `take` deploys one offered file under another name:

```toml
[targets.demo.sources]
phora = { take = [{ "README.md" = "PHORA.md" }] }
```

The file lands at `./out/PHORA.md` and not at `./out/README.md`. A rename
consumes its source path, so a glob in the same `take` does not deploy it a
second time.

Renames let one file serve several tools that each expect their own name:

```toml
[targets.agents]
path = "~/myproject"

[targets.agents.sources]
dotfiles = { take = [{ ".shared/AGENTS.md" = "AGENTS.md" }] }
claude   = { source = "dotfiles", take = [{ ".shared/AGENTS.md" = "CLAUDE.md" }] }
codex    = { source = "dotfiles", take = [{ ".shared/AGENTS.md" = "codex.md" }] }
```

One file in the source lands three times. Each copy is its own artifact, with its
own record and hash.

The destination must be a relative path inside the target, and nested paths such
as `"a/b.md"` are fine. [take](REFERENCE.md#take) lists the remaining rules.

### Subtree renames

A source path that ends in `/` renames a whole directory. Every offered file
under it moves to the new prefix, and `"."` means the target root:

```toml
[sources.generated]
path = "./.generated"
deploy = "link"

[targets.claude]
path = "~/.claude"
sources.generated = { take = [{ "claude/" = "." }] }   # claude/skills/a/SKILL.md -> skills/a/SKILL.md

[targets.codex]
path = "~/.codex"
sources.generated = { take = [{ "codex/" = "." }] }
```

One generated tree feeds two targets. Within a subtree:

- a literal path or a single-file rename wins for that file;
- the longest matching subtree wins when two overlap;
- each file deploys as its own artifact, so `collapse = true` is rejected.

## Collapse

An artifact starts as a single file. When a binding takes every file in a
directory, phora *collapses* them into one directory artifact: one record, one
destination, and in link mode one symlink.

The `docs/**` take in the `notes` target collapses to a single `docs` artifact.
`phora preview` shows a collapsed directory with a trailing slash, like `docs/`.

A binding's `collapse` setting changes this:

- Leave it out and phora collapses the topmost directory whose files are all
  taken under their own names. In link mode, an excluded file inside a directory
  stops it from collapsing, with a warning. In copy mode the excluded file is left
  out and the directory still collapses.
- `collapse = false` keeps every file its own artifact.
- `collapse = true` requires the directory artifact, and fails naming the
  directory when an exclude or a rename makes that impossible.

```toml
[targets.notes.sources]
phora = { take = ["CHANGELOG.md", "docs/**"], collapse = false }
```

## Staying in sync

`phora sync` makes the disk match your config. It will not overwrite your
changes or delete anything unless you ask.

| You want to | Run |
| --- | --- |
| deploy the locked state | `phora sync` |
| move to the newest commits | `phora update` (or `phora update <source>`) |
| overwrite local changes | `phora sync --force` |
| remove what the config no longer asks for | `phora sync --prune` |
| follow upstream when it deletes files | `phora sync --fast-forward` |
| fail when anything is missing from the lock | `phora sync --frozen` |

`update` is the only command that reaches for new commits. `--force` keeps the
locked commits and only changes how conflicts are settled.

### Conflicts

A conflict is a deployed file you edited, or a file phora did not write sitting
where an artifact should go. On a terminal, sync asks what to do:

```
[s]kip/[o]verwrite/[e]ject/[a]bort?
```

Eject stops managing the artifact and leaves its files where they are. `phora
eject` and `phora uneject` do the same on purpose.

Without a terminal, sync skips each conflict, says so, and carries on:

```
phora: skipping foreign content at <TARGET>/editor; use --force to overwrite
```

`--force` overwrites without asking.

A file whose timestamp changed while its bytes did not is not a conflict. phora
hashes it, sees the content is unchanged, and updates its record.

### Pruning

`--prune` removes what the config stopped asking for: a dropped binding, a
narrowed `take`, a deleted target. It removes only those files, and leaves
their neighbours alone.

For link-mode sources, `--prune` also removes a managed file link whose source
file disappeared from the working tree, as long as your config still selects
that path. Without `--prune`, such a link stops the sync.
A directory link that phora can no longer attribute to a binding stays put. So
does a regular file or directory that replaced a managed link.

If any artifact failed to deploy, sync skips pruning and says so.

### When upstream drops a file

An `update` can move a pin to a commit where a file you deployed no longer
exists. phora stops there, because a config that still asks for a missing file
is more often a mistake than an instruction. The error names the artifact, the
old and new commit, and the remedy.

To follow upstream, add `--fast-forward`. phora deletes the artifacts the new
commit dropped and prints one line for each. It deletes only copied artifacts; a
link it cannot prove it made stays on disk.

`phora update <source> --fast-forward --prune` advances one source, drops what
upstream removed, and prunes what your config removed, in one run.

### Orphans

Delete a `[targets.<t>]` block and its files stay on disk, along with their
registry records. Such a record is an *orphan*. Every sync reminds you:

```
phora: 2 orphaned record(s) with no config target — run `phora list --orphans` to inspect, `phora sync --prune` to remove
```

`phora list --orphans` shows where each orphan lives. The registry stores the
target's path from deploy time, so phora can find the files after the config
that named them is gone. `phora sync --prune` deletes them. If a path cannot be
reconstructed, phora drops the record and warns instead of deleting anything.

### Checking the result

`phora verify` hashes every deployed file and compares it with the record. It
reports every mismatch and exits non-zero if there was one. It skips linked and
ejected artifacts, which carry no hashes. It also fails while a dependency hook
waits for [your approval](#trusting-a-dependencys-hooks).

`phora list` shows each artifact's state, such as `clean`, `modified`,
`outdated`, or `linked`.

## Hooks

A hook is a command phora runs at a fixed point in a sync. There are five:

| Hook | Scope | Runs |
| --- | --- | --- |
| `pre_sync` | `[hooks]` | first, before phora fetches or plans anything |
| `post_prepare` | `[hooks]` | after prepare-phase targets land |
| `pre_deploy` | `[targets.<t>.hooks]` | before any target of its phase is written |
| `on_change` | `[targets.<t>.hooks]` | after that target gained or changed artifacts |
| `post_sync` | `[hooks]` | at the end of every sync that reaches deploy |

Hooks come only from your own `phora.toml` and `phora.local.toml`. A synced
repository that carries a `phora.toml` is content to phora, so it cannot run
anything on your machine. The exception is a
[transitive dependency](#trusting-a-dependencys-hooks), and its hooks wait for
your approval.

`phora sync --no-hooks` turns every hook off, gates included.

### Gates

`pre_sync` and `pre_deploy` are gates: a non-zero exit stops the sync.

```toml
[hooks]
pre_sync = "test -w ~/.config"
```

`pre_sync` runs before phora fetches or plans anything. When it is an array,
every entry runs even after one fails, and then the sync stops. If any entry
fails, nothing is deployed or pruned and no other hook runs. It receives
`$PHORA_TARGETS`, the space-separated names of every configured target.

```toml
[targets.editor.hooks]
pre_deploy = { cmd = ["mise", "trust"] }
pre_deploy_on_fail = "skip"
```

`pre_deploy` runs once phora knows what it will write, and before it writes
anything. Every deploy-phase target's gate runs before the first deploy-phase
target is touched, because a half-applied sync is the hardest state to reason
about. A prepare-phase target's gate runs in the prepare phase, before any
prepare-phase target is written. The hook receives `$PHORA_TARGET` and
`$PHORA_TARGET_PATH`.

By default a failing `pre_deploy` aborts the whole sync. With
`pre_deploy_on_fail = "skip"`, only that target is skipped and the rest deploy.
Either way `phora sync` exits non-zero.

A gate may change the disk, for example to move a file out of the way. phora
looks at the disk again after the gates and plans from what it finds.

Whatever a failed gate did before it failed stays done. Keep gates to checks.

### After the files land

`on_change` runs after a sync that added or changed artifacts in its target. A
sync that changed nothing, or only removed files, does not run it.

```toml
[targets.config.hooks]
on_change = "mise install"

[hooks]
post_sync = "git -C ~/.config add -A"
```

The hook receives `$PHORA_TARGET`, `$PHORA_CHANGED` (deployed paths, one per
line) and `$PHORA_CHANGED_NAMES` (artifact names, one per line). The files are on
disk by then, so it can read them.

phora remembers each successful `on_change` run. A hook that fails is not
remembered: `phora sync` exits non-zero, the files stay, and the hook runs again
on the next sync, even if nothing else changed.

`post_sync` runs at the end of every sync that gets as far as deploying. A gate
that aborts the sync skips it.

### Writing a hook

Every hook accepts the same shapes:

```toml
[hooks]
post_sync = "fc-cache -f"                         # runs under sh -c
pre_sync  = { run = "test -d ~/.config", shell = "bash -c" }
```

```toml
[targets.editor.hooks]
on_change = { cmd = ["nvim", "--headless", "+Lazy! sync", "+qa"] }
```

`run` goes through a shell, which gives you pipes and `$VAR` expansion. `cmd`
starts the program directly, so every argument arrives as written. An array runs
several commands in order. [hooks](REFERENCE.md#hooks) has the full list of
shapes and variables.

The same command under two shells, or once as `run` and once as `cmd`, counts
as two hooks.

## Templating

A source file named `*.tmpl` is rendered with
[minijinja](https://docs.rs/minijinja) and deployed without the suffix, so
`config.toml.tmpl` becomes `config.toml`. Values come from `[vars]`:

```toml
# phora.toml, committed
[vars]
gobin = "~/go/bin"

# mise/config.toml.tmpl in the source:
#   GOBIN = "{{ gobin }}"
```

```toml
# phora.local.toml, on one machine
[vars]
gobin = "~/.local/go/bin"
```

`phora.local.toml` overrides vars one key at a time. The committed config carries
the shape, and each machine fills in its values.

- A binding widens rendering with `template = ["*.conf"]`, on top of `*.tmpl`, or
  turns it off with `template = false`.
- An undefined variable fails that one artifact. Its siblings still deploy.
- A template that loops forever runs out of its budget and fails the artifact.

`phora verify` checks the rendered files, since those are what you deployed. The
lock records only source bytes, so two machines with different vars write the
same lock. Change a var and the next sync re-renders the affected artifacts
without moving any commit.

`phora preview --files` marks rendered files with `(templated)`.

## Link mode

`deploy = "link"` deploys a symlink into the source's working tree in place of a
copy. Edits show up in the target at once, committed or not.

Point the running example at your own checkout of phora, on this machine only:

```toml
# phora.local.toml
[sources.phora]
path = "~/projects/phora"
deploy = "link"
```

The next sync replaces the copies in `./out` with links into
`~/projects/phora`. The `include` from `phora.toml` still applies. A local
`path` with `deploy = "link"` drops the `branch`, because a working tree has no
ref. `phora add --symlink <dir>` declares a linked source in `phora.local.toml`
for you.

- The source must be a local directory. A remote is an error that names the
  source. A relative path counts only if it exists.
- Link mode works in `phora.toml` and in `phora.local.toml`. When `phora.toml`
  declares the source and the linked path is absolute, sync prints a warning
  naming the source, since that path means something else on other machines.
  This includes an overlay like the one above. A relative path never warns.
- With `allow_symlinks = true`, a linked source offers what its symlinks point to.

A linked artifact has no hashes. `phora list` shows it as `linked`, `verify`
skips it, and drift checks ignore it. `--prune` removes the link and leaves the
working tree alone. If a deployed link goes missing, the next sync links it
again. If the file it points to disappears from the working tree, the sync stops
and names it; `phora sync --prune` removes the link.

Switch back to `deploy = "copy"` and the next sync replaces the link with a
checked copy.

## History overlay

A history binding deploys a full copy of a repository that still answers
`git log`, `git blame`, `git show` and `git diff`:

```toml
[sources.gitoxide]
host = "github"
repo = "Byron/gitoxide"

[targets.resources]
path = "resources"

[targets.resources.sources.gitoxide]
history = true
```

The deployment lands at `resources/gitoxide`. It is an ordinary copy that phora
hashes and verifies; the git metadata sits on top. This suits a project that pins
third-party code for agents or other tools to read at a fixed commit.

History belongs to the binding. The same source can deploy as a plain copy in one
target and with history in another. You can create the binding from the command
line, for several targets at once:

```bash
phora add --history Byron/gitoxide --to resources
phora bind gitoxide --history --to resources --to docs
```

A history binding takes the whole repository. It cannot be combined with `take`,
`template`, `collapse`, `root`, `include`, `exclude`, `transitive`, link mode, a
URL source, or `preserve_executable = false`. It allows symlinks unless the source
sets `allow_symlinks = false`.

Things to know before you work inside one:

- Branches and commits you make there live in phora's cache. Clearing the cache
  loses them, so push anything you want to keep.
- A `git log` or `git blame` can fail while phora refreshes the clone. Run it
  again.
- Inside your own repository the deployment looks like an embedded repository.
  Add its path to `.gitignore` unless you mean to track it.
- `phora verify` reports a stale overlay without failing.

## Preparing inputs

A target with `phase = "prepare"` deploys before everything else, so a generator
can build from it. The `post_prepare` hook runs the generator, and ordinary
targets then deploy its output.

```toml
# phora.toml
[sources.input]
path = "./src-input"
include = ["editor/**"]

[sources.output]
path = "./generated"
deploy = "link"

[hooks]
post_prepare = "mkdir -p generated && cp -R stage/editor generated/"
post_sync = "test -f deployed/editor/init.lua && echo smoke-ok"

[targets.input]
path = "stage"
phase = "prepare"
sources.input = { collapse = false }

[targets.output]
path = "deployed"
sources.output = { collapse = false }
```

One `phora sync` then runs:

1. `pre_sync`.
2. The prepare phase: `input` deploys into `stage/`.
3. `post_prepare` builds `generated/` from `stage/`.
4. The deploy phase: `output` links `generated/` into `deployed/`.
5. `post_sync`.

The default phase is `"deploy"`. A target that imports a
[dependency](#transitive-dependencies) passes its phase down to everything the
dependency brings along. The generated directory may be missing on the first run.

Prepare and deploy targets need separate directory trees. phora rejects any
overlap before it writes, including overlap through a symlink.

When a step fails:

- A conflict or a failed artifact in the prepare phase stops the sync before
  `post_prepare`.
- A failing `post_prepare` command stops the commands after it, and the deploy
  phase does not run.
- Prepared files and their new pins stay in place, and the pins of anything not
  yet visited are kept. Fix the cause and sync again.
- phora cannot undo what a hook did. Keep your inputs outside the generator's
  output directory, and have the generator publish its output only after a
  successful build.

A prepare-phase target's own `pre_deploy` and `on_change` run inside the
prepare phase, before `post_prepare`. `post_prepare` runs once per sync and
receives `$PHORA_TARGETS`. `--no-hooks`
suppresses it with the rest, so a `--frozen --no-hooks` replay needs the
generated output to exist already.

Pruning in the prepare phase never touches deploy-phase records. To advance an
input and clear out both dropped inputs and stale generated links, run
`phora update <source> --fast-forward --prune`.

## Transitive dependencies

A *transitive dependency* is a source that is itself a phora project, with its
own `phora.toml`. Import it and phora deploys that project's targets inside
yours.

[`srnnkls/tropos`](https://github.com/srnnkls/tropos) is a set of agent
skills. Its `loqui` skill expects coding guidelines from a separate repository,
[`srnnkls/loqui`](https://github.com/srnnkls/loqui), under
`skills/loqui/reference/loqui/`. Tropos declares that itself:

```toml
# srnnkls/tropos: phora.toml
[sources.loqui]
host = "github"
repo = "srnnkls/loqui"

[targets.loqui]
path = "skills/loqui/reference/loqui"
sources = ["loqui"]
```

You mark tropos `transitive = true` and import it into a target:

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

`phora sync` fetches tropos, reads its manifest, fetches loqui, and deploys
loqui's files under `~/.claude/skills/loqui/reference/loqui/`. One import brings
in two repositories. `transitive = true` alone does nothing; the source needs an
`imports` entry. An import can also pick a ref:

```toml
imports = [{ source = "tropos", tag = "v2" }]
```

### How a dependency composes

The importing target's path is the *anchor*. Each of the dependency's target
paths is joined under it, and the dependency's own layouts apply. You choose
where the whole thing lands; the dependency chooses its shape.

Every imported dependency keeps its sources to itself. If you and tropos both
define `loqui`, each serves its own targets. Dependencies can import further
dependencies, up to 64 levels deep, and a repository reached twice is fetched
once. If two composed targets resolve to the same path, the sync stops and
names it.

### Subsetting a mounted dependency

`[targets.<t>.take]` and `[targets.<t>.collapse]` slice what a dependency
contributes. They are keyed by the name of the imported source:

```toml
[targets.claude]
path = "~/.claude"
imports = ["tropos"]

[targets.claude.take]
tropos = ["gestalt/**"]

[targets.claude.collapse]
tropos = false
```

The value replaces the `take` or `collapse` of every binding in every target of
that dependency, so the patterns match paths inside each bound source. A key
that names no import is ignored. The dependency cannot override these. In
`phora.local.toml`, an empty table clears the base one, and a non-empty table
replaces it.

### Local packages

A dependency can ship its own files as well as its dependencies. Such a
*package* declares itself as a source with `path = "."`:

```toml
# ~/projects/tropos/phora.toml, committed
[sources.tropos]
path = "."
exclude = ["phora.toml"]

[sources.loqui]
repo = "srnnkls/loqui"
include = ["README.md", "languages/**", "resources/**"]

[targets.tropos]
path = "."
sources.tropos = { collapse = false }

[targets.loqui]
path = "skills/loqui/reference/loqui"
sources.loqui = { collapse = false }
```

Inside an imported manifest, `path = "."` means the package's committed snapshot
at the pinned commit. Uncommitted files and your own working directory play no
part. The self source cannot set a ref, `transitive`, or link mode. Any other
local path inside a dependency is rejected.

A consumer can import one package into several targets at different refs:

```toml
[sources.tropos]
path = "~/projects/tropos"
branch = "main"
transitive = true

[targets.canonical]
path = ".phora/canonical"
imports = ["tropos"]

[targets.preview]
path = ".phora/preview"
imports = [{ source = "tropos", branch = "preview" }]
```

A package's files and its manifest share one pin. `phora sync` keeps it,
`phora update tropos --fast-forward` advances it and removes what the new commit
dropped, and `phora sync --frozen` replays it from the cache.

To work on a package in place, link it:

```toml
# phora.local.toml
[sources.tropos]
path = "~/projects/tropos"
deploy = "link"
```

phora now reads the manifest and the self source from the working tree,
uncommitted files included, and deploys the self source as symlinks into it. It
writes nothing into the package. The lock records it as `resolved = "link"`, and
`update` leaves it alone. The package's remote dependencies still lock and
update at pinned commits. Link mode inside a package's own manifest stays
rejected.

Moving a package between a pin and a link keeps its artifacts owned. One
`phora sync --prune` swaps copies for links, or links for copies, and removes
what only the other snapshot offered.

### Confinement

A dependency's manifest is input you did not write, so its writes stay inside its
anchor. phora rejects:

- a dependency target path that is absolute or climbs out with `..`;
- a write through a symlinked directory under the anchor;
- a write into your `phora.toml`, `phora.local.toml`, their lock files, the
  project's `.git`, or phora's cache and state;
- an inner source with a local or `file://` remote;
- an inner source with `deploy = "link"`.

A dependency's `host` sources resolve against your `[hosts]`, so your config
decides the protocol and forge URLs.

### Trusting a dependency's hooks

A dependency's target can carry an `on_change` hook. That command would run on
your machine, so phora asks first. On a terminal, sync prompts `[y/N]` for each
new hook. Without a terminal, or when you answer no, the hook is stripped: phora
records it and does not run it. A sync that strips hooks on a terminal exits
non-zero; without one it succeeds, since the files are deployed.

Review and approve at your own pace:

```bash
phora trust tropos --list              # each hook, and what changed around it
phora trust tropos --show skills/loqui # print a dependency file or directory
phora trust tropos                     # approve hooks one by one
phora trust tropos --revoke            # drop every approval
```

`--list` shows, for a hook you approved before, which files changed since the
approved commit. For a new hook it lists the files you compose from the
dependency. Both work offline from the cache.

Approvals live in your `phora.lock` as `[[trusted_hooks]]`, each pinned to the
command and the dependency commit. When the dependency moves to any new commit,
its hooks need approval again. Pinning trust to the commit means a hook that was
safe yesterday cannot change under you today.

An approved hook runs as you, with your full environment and privileges. phora
checks what runs; it does not sandbox it. Vet a dependency in a VM or container
before you approve hooks you would not otherwise run.

`phora sync --no-transitive-hooks` skips dependency hooks and keeps your own.

### Reproducibility

`phora sync --frozen` fetches nothing. Every source, including every nested
dependency, must already be in the lock and in the cache; a missing pin fails
and names the source and its depth. Link sources are exempt, since they have
nothing to pin.

`--frozen` also works on a read-only state directory, as in CI or a container.
There it confirms that the workspace matches the lock, and fails if it would have
to change anything. Another running phora still makes it exit with status `75`.

## What phora keeps on disk

phora writes four things:

- `phora.lock` in your project: the commit for every source. Commit it.
- `phora.local.lock`: pins for every source named in `phora.local.toml`,
  whether it declares the source or overrides one from `phora.toml`.
- The cache: a bare clone per repository and imported downloads. You can delete
  it; the next sync fetches the locked commits again.
- The state directory: the registry of what landed where, with hashes. It is the
  only record of what phora wrote, so treat it like data.

Both directories follow the XDG conventions and can be moved with `[paths]` or
environment variables. [Files and state](REFERENCE.md#files-and-state) lists the
locations, and [docs/architecture.md](docs/architecture.md) explains how staging,
locking and crash recovery work.

Two phora runs in one project never write at once: the second exits with status
`75`. An interrupted sync is finished or rolled back by the next run.

## When something looks wrong

Walk the pipeline backwards and stop at the first step that surprises you:

1. `phora check-match --source <s> <path>`: did the source offer the path?
2. `phora explain <target> <source> <path>`: did the binding's `take` keep,
   rename, collapse or drop it?
3. `phora preview`: where would the complete plan put it?
4. `phora list`: how does the plan compare with the registry and the disk?
5. `phora verify`: do the deployed bytes still match their record?

Every later step inherits a mistake from an earlier one. If the registry itself
went wrong, for example after restoring a backup, `phora rebuild-registry`
rebuilds it from the lock and the files on disk. See
[troubleshooting](REFERENCE.md#troubleshooting) for recovery recipes.

## Where to look next

- [REFERENCE.md](REFERENCE.md) lists every command, flag and config key.
- [USE-CASES.md](USE-CASES.md) has complete configs for common setups.
- [`phora.example.toml`](phora.example.toml) is an annotated config to copy from.
- The [scrut suites](tests/scrut/) run real syncs in CI. Start with
  [`showcase.md`](tests/scrut/showcase.md).
- [docs/architecture.md](docs/architecture.md) covers the internals.
