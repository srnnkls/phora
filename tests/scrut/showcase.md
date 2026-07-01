# Phora Usage Showcase

A narrated, end-to-end walkthrough that doubles as runnable documentation. It
follows a new user setting up a real project: declare a target, add a git source
of editor and lint config, project it, inspect the result, then layer a
machine-local overlay on top via a symlink. Every command is the shipped binary,
and every block asserts its exact output — so this document cannot drift from how
`phora` actually behaves.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir, so nothing touches the developer's real
`phora.toml`, `~/.phora`, or XDG roots. Output is piped through `normalize`,
which collapses the random tempdir prefix (in either its raw or macOS
`/private`-canonicalized form) to `<ROOT>`. Commit hashes and content digests are
pinned by the fixture, so they are asserted verbatim.

## Bootstrap

A real project starts with a git repository of config you want to share across
machines. Source the helpers, isolate state, and build a throwaway source repo
holding an `editor/`, a `lint/`, and a few loose files.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && repo="$(make_git_source dotfiles)" && echo ready
ready
```

## Declare a target

A target is a named deploy destination. Point one at a directory that stands in
for your home tree.

```scrut
$ phora target add home --path "$PWD/target-home" 2>&1 | normalize
Added target 'home': <ROOT>/target-home
```

## Add a git source, refined

`phora add` resolves the local repository, records it as a `path =` source, and
binds it to `home`. Refining the binding with `--include` keeps only the
subtrees you care about — here the `editor` and `lint` directories, leaving the
repo's loose root files (`README.md`, `.config/`) out of the projection.

```scrut
$ phora add "$repo" --to home --include editor --include lint 2>&1 | normalize
Added source 'src-dotfiles': <ROOT>/src-dotfiles
  bound to home
```

## Project it

`phora sync` clones the source into the mirror under `XDG_CACHE_HOME`, locks the
resolved commit, and copies the included files into the target.

```scrut
$ phora sync 2>&1 | normalize
sync complete
```

## Inspect deployment state

`phora list` reports each target's artifacts and their state. The `✓` glyph
marks a clean, in-sync artifact; only the two included subtrees appear.

```scrut
$ phora list 2>&1 | normalize
home:
  src-dotfiles/editor  ✓ clean
  src-dotfiles/lint  ✓ clean
```

`phora where` queries the global registry, reporting each artifact's resolved
commit (shortened to 8 hex) and content digest — both deterministic for the
pinned fixture, so they are asserted verbatim.

```scrut
$ phora where 2>&1 | normalize
Artifact: src-dotfiles/editor (commit ca94c83b, digest blake3:2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0)
  - home
Artifact: src-dotfiles/lint (commit ca94c83b, digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb)
  - home
```

The included files landed in the target tree, and the excluded root files did
not.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && test ! -e "$PWD/target-home/README.md" && echo projected
projected
```

## Layer a machine-local overlay

Not everything belongs in the shared, committed config. Machine-specific files
live in `phora.local.toml`, which `phora` reads but never expects to be checked
in. Declare a *local* target for them.

```scrut
$ phora target add machine --path "$PWD/target-machine" --local 2>&1 | normalize
Added target 'machine': <ROOT>/target-machine
```

`make_overlay` materializes a plain directory of machine-local files. Adding it
with `--symlink` registers it as a local overlay source that deploys by linking
in place rather than copying. Overlay sources go to `phora.local.toml`, so the
add does not take `--to` or refinement flags; the overlay path is recorded in its
canonical form (macOS resolves it under `/private`, which `normalize` collapses
to `<ROOT>`).

```scrut
$ ov="$(make_overlay machine)" && phora add "$ov" --symlink 2>&1 | normalize
Added local source 'overlay-machine': <ROOT>/overlay-machine
```

Bind the overlay to the local `machine` target. `--local` keeps the binding in
`phora.local.toml` alongside the source.

```scrut
$ phora bind overlay-machine --to machine --local 2>&1 | normalize
Bound overlay-machine to 'machine'
```

## Project the overlay

A second `phora sync` deploys the overlay. Because it was added with `--symlink`,
the target entry is a symlink back to the overlay directory rather than a copy.

```scrut
$ phora sync 2>&1 | normalize
sync complete
```

`phora where` now reports the overlay artifact too. A linked overlay carries no
git commit or content digest, so both read as `link`.

```scrut
$ phora where 2>&1 | normalize
Artifact: overlay-machine/config (commit link, digest link:)
  - machine
Artifact: overlay-machine/notes.txt (commit link, digest link:)
  - machine
Artifact: src-dotfiles/editor (commit ca94c83b, digest blake3:2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0)
  - home
Artifact: src-dotfiles/lint (commit ca94c83b, digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb)
  - home
```

`phora list` merges the machine-local config too, so the overlay shows under the
`machine` target with the `linked` state alongside the git artifacts in `home`.

```scrut
$ phora list 2>&1 | normalize
home:
  src-dotfiles/editor  ✓ clean
  src-dotfiles/lint  ✓ clean
machine:
  overlay-machine/config  linked
  overlay-machine/notes.txt  linked
```

`phora preview` renders the full projection from the lock, both targets at once —
the git artifacts copied into `home`, the overlay linked into `machine`.

```scrut
$ phora preview 2>&1 | normalize
home -> <ROOT>/target-home
  src-dotfiles@ca94c83b editor/ -> <ROOT>/target-home/editor
  src-dotfiles@ca94c83b lint/ -> <ROOT>/target-home/lint
machine -> <ROOT>/target-machine
  overlay-machine@link config/ -> <ROOT>/target-machine/config
  overlay-machine@link notes.txt -> <ROOT>/target-machine/notes.txt
```

The deployed overlay entry is a real symlink pointing back at the source
directory.

```scrut
$ test -L "$PWD/target-machine/config" && readlink "$PWD/target-machine/config" | normalize
<ROOT>/overlay-machine/config
```

## Verify everything

`phora verify` re-checks every deployed artifact — git files re-hashed against
the lock, the overlay symlink confirmed in place.

```scrut
$ phora verify 2>&1 | normalize
all verified
```
