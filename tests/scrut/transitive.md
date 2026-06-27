# A dependency that carries its own

A source can be more than a flat bag of artifacts — it can be a phora project in
its own right, shipping a `phora.toml` that pulls in further sources and lays them
out. This is a *transitive dependency*: mark it `transitive = true`, import it into
a target, and phora composes its targets into your workspace.

The worked case mirrors a real pair of repos.
[`srnnkls/tropos`](https://github.com/srnnkls/tropos) is a toolkit of agent-harness
artifacts; its `loqui` skill expects language guidelines vendored underneath it at
`skills/loqui/reference/loqui/`, and those guidelines live in a separate repo,
[`srnnkls/loqui`](https://github.com/srnnkls/loqui). tropos declares loqui as one of
its own sources, so importing tropos composes loqui straight into the skill.

This suite stays offline and hermetic. The two upstreams are built as local git
repos, and the dependency's inner remote — which phora resolves as a remote URL,
not a local filesystem path — is redirected to the local repo with git's
`insteadOf`. `HOME` points at scrut's per-document tempdir so that redirect is the
only project-level git config active; phora's own cache and state are pinned
with a `[paths]` table in `phora.toml`, so the run needs no `XDG_*` juggling and
nothing lands outside the tempdir. No commit hashes are asserted — a freshly built
local repo has a machine-dependent commit.

## Start

`HOME` is the tempdir, and a single `.gitconfig` carries both the commit identity
and the two `insteadOf` redirects that point the forge URLs at the local repos:

```scrut
$ export HOME="$PWD" && ROOT="$PWD" && echo ready
ready
```

```scrut
$ cat > "$HOME/.gitconfig" <<EOF
> [init]
> 	defaultBranch = main
> [user]
> 	email = a@b.c
> 	name = t
> [commit]
> 	gpgsign = false
> [url "$ROOT/tropos"]
> 	insteadOf = https://github.com/srnnkls/tropos.git
> [url "$ROOT/loqui"]
> 	insteadOf = https://github.com/srnnkls/loqui.git
> EOF
```

The loqui repo is the leaf: two language guideline trees and a shared resource.

```scrut
$ mkdir -p "$ROOT/loqui/languages/go" "$ROOT/loqui/languages/python" "$ROOT/loqui/resources" && printf 'go style\n' > "$ROOT/loqui/languages/go/style.md" && printf 'python style\n' > "$ROOT/loqui/languages/python/style.md" && printf 'shared\n' > "$ROOT/loqui/resources/shared.md" && printf 'version = 1\n' > "$ROOT/loqui/phora.toml" && git -C "$ROOT/loqui" init -q && git -C "$ROOT/loqui" add -A && git -C "$ROOT/loqui" commit -q -m loqui && echo built
built
```

The tropos repo is the dependency. Its `phora.toml` declares loqui as a source and
lands it under the skill that needs it — and carries an `on_change` hook to
post-process what was composed.

```scrut
$ mkdir -p "$ROOT/tropos" && cat > "$ROOT/tropos/phora.toml" <<'EOF'
> version = 1
>
> [sources.loqui]
> host = "github"
> repo = "srnnkls/loqui"
>
> [targets.loqui]
> path = "skills/loqui/reference/loqui"
> sources = ["loqui"]
>
> [targets.loqui.hooks]
> on_change = "echo composed >> \"$HOME/loqui-built.log\""
> EOF
```

```scrut
$ git -C "$ROOT/tropos" init -q && git -C "$ROOT/tropos" add -A && git -C "$ROOT/tropos" commit -q -m tropos && echo built
built
```

## Import and compose

The consumer pins phora's cache and state with `[paths]`, declares tropos as a
transitive source, and imports it into a target. The consumer config never mentions
loqui — that edge lives inside tropos.

```scrut
$ mkdir -p "$ROOT/proj" && cd "$ROOT/proj" && cat > phora.toml <<'EOF'
> version = 1
>
> [paths]
> cache = ".phora/cache"
> state = ".phora/state"
>
> [sources.tropos]
> host = "github"
> repo = "srnnkls/tropos"
> branch = "main"
> transitive = true
>
> [targets.claude]
> path = "claude"
> imports = ["tropos"]
> EOF
```

The first sync composes the dependency. Its hook is *stripped* — discovered but not
run, since a dependency's hooks need explicit approval — so the sync reports it and
still completes (a non-interactive run stays green; the files are deployed):

```scrut
$ phora sync 2>&1
phora: 1 untrusted transitive hook(s) were stripped and not run — affected artifacts are deployed but NOT post-processed and may be incomplete
phora: run `phora trust <name>` to inspect and approve 1 hook(s)
sync complete
```

loqui's `languages/` and `resources/` trees landed at the path the skill expects —
under the dependency's relative path, composed beneath the import anchor:

```scrut
$ test -f claude/skills/loqui/reference/loqui/languages/go/style.md && test -f claude/skills/loqui/reference/loqui/resources/shared.md && echo composed
composed
```

The hook never ran, so its log was never written:

```scrut
$ test ! -e "$HOME/loqui-built.log" && echo "hook stripped"
hook stripped
```

The `[paths]` table did its job: the git mirrors and the registry live under the
project, and nothing was written to the default `~/.cache` or `~/.local/state`:

```scrut
$ test -d .phora/cache/git && test -d .phora/state/projects && test ! -e "$HOME/.cache/phora" && test ! -e "$HOME/.local/state/phora" && echo isolated
isolated
```

`phora verify` re-hashes the composed copy against the registry. The bytes match, but
the dependency's hook was *stripped* rather than run, so the composed artifact may be
incomplete — verify says so and exits non-zero until the hook is trusted (the
machine-dependent hook id is matched as a glob):

```scrut
$ phora verify 2>&1
tropos: untrusted stripped hook * — deployed but not post-processed, artifact may be incomplete; run `phora trust tropos` to approve (glob)
[1]
```

## Inspecting an untrusted hook

`phora trust <source> --list` shows each discovered hook — its command, the
environment it would inherit, and the dependency surface around it — without approving
anything. Approval itself is interactive and records the hook, pinned to its command
and commit, in your `phora.lock`; off a terminal the command only lists.

```scrut
$ phora trust tropos --list 2>&1 | grep -E 'command:|env:|note:'
  command: echo composed >> "$HOME/loqui-built.log"
  env: PHORA_TARGET=<composed target path>
  note: the hook inherits the FULL process environment, not only the PHORA_* variables
```

The surface depends on history. With no prior approval — a first trust — `--list`
lists the dependency-repo-relative files the hook will run against at the candidate
commit, resolved offline from the mirror; once you have trusted the hook at an earlier
commit it renders the file-level diff between that commit and the candidate instead.
This is a first trust, so the listing is the composed surface (the candidate commit is
folded to `<HASH>`):

```scrut
$ phora trust tropos --list 2>&1 | sed -E 's/at [0-9a-f]{7,}:/at <HASH>:/' | grep -A4 'composed files'
  composed files at <HASH>:
    languages/go/style.md
    languages/python/style.md
    phora.toml
    resources/shared.md
```

## Reading the dependency tree with `--show`

`phora trust <source> --show <path>` prints a dependency file — or lists a directory —
at the pinned candidate commit, offline, so you can read a hook's surroundings before
approving it. A UTF-8 file prints verbatim:

```scrut
$ phora trust tropos --show languages/go/style.md 2>&1
go style
```

A directory lists its direct entries ls-style, with subdirectories slash-suffixed:

```scrut
$ phora trust tropos --show languages 2>&1
go/
python/
```

An absent path errors, naming the path and the commit it looked at:

```scrut
$ phora trust tropos --show no/such/path 2>&1
error: source error: no/such/path is absent at * in `tropos` (glob)
[1]
```

And `--show` refuses to guess without a source name:

```scrut
$ phora trust --show languages/go/style.md 2>&1
error: config error: `phora trust --show` needs a source name
[1]
```

## Skipping dependency hooks

`--no-transitive-hooks` composes the dependency but suppresses its hooks entirely —
no strip, no notice, just a clean sync:

```scrut
$ phora sync --no-transitive-hooks 2>&1
sync complete
```
