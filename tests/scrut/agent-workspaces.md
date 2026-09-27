# Recreate an agent workspace from its lock

A team publishes a review skill, an engineering policy, and an API reference.
Each workspace selects those files and puts them where its agent expects them.
The source repository and all consumer state live in this document's temporary
directory; Git URL rewriting keeps every fetch local.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && TEAM="$(make_git_source team)" && map_insteadof https://github.com/mock/team.git "$TEAM"
```

```scrut
$ _phora_write "$TEAM/skills/review/SKILL.md" $'Review error handling before approving.\n' && _phora_write "$TEAM/skills/deploy/SKILL.md" $'Deploy only after release approval.\n' && _phora_write "$TEAM/policy/AGENTS.md" $'Run the focused checks before proposing a change.\n' && _phora_write "$TEAM/references/api.md" $'GET /orders returns a list of orders.\n' && _phora_git -C "$TEAM" add -A && _phora_commit '@1700000002 +0000' '@1800000002 +0000' "$TEAM" 'team context'
```

```scrut
$ mkdir first second && cd first
```

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
> [sources.team]
> git = "https://github.com/mock/team.git"
> branch = "main"
> include = ["skills", "policy", "references"]
> [targets.workspace]
> path = "workspace"
> layout = "flat"
> [targets.workspace.sources.team]
> take = [{ "skills/review/" = ".agents/skills/review" }, { "policy/AGENTS.md" = "AGENTS.md" }, { "references/api.md" = "reference/api.md" }]
> EOF
```

```scrut
$ phora sync
sync complete
```

```scrut
$ cat workspace/.agents/skills/review/SKILL.md workspace/AGENTS.md workspace/reference/api.md
Review error handling before approving.
Run the focused checks before proposing a change.
GET /orders returns a list of orders.
```

```scrut
$ test ! -e workspace/.agents/skills/deploy && test ! -e workspace/skills && echo selected-only
selected-only
```

The upstream skill changes after the first workspace was created. A fresh
consumer receives the same manifest and lock, so its frozen sync must reproduce
the earlier files rather than follow the advancing branch.

```scrut
$ _phora_write "$TEAM/skills/review/SKILL.md" $'Review error handling and authorization before approving.\n' && _phora_git -C "$TEAM" add -A && _phora_commit '@1700000003 +0000' '@1800000003 +0000' "$TEAM" 'authorization review'
```

```scrut
$ cp phora.toml phora.lock ../second/ && cp phora.lock original.lock && cd ../second && phora sync --frozen
sync complete
```

```scrut
$ cmp phora.lock ../first/original.lock && cmp workspace/.agents/skills/review/SKILL.md ../first/workspace/.agents/skills/review/SKILL.md && cmp workspace/AGENTS.md ../first/workspace/AGENTS.md && cmp workspace/reference/api.md ../first/workspace/reference/api.md && echo reproduced
reproduced
```

An edit to the deployed policy is visible to verification. Forced sync restores
the recorded policy without advancing the dependency.

```scrut
$ printf 'Skip all checks.' > workspace/AGENTS.md
```

```scrut
$ phora verify 2>&1
team/AGENTS.md: AGENTS.md (content mismatch)
[1]
```

```scrut
$ phora sync --force
sync complete
```

```scrut
$ cmp workspace/AGENTS.md ../first/workspace/AGENTS.md && phora verify
all verified
```

Updating the first consumer advances its skill. The second consumer's lock and
skill remain unchanged, including after another frozen sync.

```scrut
$ cd ../first && phora update team > update.log 2>&1 && cat workspace/.agents/skills/review/SKILL.md
Review error handling and authorization before approving.
```

```scrut
$ cd ../second && phora sync --frozen && cmp phora.lock ../first/original.lock && cat workspace/.agents/skills/review/SKILL.md
sync complete
Review error handling before approving.
```
