# phora, by use case

Pick the situation closest to yours; the sections don't depend on each other.
Every flag and key is in the [reference](REFERENCE.md), and the model behind
them is in the [guide](GUIDE.md).

## Contents

- [Reference repositories for agents](#reference-repositories-for-agents)
- [Dotfiles](#dotfiles)
- [Shared configuration across repositories](#shared-configuration-across-repositories)
- [Pinned agent skills across projects](#pinned-agent-skills-across-projects)
- [Release assets, without curl | tar](#release-assets-without-curl--tar)
- [Vendoring a subtree from a larger repo](#vendoring-a-subtree-from-a-larger-repo)
- [Generating per-agent skills from one canonical set](#generating-per-agent-skills-from-one-canonical-set)
- [Smaller situations](#smaller-situations)
- [Where to look next](#where-to-look-next)

## Reference repositories for agents

Agents answer better when they can read the code you depend on, so repositories
get cloned into a `resources/` directory in project after project. Each clone
sits at whatever commit it was cloned at, carries its full history, and collects
build output, and the same repository ends up cloned twice. phora keeps one
content-addressed store per machine instead and gives each project a pinned
slice of it.

```toml
[sources.duckdb]
repo = "duckdb/duckdb"
tag = "v1.3.2"
include = ["/README.md", "src/include/"]

[sources.axum]
repo = "tokio-rs/axum"
tag = "axum-v0.8.4"

[targets.resources]
path = "resources"
layout = "by-source"
sources.duckdb = {}
sources.axum = { history = true }
```

```
$ phora list
resources:
  axum/axum  history, ✓ clean
  duckdb/README.md  ✓ clean
  duckdb/src  ✓ clean
```

`resources/duckdb` holds the README and the public headers at `v1.3.2`, and
nothing else. The store fetched that one commit without history, and file
contents only for what you selected: its duckdb mirror takes 1.7 MB, where a
full clone takes hundreds of megabytes. `resources/axum` is a
whole checkout with its history, so `git log` and `git blame` work inside it.
The history lives in the store, and the checkout's `.git` file points there.

Put the same `phora.toml` in a second project and its sync deploys from the
same store. Each project deploys its own 11 MB of files, and the one cache
holding both mirrors, axum's full history included, stays at 9.7 MB.

Each project moves on its own schedule: `phora update axum` advances only the
project you run it in. `phora verify` flags any edit, so a reference can't
quietly turn into a scratch area.

Add `resources/` to the project's `.gitignore`, or git treats `resources/axum`
as an embedded repository. A history checkout is read-only: the next sync puts
it back at the pin. A history binding takes
the whole repository and rejects `take`, `collapse` and `template`. See
[History overlay](GUIDE.md#history-overlay) and
[Under the hood](GUIDE.md#under-the-hood).

The store saves fetching and history; each project still gets its own copy of
the files it deploys. To work on a dependency, `phora eject` the history
checkout into a standalone clone.

## Dotfiles

You keep one directory per tool in a dotfiles repository, and every machine
should get the same pinned version under `~/.config`. When a deployed file
drifts from what you committed, you want to hear about it.

```
dotfiles/
  nvim/        # → ~/.config/nvim
  helix/       # → ~/.config/helix
  zsh/         # → ~/.config/zsh
  git/         # → ~/.config/git
```

All four destinations share a parent, so one target covers them:

```toml
[sources.dotfiles]
repo = "mira-sato/dotfiles"     # owner/repo on GitHub
branch = "main"
include = ["nvim", "helix", "zsh", "git"]

[targets.config]
path = "~/.config"
sources = ["dotfiles"]
```

`phora sync` copies the four directories into `~/.config` and pins the commit
in `phora.lock`. The repo's loose root files, like its `README.md`, stay behind.
`phora list` shows one artifact per directory:

```
config:
  dotfiles/git  ✓ clean
  dotfiles/helix  ✓ clean
  dotfiles/nvim  ✓ clean
  dotfiles/zsh  ✓ clean
```

A destination outside the shared parent gets its own source with a `root`, and
the subtree's contents land directly at the target path:

```toml
[sources.nvim]
repo = "mira-sato/dotfiles"
branch = "main"
root = "nvim"

[targets.nvim]
path = "~/.config/nvim"
sources = ["nvim"]
```

Upstream changes arrive when you run `phora update`. A plain `sync` keeps the
locked commit.

### When a file drifts

`phora verify` re-hashes every deployed file and exits non-zero on a mismatch:

```
$ phora verify
dotfiles/nvim: init.lua (content mismatch)
```

You have three ways out. Port the edit back to the repo and run `phora update`.
Run `phora sync --force` to put back the locked version. Or `phora eject` the
artifact and manage it by hand from then on.

A plain `sync` makes you choose. On a terminal it asks
`[s]kip/[o]verwrite/[e]ject/[a]bort?` for each modified artifact. Without a
terminal, in a script or CI, it skips the artifact and names the files:

```
phora: skipping locally modified dotfiles:nvim
    init.lua
  use --force to overwrite
```

### Per-machine values

A file whose name ends in `.tmpl` is rendered with your `[vars]` and deployed
without the suffix, so `git/config.tmpl` becomes `~/.config/git/config`. Set the
shared value in `phora.toml` and override it in `phora.local.toml`, which stays
out of version control:

```toml
# phora.toml
[vars]
git_email = "mira@sato.example"
```

```toml
# phora.local.toml
[vars]
git_email = "mira@larkspur.example"
```

Changing a variable re-renders on the next sync without moving the pin. A
template that uses an undefined variable fails that artifact. See
[Templating](GUIDE.md#templating).

The same overlay can re-point a source or narrow an `include` on one machine,
so the dotfiles repo never needs a `work` branch.

### Post-install steps

A target hook runs after the files land:

```toml
[targets.config.hooks]
on_change = "fc-cache -f"
```

`on_change` runs once per sync, and only when that target's content changed. If
it exits non-zero, the sync fails, the files stay, and the hook runs again next
time. For a check that must pass before anything is written, use `pre_deploy`.
See [Hooks](GUIDE.md#hooks).

### Editing live

While you tune a config, change-sync-check gets slow. Point the source at your
checkout and deploy it by link:

```toml
# phora.local.toml
[sources.dotfiles]
path = "~/dev/dotfiles"
deploy = "link"
```

Each artifact becomes a symlink into `~/dev/dotfiles`, so edits show up without
a sync. `phora list` labels them `linked`, and `verify` skips them. phora warns
that the absolute path isn't portable across machines, which is why this block
lives in the local overlay. `phora add --symlink ~/dev/dotfiles` writes it for
you. Delete the block and the next sync puts verified copies back. See
[Link mode](GUIDE.md#link-mode).

If the repo commits symlinks, for example `.zprofile` pointing at `.zshrc`, set
`allow_symlinks = true` on the source. Otherwise that artifact fails to deploy
and phora names the file.

phora has no secret storage, no encryption, and no machine facts; a template
sees only the `[vars]` you write. If you need those, [dotter](https://github.com/SuperCuber/dotter)
or [chezmoi](https://www.chezmoi.io) fits better. Coming from dotter, its
`recurse = false`, which links a whole directory, corresponds to phora's
`collapse = true`. dotter's default, `recurse = true`, links each file.

## Shared configuration across repositories

A dozen repositories carry diverging copies of the same lint and editor
settings. You want one canonical copy, pinned per repository, with upgrades you
can review.

```
configs/
  lint/        # ruff.toml, eslint.config.mjs, …
  ci/          # reusable workflow fragments
  editor/      # editorconfig and friends
```

Each consuming repo declares what it takes:

```toml
[sources.configs]
repo = "larkspur-labs/configs"
tag = "v7"
include = ["lint", "editor"]

[targets.configs]
path = "etc"
sources = ["configs"]
```

You get `etc/lint` and `etc/editor`; `ci` stays behind. Point your tools at
those paths. Files that must sit at the repo root or under `.github/workflows`
need their own targets, and renames where the names differ.

Each repo has its own lock, so `v8` rolls out one repository at a time:
`phora update && git diff`, then commit. State is keyed by project directory,
so two checkouts of the same repo on one machine track their deployments
separately. In CI, `phora verify` fails the build when someone hand-edits a
deployed file.

### Two versions side by side

Before you move to stricter rules, deploy both and compare. Bindings are keyed
by identity, and each can pin its own ref:

```toml
[sources.configs]
repo = "larkspur-labs/configs"
tag = "v7"
include = ["lint"]

[targets.configs]
path = "etc"
layout = "by-source"

[targets.configs.sources]
current = { source = "configs" }              # the source's v7
next    = { source = "configs", tag = "v8" }
```

`phora preview` shows where each lands:

```
configs -> etc
  current@648f923b lint/ -> etc/current/lint
  next@439ad9ff lint/ -> etc/next/lint
```

`diff -r etc/current etc/next` shows what changes. When `v8` holds up, move the
source's tag forward, go back to a single binding, and run `phora sync --prune`
to remove what the config no longer names. See [Bindings](GUIDE.md#bindings).

### When upstream removes something

If a directory you take disappears upstream, for example when you move the tag
to `v9`, `phora update` stops:

```
error: sync error: selection: configs:configs:editor — a recorded artifact is no longer in the source's offer
matched against: the current offer of source `configs` in target `configs`
binding: source `configs` → target `configs`
pin: recorded 439ad9ff → now v9 (d63d7cd5)
path: etc/editor
remedy: re-sync with `--fast-forward` to drop it, or eject it before removing it
to debug: phora explain configs configs editor
```

`phora update --fast-forward` follows the move and deletes the dropped
directory. Eject it first if you want to keep it.

phora can't merge a shared base with per-repo overrides. A repo that needs to
differ takes a separate artifact, renders the difference from a `.tmpl`, or
ejects the file.

## Pinned agent skills across projects

Claude Code skills accumulate, and each project ends up with its own
copy-pasted fork. You want every project and machine to get a chosen set at a
known version.

Keep the skills in one repository, one directory each:

```
skills/
  scope/
  implement/
  review/
  test/
```

In each consuming project:

```toml
[sources.skills]
repo = "mira-sato/skills"
tag = "v3"               # or branch = "main" to track
root = "skills"

[targets.skills]
path = ".claude/skills"
sources = ["skills"]
```

```
$ phora list
skills:
  skills/implement  ✓ clean
  skills/review  ✓ clean
  skills/scope  ✓ clean
  skills/test  ✓ clean
```

Commit `phora.toml` and `phora.lock`. Everyone who clones the project and runs
`phora sync` gets the same skills at the same commit, and each project moves to
a new version with `phora update` when it's ready.

### A subset per project

A binding's `take` narrows what one target gets, without touching the source or
other consumers:

```toml
[targets.skills]
path = ".claude/skills"

[targets.skills.sources]
skills = { take = ["scope/**", "review/**"] }
```

A typo in a literal path fails with a suggestion:

```
error: sync error: selection: reveiw/SKILL.md — not present in the offer; `take` may not widen the offer
matched against: the offer set
did you mean: review/SKILL.md, test/SKILL.md
remedy: name a leaf the source offers, or add it to the source's include
to debug: phora explain <target> <source> <path>
```

A glob that matches nothing shows up as a warning in `phora preview`. See
[take](REFERENCE.md#take).

### Several bundles in one directory

Team skills and your own can share `.claude/skills` under a `by-source` layout,
each in a directory named after its binding:

```toml
[targets.skills]
path = ".claude/skills"
sources = ["team-skills", "my-skills"]
layout = "by-source"
```

```
skills:
  my-skills/implement  ✓ clean
  my-skills/review  ✓ clean
  my-skills/scope  ✓ clean
  my-skills/test  ✓ clean
  team-skills/deploy-checklist  ✓ clean
```

### Writing a skill

While you work on a skill, link the source to your checkout in
`phora.local.toml`. The `root` from `phora.toml` still applies:

```toml
[sources.skills]
path = "~/dev/skills"
deploy = "link"
```

Edits appear in the consuming project right away. Remove the block and the next
sync restores the pinned copy.

The same shape fits subagent definitions in `.claude/agents`, shared
`CLAUDE.md` fragments, prompt libraries, and MCP server configs: one source per
bundle, one target per destination.

phora treats skill files as bytes. It doesn't check frontmatter or look for a
`SKILL.md`, and `verify` can't tell you whether an agent loads the files.

## Release assets, without curl | tar

You install a tool from a release tarball with `curl | tar` and then lose track
of which version you have. You want the download pinned, checked, and
verifiable later.

```toml
[sources.fzf-bin]
url = "https://github.com/junegunn/fzf/releases/download/v0.55.0/fzf-0.55.0-linux_amd64.tar.gz"
digest = "sha256:4df2393776942780ddab2cea713ddaac06cd5c3886cd23bc9119a6d3aa1e02bd"
include = ["fzf"]

[targets.bin]
path = "~/.local/bin"
sources = ["fzf-bin"]
```

phora checks the digest before it extracts anything, validates every archive
path, and keeps the executable bit:

```
$ phora list
bin:
  fzf-bin/fzf  ✓ clean
```

`phora where --source fzf-bin` traces the binary back to its source, and
`phora verify` catches later tampering. See [Sources](REFERENCE.md#sources) for
what a URL source accepts.

To upgrade, change the URL and the digest together, then run `phora sync`. The
lock records the URL, so a new one triggers a download, and the sync reports the
move:

```
phora: fzf-bin → bin: url (242b8dc5) → bdc18d7c
```

`phora update` downloads and checks the digest again even when the URL hasn't
changed.

This pins one URL for one platform. phora doesn't discover versions, pick a
platform, or manage `PATH`, and the digest is your only guard against what the
URL serves next.

## Vendoring a subtree from a larger repo

Several repos copy protobuf definitions out of a monorepo by hand. Nobody can
say which version any given copy is at.

The producing repo needs no changes. Each consumer declares its slice:

```toml
[sources.platform]
repo = "larkspur-labs/platform"
tag = "v2.3.0"
root = "protos"

[targets.protos]
path = "vendor/protos"
sources = ["platform"]
```

The version now lives in the consumer's `phora.lock`, under version control.
The monorepo's size barely matters: phora fetches the tagged commit without
history, and only the contents of the files under `protos`.

Check what would ship before you sync:

```
$ phora preview
protos -> vendor/protos
  platform@5d52f43a billing/ -> vendor/protos/billing
  platform@5d52f43a identity/ -> vendor/protos/identity
```

Each directory taken whole becomes one artifact. `phora preview --files` lists
the files inside, and `phora explain protos platform billing/invoice.proto`
says why one path is included. See [Collapse](GUIDE.md#collapse),
[preview](REFERENCE.md#phora-preview), and [explain](REFERENCE.md#phora-explain).

Upgrades are per consumer, and the
[two-versions pattern](#two-versions-side-by-side) works here when a migration
needs both in the tree at once.

phora delivers the files. It doesn't run `protoc`, regenerate stubs, or notice
that a new schema breaks your code.

## Generating per-agent skills from one canonical set

You keep your agent skills, agents and instructions in one repository, and each
harness (Claude Code, Codex, pi) wants them in its own format and directory. A
compiler can translate them, but it needs pinned inputs before it runs, and its
output still has to land in each harness's home. A build source covers both
ends.

This is how [srnnkls/dotfiles](https://github.com/srnnkls/dotfiles) deploys
[tropos](https://github.com/srnnkls/tropos), compiled by
[henia](https://github.com/srnnkls/henia). Tropos is a phora package: its own
`phora.toml` offers the canonical files and vendors one dependency under the
skill that needs it.

```toml
# tropos/phora.toml
[sources.tropos]
path = "."
include = ["skills/**", "agents/**", "instructions/**", "rules/fas/**", "henia.toml", ".henia/harnesses/**", "phora.toml"]

[sources.loqui]
git = "https://github.com/srnnkls/loqui.git"
branch = "main"
include = ["README.md", "languages/**", "resources/**"]

[targets.tropos]
path = "."
sources.tropos = { collapse = false }

[targets.loqui]
path = "skills/loqui/reference/loqui"
sources.loqui = { collapse = false }
```

The dotfiles repository compiles that package into a build source:

```toml
# dotfiles/phora.toml
[hooks]
post_sync = "scrut test tests/scrut/tropos.md"

[sources]
tropos = { repo = "srnnkls/tropos", branch = "main", transitive = true }
henia = { build = { inputs = ["tropos"], run = "henia build $PHORA_INPUT/tropos --output $PHORA_OUTPUT --harness claude,codex,pi", key = "henia --version" } }
```

Home directories are machine-local, so the deploy targets live in
`phora.local.toml`. Each one takes its harness's slice of the compiled output
and re-roots it at the target:

```toml
# dotfiles/phora.local.toml
[targets]
claude = { path = "~/.claude", sources.henia = { take = [{ "claude/" = "." }], collapse = false } }
codex = { path = "~/.codex", sources.henia = { take = [{ "codex/" = "." }], collapse = false } }
pi = { path = "~/.pi/agent", sources.henia = { take = [{ "pi/" = "." }], collapse = false } }
```

One `phora sync` then runs:

1. phora deploys tropos at its pinned commit into a scratch directory, with
   loqui under `skills/loqui/reference/loqui`.
2. henia compiles that tree into `$PHORA_OUTPUT/claude`, `codex` and `pi`, and
   phora commits the output into its cache.
3. Each harness's files are copied into its home. `{ "claude/" = "." }` turns
   `claude/skills/bash/SKILL.md` into `~/.claude/skills/bash/SKILL.md`.
4. `post_sync` runs a scrut check on the finished deployment.

`phora preview --target claude` shows the rename for each file:

```
claude -> ~/.claude
  henia@3f9c21ab claude/CLAUDE.md -> CLAUDE.md -> ~/.claude/CLAUDE.md
  henia@3f9c21ab claude/agents/reviewer.md -> agents/reviewer.md -> ~/.claude/agents/reviewer.md
  henia@3f9c21ab claude/skills/bash/SKILL.md -> skills/bash/SKILL.md -> ~/.claude/skills/bash/SKILL.md
```

henia runs again only when the files it reads from tropos or loqui change, or
`henia --version` prints something new. Its output is locked and hashed, so `phora verify` catches a hand
edit in `~/.claude`. If henia fails, the previous output stays deployed and the
sync exits non-zero.

To work on tropos itself, point the package at a checkout in
`phora.local.toml`. The build then reads the working tree, uncommitted edits
included, and reruns whenever a file changes:

```toml
[sources.tropos]
path = "~/projects/tropos"
deploy = "link"
```

To move to a newer tropos and drop skills it removed, run
`phora update tropos --fast-forward --prune`. See
[Building sources](GUIDE.md#building-sources),
[Local packages](GUIDE.md#local-packages) and [Renaming](GUIDE.md#renaming).

phora runs henia and deploys what it writes; it doesn't know the harness
formats.

## Smaller situations

Git hook scripts copied into every repository. Deploy a pinned set and let a
target hook wire it up once the files land:

```toml
[sources.git-hooks]
repo = "larkspur-labs/git-hooks"
tag = "v1.2.0"

[targets.githooks]
path = ".githooks"
sources = ["git-hooks"]

[targets.githooks.hooks]
on_change = "git config core.hooksPath .githooks"
```

Runbooks in a separate ops repo that responders have to go looking for during
an incident. Give each service repository the runbooks for its own service:

```toml
[sources.runbooks]
repo = "larkspur-labs/ops"
branch = "main"
root = "runbooks"
include = ["billing"]

[targets.runbooks]
path = "docs/runbooks"
sources = ["runbooks"]
```

## Where to look next

- The [reference](REFERENCE.md) for every command, flag, and config key.
- The [guide](GUIDE.md) for how phora works.
- [`phora.example.toml`](phora.example.toml) and
  [`phora.local.example.toml`](phora.local.example.toml) for annotated configs.
- The scrut suites under [`tests/scrut/`](tests/scrut), each a runnable
  walkthrough checked in CI: [showcase](tests/scrut/showcase.md) end to end,
  [selection](tests/scrut/selection.md) for offer and take,
  [mapped](tests/scrut/mapped.md) for renaming,
  [versions](tests/scrut/versions.md) for two versions side by side,
  [templates](tests/scrut/templates.md), [hooks](tests/scrut/hooks.md),
  [drift](tests/scrut/drift.md), [orphans](tests/scrut/orphans.md),
  [release-assets](tests/scrut/release-assets.md),
  [history](tests/scrut/history.md) for the history overlay,
  [build](tests/scrut/build.md) for build sources,
  [transitive](tests/scrut/transitive.md) for composed dependencies,
  [lifecycle](tests/scrut/lifecycle.md) for the core add-sync-verify loop,
  [manage](tests/scrut/manage.md) for eject, update and rebuild-registry, and
  [query](tests/scrut/query.md) for `where`, `check-match` and list states.
