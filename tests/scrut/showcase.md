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

## Ask instead of guessing

When a path does or does not ship and the refinement is not obvious by eye,
`phora check-match` answers for a single path, and shows the include and exclude
lists it judged against.

```scrut
$ phora check-match --source src-dotfiles editor/init.lua 2>&1 | normalize
artifact `editor`: allowed
path `editor/init.lua`: allowed
include: ["editor", "lint"]
exclude: []
```

```scrut
$ phora check-match --source src-dotfiles README.md 2>&1 | normalize
artifact `README.md`: excluded
path `README.md`: excluded
include: ["editor", "lint"]
exclude: []
```

## Read the lock

The lock is small enough to read whole. One entry per source: where it came
from, the revision that was asked for, the commit that revision resolved to, a
digest over the projected content, and a digest over the export-affecting
config. The last of those is how phora notices you changed *what* ships even
when upstream has not moved.

```scrut
$ cat phora.lock | normalize
version = 1

[[sources]]
name = "src-dotfiles"
git = "<ROOT>/src-dotfiles"
resolved = "default"
commit = "ca94c83b3a51aab8dea8315a9baa986e178c599d"
digest = "blake3:11b617bf6382560c7adb2d6543f9843c8e36d168dc23b6735611c5656ff17624"
config_digest = "blake3:b8e08f8762862914cb929c8180e62a901e1c24410f47ac2742b299166f16a58e"
```

## Edit against a working tree

Iterating on the config through commit-and-sync means a commit per keystroke.
For that loop, point the source at a local checkout and deploy it by symlink
instead. `phora.local.toml` overlays the committed config key by key and is
never committed, so the loop cannot leak into shared config. A directory shaped
like the repo stands in for the checkout.

```scrut
$ mkdir -p dev-dotfiles/editor dev-dotfiles/lint && printf -- '-- work in progress\n' > dev-dotfiles/editor/init.lua && printf '[rules]\n' > dev-dotfiles/lint/rules.toml && echo staged
staged
```

```scrut
$ printf 'version = 1\n\n[sources.src-dotfiles]\npath = "./dev-dotfiles"\ndeploy = "link"\n' > phora.local.toml && echo overlaid
overlaid
```

The next sync reports the transition it is making — the source moves from the
locked commit to link mode — and relinks the target.

```scrut
$ phora sync 2>&1 | normalize
phora: src-dotfiles → home: default (ca94c83b) → default (link)
sync complete
```

Both artifacts are now symlinks into the working tree, so edits show up without
re-syncing.

```scrut
$ phora list 2>&1 | normalize
home:
  src-dotfiles/editor  linked
  src-dotfiles/lint  linked
```

```scrut
$ readlink "$PWD/target-home/editor" | normalize
<ROOT>/dev-dotfiles/editor
```

A linked artifact sits outside the integrity model: its bytes change underfoot,
so phora records `link` in place of a commit and a content digest rather than
hashing something that will not stay true. `--artifact` narrows the query to one
artifact.

```scrut
$ phora where --artifact editor 2>&1 | normalize
Artifact: src-dotfiles/editor (commit link, digest link:)
  - home
```

Link mode trades that content guarantee for live edits. Removing the overlay
ends the loop, and the next sync puts the pinned, verifiable copies back.

```scrut
$ rm phora.local.toml && phora sync 2>&1 | normalize
phora: src-dotfiles → home: link (link) → default (ca94c83b)
sync complete
```

The half-written edit stayed in the working tree where it belongs; the target is
back on the locked commit, hashed and clean.

```scrut
$ test ! -L "$PWD/target-home/editor" && cat "$PWD/target-home/editor/init.lua"
-- init
```

```scrut
$ phora list 2>&1 | normalize
home:
  src-dotfiles/editor  ✓ clean
  src-dotfiles/lint  ✓ clean
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

Where a whole directory ships as one artifact it collapses to a single entry,
which preview marks with a trailing slash; `notes.txt` is a lone file and
carries none.

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
