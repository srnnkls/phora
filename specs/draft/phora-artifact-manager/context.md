# Context: Phora Artifact Package Manager

## Overview

This specification describes a comprehensive redesign of Phora as a pure git-based artifact package manager implemented in Rust. It consolidates and supersedes several earlier specifications:

- `phora-config-redesign` - config format and lock file
- `phora-targets` - target-driven deployment
- `phora-henia-split` - separation of concerns

## Key Architectural Changes

### Language Migration: Go → Rust

The spec defines Phora as a Rust application. Key motivations:

| Concern | Go (current) | Rust (proposed) |
|---------|--------------|-----------------|
| Git operations | Shell out or go-git | gitoxide (gix) native |
| Reflinks | Manual syscall | reflink crate |
| Glob matching | globset via FFI | globset native |
| Cross-platform | Good | Excellent with explicit handling |

### Cache Model: Checkouts → Exports

Current: `~/.local/share/phora/<source>/` (git worktrees or clones)

Proposed: Two-tier structure with no `.git` in exports:
```
~/.phora/
├── git/<source>.git      # bare mirrors (gitoxide-managed)
└── cache/<source>/<commit>/  # exported snapshots (plain files)
```

### Deployment Model: Symlinks → Reflinks

Current: Per-artifact symlinks to cache
Proposed: Reflink copies (COW) with fallback to regular copies

Benefits:
- Edit-safe: local changes don't corrupt cache
- No symlink complexity on Windows
- Tools that can't follow symlinks work correctly
- Same disk efficiency on supporting filesystems

### State Management: Target Manifests → Global Registry

Current: `.phora.lock` in target directories
Proposed: `~/.phora/state/...` global registry (no files in targets)

Benefits:
- Targets remain "clean" for tools that scan directories
- Single authoritative state location
- Pluggable backend interface for future optimization

## Key Files (Proposed Rust Structure)

| Module | Role |
|--------|------|
| `src/config.rs` | Config, Source, Target structs |
| `src/lock.rs` | Lock, LockedSource, LockedProjection |
| `src/cache.rs` | Cache export from git trees |
| `src/git.rs` | GitBackend wrapping gix |
| `src/project.rs` | Projection logic (reflink/copy) |
| `src/registry.rs` | Registry trait and FileRegistry impl |
| `src/matcher.rs` | PathMatcher for include/exclude |
| `src/sync.rs` | Main sync orchestration |
| `src/cli/*.rs` | Clap command implementations |

## Decisions

### Reflinks over Symlinks

Symlinks have UX issues:
- Tools that don't follow symlinks break
- Edits through symlinks modify cache
- Windows requires developer mode or admin
- ls -la shows link targets, not contents

Reflinks provide:
- COW semantics (edit-safe)
- No symlink resolution needed
- Same inode efficiency on APFS/Btrfs/XFS
- Graceful fallback to copy on unsupported FS

### Global Registry over Target Manifests

Writing `.phora-*` files into targets causes problems:
- Pollutes tool-scanned directories (IDE, linters)
- Conflicts with user's own gitignore
- Multi-machine sync requires careful handling

Global registry at `~/.phora/state/...`:
- Single source of truth
- Target directories remain pristine
- Easy backup/restore of phora state

### Artifact-Level Granularity

An artifact is a directory that is a direct child of root.
Not files. Not nested directories. This provides:
- Clear mental model
- Natural collision detection
- Efficient projection (whole directories)

### Two-Phase Pattern Matching

Patterns are classified as artifact-level or path-level:
- `editor` → matches artifact name only
- `**/test/**` → matches paths within artifacts

This allows intuitive filtering:
- `include = ["editor", "lint"]` → specific artifacts
- `exclude = ["**/test/**"]` → remove test dirs from all

## Migration Considerations

### Breaking Change

This is a full rewrite. No migration path from current Go implementation. Users would:
1. Install new Rust-based phora
2. Re-run `phora sync` to establish new cache/registry

### Interoperability Period

During transition, both tools could coexist:
- `phora` (Go) → legacy
- `phora2` or `phora-rs` → new implementation
- Eventually rename and deprecate

## Dependencies

Core dependencies chosen for the Rust implementation:

| Crate | Purpose |
|-------|---------|
| gix | Git operations (no shell-out) |
| reflink | COW file copies |
| globset | Pattern matching |
| walkdir | Directory traversal |
| blake3 | Fast hashing for digests |
| serde + toml | Config/lock serialization |
| clap | CLI argument parsing |
| thiserror | Error handling |
| chrono | Timestamps |
| dirs | Home directory resolution |
| fs2 | File locking |

## Open Questions

1. **Concurrent sync**: Should multiple `phora sync` processes be allowed? Current design uses exclusive lock.

2. **Partial failures**: If one artifact fails to project, should others continue? Spec implies yes with warnings.

3. **Cache eviction**: `phora clean` policy TBD. Spec suggests 30-day default for unreferenced snapshots.

4. **Digest algorithm**: Spec mentions blake3 in deps but shows sha256 in examples. Need to standardize.
