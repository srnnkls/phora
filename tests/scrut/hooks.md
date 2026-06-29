# Phora Hooks

End-to-end behaviour of sync hooks: a target `on_change` hook fires once after a
sync that adds or changes deployed content, files land before the hook runs, a
failed hook fails the sync but leaves files in place and re-fires next sync,
`--no-hooks` suppresses execution, a global `post_sync` hook runs every sync, and
hook-shaped config inside a *synced source tree* is inert (INV-1).

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir, so nothing touches the developer's real
config or state. Each scenario `cd`s into its own subdirectory and re-isolates,
giving it a private `HOME`/state. Hooks leave observable artifacts (a log file
under `$HOME`) so their effect — and ordering against deploy — is asserted
directly. Output is piped through `normalize`, which collapses the tempdir prefix
to `<ROOT>`; the report prints the hook command's literal `$HOME` text (it is a
format string, not a shell), so it stays byte-stable.

## Setup

Source the helpers.

```scrut
$ source "$TESTDIR"/_setup.sh && ROOT="$PWD" && echo ready
ready
```

## on_change fires once after a changing sync, files first

A target carries an `on_change` hook that appends the *deployed* `editor/init.lua`
to `$HOME/hook.log`. The first sync deploys the included subtrees, then runs the
hook once; the report names it with its `on_change` scope and `ok` status.

```scrut
$ mkdir -p s1 && cd s1 && isolate_state && seed_config_with_hooks "$(make_git_source proj)" >/dev/null && phora sync 2>&1 | normalize
hook home#cat "$HOME/target-home/editor/init.lua" >> "$HOME/hook.log"#sh -c [on_change] `cat "$HOME/target-home/editor/init.lua" >> "$HOME/hook.log"` ok
sync complete
```

The log holds the deployed file's contents — proof the file landed *before* the
hook read it.

```scrut
$ cat "$HOME/hook.log"
-- init
```

`phora list` reflects the clean, in-sync artifacts.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
  dotfiles/lint  ✓ clean
```

## A no-op sync runs no hook

Re-syncing with no upstream change deploys nothing new, so the hook does not fire
and the log is unchanged.

```scrut
$ phora sync 2>&1 | normalize
sync complete
```

```scrut
$ cat "$HOME/hook.log"
-- init
```

## A failed hook fails the sync, keeps files, and re-fires

A fresh target's `on_change` hook only succeeds once `$HOME/allow` exists. The
first sync deploys the files, the hook fails, and the sync exits non-zero with the
failure reported on stderr.

```scrut
$ cd "$ROOT" && mkdir -p s2 && cd s2 && isolate_state && seed_config_failing_hook "$(make_git_source proj)" && echo seeded
seeded
```

```scrut
$ phora sync 2>&1
hook home#test -f "$HOME/allow" && echo ran >> "$HOME/hook.log"#sh -c [on_change] `test -f "$HOME/allow" && echo ran >> "$HOME/hook.log"` failed
phora: one or more hooks failed
[1]
```

The files are deployed despite the failed hook.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && echo deployed
deployed
```

The failure was *not* recorded, so fixing the cause and re-syncing re-fires the
hook even though no upstream content changed.

```scrut
$ touch "$HOME/allow" && phora sync 2>&1 | normalize
hook home#test -f "$HOME/allow" && echo ran >> "$HOME/hook.log"#sh -c [on_change] `test -f "$HOME/allow" && echo ran >> "$HOME/hook.log"` ok
sync complete
```

```scrut
$ cat "$HOME/hook.log"
ran
```

Now that the success is recorded, a further no-op sync does not re-fire.

```scrut
$ phora sync 2>&1 | normalize
sync complete
```

```scrut
$ cat "$HOME/hook.log"
ran
```

## --no-hooks suppresses execution

A fresh deployment with `--no-hooks` deploys the files but runs no hook, so no log
is written.

```scrut
$ cd "$ROOT" && mkdir -p s3 && cd s3 && isolate_state && seed_config_with_hooks "$(make_git_source proj)" >/dev/null && phora sync --no-hooks 2>&1 | normalize
sync complete
```

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && echo deployed
deployed
```

```scrut
$ test -e "$HOME/hook.log" && echo fired || echo suppressed
suppressed
```

## INV-1 inertness and global post_sync

The source tree itself carries a hook-shaped `phora.toml` under `payload/`. The
consumer config includes that subtree and declares only a global `post_sync` hook
(no target hooks).

```scrut
$ cd "$ROOT" && mkdir -p s4 && cd s4 && isolate_state && seed_config_post_sync "$(make_evil_source)" && echo seeded
seeded
```

Syncing runs only the consumer's global `post_sync` hook.

```scrut
$ phora sync 2>&1 | normalize
hook post_sync#echo post >> "$HOME/post.log"#sh -c [post_sync] `echo post >> "$HOME/post.log"` ok
sync complete
```

The source tree's `phora.toml` lands as ordinary inert content.

```scrut
$ test -f "$PWD/target-home/payload/phora.toml" && echo present
present
```

INV-1: the synced tree's hook never executed.

```scrut
$ test -e "$HOME/PWNED" && echo PWNED || echo inert
inert
```

`post_sync` ran once.

```scrut
$ cat "$HOME/post.log"
post
```

A second sync changes no content, yet the global `post_sync` (default
`when = always`) runs again.

```scrut
$ phora sync 2>&1 | normalize
hook post_sync#echo post >> "$HOME/post.log"#sh -c [post_sync] `echo post >> "$HOME/post.log"` ok
sync complete
```

```scrut
$ cat "$HOME/post.log"
post
post
```

## A failing pre_sync hook aborts the whole sync before any file deploys

A global `pre_sync` hook that exits non-zero must gate the entire sync: the failure
is reported with its `[pre_sync]` scope, the sync exits non-zero, and NO target file
is deployed — pre_sync runs after fetch but before the deploy loop.

```scrut
$ cd "$ROOT" && mkdir -p p1 && cd p1 && isolate_state && seed_config_failing_pre_sync "$(make_git_source proj)" && echo seeded
seeded
```

The pre_sync failure is reported with the `[pre_sync]` scope and `failed` status.

```scrut
$ phora sync 2>&1 | normalize | grep -F '[pre_sync]'
hook pre_sync#exit 1#sh -c [pre_sync] `exit 1` failed
```

The sync exits non-zero.

```scrut
$ phora sync >/dev/null 2>&1; test $? -ne 0 && echo nonzero
nonzero
```

No file was deployed — the gate aborted before the deploy loop ran.

```scrut
$ test -e "$PWD/target-home/editor/init.lua" && echo deployed || echo absent
absent
```

## A passing pre_sync runs before deploy and sees PHORA_TARGETS

A global `pre_sync` hook records its `PHORA_TARGETS` env, then the sync proceeds to
deploy. The report names the hook with its `pre_sync` scope and `ok` status.

```scrut
$ cd "$ROOT" && mkdir -p p2 && cd p2 && isolate_state && seed_config_pre_sync "$(make_git_source proj)" && phora sync 2>&1 | normalize
hook pre_sync#echo "$PHORA_TARGETS" > "$HOME/pre.log"#sh -c [pre_sync] `echo "$PHORA_TARGETS" > "$HOME/pre.log"` ok
sync complete
```

The deploy ran after the passing gate.

```scrut
$ test -e "$PWD/target-home/editor/init.lua" && echo deployed
deployed
```

`PHORA_TARGETS` listed the target that would deploy.

```scrut
$ cat "$HOME/pre.log"
home
```

## --no-hooks suppresses pre_sync too

`--no-hooks` deploys the files but runs no pre_sync hook, so no log is written and no
gate report appears.

```scrut
$ cd "$ROOT" && mkdir -p p3 && cd p3 && isolate_state && seed_config_pre_sync "$(make_git_source proj)" && phora sync --no-hooks 2>&1 | normalize
sync complete
```

```scrut
$ test -e "$PWD/target-home/editor/init.lua" && echo deployed
deployed
```

```scrut
$ test -e "$HOME/pre.log" && echo fired || echo suppressed
suppressed
```

## Exec form runs shell-free with no $VAR expansion

A target's `on_change` is the shell-free argv form `{ cmd = [...] }`. The argv carries a
literal `$HOME` token; with no shell, `touch` receives it verbatim and creates a file named
`exec_ran_$HOME`. The report names the hook with a `#exec` suffix and `cmd.join(" ")` as the
command, and the two identical entries dedupe to a single line.

```scrut
$ cd "$ROOT" && mkdir -p s5 && cd s5 && isolate_state && seed_config_exec_hook "$(make_git_source proj)" && phora sync 2>&1 | normalize
hook home#touch exec_ran_$HOME#exec [on_change] `touch exec_ran_$HOME` ok
sync complete
```

No shell ran, so the `$HOME` token was not expanded — the file is named verbatim.

```scrut
$ test -e 'exec_ran_$HOME' && echo verbatim
verbatim
```
