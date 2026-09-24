# Build a source from pinned inputs

A build source runs its command over materialized inputs and deploys the committed output.
All destinations live in Scrut's temporary directory.

```scrut
$ source "$TESTDIR/_setup.sh" && isolate_state && make_git_source input >/dev/null && echo 1 > version
```

```scrut
$ cat > phora.toml <<'TOML'
> [sources.input]
> path = "./src-input"
> include = ["editor/**"]
> [sources.output]
> build = { inputs = ["input"], run = "test ! -e fail-build && cp -R \"$PHORA_INPUT/input/editor\" \"$PHORA_OUTPUT/\"", key = "cat version" }
> [hooks]
> post_sync = "test -f deployed/editor/init.lua && echo smoke-ok"
> [targets.output]
> path = "deployed"
> sources.output = { collapse = false }
> TOML
```

```scrut
$ phora sync > sync.log 2>&1 && test ! -L deployed/editor/init.lua && cat deployed/editor/init.lua
-- init
```

```scrut
$ sed -n '/^smoke-ok$/p' sync.log
smoke-ok
```

A failed rebuild keeps the previous output deployed and exits non-zero.

```scrut
$ touch fail-build && echo 2 > version && phora sync > failed.log 2>&1
[1]
```

```scrut
$ grep -c 'build `output` failed' failed.log && cat deployed/editor/init.lua
1
-- init
```

`--frozen` never runs a build, so it refuses while the key is stale.

```scrut
$ phora sync --frozen > frozen.log 2>&1
[1]
```

```scrut
$ rm fail-build && phora sync > retry.log 2>&1 && phora sync --frozen > frozen.log 2>&1 && phora verify >/dev/null && echo retry-ok
retry-ok
```
