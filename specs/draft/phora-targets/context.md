# Context: Phora Targets Config

## Key Files

| File | Role | Changes Needed |
|------|------|----------------|
| `config.go` | Root config types | Add Target struct, rename Source.Path→Root, add Config.Targets |
| `client.go` | Fetch logic | New cache structure, symlink/copy implementation |
| `lockfile.go` | Lock types | Add TargetDeploy tracking |
| `drift.go` | Drift detection | Extend for symlink validation |
| `internal/cli/sync.go` | CLI sync | Target-driven sync, --target flag |
| `internal/config/config.go` | Internal config | Mirror changes from root |

## Current Architecture

### Two Sync Systems Exist

1. **Legacy Source.Target** (`internal/cli/sync.go`)
   - Simple string field on Source
   - Files copied from cache to target directory

2. **Harness-based** (`internal/sync/sync.go`)
   - Complex harness config with transformations
   - Artifact detection and type handling
   - Per-harness lockfile (`.phora.lock`)

Both replaced by the new target-driven model.

### Current Cache Location

```
~/.local/share/phora/<source>/  # repo-based, not content-addressable
```

Changing to:

```
~/.phora/cache/<source>/<commit>/  # content-addressable
```

### Current Source.Path → Source.Root

`Source.Path` renamed to `Source.Root` to disambiguate from `Target.Path`:
- `Source.Root` = subdirectory within the git repo
- `Target.Path` = destination directory on disk

## Decisions

### Target-Side Mapping (not source-side)

Sources are pure dependency declarations. Targets declare which sources they pull.
Rationale: the common case is N sources → 1 target. Source-side mapping repeats
the target name on every source. Target-side mapping declares the routing once.

### Per-Artifact Symlinks

Individual artifacts (directories/files) are symlinked, not entire source trees.
Allows mixing artifacts from multiple sources in a single target directory.

### Default to Symlink Mode

Read-only by default prevents accidental edits. Disk efficient. Atomic updates
(repoint symlink). Copy mode available for vendor/commit use cases.

### Omitted Sources = All Sources

When `Target.Sources` is nil, the target receives all defined sources.
Covers the simplest single-target config with zero routing boilerplate.

## Native Plan

**Source:** `.claude/plans/linked-swinging-goose.md`
