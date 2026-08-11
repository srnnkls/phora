# phora, by use case

The [README](README.md) is the reference and the [guide](GUIDE.md) is the
walkthrough. This file is the third angle: situations. Each section starts from
a problem you might recognize and shows a config that addresses it, plus —
where it matters — an honest note about what phora will *not* do for you, so
you can decide whether it fits before you commit an afternoon to it.

The sections are independent. Skim for yours.

## Contents

- [Dotfiles](#dotfiles)
- [Agent artifacts: skills, subagents, prompts](#agent-artifacts-skills-subagents-prompts)
- [Shared configuration across repositories](#shared-configuration-across-repositories)
- [Release assets, without curl | tar](#release-assets-without-curl--tar)
- [Vendoring a subtree from a larger repo](#vendoring-a-subtree-from-a-larger-repo)
- [Other shapes it fits](#other-shapes-it-fits)
- [Where to look next](#where-to-look-next)

## Dotfiles

The setup: one repository holds your configuration, organized as one directory
per tool, and you want the right directories to land in the right places under
`~/.config` — on every machine, identically, with a way to notice when
something drifted.

```
dotfiles/
  nvim/        # → ~/.config/nvim
  helix/       # → ~/.config/helix
  zsh/         # → ~/.config/zsh
  git/         # → ~/.config/git
```

Each top-level directory is an artifact; each destination is a target with a
binding that re-roots the source at the slice it wants:

```toml
version = 1

[sources.dotfiles]
repo = "me/dotfiles"     # bare owner/repo defaults to github
branch = "main"

[targets.nvim]
path = "~/.config/nvim"
sources = [{ source = "dotfiles", root = "nvim" }]

[targets.helix]
path = "~/.config/helix"
sources = [{ source = "dotfiles", root = "helix" }]

[targets.zsh]
path = "~/.config/zsh"
sources = [{ source = "dotfiles", root = "zsh" }]
```

`phora sync` deploys everything; `phora.lock` pins the commit, so a second
machine syncing the same config gets byte-identical files. When you change
something upstream, `phora update` pulls it forward — deliberately, not as a
side effect of some other command.

Two phora habits turn out to suit dotfiles well:

- **Drift shows up instead of festering.** `phora verify` re-hashes every
  deployed file against what phora recorded. The config you hand-tweaked at
  midnight three weeks ago stops being a mystery — it shows up as modified, and
  you decide: port the edit back to the repo, or `phora eject` the artifact and
  own it manually from now on.
- **Machine differences live in an overlay, not in branches.** `phora.local.toml`
  overlays the committed config per-key and is never committed. A work machine
  that needs a different git config can re-point one source, or narrow an
  `include`, without your dotfiles repo growing a `work` branch that drifts
  from `main` forever.

While actively editing a config, the copy model can feel slow — change, sync,
check. For that loop, link the source to a live checkout:

```bash
phora add --symlink ~/dev/dotfiles
```

That writes a `deploy = "link"` overlay into `phora.local.toml`: the target
becomes a symlink into your working tree, edits are visible immediately, and
the next sync after you remove the overlay puts a verifiable copy back.

### Where phora stops

Worth knowing before you migrate:

- **No templating.** phora deploys files as they are in the repo. There is no
  per-machine variable substitution — no `{{ hostname }}`, no conditional
  blocks. If a file genuinely differs per machine, your options are a separate
  artifact per variant selected via `phora.local.toml`, or keeping that file
  outside phora entirely.
- **No hooks.** Nothing runs after a deploy. If your setup depends on a
  post-install step — `fc-cache`, `tic`, `bat cache --build` — you run it
  yourself.
- **Copies, not symlinks.** By default a deployed file is a copy, not a link
  back into the repo. Editing `~/.config/nvim/init.lua` does not edit your
  dotfiles repo; it creates drift, which `verify` will dutifully report. The
  workflow is: edit the repo, sync — or use the link-mode loop above while
  iterating.

If your dotfiles lean heavily on templating or hooks, a dedicated dotfiles
manager — [dotter](https://github.com/SuperCuber/dotter),
[chezmoi](https://www.chezmoi.io) — is the better home today. phora earns its
keep when the repo-shaped, machine-independent parts dominate, or when
dotfiles are just one of several things you are distributing (see the next
two sections — the appeal is one tool and one lock for all of it).

## Agent artifacts: skills, subagents, prompts

The setup: you have accumulated Claude Code skills, subagent definitions,
slash commands, prompt templates — and you want the same set, at the same
version, in every project and on every machine. Copy-paste got you here;
copy-paste is also why three projects now have three slightly different copies
of the same skill.

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

[targets.skills]
path = ".claude/skills"
sources = ["skills"]
layout = "flat"          # .claude/skills/scope, .claude/skills/implement, …
```

Commit `phora.toml` and `phora.lock` to the project, and everyone who checks
it out runs `phora sync` and gets the same skills at the same commit — not
"whatever main was when they cloned." When you cut a new version of the skill
set, each project moves forward on its own schedule with `phora update`.

A few refinements that come up in practice:

- **Not every project wants every skill.** A binding's `include` narrows the
  selection for one project without touching the source or any other consumer:

  ```toml
  sources = [{ source = "skills", include = ["scope", "review"] }]
  ```

- **Several bundles, one directory.** Personal skills and team skills can land
  side by side under a `by-source` layout, each labelled by its binding:

  ```toml
  [targets.skills]
  path = ".claude/skills"
  sources = ["team-skills", "my-skills"]
  layout = "by-source"   # .claude/skills/team-skills/…, .claude/skills/my-skills/…
  ```

- **Writing a skill is a link-mode loop.** While developing, overlay the
  source onto your checkout with `phora add --symlink ~/dev/skills` — edits
  show up in the consuming project immediately, no commit-and-sync per
  keystroke. Drop the overlay when you are done and the next sync restores a
  pinned, verifiable copy.

- **`verify` keeps agents honest.** A skill is executable prose: the agent
  does what the file says. `phora verify` in CI (it exits non-zero on the
  first mismatch) confirms the prompts your agents run are the prompts that
  were reviewed — which is a sentence that would have sounded paranoid a few
  years ago.

The same shape covers anything agent-adjacent and directory-shaped: subagent
definitions into `.claude/agents`, shared `CLAUDE.md` fragments, prompt
libraries, MCP server configs. One source per bundle, one target per
destination.

## Shared configuration across repositories

The setup: a dozen repositories, and each carries its own copy of the lint
config, the formatter settings, the `.editorconfig`, a few CI snippets. They
were identical once. They are not identical now, and nobody decided that —
it happened one innocent local tweak at a time.

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

[targets.configs]
path = "etc"
sources = [{ source = "configs", include = ["lint", "editor"] }]
```

The `lint` and `editor` artifacts land as `etc/lint` and `etc/editor`; the
repo takes exactly the directories it names and nothing else.

The properties that matter here are the boring ones:

- **Updates are explicit and per-repo.** Each consumer has its own lock, so a
  new `v8` of the lint rules rolls out one repo at a time, as a reviewable
  diff (`phora update && git diff`), not as a surprise to whoever pushes next.
  A repo that is not ready simply stays on `v7` — pinning *is* the mechanism,
  not a workaround.
- **CI can prove conformance.** `phora verify` in the pipeline fails the build
  if someone hand-edited a deployed config instead of changing it upstream.
  The error message is, in effect, "this decision belongs in `org/configs`" —
  delivered by a machine, which keeps it from being personal.
- **One repo can take two versions at once.** A binding may pin its own ref,
  so migrating to stricter rules can run as a side-by-side comparison inside a
  single repo before you commit to it:

  ```toml
  [targets.configs]
  path = "etc"
  sources = [
    { source = "configs", as = "current", tag = "v7", include = ["lint"] },
    { source = "configs", as = "next",    tag = "v8", include = ["lint"] },
  ]
  layout = "by-source"   # etc/current/lint/…, etc/next/lint/…
  ```

If you have used [vendir](https://github.com/carvel-dev/vendir), this is the
same instinct — declare what a directory should contain, sync it, lock it —
with git as the store, and per-file hashes recorded so conformance is
checkable after the fact, not just at sync time.

## Release assets, without curl | tar

The setup: a tool you want is published as a release tarball, and the usual
move is `curl | tar` into `~/.local/bin` plus a mental note about which
version that was. The mental note does not survive the week.

```toml
[sources.fzf-bin]
url = "https://github.com/junegunn/fzf/releases/download/0.55.0/fzf-0.55.0-linux_amd64.tar.gz"
digest = "sha256:…"      # verified before anything is extracted
include = ["fzf"]

[targets.bin]
path = "~/.local/bin"
sources = ["fzf-bin"]
```

What you get over the curl pipeline, point by point:

- The digest is checked **before** extraction — a corrupted or substituted
  download never touches your tree.
- Archive entries are validated path-by-path, so a malicious archive cannot
  write outside the target.
- The version-stamped wrapper directory (`fzf-0.55.0/`) is stripped
  automatically, so a version bump does not reshuffle paths.
- The content is recorded in the lock and the registry: `phora list` tells you
  what is deployed, `phora where` tells you where a binary came from, and
  `phora verify` tells you it has not been tampered with since.

Upgrading is editing the URL (and digest) and running `phora update`. And
because identical bytes always import to the identical commit, a re-download
of unchanged content is a true no-op — the lock does not churn.

## Vendoring a subtree from a larger repo

The setup: a repository — often a monorepo — publishes something
directory-shaped that other repositories consume by copy: protobuf
definitions, JSON schemas, design tokens, a documentation theme. Today the
copies are made by hand, and "which version of the protos is service X on?"
is answered by archaeology.

The producing repo needs no changes at all. Each consumer declares its slice:

```toml
[sources.platform]
repo = "org/platform"
tag = "v2.3.0"
root = "protos"

[targets.protos]
path = "vendor/protos"
sources = ["platform"]
```

Now the version question has a boring answer: it is in `phora.lock`, in the
consumer's own repo, under version control. Upgrades are per-consumer and
reviewable; a service that needs more time stays pinned; and the
stable-vs-next side-by-side pattern from the
[configuration section](#shared-configuration-across-repositories) works here
unchanged when a migration needs both versions in the tree at once.

One mechanical note: artifacts are the *top-level directories* under the
source's `root`, so point `root` at the directory whose children you want to
deploy. `phora preview` shows the whole projection tree before you sync, and
`phora check-match` answers "would this path ship?" for a single file —
between them, no guessing.

## Other shapes it fits

Shorter mentions, same machinery:

- **Git hook scripts.** Deploy a `hooks/` artifact into a directory and point
  `core.hooksPath` at it. phora delivers the files and pins the version; it
  does not run or install anything — wiring `core.hooksPath` is on you.
- **Runbooks and internal docs.** An `ops/runbooks` source projected into each
  service repo means the docs are *in* the repo people are staring at during
  an incident, at a pinned version, instead of a wiki tab away.
- **Reference checkouts.** A target full of upstream repos you keep around to
  read — pinned, updated when you say so, and cheap to hold, since mirrors
  live in git's deduplicated object store.
- **Course material, examples, starter kits.** Anything where many directories
  should receive the same files at a known version and you would like to prove
  it later.

The pattern underneath is always the same one: directory-shaped content,
moved from where it lives to where it is consumed, pinned to a commit,
verifiable afterwards. If your problem fits that sentence, the specific nouns
probably do not matter.

## Where to look next

- The [README](README.md) for every flag in one place.
- The [guide](GUIDE.md) for the mental model and the internals.
- [`phora.example.toml`](phora.example.toml) for a complete, annotated config.
- The [scrut showcase](tests/scrut/showcase.md) for a CI-verified, runnable
  walkthrough.
