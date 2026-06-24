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
repos, and the dependency's inner remote — which phora resolves against your host
registry, and which must be a real remote, never a local path — is redirected to the
local repo with git's `insteadOf`. `HOME` points at scrut's per-document tempdir so
that redirect is the only git config in play; phora's own cache and state are pinned
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
lands it under the skill that needs it — and carries an `on_change` hook, the kind a
dependency author might use to post-process what was just composed.

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

The first sync composes the dependency. Its hook is *stripped* — discovered, but not
run, because phora never trusts a dependency's hooks implicitly — so the sync says so
and still completes (a non-interactive run stays green; the files are deployed):

```scrut
$ phora sync 2>&1
phora: 1 untrusted transitive hook(s) were stripped and not run — affected artifacts are deployed but NOT post-processed and may be incomplete
phora: run `phora trust <name>` to inspect and approve 1 hook(s)
sync complete
```

loqui's `languages/` and `resources/` trees landed exactly where the skill looks for
them — under the dependency's relative path, composed beneath the import anchor:

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

`phora verify` re-hashes the composed copy against the registry:

```scrut
$ phora verify
all verified
```

## Inspecting an untrusted hook

`phora trust <source> --list` shows each discovered hook — its command, the
environment it would inherit, and (once a prior approval exists) the dependency files
that changed since you last trusted it — without approving anything. Approval itself
is interactive and records the hook, pinned to its command and commit, in your
`phora.lock`; off a terminal the command only lists.

```scrut
$ phora trust tropos --list 2>&1 | grep -E 'command:|env:|note:'
  command: echo composed >> "$HOME/loqui-built.log"
  env: PHORA_TARGET=<composed target path>
  note: the hook inherits the FULL process environment, not only the PHORA_* variables
```

## Skipping dependency hooks

`--no-transitive-hooks` composes the dependency but suppresses its hooks entirely —
no strip, no notice, just a clean sync:

```scrut
$ phora sync --no-transitive-hooks 2>&1
sync complete
```
