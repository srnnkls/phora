# Running something after a sync

Deploying files is rarely the final step — you cache-rebuild, reload a daemon,
or re-index whatever just landed. A target's `on_change` hook runs after a sync
that *changed* that target, and a global `post_sync` hook runs after *every*
sync. This suite deploys two skills from Anthropic's public
[skills repository](https://github.com/anthropics/skills), pinned to a commit,
and watches the hooks fire.

State is hermetic — the first command points `HOME` and the XDG cache/state
roots at scrut's per-document tempdir; the clones are real. Hook reports print the
command as a literal format string — `$HOME` and `$PHORA_CHANGED_NAMES` appear
verbatim, never expanded — so the output is byte-stable without normalization.

## Start

```scrut
$ export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && ROOT="$PWD" && echo ready
ready
```

The target carries an `on_change` hook that records which artifacts changed.
phora hands a hook `$PHORA_CHANGED_NAMES` — the newline-separated names of the
artifacts this sync touched:

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
>
> [targets.skills.hooks]
> on_change = "echo \"$PHORA_CHANGED_NAMES\" >> \"$HOME/deployed.log\""
> EOF
```

## It fires once, after the files land

The first sync deploys the two skills, then runs the hook once. The report names
the hook's target, its command, its scope, and its status:

```scrut
$ phora sync 2>&1
hook skills#echo "$PHORA_CHANGED_NAMES" >> "$HOME/deployed.log"#sh -c [on_change] `echo "$PHORA_CHANGED_NAMES" >> "$HOME/deployed.log"` ok
sync complete
```

The hook saw both artifacts — and saw them by name, which it could only do after
they were deployed:

```scrut
$ cat "$HOME/deployed.log"
mcp-builder
skill-creator
```

## A no-op sync stays quiet

Nothing changed upstream, so the second sync deploys nothing and the `on_change`
hook does not fire:

```scrut
$ phora sync 2>&1
sync complete
```

The log is unchanged:

```scrut
$ cat "$HOME/deployed.log"
mcp-builder
skill-creator
```

## post_sync runs regardless

Add a global `post_sync` hook. It runs after *every* sync, for work that should
happen whether or not content moved:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [hooks]
> post_sync = "echo synced >> \"$HOME/runs.log\""
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
>
> [targets.skills.hooks]
> on_change = "echo \"$PHORA_CHANGED_NAMES\" >> \"$HOME/deployed.log\""
> EOF
```

This sync still changes no content, so `on_change` stays silent while `post_sync`
runs:

```scrut
$ phora sync 2>&1
hook post_sync#echo synced >> "$HOME/runs.log"#sh -c [post_sync] `echo synced >> "$HOME/runs.log"` ok
sync complete
```

The `on_change` log did not grow; the `post_sync` log recorded the run:

```scrut
$ cat "$HOME/deployed.log"
mcp-builder
skill-creator
```

```scrut
$ cat "$HOME/runs.log"
synced
```

## A failing hook fails the sync — and retries

A hook that exits non-zero makes the sync exit non-zero too, but the files are
already on disk and the failure is *not* recorded — so the next sync retries it.
Here the hook only succeeds once a sentinel file exists:

```scrut
$ cd "$ROOT" && mkdir -p retry && cd retry && export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && echo isolated
isolated
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
>
> [targets.skills.hooks]
> on_change = "test -f \"$HOME/ready\" && echo built >> \"$HOME/build.log\""
> EOF
```

The first sync deploys, the hook fails, and the sync exits non-zero:

```scrut
$ phora sync 2>&1
hook skills#test -f "$HOME/ready" && echo built >> "$HOME/build.log"#sh -c [on_change] `test -f "$HOME/ready" && echo built >> "$HOME/build.log"` failed
phora: one or more hooks failed
[1]
```

The files landed anyway — a failed hook never rolls back a deploy:

```scrut
$ test -f claude-skills/skill-creator/SKILL.md && echo deployed
deployed
```

Fix the cause and sync again. Even though no upstream content changed, the hook
re-fires because its earlier failure was never recorded as success:

```scrut
$ touch "$HOME/ready" && phora sync 2>&1
hook skills#test -f "$HOME/ready" && echo built >> "$HOME/build.log"#sh -c [on_change] `test -f "$HOME/ready" && echo built >> "$HOME/build.log"` ok
sync complete
```

```scrut
$ cat "$HOME/build.log"
built
```

Now that it has succeeded, a further no-op sync leaves it alone:

```scrut
$ phora sync 2>&1
sync complete
```

```scrut
$ cat "$HOME/build.log"
built
```

## --no-hooks deploys without running anything

When you want the files but not the side effects, `--no-hooks` suppresses every
hook for that run:

```scrut
$ cd "$ROOT" && mkdir -p quiet && cd quiet && export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && echo isolated
isolated
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
>
> [targets.skills.hooks]
> on_change = "echo ran >> \"$HOME/hook.log\""
> EOF
```

```scrut
$ phora sync --no-hooks 2>&1
sync complete
```

The skill deployed, but the hook never ran:

```scrut
$ test -f claude-skills/skill-creator/SKILL.md && test ! -e "$HOME/hook.log" && echo "deployed, no hook"
deployed, no hook
```

Hooks come only from *your* config, never from a synced source tree — a
downloaded repo that happens to carry its own `phora.toml` is inert content, read
as files and never executed.
