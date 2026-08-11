# One file, every name a tool wants

You keep one `AGENTS.md`. Then Claude wants it as `CLAUDE.md`, Codex wants
`codex.md`, and you are back to copy-paste and three files that quietly diverge.
A binding's `take` can *rename* as it subsets — `{ "<source-leaf>" = "<dest>" }`
projects one offered file to a new destination name — so one upstream file lands
under as many names as you need, with no copies in the source tree. This suite
drives a source repository that carries an `AGENTS.md` at its root — the shape a
spec or toolkit repo publishes — pinned to tag `v0.9.5`.

State is hermetic — `isolate_state` points `HOME` and the XDG cache/state roots
at scrut's per-document tempdir, and the clone is a real git clone whose remote
URL is redirected onto a local fixture repo through git's `insteadOf`, so the
run never leaves the machine. Both the commit and the content digests are
functions of the pinned tag, so they are asserted verbatim.

## Start

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && SPECKIT="$(make_agents_source speckit v0.9.5)" && map_insteadof https://github.com/mock/spec-kit.git "$SPECKIT" && echo ready
ready
```

A rename entry `{ "<leaf>" = "<dest>" }` names one offered file and the name it
should take. A target's `[targets.<t>.sources]` table keys each binding by its
identity, so bind the source twice under distinct keys to fan the one file out:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.speckit]
> git = "https://github.com/mock/spec-kit.git"
> tag = "v0.9.5"
>
> [targets.agents]
> path = "agent-config"
> layout = "flat"
>
> [targets.agents.sources]
> speckit = { take = [{ "AGENTS.md" = "AGENTS.md" }] }
> claude  = { source = "speckit", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
> EOF
```

## Fan it out

```scrut
$ phora sync
sync complete
```

Both names land at the target root, keyed in the registry by `<identity>/<dest>`:

```scrut
$ phora list
agents:
  claude/CLAUDE.md  ✓ clean
  speckit/AGENTS.md  ✓ clean
```

```scrut
$ diff -q agent-config/AGENTS.md agent-config/CLAUDE.md && echo same-file
same-file
```

`where` records each dest's commit and content digest. The two digests differ
even though the bytes are identical — the digest frames the destination path in,
so a rename gets its own artifact identity rather than sharing its source's:

```scrut
$ phora where
Artifact: claude/CLAUDE.md (commit 5aba8ba6, digest blake3:18edef6025f6e39a436927839fb730686e71611cf058344220f168e9b8933ef9)
  - agents
Artifact: speckit/AGENTS.md (commit 5aba8ba6, digest blake3:cc6e38348c0ce69167b988ed3678d3587e3f1a344c21d76ab0d852bb055dcf01)
  - agents
```

`preview` shows the renames straight from the lock — the target header names the
path it deploys to, and a rename entry reads `src -> dest -> destination`, so you
can see the offered leaf, the name it takes, and where it lands:

```scrut
$ phora preview
agents -> agent-config
  claude@5aba8ba6 AGENTS.md -> CLAUDE.md -> agent-config/CLAUDE.md
  speckit@5aba8ba6 AGENTS.md -> agent-config/AGENTS.md
```

## Two names that fight

Rename two bindings to the *same* dest and phora refuses rather than letting one
silently clobber the other — the structured selection diagnostic names the
contested destination relative to the target root, and points at `phora preview`
to see the whole tree:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.speckit]
> git = "https://github.com/mock/spec-kit.git"
> tag = "v0.9.5"
>
> [targets.agents]
> path = "agent-config"
> layout = "flat"
>
> [targets.agents.sources]
> speckit = { take = [{ "AGENTS.md" = "AGENTS.md" }] }
> claude  = { source = "speckit", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
> codex   = { source = "speckit", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
> EOF
```

```scrut
$ phora sync 2>&1
error: sync error: selection: CLAUDE.md / CLAUDE.md — two bindings resolve to the same destination
matched against: the target's destinations across all bindings
remedy: rename one source's leaf, or separate the bindings under the layout
to debug: phora preview --target agents
[1]
```

Give Codex its own name and all three coexist:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.speckit]
> git = "https://github.com/mock/spec-kit.git"
> tag = "v0.9.5"
>
> [targets.agents]
> path = "agent-config"
> layout = "flat"
>
> [targets.agents.sources]
> speckit = { take = [{ "AGENTS.md" = "AGENTS.md" }] }
> claude  = { source = "speckit", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
> codex   = { source = "speckit", take = [{ "AGENTS.md" = "codex.md" }] }
> EOF
```

```scrut
$ phora sync && ls agent-config
sync complete
AGENTS.md
CLAUDE.md
codex.md
```

## Dropping a name

Stop using Codex; drop its binding. A plain sync would leave `codex.md` orphaned
on disk — `--prune` reclaims what the config no longer names, by identity:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.speckit]
> git = "https://github.com/mock/spec-kit.git"
> tag = "v0.9.5"
>
> [targets.agents]
> path = "agent-config"
> layout = "flat"
>
> [targets.agents.sources]
> speckit = { take = [{ "AGENTS.md" = "AGENTS.md" }] }
> claude  = { source = "speckit", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
> EOF
```

```scrut
$ phora sync --prune 2>&1 && ls agent-config
phora: pruning orphaned codex:codex.md
sync complete
AGENTS.md
CLAUDE.md
```

## Drift, on a renamed file too

A renamed dest is tracked like any other artifact, so a hand-edit shows up:

```scrut
$ printf 'my own notes\n' > agent-config/CLAUDE.md && phora list
agents:
  claude/CLAUDE.md  modified
  speckit/AGENTS.md  ✓ clean
```

```scrut
$ phora verify 2>&1
claude/CLAUDE.md: CLAUDE.md (content mismatch)
[1]
```

```scrut
$ phora sync --force && phora verify
sync complete
all verified
```

## Editing the source itself: link mode

While you are actually writing that `AGENTS.md`, you want edits to show through
without a commit-and-sync each time. Point the source at a local checkout and
deploy the renamed leaf by symlink — honored only in `phora.local.toml`:

```scrut
$ mkdir -p coda && cd coda && export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state checkout && printf '# Agent instructions (draft)\n' > checkout/AGENTS.md && echo seeded
seeded
```

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.speckit]
> git = "https://github.com/mock/spec-kit.git"
> tag = "v0.9.5"
>
> [targets.agents]
> path = "agent-config"
> layout = "flat"
>
> [targets.agents.sources]
> claude = { source = "speckit", take = [{ "AGENTS.md" = "CLAUDE.md" }] }
> EOF
```

```scrut
$ cat > phora.local.toml <<EOF
> version = 1
>
> [sources.speckit]
> path = "$PWD/checkout"
> deploy = "link"
> EOF
```

```scrut
$ phora sync
sync complete
```

The dest is a real symlink to the leaf in your working tree — so the remote
clone never even happens, and a linked artifact carries no commit or digest:

```scrut
$ test -L agent-config/CLAUDE.md && readlink agent-config/CLAUDE.md | sed "s#/private$PWD#<ROOT>#g;s#$PWD#<ROOT>#g"
<ROOT>/checkout/AGENTS.md
```

```scrut
$ phora where
Artifact: claude/CLAUDE.md (commit link, digest link:)
  - agents
```

An edit to the source shows through the renamed name immediately, no re-sync:

```scrut
$ printf '# Agent instructions (revised)\n' > checkout/AGENTS.md && cat agent-config/CLAUDE.md
# Agent instructions (revised)
```
