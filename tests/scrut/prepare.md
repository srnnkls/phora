# Prepare, generate, deploy in one sync

The input target and generated-output target share one config, registry and lock.
All destinations live in Scrut's temporary directory.

```scrut
$ source "$TESTDIR/_setup.sh" && isolate_state && make_git_source input >/dev/null
```

```scrut
$ cat > phora.toml <<'TOML'
> [sources.input]
> path = "./src-input"
> include = ["editor/**"]
> [sources.output]
> path = "./generated"
> deploy = "link"
> [hooks]
> post_prepare = "test ! -e fail-build && mkdir -p generated && cp -R stage/editor generated/"
> post_sync = "test -f deployed/editor/init.lua && echo smoke-ok"
> TOML
```

```scrut
$ cat > phora.local.toml <<'TOML'
> [targets.input]
> path = "stage"
> phase = "prepare"
> sources.input = { collapse = false }
> [targets.output]
> path = "deployed"
> sources.output = { collapse = false }
> TOML
```

```scrut
$ phora sync > sync.log 2>&1 && test -f stage/editor/init.lua && test -L deployed/editor/init.lua && cat deployed/editor/init.lua
-- init
```

```scrut
$ sed -n '/^smoke-ok$/p' sync.log
smoke-ok
```

A failed generator stops before output deployment and the smoke hook.

```scrut
$ touch fail-build && phora sync > failed.log 2>&1
[1]
```

```scrut
$ test -z "$(sed -n '/^smoke-ok$/p' failed.log)" && cat deployed/editor/init.lua
-- init
```

```scrut
$ phora sync --frozen --no-hooks > frozen.log 2>&1 && cat deployed/editor/init.lua
-- init
```

```scrut
$ rm fail-build && phora sync --prune > retry.log 2>&1 && phora verify >/dev/null && echo retry-ok
retry-ok
```
