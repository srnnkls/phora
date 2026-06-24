# One config, filled in per machine

Most config is identical everywhere; a few values are not — your email, a
hostname, a path that differs between the laptop and the box under the desk.
phora renders `*.tmpl` files through [minijinja](https://docs.rs/minijinja)
before they deploy, filling them from a `[vars]` table that `phora.local.toml`
overrides per machine. This is the one inherently *local* story in the suite, so
the source here is a small local repo rather than a clone — but it is a real git
repo, resolved and locked exactly like any other source.

State is hermetic — each machine's block points `HOME` and the XDG cache/state
roots at scrut's per-document tempdir. No commit hashes are asserted (a freshly
built local repo has a machine-dependent commit); the assertions are on rendered
content, `verify`, and a self-comparison of two machines' lock files.

## Start

Build a tiny dotfiles repo: a templated `git/config.tmpl` and a plain
`git/ignore` sibling beside it.

```scrut
$ ROOT="$PWD" && export SRC="$ROOT/dotfiles" && mkdir -p "$SRC/git" && printf 'email = {{ email }}\nname = {{ name }}\n' > "$SRC/git/config.tmpl" && printf '*.log\n.DS_Store\n' > "$SRC/git/ignore" && git -c init.defaultBranch=main init -q "$SRC" && git -C "$SRC" -c user.email=a@b.c -c user.name=t add -A && git -C "$SRC" -c user.email=a@b.c -c user.name=t commit -q -m init && echo built
built
```

The base `[vars]` live in the committed config:

```scrut
$ cd "$ROOT" && mkdir -p laptop && cd laptop && export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && cat > phora.toml <<EOF
> version = 1
>
> [vars]
> email = "base@example.com"
> name = "Base"
>
> [sources.dotfiles]
> path = "$SRC"
> branch = "main"
> include = ["git"]
>
> [targets.home]
> path = "home-config"
> layout = "flat"
> sources = ["dotfiles"]
> EOF
```

## Render and strip

The `.tmpl` is rendered and lands with the suffix gone; the plain sibling copies
byte-for-byte:

```scrut
$ phora sync
sync complete
```

```scrut
$ cat home-config/git/config
email = base@example.com
name = Base
```

```scrut
$ test ! -e home-config/git/config.tmpl && echo stripped
stripped
```

```scrut
$ cat home-config/git/ignore
*.log
.DS_Store
```

`verify` checks the *rendered* bytes — it does not flag the deployed file as
diverging from its template source:

```scrut
$ phora verify
all verified
```

`preview --files` shows the deployed name (suffix stripped) and flags what
renders; the plain sibling carries no annotation:

```scrut
$ phora preview --files 2>&1 | grep -E 'templated|ignore'
    config (templated)
    ignore
```

The `--json` form carries the same per-file `templated` flag:

```scrut
$ phora preview --files --json 2>&1 | grep -E '"path"|"templated"' | sed -e 's/^ *//'
"path": "config",
"templated": true
"path": "ignore",
"templated": false
```

## Two machines, one source

Each machine overrides only the vars it cares about in `phora.local.toml` — the
keys it omits keep their base value. The laptop is Alice's:

```scrut
$ printf 'version = 1\n[vars]\nemail = "alice@laptop"\n' > phora.local.toml && phora sync
sync complete
```

```scrut
$ cat home-config/git/config
email = alice@laptop
name = Base
```

A second machine is Bob's, against the very same source:

```scrut
$ cd "$ROOT" && mkdir -p desktop && cd desktop && export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && cat > phora.toml <<EOF
> version = 1
>
> [vars]
> email = "base@example.com"
> name = "Base"
>
> [sources.dotfiles]
> path = "$SRC"
> branch = "main"
> include = ["git"]
>
> [targets.home]
> path = "home-config"
> layout = "flat"
> sources = ["dotfiles"]
> EOF
```

```scrut
$ printf 'version = 1\n[vars]\nemail = "bob@desktop"\n' > phora.local.toml && phora sync
sync complete
```

```scrut
$ cat home-config/git/config
email = bob@desktop
name = Base
```

They rendered differently — but the lock hashes *source* bytes, not rendered
output, so both machines' locks are byte-identical. The integrity check stays
machine-independent:

```scrut
$ diff "$ROOT/laptop/phora.lock" "$ROOT/desktop/phora.lock" && echo identical
identical
```

## Editing a value re-renders, without churning the lock

Back on the laptop, Alice changes jobs. Editing the var — no source commit moved
— re-renders on the next sync:

```scrut
$ cd "$ROOT/laptop" && export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && COMMIT_BEFORE="$(grep '^commit' phora.lock)" && printf 'version = 1\n[vars]\nemail = "alice@newjob"\n' > phora.local.toml && phora sync
sync complete
```

```scrut
$ cat home-config/git/config
email = alice@newjob
name = Base
```

The lock did not move — no new commit, just a re-render — and `verify` is clean
on the new output:

```scrut
$ test "$(grep '^commit' phora.lock)" = "$COMMIT_BEFORE" && echo lock-unchanged
lock-unchanged
```

```scrut
$ phora verify
all verified
```
