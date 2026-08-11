# Phora Transitive Dependencies

A source can be more than a flat bag of artifacts — it can be a phora project in
its own right, shipping a `phora.toml` that pulls in further sources and lays
them out. That is a *transitive dependency*: mark it `transitive = true`, import
it into a target, and phora composes the dependency's own targets into your
workspace. A toolkit repo whose skill expects language guidelines vendored
underneath it can declare the guidelines repo as one of its sources, and a
consumer that imports the toolkit gets the guidelines composed into place
without ever naming them.

A dependency's hooks are a different matter. They are discovered but never run
until you approve them, and `phora trust` is where you inspect one before
deciding: `--list` shows the hook's command, the environment it would inherit,
and the dependency surface around it, while `--show <path>` reads a single file
or lists a directory at the pinned candidate commit. Both resolve offline from
the cache mirror.

The suite is hermetic like `hooks.md`: `isolate_state` redirects `HOME` and the
XDG roots into scrut's per-document tempdir. A transitive source must resolve as
a remote URL rather than a bare local path, so the two fixture repos are reached
through git's `insteadOf` under the isolated `HOME`. The dep manifest embeds the
leaf's absolute tempdir path, so the dep's commit hash is machine-dependent; the
candidate short-hash in the listing is folded to `<HASH>`, the hook identifier is
matched as a glob, and `normalize` collapses the tempdir prefix to `<ROOT>`. The
composed-file paths and the `--show` reads carry no volatile data, so they assert
verbatim.

## Setup

Source the helpers, build the leaf and the composing dep, seed the consumer. The
consumer config names only the dep — the edge to the leaf lives inside the dep's
own manifest.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && ROOT="$PWD" && LEAF="$(make_composed_leaf leaf)" && DEP="$(make_composing_dep dep https://github.com/mock/leaf.git)" && map_insteadof https://github.com/mock/leaf.git "$LEAF" && map_insteadof https://github.com/mock/dep.git "$DEP" && seed_config_transitive https://github.com/mock/dep.git && echo ready
ready
```

## Composing a dependency strips its untrusted hooks

The first sync composes the dependency and records its hook as a candidate
without trusting it. The hook is stripped rather than run, and phora says so on
stderr — the artifacts are deployed, but whatever post-processing the hook would
have done has not happened. A non-interactive run still completes.

```scrut
$ phora sync 2>&1 | normalize
phora: 1 untrusted transitive hook(s) were stripped and not run — affected artifacts are deployed but NOT post-processed and may be incomplete
phora: run `phora trust <name>` to inspect and approve 1 hook(s)
sync complete
```

The leaf's `nvim` subtree landed under the dep's own target path, itself anchored
at the consumer's import target — the consumer never had to know that layout.

```scrut
$ find "$PWD/target-cfg" -type f | sort | normalize
<ROOT>/target-cfg/nvim/nvim/init.lua
<ROOT>/target-cfg/nvim/nvim/lua/opts.lua
```

The dep's hook would have touched a sentinel file. It never ran, so there is
none.

```scrut
$ test -e "$HOME/dep-hook.sentinel" && echo ran || echo stripped
stripped
```

## Verification reports the stripped hook

`phora verify` re-hashes the composed copy against the registry. The bytes match,
but a stripped hook means the artifact may be incomplete, so verify reports it
and exits non-zero until the hook is trusted.

```scrut
$ phora verify 2>&1
mydeps: untrusted stripped hook * — deployed but not post-processed, artifact may be incomplete; run `phora trust mydeps` to approve (glob)
[1]
```

## Trust lists the hook without approving it

`phora trust <source> --list` reports each discovered hook: the command it would
run, and the environment it would inherit. Approval itself is interactive and
records the hook pinned to its command and commit; off a terminal the command
only lists.

```scrut
$ phora trust mydeps --list 2>&1 | normalize | grep -E 'command:|env:|note:'
  command: touch "$HOME/dep-hook.sentinel"
  env: PHORA_TARGET=<composed target path>
  note: the hook inherits the FULL process environment, not only the PHORA_* variables
```

## Skipping dependency hooks keeps a run quiet

`--no-transitive-hooks` composes the dependency but suppresses its hooks
outright — no strip, no notice.

```scrut
$ phora sync --no-transitive-hooks 2>&1 | normalize
sync complete
```

## Cache and registry can be pinned to the project

A `[paths]` table moves phora's git mirrors and its registry under the project
instead of the XDG roots, which keeps a checkout self-contained. A second
consumer, isolated on its own `HOME`, declares them.

```scrut
$ mkdir -p "$ROOT/pinned" && cd "$ROOT/pinned" && isolate_state && map_insteadof https://github.com/mock/leaf.git "$LEAF" && map_insteadof https://github.com/mock/dep.git "$DEP" && printf 'version = 1\n\n[paths]\ncache = ".phora/cache"\nstate = ".phora/state"\n\n[sources.mydeps]\ngit = "https://github.com/mock/dep.git"\ntransitive = true\n\n[targets.dotcfg]\npath = "out"\nimports = ["mydeps"]\n' > phora.toml && echo seeded
seeded
```

```scrut
$ phora sync 2>&1 | normalize
phora: 1 untrusted transitive hook(s) were stripped and not run — affected artifacts are deployed but NOT post-processed and may be incomplete
phora: run `phora trust <name>` to inspect and approve 1 hook(s)
sync complete
```

The mirrors and the registry live under the project, and the XDG roots stayed
empty.

```scrut
$ test -d .phora/cache/git && test -d .phora/state/projects && test ! -e "$XDG_CACHE_HOME/phora" && test ! -e "$XDG_STATE_HOME/phora" && echo pinned
pinned
```

Return to the first consumer for the remaining scenarios.

```scrut
$ cd "$ROOT" && isolate_state && echo restored
restored
```

## First trust lists the composed file surface, offline

The surface a `--list` renders depends on history. With no prior approval — a
first trust — it lists the dependency-repo-relative files the hook will run
against at the candidate commit; once the hook has been trusted at an earlier
commit it renders the file-level diff between that commit and the candidate
instead.

With the source repos and the deployed surface deleted, the candidate's composed
files still resolve from the cache mirror. The listing honors the binding's
`include = ["nvim"]`, so the leaf's root `phora.toml` never appears.

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

The error names the path and the commit it looked at.

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
