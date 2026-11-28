# Phora State Garbage Collection

`phora gc` lists every state dir that is not live, with its class, reason, root,
the evidence behind the reason and the planned action, then applies the plan.
Drop-class dirs (`gone`, `empty`) go without confirmation; every other drop
needs `--eject` and a confirmation (`--yes` or a TTY answer). `gc` never deletes
a deployed file: the files a drop leaves behind are listed as `leaves`.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir, and every `gc` runs from that tempdir,
outside any project. Output is piped through `normalize`, which collapses the
tempdir prefix to `<ROOT>`, and through `ids`, which collapses project ids (16
hex digits hashed from the tempdir path) to `<ID>`. Each section leaves exactly
one dead state dir, so every listing names one record.

## Setup

Source the helpers, build a git source, and define `project NAME TARGET`, which
writes a project deploying the source's `editor` subtree to `TARGET`.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && repo="$(make_git_source dots)" && ids() { sed -E 's/[0-9a-f]{16}/<ID>/g'; } && project() { mkdir -p "$PWD/$1" && printf 'version = 1\n\n[sources.dotfiles]\npath = "%s"\nbranch = "main"\ninclude = ["editor"]\n\n[targets.home]\npath = "%s"\nsources = ["dotfiles"]\nlayout = "flat"\n' "$repo" "$2" >"$PWD/$1/phora.toml"; } && echo ready
ready
```

A live project syncs once. Its state dir is never listed below.

```scrut
$ project live "$PWD/live/out" && (cd live && phora sync >/dev/null 2>&1) && ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' '
1
```

## A deleted root is planned for a drop

A project deploying only inside its own root syncs, then its root is deleted.

```scrut
$ project gone "$PWD/gone/out" && (cd gone && phora sync >/dev/null 2>&1) && rm -rf gone && ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' '
2
```

`--dry-run` prints the plan: the dir is `gone`, a Drop-class reason, and the
evidence proves the root deleted.

```scrut
$ phora gc --dry-run 2>&1 | normalize | ids
<ID> gone (drop) root <ROOT>/gone
    root: deleted: absent, recorded on the state dir's mount, state-dir path unchanged, nearest ancestor on that mount, unencrypted, no symlinked component
    action: drop
```

A dry run changes nothing: both state dirs remain.

```scrut
$ ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' '
2
```

## Plain gc drops it without confirmation

Without `--eject` or `--yes`, `gc` applies the Drop-class drop, prunes the
content store and reports the outcome.

```scrut
$ phora gc 2>&1 | normalize | ids
<ID> gone (drop) root <ROOT>/gone
    root: deleted: absent, recorded on the state dir's mount, state-dir path unchanged, nearest ancestor on that mount, unencrypted, no symlinked component
    action: drop
phora: pruned 0 content-store object(s), 0 bytes
<ID>: dropped
```

Only the live project's state dir remains.

```scrut
$ ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' '
1
```

## A deleted root with a file outside it is stranded

A project deploying outside its root syncs, then its root is deleted. Its
deployed file outside the root still exists.

```scrut
$ project stranded "$PWD/outside" && (cd stranded && phora sync >/dev/null 2>&1) && rm -rf stranded && cat outside/editor/init.lua
-- init
```

`--json` prints the plan as one NDJSON record per dir: a `stranded` Release-class
dir, whose evidence lists the present outside destination, planned for a report
only, with that destination left behind and no outcome on a dry run.

```scrut
$ phora gc --dry-run --json 2>&1 | normalize | ids
{"action":{"eject":[],"keep_linked":[],"kind":"report"},"class":"release","evidence":{"pending_journal_entries":0,"present_outside":["<ROOT>/outside/editor"],"releasable_links":[],"root":"deleted: absent, recorded on the state dir's mount, state-dir path unchanged, nearest ancestor on that mount, unencrypted, no symlinked component","unresolvable_records":[]},"id":"<ID>","leftovers":["<ROOT>/outside/editor"],"outcome":null,"root":"<ROOT>/stranded","state_dir":"<ROOT>/state/phora/projects/<ID>","status":"stranded","type":"gc"}
```

Plain `gc` keeps a stranded dir and names the command that drops it.

```scrut
$ phora gc 2>&1 | normalize | ids
<ID> stranded (release) root <ROOT>/stranded
    root: deleted: absent, recorded on the state dir's mount, state-dir path unchanged, nearest ancestor on that mount, unencrypted, no symlinked component
    present_outside: <ROOT>/outside/editor
    pending_journal_entries: 0
    action: keep; `phora gc --eject` drops it
    leaves: <ROOT>/outside/editor
phora: pruned 0 content-store object(s), 0 bytes
```

## Eject needs a confirmation

`--eject` without a TTY and without `--yes` plans the drop but applies nothing:
the dir is reported as pending.

```scrut
$ phora gc --eject </dev/null 2>&1 | normalize | ids
<ID> stranded (release) root <ROOT>/stranded
    root: deleted: absent, recorded on the state dir's mount, state-dir path unchanged, nearest ancestor on that mount, unencrypted, no symlinked component
    present_outside: <ROOT>/outside/editor
    pending_journal_entries: 0
    action: eject the history worktrees, then drop
    leaves: <ROOT>/outside/editor
phora: pruned 0 content-store object(s), 0 bytes
<ID>: pending; confirm with `phora gc --eject --yes`
```

```scrut
$ ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' '
2
```

`--eject --yes` confirms: the stranded dir is dropped.

```scrut
$ phora gc --eject --yes 2>&1 | normalize | ids
<ID> stranded (release) root <ROOT>/stranded
    root: deleted: absent, recorded on the state dir's mount, state-dir path unchanged, nearest ancestor on that mount, unencrypted, no symlinked component
    present_outside: <ROOT>/outside/editor
    pending_journal_entries: 0
    action: eject the history worktrees, then drop
    leaves: <ROOT>/outside/editor
phora: pruned 0 content-store object(s), 0 bytes
<ID>: dropped
```

The state dir is gone, and the file it left behind is untouched.

```scrut
$ ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' ' && cat outside/editor/init.lua
1
-- init
```

## A root without phora.toml is retired

A synced project whose `phora.toml` is removed keeps its root. Plain `gc` lists
it as `retired`, keeps it, and notes that it may be a live project on a branch
without `phora.toml`.

```scrut
$ project retired "$PWD/retired/out" && (cd retired && phora sync >/dev/null 2>&1) && mv retired/phora.toml retired.toml && phora gc 2>&1 | normalize | ids
<ID> retired (release) root <ROOT>/retired
    root: present
    manifest: phora.toml absent
    action: keep; `phora gc --eject` drops it
    leaves: <ROOT>/retired/out/editor
note: a retired dir may be a live project on a branch without phora.toml
phora: pruned 0 content-store object(s), 0 bytes
```

`--eject --yes --json` drops it and records the outcome; the note goes to
stderr, the record to stdout.

```scrut
$ phora gc --eject --yes --json 2>/dev/null | normalize | ids
{"action":{"eject":[],"keep_linked":[],"kind":"eject"},"class":"release","evidence":{"manifest":"phora.toml absent","root":"present"},"id":"<ID>","leftovers":["<ROOT>/retired/out/editor"],"outcome":"dropped","root":"<ROOT>/retired","state_dir":"<ROOT>/state/phora/projects/<ID>","status":"retired","type":"gc"}
```

The deployed file inside the retired root stays.

```scrut
$ ls "$XDG_STATE_HOME/phora/projects" | wc -l | tr -d ' ' && cat retired/out/editor/init.lua
1
-- init
```
