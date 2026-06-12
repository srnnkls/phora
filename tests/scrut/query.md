# Phora Query & State

Read-only inspection of the registry and deployment state: `phora where`
(registry queries and filters), `phora check-match` (include/exclude debugging),
and the `phora list` state labels (clean, modified, ejected).

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir. Rather than narrate a full `add`, this
suite bootstraps from a pre-seeded `phora.toml` (`seed_config`) — one source
`dotfiles` (including `editor` and `lint`) bound to one target `home` — then
syncs. Output is piped through `normalize`, which collapses the tempdir prefix
to `<ROOT>`; commit hashes and digests are pinned by the fixture and asserted
verbatim.

## Setup

Source the helpers, isolate state, write the seed config, and project it.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && seed_config "$(make_git_source proj)" >/dev/null && phora sync 2>&1 | normalize
sync complete
```

## Registry query — all artifacts

`phora where` lists every deployed artifact with its resolved commit (shortened
to 8 hex) and content digest, followed by the targets it lands in.

```scrut
$ phora where 2>&1 | normalize
Artifact: dotfiles/editor (commit ca94c83b, digest blake3:2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0)
  - home
Artifact: dotfiles/lint (commit ca94c83b, digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb)
  - home
```

## Filter by artifact

`--artifact` narrows the result to a single artifact name.

```scrut
$ phora where --artifact editor 2>&1 | normalize
Artifact: dotfiles/editor (commit ca94c83b, digest blake3:2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0)
  - home
```

## Filter by digest

`--digest` matches the full `blake3:` content digest, isolating one artifact.

```scrut
$ phora where --digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb 2>&1 | normalize
Artifact: dotfiles/lint (commit ca94c83b, digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb)
  - home
```

## Filter with no match

A filter that matches nothing produces no output.

```scrut
$ phora where --artifact nonexistent 2>&1 | normalize
```

## Include/exclude matching — an included path

`phora check-match` debugs whether a path is selected by a source's
include/exclude rules. `editor/init.lua` lives under the included `editor`
subtree, so both its artifact and its full path are allowed.

```scrut
$ phora check-match --source dotfiles editor/init.lua 2>&1 | normalize
artifact `editor`: allowed
path `editor/init.lua`: allowed
include: ["editor", "lint"]
exclude: []
```

## Include/exclude matching — an excluded path

`README.md` is a loose root file outside the included subtrees, so its artifact
is excluded.

```scrut
$ phora check-match --source dotfiles README.md 2>&1 | normalize
artifact `README.md`: excluded
path `README.md`: allowed
include: ["editor", "lint"]
exclude: []
```

## State label — clean

Immediately after sync every artifact is in sync. `phora list` marks a clean,
matching deployment with the `✓` glyph.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
  dotfiles/lint  ✓ clean
```

## State label — modified

Mutating a deployed file on disk diverges it from the lock. `phora list` then
reports that artifact as `modified`.

```scrut
$ printf 'tampered\n' >> "$PWD/target-home/editor/init.lua" && phora list 2>&1 | normalize
home:
  dotfiles/editor  modified
  dotfiles/lint  ✓ clean
```

## State label — ejected

`phora eject` stops managing an artifact while keeping its files on disk. The
record is retained and marked ejected, so `phora list` reports it as `ejected`
(this overrides the earlier `modified` tamper — an ejected artifact is no longer
checked against the lock).

```scrut
$ phora eject --source dotfiles --target home editor 2>&1 | normalize
ejected dotfiles/editor from home (files kept)
```

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ejected
  dotfiles/lint  ✓ clean
```

`phora where` keeps reporting the artifact too, annotating the target it was
ejected from.

```scrut
$ phora where --artifact editor 2>&1 | normalize
Artifact: dotfiles/editor (commit ca94c83b, digest blake3:2316b2c05d3f72e93270833746381341b70a008daf5af59a2ddb2a8c83206bc0)
  - home (ejected)
```

The ejected artifact's files remain in the target tree.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && echo kept
kept
```
