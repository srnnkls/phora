# Phora Lifecycle

End-to-end walkthrough of the core `phora` workflow: declare a target, add a git
source, project it, and verify the result. Each block runs the shipped binary
and asserts its exact output.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir, so nothing touches the developer's real
`phora.toml`, `~/.phora`, or XDG roots. Output is piped through `normalize`,
which collapses the random tempdir prefix to `<ROOT>`; commit hashes and digests
are pinned by the fixture and asserted verbatim.

## Setup

Source the helpers and build a throwaway git source repository.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && repo="$(make_git_source proj)" && echo ready
ready
```

## Declare a target

A target names a deploy destination. Create one pointing at a directory inside
the tempdir.

```scrut
$ phora target add home --path "$PWD/target-home" 2>&1 | normalize
Added target 'home': <ROOT>/target-home
```

## Add a git source

`phora add` resolves the local repository path, records it as a `path =` source,
and binds it to the `home` target, including only the `editor` subtree.

```scrut
$ phora add "$repo" --to home --include editor 2>&1 | normalize
Added source 'src-proj': <ROOT>/src-proj
  bound to home
```

## Sync

`phora sync` clones the source into the mirror, locks the resolved commit, and
projects the included files into the target.

```scrut
$ phora sync 2>&1 | normalize
sync complete
```

## List deployment state

`phora list` reports each target's artifacts and their state. The `✓` glyph
marks a clean, in-sync artifact.

```scrut
$ phora list 2>&1 | normalize
home:
  src-proj/editor  ✓ clean
```

## Verify deployed contents

`phora verify` re-hashes every deployed file against the lock.

```scrut
$ phora verify 2>&1 | normalize
all verified
```

## On-disk effect

The included files landed in the target tree.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && test -f "$PWD/target-home/editor/lua/opts.lua" && echo deployed
deployed
```

## Registry effect

The global registry under `XDG_STATE_HOME` recorded the deployed artifact.

```scrut
$ test -f "$XDG_STATE_HOME"/phora/projects/*/targets/home/artifacts/src-proj/editor.toml && echo recorded
recorded
```

## Registry query

`phora where` reports the deployed artifact with its resolved commit and content
digest — both deterministic, so they are asserted verbatim.

```scrut
$ phora where 2>&1 | normalize
Artifact: src-proj/editor (commit ca94c83b, digest blake3:2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0)
  - home
```
