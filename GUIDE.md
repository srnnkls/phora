# The phora guide

Every flag and config key is listed in [REFERENCE.md](REFERENCE.md); the mechanisms behind a sync are in
[Under the hood](#under-the-hood).

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
- [Under the hood](#under-the-hood)
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

Everything phora reads becomes a git commit in one content-addressed store per
machine, including a downloaded tarball. That is why a URL source locks, deploys
and verifies the same way a repository does. Every project on the machine shares
the store: a repository is fetched once, only at the commits you pin and only for
the files you select unless a binding asks for [history](#history-overlay), and
each project gets a copy of its own slice.
[Under the hood](#under-the-hood) shows how the store is laid out.

## Your first sync

Start with an empty directory and a `phora.toml` that deploys phora's own README:

```toml
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
2. It resolved `main` to one commit, fetched that commit into its cache, and
   wrote it to `phora.lock`. The cache gets the commit alone, without history,
   and only the file contents the source selects: here, one README.
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

All spellings of one repository share a single cached mirror, so switching
between https, ssh and the `host` form never fetches it again.

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

Both bindings read from one mirror and resolve to two commits. Each distinct ref
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

### A worked example

The [dotfiles](https://github.com/srnnkls/dotfiles) repository uses two hooks to
compile agent skills between the two phases of a sync:

```toml
[hooks]
post_prepare = "henia build .tropos --output .henia --clean --harness claude,codex,pi"
post_sync = "scrut test tests/scrut/tropos.md"

[targets.tropos]
phase = "prepare"
path = ".tropos"
imports = ["tropos"]
```

The prepare phase stages the pinned tropos package in `.tropos`. `post_prepare`
compiles it into `.henia`, and the deploy phase links the output into each
harness's home. `post_sync` then checks the finished deployment. If the compiler
fails, nothing is deployed and the check doesn't run. The full setup is in
[Generating per-agent skills](USE-CASES.md#generating-per-agent-skills-from-one-canonical-set).

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

A history binding is the one thing that makes phora fetch a repository's full
history; every other binding gets only its pinned commits. Once a repository's
mirror has full history it keeps it. `phora sync --frozen` fails when a history
binding meets a mirror that holds only pinned commits, because it can't fetch
the rest.

A history binding takes the whole repository. It cannot be combined with `take`,
`template`, `collapse`, `root`, `include`, `exclude`, `transitive`, link mode, a
URL source, or `preserve_executable = false`. It allows symlinks unless the source
sets `allow_symlinks = false`.

A history deployment is read-only: it shows the pinned commit and nothing else.
When its `HEAD` or index moves off the pin, `phora verify` fails and the next
`phora sync` puts them back, dropping any commit made there. A branch created
inside lands in the shared cache mirror, where every other overlay of that
repository sees it.

To work on the code, eject it:

```bash
phora eject gitoxide --source gitoxide --target resources
```

The deployment becomes a standalone clone: a real `.git` directory with its own
objects, hard-linked from the cache where possible, detached at the pin, with
`origin` set to the upstream and its branches as `origin/*`. Local edits show up
in `git status`. phora stops managing the directory, and `phora uneject` refuses
while the clone's `.git` is there.

Inside your own repository the deployment looks like an embedded repository. Add
its path to `.gitignore` unless you mean to track it.

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
dependency. Both read from the cache, fetching a missing commit or file from
the dependency's remote when the cache lacks it.

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

## Under the hood

### What phora keeps on disk

phora writes four things:

- `phora.lock` in your project: the commit for every source. Commit it.
- `phora.local.lock`: pins for every source named in `phora.local.toml`,
  whether it declares the source or overrides one from `phora.toml`.
- The cache: one mirror per repository, holding just the pinned commits and the
  files you deploy from them, plus imported downloads. You can delete it; the
  next sync fetches the locked commits again.
- The state directory: the registry of what landed where, with hashes. It is the
  only record of what phora wrote, so treat it like data.

Both directories follow the XDG conventions and can be moved with `[paths]` or
environment variables. [Files and state](REFERENCE.md#files-and-state) lists the
locations.

Two phora runs in one project never write at once: the second exits with status
`75`. An interrupted sync is finished or rolled back by the next run.

### Cache and state

phora keeps two trees. The *cache root* holds regenerable git mirrors. The
*state root* holds per-project records that can't be regenerated.

The cache root contains `git/`:

```
<cache>/git/
  <mirror-key>.git/                  bare mirror
  <mirror-key>.git.lock              per-mirror flock
  .<mirror-key>.staging-<pid>-<n>/   clone or import in progress
  .phora-download-<pid>-<n>.tmp      URL download in progress
```

The state root contains one directory per project:

```
<state>/projects/<project-id>/
  locks/state.lock                                  per-project lock
  locks/journal.toml                                deploy journal
  targets/<target>/meta.toml                        ejections, hook state
  targets/<target>/artifacts/<identity>/<artifact>.toml   registry record
```

The record path uses the binding *identity*, so two bindings of one source in
the same target keep separate records.

One mirror serves every source that names the same remote. phora maps
equivalent spellings of a remote to one string:

1. Trim whitespace and a trailing `/`.
2. Rewrite scp-style `git@host:owner/repo` to `host/owner/repo`.
3. Otherwise drop the scheme and any `user@` prefix.
4. Strip a trailing `.git`.
5. Lowercase the host.

The mirror key is the first 16 hex characters of the BLAKE3 hash of that
string. HTTPS and SSH spellings of one repository share a mirror.

The project id is the first 16 hex characters of the BLAKE3 hash of the
canonicalized project root. A symlinked checkout and its target resolve to the
same id and share records. Two clones at different paths get different ids.

### Mirrors

A mirror is a bare repository. Resolving a ref looks up a commit id, and reading
a file walks tree objects and reads the blob from the object database. Nothing
is checked out for copy-mode deployment. Two bindings at different refs of one
source are two commit ids in one mirror, sharing every unchanged object.

A mirror is either sliced or full:

- A sliced mirror holds depth-1 slices of the revisions its bindings pin
  (branch, tag, default `HEAD` or commit id), merged into one bare repository.
  Its fetches carry the `blob:none` filter, so it holds commits and trees but no
  file contents until something reads them. A `phora-sliced` file in the mirror
  marks it.
- A full mirror holds every head and tag with full history. Only a
  `history = true` binding makes a mirror full, and a full mirror is never
  narrowed again. A mirror without the marker, including every cache created
  before slicing existed, is full.

Selection runs on trees, so phora can list what a sliced mirror offers without
any blobs.

Two exceptions add files outside the object database. A history binding creates
linked-worktree administration inside the mirror and a gitlink in the target
(see [How a history overlay is built](#how-a-history-overlay-is-built)). A
mirror that serves history bindings also carries `refs/phora/worktrees/*` pins.

### Fetching a repository

Before resolving, phora plans one fetch per mirror, with every revision the
mirror's bindings pin and whether any of them sets `history`. One fetch then
serves every source on that mirror.

Refreshing a mirror runs under the mirror's flock:

1. Remove `.<mirror-key>.staging-*` directories whose newest mtime is more than
   an hour old. A younger one may belong to a running clone.
2. Pick the fetch. A planned history binding asks for a full mirror. Otherwise
   the fetch is a slice of the planned revisions plus the requested one.
3. If the mirror opens and is sliced, and a slice is wanted: skip the fetch when
   the mirror already holds the revision, else fetch the slice at depth 1 with
   `blob:none`.
4. If the mirror opens and is full: detach the `HEAD` of every managed linked
   worktree (`worktrees/ph-*`), then fetch with the refspecs
   `+refs/heads/*:refs/heads/*` and `+refs/tags/*:refs/tags/*`.
5. If the fetch reports a rejected ref update, return the error. The mirror
   stays as it is.
6. If the mirror is missing, is not a repository, or the fetch fails for any
   other reason, re-clone. A sliced mirror that now needs history takes this
   path: its commits would be advertised as already present, and the server
   would skip blobs it never sent, so it is re-cloned whole.

A sliced clone initializes a bare repository, saves an `origin` remote, writes
the marker file, and runs one filtered fetch. A full clone is a bare clone with
the mirror refspecs. Either way the new mirror is built in a staging directory.
Before the swap, a re-clone carries over every managed worktree whose pin commit
exists in the new clone: the `worktrees/ph-<id>/` administration directory and
its `refs/phora/worktrees/<id>` pin. Then it removes the old mirror and renames
the staging directory into place. A failed clone leaves the old mirror
untouched. An open failure other than "not a repository" is an error and doesn't
trigger a re-clone, because it may be transient.

The filtered fetch keeps its connection open for the blob fetch that follows.
Blobs arrive in two ways:

- After resolving a source, whether it fetched or reused its lock entry, phora
  fetches the missing blobs of the selected files in one request that names the
  object ids and sends no haves, over the connection the tree fetch left open.
- Any other read of a missing blob (a dependency's `phora.toml`,
  `.gitattributes`, `phora trust --show`) fetches that one blob by id.
  `phora trust` diffs fetch a trusted commit the mirror lacks the same way.

Resolution is a local lookup. A `branch` peels `refs/heads/<name>`, a `tag`
peels `refs/tags/<name>`, a `rev` is parsed as a 40- or 64-hex object id, and a
source with no ref takes the mirror's `HEAD`.

A history binding over a sliced mirror can't use the cached snapshot. On a lock
hit, sync refreshes that mirror anyway. Under `--frozen`, which never fetches,
the run fails instead of deploying a truncated history.

### Importing a download

A URL source is imported in four steps:

1. Download into `<cache>/git/.phora-download-<pid>-<n>.tmp`. phora follows
   redirects itself, up to 10. A redirect may go to `https`, or to `http` only
   when the original URL was `http`. Connecting times out after 30 seconds and
   reading the body after 5 minutes. A non-2xx status is an error. The temporary
   file is removed on every exit path.
2. Verify. When the source declares a `digest`, hash the downloaded bytes with
   its algorithm (`sha256` or `blake3`) and compare. A mismatch stops here.
3. Extract in memory. The format is detected from the bytes: gzip-compressed
   tar, tar, zip, or a single raw file named after the URL. Each entry path must
   be relative, contain no `..`, backslash or NUL, and not start with a drive
   letter. Extraction stops once the decompressed total passes 1 GiB. A single
   common top-level directory is stripped.
4. Take the mirror's flock and import the entries as git objects. Colliding
   entries, or a file where a directory is expected, are rejected.
   `refs/heads/phora` points at the new commit.

The flock is taken only for step 4, so two runs downloading the same archive
overlap on the slow part.

The import is deterministic. The commit has a fixed author and committer
(`phora <phora@localhost>`), the time 1 second after the epoch, the message
`phora synthetic import`, and no parents. Tree entries are sorted in git order
before writing. The commit id therefore depends only on file paths, modes and
contents. Re-importing unchanged bytes produces the same id and leaves the lock
unchanged.

A URL source resolves to `refs/heads/phora`, or to a pinned commit. Asking it
for a branch, tag or default ref is an error.

### Capturing a working tree

A `deploy = "link"` source is captured before it resolves. phora walks the
canonical working-tree root, skipping a cache directory inside it, and imports
the files into a mirror keyed by the root path, using the same import as URL
sources. The resulting commit id identifies the capture. Projection and
copy-mode reads use that frozen tree. Link artifacts point at the live files.

### The sync pipeline

A sync runs these steps. Each step's progress phase, where it has one, is in
parentheses.

1. Merge `phora.toml` with `phora.local.toml` and validate.
2. Run the `pre_sync` hooks. If one fails, the run ends here.
3. Compose transitive dependencies into the config (compose).
4. If any target has `phase = "prepare"` or `post_prepare` is set, split the run
   (see [Preparation](#preparation)). Otherwise run the workspace pass once over
   every target.

A *workspace pass* runs:

1. Open the journal and run the recovery sweep.
2. Resolve every source (resolve).
3. Project every target (project), check the sealed offer, and plan
   `--fast-forward` drops.
4. With `--prune`, sweep stale history-overlay administration.
5. Observe the disk and reconcile it against the projection (observe). Resolve
   conflicts by prompt or policy.
6. Run each target's `pre_deploy` hooks. A failure under
   `pre_deploy_on_fail = "abort"` ends the run; under `"skip"` it skips that
   target.
7. If any `pre_deploy` hook ran, observe and reconcile again (observe). Earlier
   conflict answers are reused.
8. Apply fast-forward drops, then deploy each target's changes (apply).
9. If nothing failed, apply the reconciled removals (prune). With `--prune`,
   these include records the projection no longer produces.
10. Run hooks (hooks): each target's `on_change`, then the global `post_sync`,
    then trusted transitive `on_change` hooks.

Under `--frozen` on a read-only state root the run holds no lock. If observation
finds anything to write, including a stat refresh, the run stops with an error
naming the state root.

### Preparation

[Prepare-phase targets](#preparing-inputs) split one run into two passes that
share the registry and journal:

1. Partition targets by `phase`. Each pass resolves the sources its own targets
   bind. A deploy target whose path lies inside a prepare target is rejected. A
   deploy target above a prepare target is projected first, and rejected if any
   artifact lands inside the prepare tree.
2. Run the workspace pass over the prepare targets, with the global `[hooks]`
   removed.
3. If that pass skipped or ejected anything, or failed, the run fails. Lock
   entries resolved so far are merged into the previous lock.
4. Run `post_prepare` hooks in order, stopping at the first failure. A failure
   fails the run.
5. Rebuild the prepare roots from the current config, so symlinks a generator
   created are seen, and run the workspace pass over the deploy targets. Sources
   resolve against the lock that the prepare pass produced.

### Resolution and the lock

phora turns the config into resolution units. A unit is one pair of source and
effective ref. Units group by mirror key. Groups resolve in parallel; units
inside a group resolve one after another, so a shared remote is fetched once. A
git unit fetches only if no earlier unit in its group already did. A URL unit
always downloads when it has no lock hit, because each source verifies its own
digest.

The number of parallel workers is `--jobs` when given, else
`min(units, max(50, 2 × cores))`. The ceiling is at least 50 because fetching
waits on the network. `--jobs 0` is rejected.

Composition, staging and applying run on the main thread. Only resolution is
parallel.

`phora.lock` holds one entry per unit. An entry records:

- `name`, `git` (the remote or URL), `resolved` (the effective ref, or `url`, or
  `link`), `commit`;
- `digest`: the BLAKE3 framed digest of the source's offered bytes at that
  commit;
- `config_digest`: BLAKE3 over the source's `include`, `exclude` and `root` and
  its `allow_symlinks` and `preserve_executable` settings;
- `ref`: the kind-tagged ref (`branch:x`, `tag:x`, `rev:x`), present only when a
  binding overrides the source's ref;
- `instance`: the owning transitive instance, absent for the consumer's own
  sources.

A binding's `take` never enters the lock. Narrowing a take changes the registry
records and moves no commit. A source listed in `phora.local.toml` locks into
`phora.local.lock`; transitive entries always go to the base lock. When merging
the two, entries match on name, `ref` and `instance`.

A link-mode source locks as `resolved = "link"` and `digest = "link:"`, with the
working tree's `HEAD` as commit, or `link` when it has none.

The lock also holds `[[trusted_hooks]]` and `[[candidate_hooks]]` for transitive
hooks. Both are omitted when empty.

A unit reuses its entry when:

- git source: the normalized resolved remote, the effective ref, and
  `config_digest` all match;
- URL source: the normalized URL and `config_digest` match. The synthetic commit
  is content-addressed, so the URL and config fully identify it.

On a match, phora resolves the locked commit from the cache. If the commit is
missing, it fetches, unless `--frozen` is set, in which case it errors. The
source digest is reused when the commit is unchanged. Without a match, the unit
resolves its ref from the network, and `--frozen` fails naming the source.
`update` drops the entries it advances before running the same path.

### Staging, applying and recovery

phora writes each artifact into a staging directory first:

```
<parent of the artifact's destination>/.phora-stage/<artifact>-<n>/
```

The staging directory sits beside the artifact's destination so the final rename
stays on one filesystem. For each leaf, staging:

1. skips a destination with a `.git` path component, unless an `include` pattern
   has a `.git` segment or the binding is a history overlay;
2. renders `*.tmpl` files with the effective vars;
3. writes the bytes, sets the executable bit when the source had it and
   `preserve_executable` is on, and sets the mtime to the commit's author time;
4. rejects a symlink unless `allow_symlinks` is on (the default for history
   bindings), and rejects one whose target leaves the artifact;
5. rejects two leaves whose deployed names fold to the same path;
6. adds the entry to the manifest (size, mtime, BLAKE3) and to the artifact
   digest.

The artifact digest and the lock's source digest use one framing. Each entry
contributes its path length as a little-endian u64, the path, a type tag
(`\0file\0`, `\0exec\0` or `\0link\0`), the payload length and the payload.
Without the lengths, two different trees could hash alike.

Applying moves a staged artifact into place:

1. Append a journal entry (staging path, destination, record,
   `swap_completed = false`).
2. Rename an existing destination to `.phora-stage/.phora-backup-<name>`.
3. Rename the staging directory onto the destination. If the rename fails with a
   cross-device error, copy instead (reflink when available) and warn.
4. Mark the entry `swap_completed = true`.
5. For a history binding, publish the overlay.
6. Write the registry record.
7. Remove the journal entry.

If step 5 or 6 fails, the destination is removed, the backup is restored, and
the journal entry is dropped. Link-mode artifacts follow the same journal
protocol with a symlink created in `.phora-stage/` instead of a staged tree.

phora installs no signal handler. Ctrl-C kills the process, and the journal and
staging directories leave enough to recover on the next run.

The recovery sweep runs at the start of each workspace pass:

1. For each journal entry: if the swap completed, write its record. Otherwise
   restore the backup if one exists and remove the staging path. Then drop the
   entry.
2. Remove `.phora-stage*` entries in the parent of every configured target path,
   or in the confine anchor of a composed target.

Step 1 covers every journaled deploy. Step 2 scans only target parents, so a
staging directory abandoned inside a target directory before its journal entry
was written stays until that directory is next deployed.

Under `--frozen` on a read-only state root, a pending journal entry is an error;
nothing is discarded.

### Registry and drift

A registry record stores the target, identity and artifact name, the underlying
source, the commit, the staged artifact digest, the layout, the export policy,
and a manifest of every file with size, mtime and BLAKE3 hash. Optional fields
cover:

- `directories`: device, inode and mtime of each staged directory, for spotting
  added files without a full walk;
- `linked`, for link-mode artifacts;
- `history`, `worktree_admin_id`, `mirror_key`, `cache_git_root`, for history
  overlays;
- `vars_digest`, for templated artifacts;
- `deploy_root` and `layout_separator`, the target path and prefixed-layout
  separator as they were at deploy time.

phora never recomputes `deploy_root` or `layout_separator`, so a record whose
target left the config still names where its files are.

Records are written to a temporary file and renamed into place.

Drift detection classifies each recorded artifact. For every manifest file:

1. Stat without following links. A missing file, or a file that is no longer a
   regular file, is modified.
2. If size and mtime match the manifest, the file is clean without reading it.
3. Otherwise open the file and check that the descriptor has the inode the stat
   saw. Read it, stat the descriptor again, and hash the bytes.
4. If the stat stayed stable and the hash matches, the file is revalidated with
   its new size and mtime. Any read error, inode mismatch or hash mismatch makes
   it modified.

A symlink entry matches when the link target's length and hash match. New files
inside the artifact are found through the `directories` snapshot; history
artifacts use a full scan that ignores `.git`.

The artifact is modified if any file is. Otherwise it is outdated if the commit
or the vars digest changed, revalidated if some file only changed stat, and
clean if nothing changed. `sync` writes revalidated stats back to the record so
the next run takes step 2. A partly modified artifact keeps its old stats.

`phora verify` hashes every manifest file regardless of stat, skips linked and
ejected records, and also fails on untrusted transitive hook candidates. History
overlay findings are reported without failing.

Templated artifacts carry two digests that answer different questions:

- The lock's `digest` covers source bytes before rendering. Two machines with
  different vars write the same lock.
- The record's manifest and artifact digest cover rendered bytes. `verify` and
  drift detection compare against what was deployed.

The record's `vars_digest` is BLAKE3 over the effective vars (base overlaid with
local), framed per key. When it differs from the current vars, the artifact is
outdated and redeploys, even though no commit moved. A rendering error fails
only that artifact.

### Locks

- Per project: `sync`, `update`, `eject`, `uneject`, `rebuild-registry` and
  `trust` take a non-blocking exclusive lock on `locks/state.lock` for the whole
  command. If another process holds it, the command exits 75 (`EX_TEMPFAIL`). If
  the state root is on a network filesystem (NFS, SMB, CIFS, AFP, WebDAV),
  `sync` prints a one-line warning that the lock may not exclude other machines.
- Per mirror: fetch, URL import, working-tree capture and history-overlay
  changes take a blocking flock on `<mirror-key>.git.lock`. The cache root is
  shared across projects, so this lock serializes two projects fetching one
  remote.

### Hook dispatch

[Hooks](#hooks) come only from your own config files. In a run, hook scopes go
in this order:

1. `pre_sync`, before any source is resolved.
2. The prepare pass, when prepare-phase targets exist: their `pre_deploy` gates,
   their deploys, their `on_change` hooks, then trusted transitive `on_change`
   hooks under prepare-target paths. Global `[hooks]` don't run inside this
   pass.
3. `post_prepare`, after the prepare pass succeeds.
4. The deploy pass: `pre_deploy` gates of deploy-phase targets, their deploys,
   `on_change`, `post_sync`, then trusted transitive `on_change` hooks.

Commands within a scope are deduplicated by command and shell.

`on_change` fires by comparing digests:

1. Each hook has an id: target name, command, and the shell or `exec` for the
   `cmd` form.
2. On success, phora stores the set of artifact digests in the target at that
   moment (`meta.toml`).
3. On the next run, the hook fires if any current record has a digest outside
   the stored set.

Removing an artifact leaves every remaining digest in the set, so removals alone
don't fire the hook. A failed hook records nothing and fires again next run.
`post_sync` runs on every run that reaches the hook phase. The only accepted
`when` value is `"always"`.

A gate failure (`pre_sync`, or `pre_deploy` with `abort`) ends the run before
any drop, deploy or prune in its pass, and later hooks don't run. A deploy-phase
`pre_deploy` abort comes after the prepare pass, so prepare-phase targets stay
deployed. What the hook itself did is not undone.

A [transitive hook candidate](#trusting-a-dependencys-hooks) is pinned by a
preimage: BLAKE3 over the command, shell, hook kind and the dependency's
resolved commit. It runs only when a `[[trusted_hooks]]` entry pins that
preimage, or when the user approves it at the prompt. A new dependency commit
changes the preimage and needs approval again.

### Transitive composition

phora walks `transitive = true` imports one at a time, before resolution:

1. Resolve the dependency and read its `phora.toml` at that commit. Only
   `[sources]`, `[targets]` and per-target hooks are kept; trust fields and the
   global `[hooks]` are dropped.
2. Key the download as a *fetch node*: normalized URL, ref and commit. Two paths
   to the same node fetch once.
3. Key its use as an *instance*: parent, source name, anchor target and fetch
   node. The same node mounted twice is two instances with separate names, hooks
   and confinement.
4. Compose each dependency target as a synthetic target at
   `<anchor path>/<dependency target path>`. Two composed targets with one
   destination are an error.
5. Recurse into the dependency's own imports, up to depth 64.

At depth 1 a source may use a local `path`, including the package's self source
`path = "."`. Deeper, local paths, relative paths and `file://` remotes are
rejected. Inner sources can't use `deploy = "link"`; the imported source itself
can.

Every composed write is confined to its anchor. It is also kept out of a
protected set: the project's `phora.toml`, `phora.local.toml`, `phora.lock`,
`phora.local.lock` and `.git`, the state root's `projects/` directory, and the
cache root. [Confinement](#confinement) lists the rest of the rules.

### How a history overlay is built

A [history binding](#history-overlay) deploys the source's whole tree through
normal staging. phora adds the git metadata after the swap, making the
destination a linked git worktree of the mirror.

The overlay's admin id is the first 16 hex characters of a framed BLAKE3 over
the canonical project root, the deploy root, the target name and the binding
identity. Publishing runs under the mirror's flock:

1. Build the administration in `<mirror>/.ph-<id>.staging-<pid>-<n>/`: `HEAD`
   with the detached commit, `commondir` (`../..`), `gitdir` pointing at
   `<deploy root>/.git`, and an index built from the commit's tree with the stat
   data of the deployed files.
2. Create empty directories in the deploy root for submodule entries.
3. Write `<deploy root>/.git.phora-staging-<n>` containing
   `gitdir: <mirror>/worktrees/ph-<id>`.
4. Move an existing `worktrees/ph-<id>` aside to `.ph-<id>.backup-<pid>-<n>`,
   rename the staging directory into place, write the pin
   `refs/phora/worktrees/<id>`, and rename the staged gitlink to `.git`. Then
   remove the backup.

The pin keeps the commit reachable in the mirror. Fetches detach each managed
`HEAD` first so a ref update is not rejected, and a re-clone carries the
administration and pins across.

Observation checks that `HEAD`, the gitlink back-pointer and the index still
match the recorded commit. A stale overlay is republished without redeploying
files. A root `.git` entry in the source tree is rejected. If the source has a
`.gitattributes` with `text`, `eol`, `ident` or `filter` attributes, or the
mirror sets `core.autocrlf`, sync warns that `git status` may show changes.

With `--prune`, sync sweeps each mirror that a history record uses: it removes
`worktrees/ph-*` directories whose gitlink no longer points back, their pins,
and leftover staging and backup directories. When the mirror is gone, it removes
the gitlink from the target instead.

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
- [Under the hood](#under-the-hood) covers what a sync does to your disk, step by step.
