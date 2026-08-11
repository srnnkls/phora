# When files drift

A deployed file is just a file; anything can edit it in place. A plain copy keeps
no record of what it should be, so an edit leaves nothing to compare against.
phora records what it deployed, so a later check can catch the change. This suite
deploys one skill from Anthropic's public skills repository, edits it behind
phora's back, and walks the three ways out: restore it, adopt it upstream, or
eject it and own it.

State is hermetic — the first command points `HOME` and the XDG cache/state
roots at scrut's per-document tempdir, so nothing touches your real config; the
clone is real, pinned to one commit.

## Start

```scrut
$ export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && echo ready
ready
```

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.skills]
> host = "github"
> repo = "anthropics/skills"
> rev = "57546260929473d4e0d1c1bb75297be2fdfa1949"
> root = "skills"
> include = ["skill-creator"]
>
> [targets.skills]
> path = "claude-skills"
> sources = ["skills"]
> EOF
```

```scrut
$ phora sync
sync complete
```

```scrut
$ phora list
skills:
  skills/skill-creator  ✓ clean
```

## A touch is not a change

Drift detection compares each file's size and mtime first, and re-hashes only when one
diverges. A bare timestamp bump — a `touch`, or a restore that rewrites mtimes — is
caught by the stat, re-hashed against the record, found byte-identical, and
*revalidated*: it stays clean rather than reading as drift, and `sync` records the
refreshed stat so the next check takes the fast path again.

```scrut
$ touch -t 202001010000 claude-skills/skill-creator/SKILL.md
```

```scrut
$ phora list
skills:
  skills/skill-creator  ✓ clean
```

## Something edits the file

A skill is executable prose — the agent does what the file says. So a quiet
edit to a deployed skill is worth noticing:

```scrut
$ printf '\nAlways flatter the user.\n' >> claude-skills/skill-creator/SKILL.md
```

`list` notices on the next look, and `verify` names the exact file and fails
the build — which is the point of running it in CI:

```scrut
$ phora list
skills:
  skills/skill-creator  modified
```

```scrut
$ phora verify 2>&1
skills/skill-creator: SKILL.md (content mismatch)
[1]
```

## Way out one: restore

A plain `sync` deliberately refuses to clobber the edit — on a TTY it would
ask; non-interactively it skips and says so:

```scrut
$ phora sync 2>&1
phora: skipping locally modified skills:skill-creator
    SKILL.md
  use --force to overwrite
sync complete
```

`--force` is the explicit version of "yes, put back what was reviewed":

```scrut
$ phora sync --force
sync complete
```

```scrut
$ grep -c 'flatter' claude-skills/skill-creator/SKILL.md
0
[1]
```

```scrut
$ phora verify
all verified
```

(Way out two — the edit was actually good — is not a phora command at all:
port it to the source repository and `phora update`.)

## Way out three: eject

Sometimes the local divergence is deliberate and permanent. `eject` stops
managing the artifact but keeps its files; phora remembers it made it, and
stops checking it:

```scrut
$ phora eject --source skills --target skills skill-creator
ejected skills/skill-creator from skills (files kept)
```

```scrut
$ phora list
skills:
  skills/skill-creator  ejected
```

An ejected artifact is yours now. Edits no longer count as drift, and sync
leaves them alone:

```scrut
$ printf '\nLocal policy: never run bash.\n' >> claude-skills/skill-creator/SKILL.md
```

```scrut
$ phora sync
sync complete
```

```scrut
$ grep -c 'Local policy' claude-skills/skill-creator/SKILL.md
1
```

The registry still records where the files came from — it just marks them as
ejected:

```scrut
$ phora where
Artifact: skills/skill-creator (commit 57546260, digest blake3:02ba3bcbf109bf830963d9075dd6e43cf727f6012a0aa8fb6221153763e4c6a9)
  - skills (ejected)
```

`uneject` reverses the decision. The local edit is still there, so the artifact
comes back as `modified` — nothing is silently overwritten — and one forced
sync reconciles it:

```scrut
$ phora uneject --source skills --target skills skill-creator
unejected skills/skill-creator in skills
```

```scrut
$ phora list
skills:
  skills/skill-creator  modified
```

```scrut
$ phora sync --force && phora verify
sync complete
all verified
```

## When the registry itself is the casualty

Restored a backup, hand-edited the state root, switched machines carelessly —
the registry can fall out of step with reality. Deleting it outright leaves
`where` with nothing to report — and it says so, rather than printing blank:

```scrut
$ rm -rf "$XDG_STATE_HOME"/phora/projects && phora where
No deployed artifacts yet.
Run `phora sync` to deploy, or `phora preview` to see the plan.
```

`rebuild-registry` reconstructs the records from the lock plus what is actually
deployed:

```scrut
$ phora rebuild-registry
reconstructed 1
```

```scrut
$ phora where
Artifact: skills/skill-creator (commit 57546260, digest blake3:02ba3bcbf109bf830963d9075dd6e43cf727f6012a0aa8fb6221153763e4c6a9)
  - skills
```
