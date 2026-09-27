# Collect an agent's knowledge from two teams

An agent working on the billing service needs the platform team's API
documentation and examples, plus the operations team's incident runbook. Phora
takes the relevant files from each repository, places them in one directory
and combines the documentation and runbook with a hook.

The example Git URLs resolve to local repositories. All files and Phora state
stay in this walkthrough's temporary directory.

## Collect the files

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && PLATFORM="$(make_git_source platform)" && OPS="$(make_git_source ops)" && map_insteadof https://github.com/mock/platform.git "$PLATFORM" && map_insteadof https://github.com/mock/ops.git "$OPS"
```

The platform release contains an unfinished internal document next to the API
documentation.

```scrut
$ _phora_write "$PLATFORM/docs/billing/api.md" $'# Billing API\n' && _phora_write "$PLATFORM/docs/billing/examples/charge.sh" $'curl -X POST /charges\n' && _phora_write "$PLATFORM/docs/billing/internal.md" $'draft\n' && _phora_git -C "$PLATFORM" add -A && _phora_commit '@1700000002 +0000' '@1800000002 +0000' "$PLATFORM" 'billing docs' && _phora_git -C "$PLATFORM" tag v2.3.0
```

```scrut
$ _phora_write "$OPS/runbooks/billing/incident.md" $'Page the billing on-call.\n' && _phora_git -C "$OPS" add -A && _phora_commit '@1700000002 +0000' '@1800000002 +0000' "$OPS" 'billing runbook'
```

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
> [sources.api]
> git = "https://github.com/mock/platform.git"
> tag = "v2.3.0"
> root = "docs/billing"
> [sources.operations]
> git = "https://github.com/mock/ops.git"
> branch = "main"
> root = "runbooks/billing"
> [targets.knowledge]
> path = "resources/knowledge"
> [targets.knowledge.sources]
> api = { take = ["api.md", "examples/**"] }
> operations = { take = [{ "incident.md" = "runbook.md" }] }
> [targets.knowledge.hooks]
> on_change = "cat resources/knowledge/api.md resources/knowledge/runbook.md > resources/knowledge.txt"
> EOF
```

```scrut
$ phora sync 2>&1
hook knowledge#cat resources/knowledge/api.md resources/knowledge/runbook.md > resources/knowledge.txt#sh -c [on_change] `cat resources/knowledge/api.md resources/knowledge/runbook.md > resources/knowledge.txt` ok
sync complete
```

The directory holds the selected documentation, the examples and the renamed
runbook. The internal document stays behind.

```scrut
$ find resources | sort
resources
resources/knowledge
resources/knowledge.txt
resources/knowledge/api.md
resources/knowledge/examples
resources/knowledge/examples/charge.sh
resources/knowledge/runbook.md
```

```scrut
$ cat resources/knowledge.txt
# Billing API
Page the billing on-call.
```

## Adopt a newer runbook

The operations team extends the runbook. Updating that source redeploys the
runbook and the hook rebuilds the combined file. The API documentation stays
at its tag.

```scrut
$ _phora_write "$OPS/runbooks/billing/incident.md" $'Page the billing on-call, then open an incident channel.\n' && _phora_git -C "$OPS" add -A && _phora_commit '@1700000003 +0000' '@1800000003 +0000' "$OPS" 'escalation step'
```

```scrut
$ phora update operations 2>&1
hook knowledge#cat resources/knowledge/api.md resources/knowledge/runbook.md > resources/knowledge.txt#sh -c [on_change] `cat resources/knowledge/api.md resources/knowledge/runbook.md > resources/knowledge.txt` ok
sync complete
```

```scrut
$ cat resources/knowledge.txt && phora verify
# Billing API
Page the billing on-call, then open an incident channel.
all verified
```
