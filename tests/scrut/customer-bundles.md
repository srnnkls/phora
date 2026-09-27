# Shared capabilities, customer-specific context

A consultancy publishes a support skill alongside each customer's terminology
and escalation procedure. Each target selects the shared skill and its own
customer subtree. These selections control which files are deployed; they do
not restrict access to the source repository.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && BUNDLES="$(make_git_source bundles)" && map_insteadof https://github.com/mock/bundles.git "$BUNDLES"
```

```scrut
$ _phora_write "$BUNDLES/skills/support/SKILL.md" $'Read the customer procedure before replying.\n' && _phora_write "$BUNDLES/customers/acme/terminology.md" $'Acme calls customers members.\n' && _phora_write "$BUNDLES/customers/acme/procedure.md" $'Escalate outages to Acme operations.\n' && _phora_write "$BUNDLES/customers/birch/terminology.md" $'Birch calls customers subscribers.\n' && _phora_write "$BUNDLES/customers/birch/procedure.md" $'Escalate outages to Birch support.\n' && _phora_git -C "$BUNDLES" add -A && _phora_commit '@1700000002 +0000' '@1800000002 +0000' "$BUNDLES" 'customer bundles'
```

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
> [sources.bundles]
> git = "https://github.com/mock/bundles.git"
> branch = "main"
> include = ["skills", "customers"]
> [targets.acme]
> path = "acme"
> layout = "flat"
> [targets.acme.sources.bundles]
> take = [{ "skills/support/" = ".agents/skills/support" }, { "customers/acme/" = "context" }]
> [targets.birch]
> path = "birch"
> layout = "flat"
> [targets.birch.sources.bundles]
> take = [{ "skills/support/" = ".agents/skills/support" }, { "customers/birch/" = "context" }]
> EOF
```

```scrut
$ phora sync
sync complete
```

```scrut
$ cat acme/context/terminology.md acme/context/procedure.md birch/context/terminology.md birch/context/procedure.md
Acme calls customers members.
Escalate outages to Acme operations.
Birch calls customers subscribers.
Escalate outages to Birch support.
```

```scrut
$ find acme birch -type f | LC_ALL=C sort
acme/.agents/skills/support/SKILL.md
acme/context/procedure.md
acme/context/terminology.md
birch/.agents/skills/support/SKILL.md
birch/context/procedure.md
birch/context/terminology.md
```

```scrut
$ cat acme/.agents/skills/support/SKILL.md && cmp acme/.agents/skills/support/SKILL.md birch/.agents/skills/support/SKILL.md
Read the customer procedure before replying.
```

A shared skill update reaches both targets while preserving their separate
customer context.

```scrut
$ cp -R acme/context acme-before && cp -R birch/context birch-before && _phora_write "$BUNDLES/skills/support/SKILL.md" $'Read the customer procedure and cite the incident identifier before replying.\n' && _phora_git -C "$BUNDLES" add -A && _phora_commit '@1700000003 +0000' '@1800000003 +0000' "$BUNDLES" 'support update'
```

```scrut
$ phora update > update.log 2>&1 && cat acme/.agents/skills/support/SKILL.md && cmp acme/.agents/skills/support/SKILL.md birch/.agents/skills/support/SKILL.md && diff -r acme-before acme/context && diff -r birch-before birch/context
Read the customer procedure and cite the incident identifier before replying.
```

```scrut
$ find acme birch -type f | LC_ALL=C sort
acme/.agents/skills/support/SKILL.md
acme/context/procedure.md
acme/context/terminology.md
birch/.agents/skills/support/SKILL.md
birch/context/procedure.md
birch/context/terminology.md
```

```scrut
$ phora verify
all verified
```
