# Phora Templates

End-to-end behaviour of per-machine minijinja templating: a `*.tmpl` source file
is rendered with the effective `[vars]` and deployed with its `.tmpl` suffix
stripped, plain siblings copy untouched, `verify` passes on the rendered bytes
(INV-5), two machines with different vars render differently yet produce
byte-identical lock files (INV-6), editing a var re-renders on the next sync with
no new commit (INV-7), and `rebuild-registry` reconciles against the merged
base+local vars so a templated artifact stays clean.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir. Each scenario `cd`s into its own
subdirectory and re-isolates, giving it a private `HOME`/state. No commit SHAs or
digests are pinned — assertions are on deployed file contents, `verify`/`list`
labels, and a self-comparison of two lock files; path-bearing output is piped
through `normalize`.

## Setup

Source the helpers.

```scrut
$ source "$TESTDIR"/_setup.sh && ROOT="$PWD" && echo ready
ready
```

## Render and strip the .tmpl suffix

The first sync renders `editor/motd.tmpl` with the base `greeting` and deploys it
as `editor/motd` — suffix stripped.

```scrut
$ mkdir -p s1 && cd s1 && isolate_state && seed_config_with_vars "$(make_templated_source proj)" && phora sync 2>&1 | normalize
sync complete
```

The deployed file holds the rendered text.

```scrut
$ cat "$PWD/target-home/editor/motd"
hello base!
```

The `.tmpl` source name is gone from the target tree.

```scrut
$ test -e "$PWD/target-home/editor/motd.tmpl" && echo present || echo stripped
stripped
```

The plain sibling deployed verbatim.

```scrut
$ cat "$PWD/target-home/editor/static.txt"
plain content
```

## verify passes on rendered output (INV-5)

The manifest hashes the rendered bytes, so `verify` is clean against the rendered
tree — it does not flag the rendered `motd` as a mismatch against the source.

```scrut
$ phora verify 2>&1 | normalize
all verified
```

`phora list` shows the templated artifact clean.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
```

## Two machines render differently, locks identical (INV-6)

One shared source repo; two machines each seed against it with a different
`phora.local.toml` `greeting` overlay.

```scrut
$ cd "$ROOT" && SHARED="$(make_templated_source shared)" && echo seeded
seeded
```

```scrut
$ mkdir -p m1 && cd m1 && isolate_state && seed_config_with_vars "$SHARED" && seed_local_vars alice && phora sync 2>&1 | normalize
sync complete
```

```scrut
$ cd "$ROOT" && mkdir -p m2 && cd m2 && isolate_state && seed_config_with_vars "$SHARED" && seed_local_vars bob && phora sync 2>&1 | normalize
sync complete
```

Each machine rendered its own greeting.

```scrut
$ cat "$ROOT/m1/target-home/editor/motd"
hello alice!
```

```scrut
$ cat "$ROOT/m2/target-home/editor/motd"
hello bob!
```

The lock hashes source bytes only, so the two lock files are byte-identical
despite the differing vars.

```scrut
$ diff "$ROOT/m1/phora.lock" "$ROOT/m2/phora.lock" && echo identical
identical
```

Each machine's manifest hashes its OWN rendered bytes, so `verify` is clean on
both — not only on the first (INV-5 holds per machine).

```scrut
$ cd "$ROOT/m1" && phora verify 2>&1 | normalize
all verified
```

```scrut
$ cd "$ROOT/m2" && phora verify 2>&1 | normalize
all verified
```

## Var edit re-renders on next sync (INV-7)

Changing the local `greeting` overlay — with no source commit advance — re-renders
the artifact on the next sync.

```scrut
$ cd "$ROOT" && mkdir -p s7 && cd s7 && isolate_state && seed_config_with_vars "$(make_templated_source proj)" && seed_local_vars first && phora sync 2>&1 | normalize
sync complete
```

```scrut
$ cat "$PWD/target-home/editor/motd"
hello first!
```

The deployed commit is recorded in the lock; capture it to prove the re-render is
driven by the var change alone, not a commit advance.

```scrut
$ COMMIT_BEFORE="$(grep '^commit ' phora.lock)" && echo "${COMMIT_BEFORE:+captured}"
captured
```

```scrut
$ seed_local_vars second && phora sync 2>&1 | normalize
sync complete
```

```scrut
$ cat "$PWD/target-home/editor/motd"
hello second!
```

No new commit: the lock's commit is unchanged across the var-driven redeploy, and
`verify` is clean on the re-rendered output.

```scrut
$ test "$(grep '^commit ' phora.lock)" = "$COMMIT_BEFORE" && echo same-commit
same-commit
```

```scrut
$ phora verify 2>&1 | normalize
all verified
```

## rebuild-registry reconciles against merged vars

With a `phora.local.toml` `[vars]` overlay present, `rebuild-registry` reconciles
against the merged base+local vars — the same vars sync rendered with — so the
templated artifact is reconstructed clean, not `modified`.

```scrut
$ cd "$ROOT" && mkdir -p s5 && cd s5 && isolate_state && seed_config_with_vars "$(make_templated_source proj)" && seed_local_vars overlaid && phora sync 2>&1 | normalize
sync complete
```

```scrut
$ cat "$PWD/target-home/editor/motd"
hello overlaid!
```

```scrut
$ phora rebuild-registry 2>&1 | normalize
reconstructed 1
```

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
```

## preview --files shows the deployed name and annotates templated files (M004)

`phora preview --files` lists the RENDERED deployed name of a templated file
(`motd`, suffix stripped) annotated `(templated)`, while a plain sibling keeps its
name with no annotation. The source name `motd.tmpl` never appears.

```scrut
$ cd "$ROOT" && mkdir -p s6 && cd s6 && isolate_state && seed_config_with_vars "$(make_templated_source proj)" && phora sync 2>&1 | normalize
sync complete
```

```scrut
$ phora preview --files 2>&1 | normalize | grep -E 'motd|static'
    motd (templated)
    static.txt
```

The `--json` form carries the deployed path and a per-file `templated` flag.

```scrut
$ phora preview --files --json 2>&1 | grep -E '"path"|"templated"' | sed -e 's/^ *//' | grep -E 'motd|true|false|static'
"path": "motd",
"templated": true
"path": "static.txt",
"templated": false
```
