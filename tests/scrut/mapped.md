# Phora Mapped Leaves

End-to-end behaviour of *mapped leaves*: a binding's `map = { "<leaf>" = "<dest>" }`
aliases a single source file to a renamed destination at `target/<dest>`, bypassing
the dir-artifact layout entirely. This suite drives the shipped binary to prove the
on-disk results: one leaf fanned out to two distinct dests, a renamed dest pruned on
the next `--prune` sync, a link-mode mapped dest landing as a symlink to the source
working tree, a dest collision surfaced as a conflict, an ejected mapped record that
keeps its file, a mapped dest under a `by-source` target that still lands at the
target root (no layout leak), a missing link leaf degrading gracefully, and drift on
a mapped dest reported by `list`/`verify`.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state roots
into scrut's per-document tempdir. Each scenario `cd`s into its own subdirectory and
re-isolates, giving it a private `HOME`/state. Output is piped through `normalize`,
which collapses the tempdir prefix to `<ROOT>`; commit hashes and content digests are
pinned by the fixture and asserted verbatim. The fixture (`make_git_source`) carries a
loose `README.md` at the repo root and a nested `lint/rules.toml`, both of which are
used as mapped source leaves.

## Setup

Source the helpers and pin the project root.

```scrut
$ source "$TESTDIR"/_setup.sh && ROOT="$PWD" && echo ready
ready
```

## Fan-out — one leaf, two dests

A single source leaf can be mapped to several destinations at once by binding the same
source twice under distinct aliases. Here `README.md` fans out to `READER.md` (the bare
source) and `COPY.md` (aliased `second`).

```scrut
$ cd "$ROOT" && mkdir -p fanout && cd fanout && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && phora sync 2>&1 | normalize
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [
>   { source = "dotfiles", map = { "README.md" = "READER.md" } },
>   { source = "dotfiles", as = "second", map = { "README.md" = "COPY.md" } },
> ]
> EOF
sync complete
```

Both dests materialize at the target root, each carrying the leaf's content.

```scrut
$ cat target-home/READER.md target-home/COPY.md
loose root file
loose root file
```

`phora list` reports both mapped artifacts, keyed by `<alias>/<dest>`.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/READER.md  ✓ clean
  second/COPY.md  ✓ clean
```

`phora preview` renders both projections from the lock, each at its renamed path.

```scrut
$ phora preview 2>&1 | normalize
home
  dotfiles@ca94c83b READER.md -> <ROOT>/target-home/READER.md
  second@ca94c83b COPY.md -> <ROOT>/target-home/COPY.md
```

## Rename + prune — old dest is reclaimed

Changing a mapped dest leaves the old file orphaned until a `--prune` sync reclaims it.
First project `README.md` to `OLD.md`.

```scrut
$ cd "$ROOT" && mkdir -p rename && cd rename && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cfg() { cat > phora.toml <<EOF
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [{ source = "dotfiles", map = { "README.md" = "$1" } }]
> EOF
> } && cfg OLD.md && phora sync 2>&1 | normalize
sync complete
```

Rewrite the dest to `NEW.md` and redeploy with `--prune`. The orphaned `OLD.md` record
is reported and its file removed; `NEW.md` is deployed.

```scrut
$ cfg NEW.md && phora sync --prune 2>&1 | normalize
phora: pruning orphaned dotfiles:OLD.md
sync complete
```

The old dest is gone, the new dest exists.

```scrut
$ test ! -e target-home/OLD.md && test -f target-home/NEW.md && echo reclaimed
reclaimed
```

`phora where` reports only the surviving dest.

```scrut
$ phora where 2>&1 | normalize
Artifact: dotfiles/NEW.md (commit ca94c83b, digest blake3:dd02771b145c70dd7166e6ab2a0d74b4bc80835bd6f33c6f9080b4ebbb67bc5e)
  - home
```

## Link mode — dest is a symlink to the source leaf

A source flipped to `deploy = "link"` in `phora.local.toml` links its mapped leaf in
place rather than copying it. The committed config carries the mapped binding with an
absolute source path; activating link mode on that committed absolute path emits a
non-fatal portability warning, then sync proceeds.

```scrut
$ cd "$ROOT" && mkdir -p link && cd link && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && cat > phora.local.toml <<EOF2 && phora sync 2>&1 | normalize
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [{ source = "dotfiles", map = { "README.md" = "LINKED.md" } }]
> EOF
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> deploy = "link"
> EOF2
phora: source `dotfiles`: deploy = "link" uses the absolute path `<ROOT>/src-dotfiles`, which is not portable across machines
sync complete
```

The mapped dest is a real symlink pointing at the source leaf in the working tree.

```scrut
$ test -L target-home/LINKED.md && readlink target-home/LINKED.md | normalize
<ROOT>/src-dotfiles/README.md
```

A linked mapped artifact carries no commit or digest, so `where` reads both as `link`
and `preview` renders the `@link` revision.

```scrut
$ phora where 2>&1 | normalize
Artifact: dotfiles/LINKED.md (commit link, digest link:)
  - home
```

```scrut
$ phora preview 2>&1 | normalize
home
  dotfiles@link LINKED.md -> <ROOT>/target-home/LINKED.md
```

## Collision — two dests, same name

Two bindings mapping to the *same* dest collide. `phora sync` surfaces the conflict and
fails, naming the contested artifact and both sources.

```scrut
$ cd "$ROOT" && mkdir -p collision && cd collision && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && echo configured
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [
>   { source = "dotfiles", map = { "README.md" = "DUP.md" } },
>   { source = "dotfiles", as = "two", map = { "lint/rules.toml" = "DUP.md" } },
> ]
> EOF
configured
```

```scrut
$ phora sync 2>&1
error: artifact `DUP.md` collides in target `home` from sources: ["dotfiles", "two"]
[1]
```

## Eject — mapped record kept, file kept

`phora eject` stops managing a mapped artifact while leaving its deployed file in place.

```scrut
$ cd "$ROOT" && mkdir -p eject && cd eject && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && phora sync 2>&1 | normalize
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [{ source = "dotfiles", map = { "README.md" = "KEEP.md" } }]
> EOF
sync complete
```

```scrut
$ phora eject --source dotfiles --target home KEEP.md 2>&1 | normalize
ejected dotfiles/KEEP.md from home (files kept)
```

The file survives, and the record is kept but marked ejected in both `list` and `where`.

```scrut
$ test -f target-home/KEEP.md && echo kept
kept
```

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/KEEP.md  ejected
```

```scrut
$ phora where 2>&1 | normalize
Artifact: dotfiles/KEEP.md (commit ca94c83b, digest blake3:6be6e4f8dce9e6498d7b5ca226a4f7cb4a658407ef5350b929a12a27861dc961)
  - home (ejected)
```

## By-source — mapped dest lands at the target root

A mapped binding under a `layout = "by-source"` target ignores the by-source layout: the
dest lands directly at the target root, never under a per-source subdirectory.

```scrut
$ cd "$ROOT" && mkdir -p bysource && cd bysource && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && phora sync 2>&1 | normalize
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "by-source"
> sources = [{ source = "dotfiles", map = { "README.md" = "BYSRC.md" } }]
> EOF
sync complete
```

The dest is at the target root, and no by-source subdirectory was created.

```scrut
$ test -f target-home/BYSRC.md && test ! -d target-home/dotfiles && echo "at root, no leak"
at root, no leak
```

```scrut
$ phora preview 2>&1 | normalize
home
  dotfiles@ca94c83b BYSRC.md -> <ROOT>/target-home/BYSRC.md
```

## Missing link leaf degrades gracefully

A link-mode binding whose source leaf is absent from the working tree does not crash:
`preview` reports the missing tree and `sync` still completes.

```scrut
$ cd "$ROOT" && mkdir -p missing && cd missing && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && cat > phora.local.toml <<EOF2 && phora preview 2>&1 | normalize
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [{ source = "dotfiles", map = { "ghost.md" = "GHOST.md" } }]
> EOF
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> deploy = "link"
> EOF2
home
  dotfiles — link working tree gone
```

```scrut
$ phora sync 2>&1 | normalize
phora: source `dotfiles`: deploy = "link" uses the absolute path `<ROOT>/src-dotfiles`, which is not portable across machines
sync complete
```

## Drift — a tampered mapped dest is reported

A clean mapped dest reports `✓ clean`; a manual edit drifts it to `modified`, and
`verify` reports the content mismatch and exits non-zero.

```scrut
$ cd "$ROOT" && mkdir -p drift && cd drift && isolate_state && repo="$(make_git_source dotfiles)" && mkdir -p target-home && cat > phora.toml <<EOF && phora sync 2>&1 | normalize
> version = 1
> [sources.dotfiles]
> path = "$repo"
> branch = "main"
> [targets.home]
> path = "$PWD/target-home"
> layout = "flat"
> sources = [{ source = "dotfiles", map = { "README.md" = "DRIFT.md" } }]
> EOF
sync complete
```

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/DRIFT.md  ✓ clean
```

```scrut
$ printf 'tampered\n' > target-home/DRIFT.md && phora list 2>&1 | normalize
home:
  dotfiles/DRIFT.md  modified
```

```scrut
$ phora verify 2>&1
dotfiles/DRIFT.md: DRIFT.md (content mismatch)
[1]
```
