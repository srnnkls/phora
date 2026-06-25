# One source, two versions

fzf's shell integration — completions and key bindings — changes occasionally
across releases, and reviewing the diff before deploying beats finding out after.
This suite holds fzf v0.55.0 and v0.56.0 side by side in one target, from one
mirror, then promotes the newer one and lets `--prune` clean up.

State is hermetic — the first command points `HOME` and the XDG cache/state
roots at scrut's per-document tempdir; the clone of github.com/junegunn/fzf is
real. Both refs are release tags, so every hash below is stable.

## Start

```scrut
$ export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && echo ready
ready
```

The source pins v0.55.0 and selects the `shell` directory. The target binds it
twice: `stable` inherits the source's tag, `canary` overrides it. Two bindings
of one source need distinct identities — that is what the `[targets.<t>.sources]`
table keys provide — and the `by-source` layout uses those identities as
directory labels, so the two versions cannot collide:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.fzf]
> git = "https://github.com/junegunn/fzf.git"
> tag = "v0.55.0"
> include = ["shell"]
>
> [targets.shell]
> path = "shell-integration"
> layout = "by-source"
>
> [targets.shell.sources]
> stable = { source = "fzf" }
> canary = { source = "fzf", tag = "v0.56.0" }
> EOF
```

## Sync

One fetch, one mirror, two commits resolved out of it, two projections:

```scrut
$ phora sync
sync complete
```

```scrut
$ phora list
shell:
  canary/shell  ✓ clean
  stable/shell  ✓ clean
```

```scrut
$ phora preview
shell
  canary@ff168774 shell/ -> shell-integration/canary/shell
  stable@fc693080 shell/ -> shell-integration/stable/shell
```

Two different commits, two different content digests — same source, same
mirror:

```scrut
$ phora where
Artifact: canary/shell (commit ff168774, digest blake3:2de40a4c3e2bd2e47e08233f7b66e2562007bb2a4d023225b24631a1ab37b698)
  - shell
Artifact: stable/shell (commit fc693080, digest blake3:39cdc3c10ad1b93ac21c9459a1c8296be2fe8b230427faf3907c99dbb9bfc4ab)
  - shell
```

With both versions deployed, the difference is a plain `diff` between two
directories on disk. Between these two tags, the bash completion changed:

```scrut
$ diff -q shell-integration/stable/shell/completion.bash shell-integration/canary/shell/completion.bash
Files shell-integration/stable/shell/completion.bash and shell-integration/canary/shell/completion.bash differ
[1]
```

The lock shows how the splitting works: one entry per distinct ref, and the
discriminator appears only on the override — a config with no binding refs
locks exactly as it would have before per-target versions existed:

```scrut
$ grep -c '\[\[sources\]\]' phora.lock
2
```

```scrut
$ grep -e 'resolved' -e '^ref' phora.lock
resolved = "v0.55.0"
resolved = "v0.56.0"
ref = "tag:v0.56.0"
```

## Promote

The canary held up. Move the source's tag forward and drop back to a single
bare binding:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.fzf]
> git = "https://github.com/junegunn/fzf.git"
> tag = "v0.56.0"
> include = ["shell"]
>
> [targets.shell]
> path = "shell-integration"
> sources = ["fzf"]
> layout = "by-source"
> EOF
```

The mirror already holds the tag, so no network is needed; `--prune` also
removes the two artifacts the config no longer names:

```scrut
$ phora sync --prune 2>&1
phora: pruning orphaned canary:shell
phora: pruning orphaned stable:shell
sync complete
```

```scrut
$ phora list
shell:
  fzf/shell  ✓ clean
```

```scrut
$ phora where
Artifact: fzf/shell (commit ff168774, digest blake3:2de40a4c3e2bd2e47e08233f7b66e2562007bb2a4d023225b24631a1ab37b698)
  - shell
```

The lock collapses back to one entry, no discriminator:

```scrut
$ grep -c '\[\[sources\]\]' phora.lock
1
```

Note: prune removes the files it tracked, but the now-empty `stable/` and
`canary/` identity directories stay behind — `rmdir` them if needed:

```scrut
$ find shell-integration -mindepth 1 -maxdepth 1 | LC_ALL=C sort
shell-integration/canary
shell-integration/fzf
shell-integration/stable
```
