# One skill set, every project

You have accumulated Claude Code skills, and every project that wants them has
its own slightly different copy. This walkthrough pulls two skills from
Anthropic's public [skills repository](https://github.com/anthropics/skills)
into a project's `.claude/skills`-shaped directory, pinned to an exact commit —
then layers a local working tree on top for the editing loop, and takes it off
again.

Every command below is the shipped binary and every block asserts its exact
output, so any divergence from how phora behaves fails the suite. State is
hermetic — the first command points `HOME` and the XDG cache/state roots at
scrut's per-document tempdir — but the clone is real: this suite talks to
github.com, and its assertions hold as long as the pinned upstream commit exists.

## Start

```scrut
$ export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && echo ready
ready
```

## A target and a source

A target is a directory that consumes artifacts. Ours stands in for a project's
`.claude/skills`.

```scrut
$ phora target add skills --path claude-skills
Added target 'skills': claude-skills
```

`phora add` takes the bare `owner/repo` form and records it symbolically —
intent, not a baked-in URL.

```scrut
$ phora add anthropics/skills --to skills --root skills --include mcp-builder --include skill-creator
Added source 'skills': github:anthropics/skills
  bound to skills
```

That tracks `main`, which is the right default for a tool and the wrong one for
a document that asserts commit hashes. The flags only edit `phora.toml`, and the
file is ours to edit too — so pin the rev, and while we are in there, move the
selection onto the source so it becomes the default for every consumer:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.skills]
> host = "github"
> repo = "anthropics/skills"
> rev = "57546260929473d4e0d1c1bb75297be2fdfa1949"
> root = "skills"
> include = ["mcp-builder", "skill-creator"]
>
> [targets.skills]
> path = "claude-skills"
> sources = ["skills"]
> EOF
```

## Sync

One command: mirror the repository into the cache, resolve the rev, write the
lock, project the two selected skills into the target.

```scrut
$ phora sync
sync complete
```

The upstream repo carries seventeen skills at this commit; the `include` keeps
exactly two.

```scrut
$ phora list
skills:
  skills/mcp-builder  ✓ clean
  skills/skill-creator  ✓ clean
```

```scrut
$ test -f claude-skills/mcp-builder/SKILL.md && test ! -e claude-skills/internal-comms && echo projected
projected
```

`phora where` reads the registry back: which commit, which content digest,
which targets. Both values are functions of the pinned commit, which is why
this document can assert them verbatim.

```scrut
$ phora where
Artifact: skills/mcp-builder (commit 57546260, digest blake3:d6b9907115af0caed507032d448ac4dc7a47d8ed29e8463bbcb14e730b43d264)
  - skills
Artifact: skills/skill-creator (commit 57546260, digest blake3:02ba3bcbf109bf830963d9075dd6e43cf727f6012a0aa8fb6221153763e4c6a9)
  - skills
```

The lock is small enough to read whole. One entry: the source, the resolved
commit, a digest over the projected content, and a digest over the
export-affecting config — the latter is how phora notices you changed *what*
ships even when upstream did not move.

```scrut
$ cat phora.lock
version = 1

[[sources]]
name = "skills"
git = "https://github.com/anthropics/skills.git"
resolved = "57546260929473d4e0d1c1bb75297be2fdfa1949"
commit = "57546260929473d4e0d1c1bb75297be2fdfa1949"
digest = "blake3:e1608b00c776c964b9bd32eafac59ebfe5093778e56e1c3cd1c8cfbb75563889"
config_digest = "blake3:6add44654fda0f665dc64860f646e3cc329bcf3c68e67fd0eeccfbc32dfd558c"
```

## Asking instead of guessing

When a path does or does not ship and the globs are not obvious by eye,
`check-match` answers for one path:

```scrut
$ phora check-match --source skills mcp-builder/SKILL.md
artifact `mcp-builder`: allowed
path `mcp-builder/SKILL.md`: allowed
include: ["mcp-builder", "skill-creator"]
exclude: []
```

```scrut
$ phora check-match --source skills internal-comms/SKILL.md
artifact `internal-comms`: excluded
path `internal-comms/SKILL.md`: excluded
include: ["mcp-builder", "skill-creator"]
exclude: []
```

And `preview` shows the whole projection — what a sync would do, from the lock,
without writing anything:

```scrut
$ phora preview
skills
  skills@57546260 mcp-builder/ -> claude-skills/mcp-builder
  skills@57546260 skill-creator/ -> claude-skills/skill-creator
```

Each skill's whole tree is taken, so it *collapses* to a single directory
artifact — preview marks a collapsed directory with a trailing slash
(`mcp-builder/`).

`verify` re-hashes every deployed file against the registry — the difference
between "the files are there" and "the files are exactly what phora put there,"
which matters when the files are prompts an agent will follow.

```scrut
$ phora verify
all verified
```

## The editing loop

Editing a skill through copy-and-sync means a commit per keystroke. For the
loop, point the source at a local checkout and deploy by symlink instead.
`phora.local.toml` overlays the committed config per-key and is never
committed; a `deploy = "link"` source is only honored there, so the loop cannot
leak into shared config. Our stand-in for the checkout is a directory with the
repo's shape:

```scrut
$ mkdir -p dev-skills/skills/mcp-builder dev-skills/skills/skill-creator && printf 'name: mcp-builder (work in progress)\n' > dev-skills/skills/mcp-builder/SKILL.md && printf 'name: skill-creator (work in progress)\n' > dev-skills/skills/skill-creator/SKILL.md
```

```scrut
$ cat > phora.local.toml <<'EOF'
> version = 1
>
> [sources.skills]
> path = "./dev-skills"
> deploy = "link"
> EOF
```

```scrut
$ phora sync
sync complete
```

The target entries are now symlinks into the working tree — edits show up
immediately, no re-sync:

```scrut
$ phora list
skills:
  skills/mcp-builder  linked
  skills/skill-creator  linked
```

```scrut
$ readlink claude-skills/mcp-builder | sed "s#/private$PWD#<ROOT>#g;s#$PWD#<ROOT>#g"
<ROOT>/dev-skills/skills/mcp-builder
```

A linked artifact sits outside the integrity model — its bytes change
underfoot, so phora records `link` as the digest rather than hashing:

```scrut
$ phora where --artifact mcp-builder
Artifact: skills/mcp-builder (commit link, digest link:)
  - skills
```

Link mode trades the content guarantee for live edits. Done editing, remove the
overlay, and the next sync puts pinned, verifiable copies back:

```scrut
$ rm phora.local.toml && phora sync
sync complete
```

```scrut
$ test ! -L claude-skills/mcp-builder && phora verify
all verified
```

The half-written `work in progress` edit stayed in the working tree where it
belongs; the target is back on the locked commit.

```scrut
$ phora list
skills:
  skills/mcp-builder  ✓ clean
  skills/skill-creator  ✓ clean
```
