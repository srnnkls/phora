# History overlay

A history binding remains a content-verified copy deployment while exposing its
pinned Git history in the deployed directory.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && source="$(make_git_source history)" && target="$PWD/target-home" && mkdir -p "$target" && printf 'version = 1\n\n[sources.history]\npath = "%s"\nbranch = "main"\n\n[targets.home]\npath = "%s"\nlayout = "flat"\n\n[targets.home.sources.history]\nhistory = true\n' "$source" "$target" > phora.toml && phora sync 2>&1 | normalize
sync complete
```

The deployed artifact is shown as history-enabled and clean.

```scrut
$ phora list 2>&1 | normalize
home:
  history/history  history, ✓ clean
```

```scrut
$ phora verify 2>&1 | normalize
all verified
```

Preview identifies the current history deployment.

```scrut
$ phora preview 2>&1 | normalize
home -> <ROOT>/target-home
  history@ca94c83b history -> <ROOT>/target-home/history
```
