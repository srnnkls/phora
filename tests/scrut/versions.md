# One source, two versions

A tool's shell integration — completions and key bindings — changes occasionally
across releases, and reviewing the diff before deploying beats finding out after.
This suite holds a source's v0.55.0 and v0.56.0 side by side in one target, from
one mirror, then promotes the newer one and lets `--prune` clean up.

State is hermetic — `isolate_state` points `HOME` and the XDG cache/state roots
at scrut's per-document tempdir, and the clone is a real git clone whose remote
URL is redirected onto a local fixture repo through git's `insteadOf`, so the
run never leaves the machine. The fixture carries both release tags, so every
hash below is stable.

## Start

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && SHELLKIT="$(make_two_tag_source shellkit v0.55.0 v0.56.0)" && map_insteadof https://github.com/mock/shellkit.git "$SHELLKIT" && echo ready
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
> [sources.shellkit]
> git = "https://github.com/mock/shellkit.git"
> tag = "v0.55.0"
> include = ["shell"]
>
> [targets.shell]
> path = "shell-integration"
> layout = "by-source"
>
> [targets.shell.sources]
> stable = { source = "shellkit" }
> canary = { source = "shellkit", tag = "v0.56.0" }
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
shell -> shell-integration
  canary@7f2085f6 shell/ -> shell-integration/canary/shell
  stable@02c9b936 shell/ -> shell-integration/stable/shell
```

Two different commits, two different content digests — same source, same
mirror:

```scrut
$ phora where
Artifact: canary/shell (commit 7f2085f6, digest blake3:345bb599d66321076f56b55943d1aab0f93a298559536e24f8897be38ffda5af)
  - shell
Artifact: stable/shell (commit 02c9b936, digest blake3:def4f177464644f9387fd4373997f6aa64d3b2bff25400268a00172b010d0599)
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
> [sources.shellkit]
> git = "https://github.com/mock/shellkit.git"
> tag = "v0.56.0"
> include = ["shell"]
>
> [targets.shell]
> path = "shell-integration"
> sources = ["shellkit"]
> layout = "by-source"
> EOF
```

The mirror already holds the tag, so no network is needed; sync reports the move
as a version transition, and `--prune` also removes the two artifacts the config
no longer names:

```scrut
$ phora sync --prune 2>&1
phora: shellkit → shell: v0.55.0 (02c9b936) → v0.56.0 (7f2085f6)
phora: pruning orphaned canary:shell
phora: pruning orphaned stable:shell
sync complete
```

```scrut
$ phora list
shell:
  shellkit/shell  ✓ clean
```

```scrut
$ phora where
Artifact: shellkit/shell (commit 7f2085f6, digest blake3:345bb599d66321076f56b55943d1aab0f93a298559536e24f8897be38ffda5af)
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
shell-integration/shellkit
shell-integration/stable
```
