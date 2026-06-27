# Phora Transitive-Hook Inspection

`phora trust` lets a consumer inspect a transitive dep's hooks before approving
them. A candidate with no prior trusted commit lists the dependency-repo-relative
composed files at the candidate commit, resolved offline from the mirror;
`phora trust <source> --show <path>` reads one of those files (or lists a
directory) at the pinned candidate commit, also offline.

The suite is hermetic like `hooks.md`: `isolate_state` redirects `HOME` and the
XDG roots into scrut's per-document tempdir. The dep manifest embeds the leaf's
absolute tempdir path, so the dep's commit hash is machine-dependent; the
candidate short-hash in the listing is folded to `<HASH>`, and `normalize`
collapses the tempdir prefix to `<ROOT>`. The composed-file paths and the
`--show` reads carry no volatile data, so they assert verbatim.

## Setup

Source the helpers, build the leaf and the composing dep, seed the consumer.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && LEAF="$(make_composed_leaf leaf)" && DEP="$(make_composing_dep dep https://github.com/mock/leaf.git)" && map_insteadof https://github.com/mock/leaf.git "$LEAF" && map_insteadof https://github.com/mock/dep.git "$DEP" && seed_config_transitive https://github.com/mock/dep.git && echo ready
ready
```

The first sync records the candidate hook (and its resolved commit) without
trusting it; `--no-transitive-hooks` keeps the run green.

```scrut
$ phora sync --no-transitive-hooks 2>&1 | normalize >/dev/null && echo synced
synced
```

## First trust lists the composed file surface, offline

With the source repos and the deployed surface deleted, the candidate's composed
files still resolve from the cache mirror. A first-trust candidate (no prior
trusted commit) lists the dep-repo-relative files it composes — honoring the
binding's `include = ["nvim"]`, so the leaf's root `phora.toml` never appears.

```scrut
$ rm -rf "$LEAF" "$DEP" "$PWD/target-cfg" && phora trust mydeps --list 2>&1 | normalize | sed -E 's/at [0-9a-f]{7,}:/at <HASH>:/' | grep -A2 'composed files'
  composed files at <HASH>:
    nvim/init.lua
    nvim/lua/opts.lua
```

## `--show` prints a tracked file at the candidate commit

A UTF-8 file is printed verbatim, resolved offline from the mirror.

```scrut
$ phora trust mydeps --show nvim/init.lua 2>&1 | normalize
-- init
```

## `--show` lists a directory's direct entries ls-style

A directory lists its direct children without recursing; a subdirectory carries a
trailing slash.

```scrut
$ phora trust mydeps --show nvim 2>&1 | normalize
init.lua
lua/
```

## `--show` errors clearly for an absent path

```scrut
$ phora trust mydeps --show no/such/path 2>&1
error: source error: no/such/path is absent at * in `mydeps` (glob)
[1]
```

## `--show` without a source refuses rather than guessing

```scrut
$ phora trust --show nvim/init.lua 2>&1
error: config error: `phora trust --show` needs a source name
[1]
```
