# Phora Selection Model

End-to-end acceptance for the redesigned selection model: the source publishes an
*offer* (`include` − `exclude`), and the consumer binding subsets that offer into
deployed artifacts with a `take` table — literal leaves, rename pairs, and a
`take = []` that deploys nothing. It also exercises the model's guard rails: a
rename whose destination escapes the deploy root is rejected, binding-level scope
(`root`/`include`/`exclude`/`map`) is rejected with a redirect, and the two
offline diagnostics commands — `phora explain` and `phora preview` — attribute the
offer/take decision and surface no-match warnings.

The suite is hermetic: `isolate_state` redirects `HOME` and the XDG cache/state
roots into scrut's per-document tempdir. It bootstraps a git source with the
shared `make_git_source` helper, so its commit (`ca94c83b`) and content digests
are byte-stable. Each binding's config is written with `seed_selection` (source
`dotfiles` plus a `home` target), and output is piped through `normalize`, which
collapses the tempdir prefix to `<ROOT>`.

## Setup

Source the helpers, isolate state, and build the fixture source repo (an
`editor/`, a `lint/`, and a few loose root files).

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && repo="$(make_git_source proj)" && echo ready
ready
```

## A `take` subset deploys only the named leaves

The source offers its whole tree (no `include`). The consumer binding's `take`
keeps one leaf verbatim and renames another; everything else the offer exposes
stays out. This is the offer-then-subset split: the source decides what is *on
offer*, the binding decides what is *taken*.

```scrut
$ reset_deploy && seed_selection "$repo" "" 'take = ["editor/init.lua", { "lint/rules.toml" = "lint/local.toml" }]' && phora sync 2>&1 | normalize
sync complete
```

`phora list` shows exactly the two taken leaves as deployed artifacts — the
renamed leaf appears under its destination name.

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor/init.lua  ✓ clean
  dotfiles/lint/local.toml  ✓ clean
```

Only the taken files land on disk: `editor/init.lua` kept at identity and
`lint/rules.toml` written to its renamed destination `lint/local.toml`. The
offered-but-not-taken leaf (`editor/lua/opts.lua`) never deploys.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && test -f "$PWD/target-home/lint/local.toml" && test ! -e "$PWD/target-home/editor/lua/opts.lua" && test ! -e "$PWD/target-home/lint/rules.toml" && echo subset
subset
```

## `take = []` deploys nothing

An explicit empty `take` is distinct from an omitted one: omitting `take` projects
everything the offer exposes, while `take = []` keeps the offer intact yet selects
no leaf, so the binding deploys no artifacts.

```scrut
$ reset_deploy && seed_selection "$repo" 'include = ["editor", "lint"]' 'take = []' && phora sync 2>&1 | normalize
sync complete
```

`phora list` reports the target as empty.

```scrut
$ phora list 2>&1 | normalize
home:
  (nothing deployed — run `phora sync`)
```

`phora explain` proves the offer is *not* empty — every offered leaf is still
attributed — but each is dropped by the narrowing `take = []`. The offer and the
take are separate decisions: the source still offers `editor`/`lint`, the binding
just takes none of it.

```scrut
$ phora explain home dotfiles 2>&1 | normalize
dotfiles under home
  editor/init.lua: dropped by a narrowing take (not taken)
  editor/lua/opts.lua: dropped by a narrowing take (not taken)
  lint/rules.toml: dropped by a narrowing take (not taken)
```

The target tree is left empty.

```scrut
$ test -z "$(ls -A "$PWD/target-home")" && echo empty
empty
```

## Leaf-granular prune touches only the now-unselected leaf

The offer exposes two distinct leaves and the binding takes both. After narrowing
the `take` to drop one, `phora sync --prune` removes exactly that orphaned artifact
and leaves its sibling in place — the prune is leaf-granular, never wholesale
(RQ-5 / SMR-033).

```scrut
$ reset_deploy && seed_selection "$repo" 'include = ["editor/init.lua", "lint/rules.toml"]' 'take = ["editor/init.lua", "lint/rules.toml"]' && phora sync 2>&1 | normalize
sync complete
```

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
  dotfiles/lint  ✓ clean
```

Narrow the take to keep only `editor/init.lua`, then prune. The now-unselected
`lint` artifact is reported as pruned; the `editor` sibling is untouched.

```scrut
$ seed_selection "$repo" 'include = ["editor/init.lua", "lint/rules.toml"]' 'take = ["editor/init.lua"]' && phora sync --prune 2>&1 | normalize
phora: pruning orphaned dotfiles:lint
sync complete
```

```scrut
$ phora list 2>&1 | normalize
home:
  dotfiles/editor  ✓ clean
```

The pruned leaf's file is gone; the kept sibling remains on disk.

```scrut
$ test -f "$PWD/target-home/editor/init.lua" && test ! -e "$PWD/target-home/lint/rules.toml" && echo pruned-one
pruned-one
```

## A rename whose destination escapes the deploy root is rejected

A `take` rename may not write outside the deploy root. A `../` destination is
rejected with the structured selection diagnostic — `selection:` < `matched
against:` < `remedy:` < `to debug:` — and the kernel take rejection renders the
literal `phora explain <target> <source> <path>` debug placeholder (SMR-063).

```scrut
$ seed_selection "$repo" "" 'take = [{ "editor/init.lua" = "../escape.lua" }]' && phora sync 2>&1 | normalize
error: sync error: selection: ../escape.lua — rename destination is not a portable relative path
matched against: the deploy root
remedy: use a forward-slashed relative path inside the deploy root
to debug: phora explain <target> <source> <path>
```

The same rejection guards every cross-platform escape. An absolute destination is
rejected with a non-zero exit code.

```scrut
$ seed_selection "$repo" "" 'take = [{ "editor/init.lua" = "/abs/escape.lua" }]' && phora sync >/dev/null 2>&1; echo "exit=$?"
exit=1
```

A reserved DOS device name (inert on Unix, a device on Windows) is rejected
identically.

```scrut
$ seed_selection "$repo" "" 'take = [{ "editor/init.lua" = "NUL" }]' && phora sync 2>&1 | normalize
error: sync error: selection: NUL — rename destination is not a portable relative path
matched against: the deploy root
remedy: use a forward-slashed relative path inside the deploy root
to debug: phora explain <target> <source> <path>
```

## Binding-level scope is rejected with a redirect

Scope is owned by the source, not the binding. A binding carrying `root`,
`include`, `exclude`, or `map` is rejected at parse time. The `include`/`exclude`/
`root` redirect points at the source offer; `map` points at the target `take`
table. Each rejection renders the structured diagnostic phrases.

```scrut
$ seed_selection "$repo" "" 'include = ["editor"]' && phora sync 2>&1 | normalize
error: config error: selection: include — binding-level scope is removed
matched against: binding `dotfiles` of target `home`
remedy: move `include` to the source offer on `[sources.dotfiles]`; scope is owned by the source, not the binding
to debug: phora explain home dotfiles
```

A binding-level `map` redirects to the target `take` table instead.

```scrut
$ seed_selection "$repo" "" 'map = { "a/X.md" = "a/x.md" }' && phora sync 2>&1 | normalize
error: config error: selection: map — binding-level scope is removed
matched against: binding `dotfiles` of target `home`
remedy: rename via the target `take` table; binding-level `map` is gone, e.g. `[targets.home.take]`
to debug: phora explain home dotfiles
```

## A non-offered `take` literal is rejected with a suggestion

A `take` may subset the offer but never widen it. A literal naming a leaf the offer
does not expose is rejected — with a `did you mean:` suggesting the nearest offered
leaf, and the kernel debug placeholder.

```scrut
$ seed_selection "$repo" 'include = ["editor", "lint"]' 'take = ["editor/init.lus"]' && phora sync 2>&1 | normalize
error: sync error: selection: editor/init.lus — not present in the offer; `take` may not widen the offer
matched against: the offer set
did you mean: editor/init.lua
remedy: name a leaf the source offers, or add it to the source's include
to debug: phora explain <target> <source> <path>
```

## `phora explain` attributes the offer and the take

With a clean lock in place, `phora explain` is an offline attribution: it names the
include that offered a path and how `take` resolves it. Here the source offers
leaf-pattern includes and the binding renames one leaf, collapses another.

```scrut
$ reset_deploy && seed_selection "$repo" 'include = ["editor/*.lua", "lint/*.toml"]' 'take = ["editor/init.lua", { "lint/rules.toml" = "lint/local.toml" }]' && phora sync 2>&1 | normalize
sync complete
```

A renamed leaf is attributed to its include and shows its `src -> dest` mapping.

```scrut
$ phora explain home dotfiles lint/rules.toml 2>&1 | normalize
dotfiles under home
  offer: `lint/rules.toml` allowed by include `lint/*.toml`
  take: renamed `lint/rules.toml` -> `lint/local.toml`
```

A path outside the offer is reported as such, with a `did you mean:` suggesting the
nearest offered leaf.

```scrut
$ phora explain home dotfiles editor/init.lus 2>&1 | normalize
dotfiles under home
  offer: `editor/init.lus` is outside the offer
  did you mean: editor/init.lua
```

## `phora preview` shows renames, collapsed dirs, and warnings

`phora preview` renders the offline projection from the lock. Here the binding
collapses the wholly-taken `editor/` dir into one artifact, renames a lint leaf,
and carries a no-match glob whose warning suggests the nearest leaf.

```scrut
$ reset_deploy && seed_selection "$repo" 'include = ["editor", "lint"]' 'take = ["editor/**", { "lint/rules.toml" = "lint/local.toml" }, "editor/nonexist*.lua"]' && phora sync >/dev/null 2>&1; phora preview 2>&1 | normalize
home -> <ROOT>/target-home
  dotfiles@ca94c83b editor/ -> <ROOT>/target-home/editor
  dotfiles@ca94c83b lint/rules.toml -> lint/local.toml -> <ROOT>/target-home/lint/local.toml
  warning: dotfiles take `editor/nonexist*.lua` matched no offered leaf
    did you mean: editor/init.lua
```

With `--files` the collapsed dir lists the children it folds in, alongside the
renamed leaf's deployed file; the rename and the warning stay.

```scrut
$ phora preview --files 2>&1 | normalize
home -> <ROOT>/target-home
  dotfiles@ca94c83b editor/ -> <ROOT>/target-home/editor
    init.lua
    lua/opts.lua
  dotfiles@ca94c83b lint/rules.toml -> lint/local.toml -> <ROOT>/target-home/lint/local.toml
    local.toml
  warning: dotfiles take `editor/nonexist*.lua` matched no offered leaf
    did you mean: editor/init.lua
```
