# Recreate an agent workspace from its lock

A team shares a review skill, engineering instructions and an API reference.
Each workspace needs the same versions until its owner chooses to update them.
This walkthrough creates two workspaces, changes the shared files and shows how
each workspace keeps or updates its chosen version.

All repositories, deployed files and Phora state live in a temporary directory.
The example Git URLs resolve to local repositories.

## Prepare the first workspace

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

## Recreate the workspace after the source changes

The team adds authorization checks to its review skill. Copy the first
workspace's configuration and lockfile into a second project. The original
version is already cached, so `phora sync --frozen` can deploy it there.

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

## Detect and restore a local edit

Edit the second workspace's `AGENTS.md`, then run `phora verify` to see the
mismatch. `phora sync --force` restores the version recorded in the lockfile.

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

## Update one workspace

Run `phora update team` in the first project to adopt the revised review skill.
The second project keeps its original lockfile and skill, including after
another frozen sync.

```scrut
$ cd ../first && phora update team > update.log 2>&1 && cat workspace/.agents/skills/review/SKILL.md
Review error handling and authorization before approving.
```

```scrut
$ cd ../second && phora sync --frozen && cmp phora.lock ../first/original.lock && cat workspace/.agents/skills/review/SKILL.md
sync complete
Review error handling before approving.
```
