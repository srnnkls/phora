# Phora Manage

Lifecycle management of an already-deployed projection: `phora eject` /
`phora uneject` (stop and resume managing an artifact), `phora update` (bump the
lock to the latest commit), and `phora rebuild-registry` (reconstruct the global
registry from the lock and on-disk targets).

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir. It bootstraps from a pre-seeded
`phora.toml` (`seed_config`) — one source `dotfiles` (including `editor` and
`lint`) bound to one target `home` — then syncs. Output is piped through
`normalize`, which collapses the tempdir prefix to `<ROOT>`; commit hashes and
digests are pinned by the fixture and asserted verbatim.

## Setup

Source the helpers, isolate state, write the seed config, and project it.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && seed_config "$(make_git_source proj)" >/dev/null && phora sync 2>&1 | normalize
sync complete
```

## Eject — stop managing an artifact

`phora eject` stops managing an artifact while leaving its deployed files in
place. It is addressed by `--source`, `--target`, and the bare artifact name.

```scrut
$ phora eject --source dotfiles --target home editor 2>&1 | normalize
ejected dotfiles/editor from home (files kept)
```

The record is kept and marked ejected, so `phora list` still reports the
artifact — now labelled `ejected` rather than dropped.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ejected
  dotfiles/lint  ✓ clean
```

Its files survive untouched in the target tree.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && echo kept
kept
```

## Uneject — resume managing an artifact

`phora uneject` clears the ejected mark. Because the record was kept, management
resumes immediately — no re-sync needed.

```scrut
$ phora uneject --source dotfiles --target home editor 2>&1 | normalize
unejected dotfiles/editor in home
```

`phora list` shows the artifact managed and clean again.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
  dotfiles/lint  ✓ clean
```

## Update — bump the lock to the latest commit

The seed fixture is pinned at commit `ca94c83b`. `add_commit` lands a second,
deterministic commit on the source repository that rewrites `editor/init.lua`.

```scrut
$ add_commit proj && echo committed
committed
```

`phora update` re-resolves the branch, advances the lock to the new commit, and
redeploys the affected artifacts in one step — the previously deployed files are
phora's own (clean against the old lock), so they are refreshed, not skipped as
foreign content.

```scrut
$ phora update 2>&1 | normalize
sync complete
```

`phora where` now reports the second commit `7541e58d` and the updated `editor`
digest; `lint` is unchanged, so its digest is identical to the first commit.

```scrut
$ phora where 2>&1 | normalize
Artifact: dotfiles/editor (commit 7541e58d, digest blake3:be90c12a92d299cfbc67b4c387b8266675bb89417983b30668e3bc3e04ac40f6)
  - home
Artifact: dotfiles/lint (commit 7541e58d, digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb)
  - home
```

The redeployed `editor/init.lua` carries the second commit's contents.

```scrut
$ cat "$PWD/target-home/editor/init.lua"
-- init v2
```

## Rebuild registry — reconstruct from lock and on-disk targets

Deleting the global registry under `XDG_STATE_HOME` leaves `phora where` empty.

```scrut
$ rm -rf "$XDG_STATE_HOME"/phora/projects && phora where 2>&1 | normalize
```

`phora rebuild-registry` walks the lock and the on-disk targets to reconstruct
every artifact record, reporting how many it recovered.

```scrut
$ phora rebuild-registry 2>&1 | normalize
reconstructed 2
```

The registry is whole again: `phora where` reports both artifacts at the locked
second commit.

```scrut
$ phora where 2>&1 | normalize
Artifact: dotfiles/editor (commit 7541e58d, digest blake3:be90c12a92d299cfbc67b4c387b8266675bb89417983b30668e3bc3e04ac40f6)
  - home
Artifact: dotfiles/lint (commit 7541e58d, digest blake3:d26cc52a7261d7a76fa1f6dadda5cba932687bd6cf626e7ea746e46dc8937cfb)
  - home
```

`phora list` confirms both artifacts are managed and clean.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
  dotfiles/lint  ✓ clean
```
