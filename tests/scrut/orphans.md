# Phora Orphan Visibility and Physical Prune

CLIFF-ORPHANS-002. When a target is removed from config while its artifacts are
still deployed (`target rm --force`), its registry records become *orphans*: no
config target resolves them. This suite pins the recovery path — orphans stay
visible via `phora list --orphans` (with the on-disk path reconstructed from the
persisted deploy root), plain `phora sync` warns that orphans exist, and
`phora sync --prune` physically removes the orphaned files before dropping the
records.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir. It bootstraps from `seed_config` — one
source `dotfiles` (including `editor` and `lint`) bound to one target `home` —
then syncs. Output is piped through `normalize`, which collapses the tempdir
prefix to `<ROOT>`.

## Setup

Source the helpers, isolate state, seed the config, and project it.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && seed_config "$(make_git_source proj)" >/dev/null && phora sync 2>&1 | normalize
sync complete
```

Both artifacts deploy to the `home` target and read clean.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
  dotfiles/lint  ✓ clean
```

## Forced target rm strands the deployed records as orphans

`target rm --force` removes the `home` target block even though its artifacts
are still deployed.

```scrut
$ phora target rm --force home 2>&1 | normalize
Removed target 'home'
```

Config-driven `phora list` can no longer see the deployment — the cliff this
task closes.

```scrut
$ phora list 2>&1 | normalize
No targets configured.
```

## list --orphans surfaces the stranded records with on-disk paths

Registry-driven `phora list --orphans` reports each orphaned record.

```scrut
$ phora list --orphans 2>&1 | normalize | grep -q 'dotfiles/editor' && echo found-editor-orphan
found-editor-orphan
```

```scrut
$ phora list --orphans 2>&1 | normalize | grep -q 'dotfiles/lint' && echo found-lint-orphan
found-lint-orphan
```

Each orphan carries its on-disk path, reconstructed from the deploy root that
was persisted on the record at deploy time.

```scrut
$ phora list --orphans 2>&1 | normalize | grep -q 'target-home/editor' && echo editor-path-shown
editor-path-shown
```

```scrut
$ phora list --orphans 2>&1 | normalize | grep -q 'target-home/lint' && echo lint-path-shown
lint-path-shown
```

## Plain sync warns that orphan records exist

A plain `phora sync` prints a one-line notice whenever orphan records are
present.

```scrut
$ phora sync 2>&1 | normalize | grep -qi 'orphan' && echo orphan-notice
orphan-notice
```

The orphaned files are still on disk before pruning.

```scrut
$ test -e "$PWD/target-home/editor" && echo editor-present
editor-present
```

```scrut
$ test -e "$PWD/target-home/lint" && echo lint-present
lint-present
```

## sync --prune removes orphaned files, then their records

```scrut
$ phora sync --prune 2>&1 | normalize | tail -n 1
sync complete
```

The orphaned files are physically gone.

```scrut
$ test -e "$PWD/target-home/editor" && echo present || echo editor-removed
editor-removed
```

```scrut
$ test -e "$PWD/target-home/lint" && echo present || echo lint-removed
lint-removed
```

Their registry records are dropped too.

```scrut
$ phora where 2>&1 | normalize | head -n 1
No deployed artifacts yet.
```

## After prune, list --orphans is an empty, clean exit

With every orphan cleared, `phora list --orphans` exits 0 with no orphan rows.

```scrut
$ phora list --orphans >/dev/null 2>&1; echo "exit $?"
exit 0
```
