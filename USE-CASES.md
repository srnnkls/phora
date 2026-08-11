# phora, by use case

Start with the situation that matches yours; the sections are independent. Each
full recipe states its limits and then gives a working configuration, and the
final catalogue only sketches smaller fits. Use the [README](README.md) for
reference and the [guide](GUIDE.md) for internals.

## Contents

- [Dotfiles](#dotfiles)
- [Shared configuration across repositories](#shared-configuration-across-repositories)
- [Pinned agent skills across projects](#pinned-agent-skills-across-projects)
- [Release assets, without curl | tar](#release-assets-without-curl--tar)
- [Vendoring a subtree from a larger repo](#vendoring-a-subtree-from-a-larger-repo)
- [Smaller situations to recognize](#smaller-situations-to-recognize)
- [Where to look next](#where-to-look-next)

## Dotfiles

You keep one directory per tool in a dotfiles repository and want a pinned
source version deployed under `~/.config` on every machine, with drift
detection. Before migrating, note that phora deploys copies by default and
provides no secret storage, encryption, or automatic machine facts; if those are
central, use [dotter](https://github.com/SuperCuber/dotter) or
[chezmoi](https://www.chezmoi.io) instead.

```
dotfiles/
  nvim/        # → ~/.config/nvim
  helix/       # → ~/.config/helix
  zsh/         # → ~/.config/zsh
  git/         # → ~/.config/git
```

All four destinations share a parent, so one target covers them:

```toml
version = 1

[sources.dotfiles]
repo = "me/dotfiles"     # bare owner/repo defaults to github
branch = "main"
include = ["nvim", "helix", "zsh", "git"]

[targets.config]
path = "~/.config"
sources = ["dotfiles"]
```

Selection belongs to the source. A source's `root`, `include`, and `exclude`
decide what it puts on *offer*; a target's binding may subset that offer but
never widen it. Here the offer is the four named directories, so the repo's
loose root files never leave the repository, and `phora list` reports one
artifact per directory:

```
config:
  dotfiles/git  ✓ clean
  dotfiles/helix  ✓ clean
  dotfiles/nvim  ✓ clean
  dotfiles/zsh  ✓ clean
```

When a destination does not sit under a shared parent, give it its own source
with a `root`. That re-anchors the offer at the subtree, so its contents land
directly at the target path rather than one directory deeper:

```toml
[sources.nvim]
repo = "me/dotfiles"
branch = "main"
root = "nvim"

[targets.nvim]
path = "~/.config/nvim"
sources = ["nvim"]
```

Several sources naming the same repository share a single mirror, so the second
one costs a lock entry rather than a second clone.

`phora sync` deploys everything and `phora.lock` pins the commit, so this
non-templated configuration produces byte-identical files on another machine.
Template outputs can differ when local variables differ, as described below.
When you change something upstream, `phora update` pulls it forward —
deliberately, not as a side effect of some other command.

Drift shows up instead of festering. `phora verify` re-hashes every deployed
file against what phora recorded, so the config you hand-tweaked at midnight
three weeks ago stops being a mystery: it reads as `modified`, and you decide.
Port the edit back to the repo and `phora update`; or run `phora sync --force`
to put back what was reviewed; or `phora eject` the artifact and own it
manually from then on. A plain `sync` will not choose for you — it skips a
locally modified artifact, names the files that diverged, and tells you
`--force` exists.

Machine differences live in an overlay rather than in branches.
`phora.local.toml` overlays the committed config per key; keep it out of version
control. A work machine that needs a different git config can re-point one
source, or narrow an `include`, without your dotfiles repo growing a `work`
branch that drifts from `main` forever.

Per-machine *values* — as opposed to per-machine files — live in `[vars]`. A
source file whose name ends in `.tmpl` is rendered with the effective variables
and deployed with the suffix stripped, so `git/config.tmpl` in the repository
becomes `git/config` in the target:

```toml
# phora.toml, committed
[vars]
git_email = "me@personal.example"
```

```toml
# phora.local.toml; keep uncommitted
version = 1

[vars]
git_email = "me@work.example"
```

The lock hashes the source bytes, so two machines rendering different values
still produce byte-identical lock files. The registry hashes the rendered
bytes, so each machine verifies clean against what it actually deployed.
Editing a variable re-renders on the next sync without advancing any commit.

Post-install steps run as hooks. A target's `on_change` fires once after a sync
that actually changed that target's content, and files land before it runs:

```toml
[targets.config.hooks]
on_change = "fc-cache -f"
```

A no-op sync runs nothing. A hook that exits non-zero fails the sync, leaves
the deployed files in place, and re-fires on the next sync rather than being
recorded as done. If you need a check that runs *before* anything is written,
use `pre_deploy` instead: every target's gate runs ahead of every deploy, so a
failure under the default `abort` leaves the whole run unapplied rather than
half-applied.

While you are actively editing a config, the copy model can feel slow — change,
sync, check. For that loop, point the source at your live checkout and deploy it
by link. Link mode is honored only in the local overlay:

```toml
# phora.local.toml
version = 1

[sources.dotfiles]
path = "/home/me/dev/dotfiles"
deploy = "link"
```

A source `path` is used verbatim as the remote, so write it out in full — `~`
expands in a target `path`, not in a source's. The artifact destinations become
symlinks into your working tree, so edits are visible immediately with no
re-sync. `phora add --symlink ~/dev/dotfiles` writes that block for you, with
the shell expanding the path and phora naming the source after the directory —
which is what makes it override the source of the same name. Delete the overlay
and the next sync puts a verifiable copy back.

### Where phora stops

Templating is deliberately small. A template sees exactly the strings you put
in `[vars]` and nothing else: there is no populated namespace of machine facts,
no hostname or OS to branch on unless you write it down yourself, and no secret
storage or encryption anywhere in phora. An undefined variable is a hard error
that fails the artifact, not an empty string.

Hooks are per target, not per file. A target's `on_change` fires once for the
whole target when its content changed; you cannot attach a script to an
individual file, and nothing runs for a file that was already up to date.

Copies, not symlinks, by default. Editing `~/.config/nvim/init.lua` does not
edit your dotfiles repo — it creates drift, which `verify` will dutifully
report. Link mode is the exception, and it is a narrow one: it is confined to
`phora.local.toml` and to local sources, and a linked artifact sits outside the
integrity model, so verify, drift detection, and `rebuild-registry` all skip it
and `phora list` labels it `linked`.

Symlinks committed inside the source are refused unless that source sets
`allow_symlinks = true`. A dotfiles repo that keeps, say, `.zprofile` as a link
to `.zshrc` will fail its first sync with the path named until you opt in.

phora fits dotfiles best when repo-shaped, machine-independent files dominate,
or when dotfiles share one tool and lock with the other artifact types below.

## Shared configuration across repositories

A dozen repositories carry diverging copies of lint and editor settings. phora
can pin and verify copied files, but it cannot merge a shared base with
repository-specific overrides.

If you already use [vendir](https://github.com/carvel-dev/vendir), compare it
before adopting this recipe: both tools declare, synchronize, and lock directory
contents; phora uses Git as its store and records per-file hashes for later
integrity checks.

Put the canonical copies in one repository:

```
configs/
  lint/        # ruff.toml, eslint.config.mjs, …
  ci/          # reusable workflow fragments
  editor/      # .editorconfig and friends
```

Each consuming repo declares what it takes:

```toml
version = 1

[sources.configs]
repo = "org/configs"
tag = "v7"
include = ["lint", "editor"]

[targets.configs]
path = "etc"
sources = ["configs"]
```

This consumer takes only `lint` and `editor`; the `ci` bundle is not deployed.
The selected directories land as `etc/lint` and `etc/editor`, so this recipe
works only for tools configured to read those paths. Root-level files such as
`.editorconfig` and files required under `.github/workflows` need separate
targets and, where necessary, binding renames.

Updates are explicit and per repo. Each consumer has its own lock, so a new
`v8` of the lint rules rolls out one repository at a time, as a reviewable diff
(`phora update && git diff`), not as a surprise to whoever pushes next. A repo
that is not ready simply stays on `v7` — pinning *is* the mechanism, not a
workaround. State is keyed by the project directory, so two checkouts of the
same repository on one machine track their deployments independently.

`phora verify` in CI detects hand edits to files phora deployed and fails the
build. It does not prove that the linter, editor, or workflow actually reads
those files; test that separately.

One repo can hold two versions at once. Bindings are keyed by identity, and
each may pin its own ref, so migrating to stricter rules can run as a
side-by-side comparison inside a single repository before you commit to it:

```toml
[sources.configs]
repo = "org/configs"
tag = "v7"
include = ["lint"]

[targets.configs]
path = "etc"
layout = "by-source"

[targets.configs.sources]
current = { source = "configs" }              # inherits the source's v7
next    = { source = "configs", tag = "v8" }
```

The two identities become the directory labels under `by-source`, so
`etc/current/lint` and `etc/next/lint` cannot collide, and the difference is a
plain `diff` between two directories. One mirror serves both; the lock carries
one entry per distinct ref. When the canary holds up, move the source's tag
forward, drop back to a single bare binding, and `phora sync --prune` reclaims
the artifacts the config no longer names.

Upstream removals are not silent. If a directory you were taking disappears
from the new commit, `phora update` stops and says so, naming the recorded
artifact, the pin it moved from and to, and the path on disk. Re-run with
`--fast-forward` to follow the move and delete what upstream dropped, or eject
the artifact first if you meant to keep it.

Where phora stops: if one repo genuinely needs to deviate, the options are a
separate artifact for that variant, a rendered `.tmpl` if the difference is a
value rather than a structure, or ejecting the artifact and accepting that it
has left the shared set.

## Pinned agent skills across projects

You have accumulated Claude Code skills and want each project and machine to
receive an explicitly chosen, pinned set instead of another copy-paste fork.
phora treats skill files as opaque bytes: it does not validate them or prove
that an agent loads or follows them.

Keep the artifacts in one repository, one directory per skill:

```
skills/
  scope/
  implement/
  review/
  test/
```

Then, in each consuming project:

```toml
version = 1

[sources.skills]
repo = "me/skills"
tag = "v3"               # or branch = "main" to track
root = "skills"          # the repo's skills/ dir is the offer

[targets.skills]
path = ".claude/skills"
sources = ["skills"]
layout = "flat"          # .claude/skills/scope, .claude/skills/implement, …
```

Commit `phora.toml` and `phora.lock` to the project, and everyone who checks it
out runs `phora sync` and gets the same skills at the same commit — not
"whatever main was when they cloned." When you cut a new version of the skill
set, each project moves forward on its own schedule with `phora update`.

Not every project wants every skill. A binding's `take` subsets the offer for
one target without touching the source or any other consumer. The binding lives
in a table keyed by its identity:

```toml
[targets.skills]
path = ".claude/skills"

[targets.skills.sources]
skills = { take = ["scope/**", "review/**"] }
```

A `take` may narrow the offer but never widen it. Naming a leaf the source does
not offer is an error with a spelling suggestion, and a glob that matches
nothing is reported as a warning by `phora preview` — either way the mistake
surfaces rather than silently shipping less than you meant.

Several bundles can share one directory. Personal skills and team skills land
side by side under a `by-source` layout, each under its binding's identity:

```toml
[targets.skills]
path = ".claude/skills"
sources = ["team-skills", "my-skills"]
layout = "by-source"   # .claude/skills/team-skills/…, .claude/skills/my-skills/…
```

Writing a skill is a link-mode loop. While developing, overlay the source onto
your checkout in `phora.local.toml` with `deploy = "link"` — edits show up in
the consuming project immediately, with no commit-and-sync per keystroke. Drop
the overlay when you are done and the next sync restores a pinned, verifiable
copy.

`phora verify` in CI re-hashes every deployed file, names mismatches, and exits
non-zero if any deployed bytes differ from phora's registry. It does not prove
that an agent loaded, followed, or correctly interpreted those files.

The same shape covers anything agent-adjacent and directory-shaped: subagent
definitions into `.claude/agents`, shared `CLAUDE.md` fragments, prompt
libraries, MCP server configs. One source per bundle, one target per
destination.

Nothing validates frontmatter, checks that a `SKILL.md` exists, or warns you
that a subagent definition is malformed. phora can verify that the deployed
bytes still match the pinned artifact; review, format validation, and agent
behavior remain separate concerns.

## Release assets, without curl | tar

You install a tool from a release tarball with `curl | tar`, then lose track of
the version and cannot verify the bytes later. This recipe pins one known URL
and digest for one platform. If you need version discovery, platform selection,
dependency handling, or `PATH` management, use a package or version manager
instead.

```toml
version = 1

[sources.fzf-bin]
url = "https://github.com/junegunn/fzf/releases/download/v0.55.0/fzf-0.55.0-linux_amd64.tar.gz"
digest = "sha256:4df2393776942780ddab2cea713ddaac06cd5c3886cd23bc9119a6d3aa1e02bd"
include = ["fzf"]

[targets.bin]
path = "~/.local/bin"
sources = ["fzf-bin"]
layout = "flat"
```

A url source is fetched once and imported; it takes no `branch`, `tag`, `rev`,
or `root`, because there is no history to point at. Compared with `curl | tar`:

- The digest is checked against the raw bytes before extraction, so a corrupted
  or substituted download never touches your tree.
- Archive entries are validated path by path, so a malicious archive cannot
  write outside the target.
- A single top-level directory is stripped automatically, so the version-stamped
  wrapper that release tarballs commonly carry does not reshuffle your paths
  when the version moves. An archive whose payload sits at the root, as fzf's
  does, needs nothing special either way.
- The executable bit survives, so the deployed `fzf` is runnable.
- The content is recorded in the lock and the registry: `phora list` tells you
  what is deployed, `phora where` tells you where a binary came from, and
  `phora verify` tells you it has not been tampered with since.

Upgrading is editing the URL and the digest and running `phora update`. A plain
`sync` deliberately does not reach for the network — it honors the lock — so
`update` is the command that re-downloads and re-checks. Because identical
bytes always import to the identical synthetic commit, an update that finds
unchanged content is a true no-op and the lock does not churn.

Where phora stops: both the URL and the digest are yours to edit, and the digest
is the only thing standing between you and whatever the URL serves next.

## Vendoring a subtree from a larger repo

Other repositories manually copy protobuf definitions, JSON schemas, design
tokens, or a documentation theme from a larger repository — often a monorepo —
and nobody can readily identify each copy's version. phora can pin and deliver
those source files; it does not run generators or detect compatibility breaks.

The producing repo needs no changes at all. Each consumer declares its slice:

```toml
version = 1

[sources.platform]
repo = "org/platform"
tag = "v2.3.0"
root = "protos"

[targets.protos]
path = "vendor/protos"
sources = ["platform"]
```

Now the version question has a boring answer: it is in `phora.lock`, in the
consumer's own repo, under version control. Upgrades are per consumer and
reviewable, a service that needs more time stays pinned, and the
stable-versus-next pattern from the
[configuration section](#shared-configuration-across-repositories) works here
unchanged when a migration needs both versions in the tree at once.

Here, each offered *leaf* is an artifact, not necessarily a top-level
directory — a single loose file deploys as readily as a tree — but a directory
that is taken whole collapses into one artifact named after it. That is why
`root = "protos"` with a `protos/billing/` and a `protos/identity/` yields
exactly two artifacts, `platform/billing` and `platform/identity`, and why
widening the offer to a loose file at the root would add a third.

Two commands answer "would this ship?" before you sync. `phora preview` renders
the whole projection from the lock, one line per artifact, showing renames and
the exact destination path; add `--files` to expand a collapsed directory into
the files it folds in. `phora explain <target> <source> [path]` attributes a
single path: which `include` offered it, and how `take` resolved it. Use them
together to inspect both the complete projection and the rule responsible for
one path.

Where phora stops: it will not run `protoc`, regenerate stubs, or notice that
the schema you just pulled forward is incompatible with your code. Vendoring is
a delivery step; the build step after it is still yours.

## Smaller situations to recognize

These are fit checks, not complete recipes.

Your repositories carry duplicated Git hook scripts, and you want every checkout
to use the same pinned set. Deploy a `hooks/` artifact into a directory and
point `core.hooksPath` at it. phora delivers the files and pins the version, and
a target `on_change` hook can do the wiring once the files land:

```toml
[targets.githooks]
path = ".githooks"
sources = ["hooks"]

[targets.githooks.hooks]
on_change = "git config core.hooksPath .githooks"
```

During incidents, responders lose time switching from a service repository to a
separate wiki. Project an `ops/runbooks` source into each service repository so
the relevant docs are present at a pinned version.

You want pinned, read-only copies of upstream source trees nearby for browsing.
Project them into a target and update them explicitly; sources naming the same
remote share one bare mirror under the cache root. These are exported files, not
Git checkouts: they contain no `.git` directory, branches, or working-tree
workflow.

You maintain course material, examples, or starter kits that several local
directories should receive at a known, verifiable version.

These situations share one constraint: directory-shaped content must move from
its source to a local consumer at a pinned, later-verifiable version.

## Where to look next

- The [README](README.md) for every flag and config key in one place.
- The [guide](GUIDE.md) for the mental model and the internals.
- [`phora.example.toml`](phora.example.toml) and
  [`phora.local.example.toml`](phora.local.example.toml) for complete, annotated
  configs.
- The scrut suites under [`tests/scrut/`](tests/scrut) for CI-verified, runnable
  walkthroughs: [showcase](tests/scrut/showcase.md) end to end,
  [selection](tests/scrut/selection.md) for offer and take,
  [templates](tests/scrut/templates.md), [hooks](tests/scrut/hooks.md),
  [versions](tests/scrut/versions.md) for two versions side by side,
  [drift](tests/scrut/drift.md), [mapped](tests/scrut/mapped.md) for renaming,
  [release-assets](tests/scrut/release-assets.md), and
  [orphans](tests/scrut/orphans.md).
