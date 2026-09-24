# Architecture

Three capability modules carry the work: `source`, `projection` and `sync`. `cli` sits on top and drives them. `config` parses `phora.toml` at the edge; `sync` converts it into value types before projection sees it. `lock`, `paths`, `digest`, `diagnostic` and `error` are shared support modules.

Configuration keys, flags and exit codes are in [REFERENCE.md](../REFERENCE.md). The user-facing model is in [GUIDE.md](../GUIDE.md).

## The source → projection → sync decision

The architectural decision is the capability chain source → projection → sync. It decides where every responsibility lives.

```mermaid
flowchart LR
    source --> projection
    projection --> sync
```

- *source* obtains immutable content. Given a git remote, a URL or a local working tree, it resolves a snapshot and exposes an inventory of entries and their bytes. It performs source and cache I/O.
- *projection* computes the desired target structure from source values and inventories: a `Projection` of artifacts and their leaves. It performs no I/O.
- *sync* owns target-side machine state. It observes the disk, reconciles it against the projection, stages and applies changes, journals them for recovery, and owns registry records, ejections, hook state and locking.

Link-mode artifacts are the exception to snapshot immutability: link artifacts materialize as symlinks into the live worktree, while `SnapshotId::Worktree` freezes inventory and copy-mode reads.

Data flows one way, from source to projection to sync. Imports follow the same direction: sync imports projection and source, projection imports source's pure value types, and an upstream module imports nothing downstream.

## Module placement rule

A change belongs to the module whose responsibility it matches:

- source obtains immutable content: it reads and fetches origin bytes into a snapshot and never mutates them.
- projection is a pure, I/O-free calculation: it computes a deterministic `Projection` of the desired structure from values and inventories alone.
- sync is the sole owner of target machine state: observation, reconciliation, staging, application, recovery, state records and locking.

Work that needs the filesystem or the network to decide the target shape does not go in projection. Work that inspects or changes the machine goes in sync. Work that acquires origin content goes in source. Projection owns `TargetName` and `ArtifactName`; source owns `SourceName` and `Commit`.

## Invariants

`scripts/arch-check.sh` lints the imports and I/O calls in `src/`. It runs through `tests/arch_check.rs` and in CI. Module privacy covers the rest.

### INV-1: projection purity

Projection performs no I/O; it is a pure calculation of the desired target structure.

The lint gives `src/projection/` a positive allowlist. A projection file may import:

- its own modules (`self`, `super`, `crate::projection`);
- `crate::error` and `crate::diagnostic`;
- from `crate::source`, only `SourcePath`, `SourceInventory`, `SourceEntryMeta`, `SourceEntryKind`, `SourceName`, `Commit`, `safe_component` and `safe_relpath`;
- `std`, except `std::fs`, `std::io`, `std::net`, `std::os` and `std::process`;
- the external crates `globset` and `unicode_normalization`.

Any other import fails the lint, including `config`, `sync`, `cli`, `gix` and `serde`. The lint also checks fully qualified `crate::` paths and `std::fs::`-style calls outside `use` lines.

### INV-2: source boundary

Source never imports projection, sync or target types. A file under `src/source/` may not import `crate::projection`, `crate::sync`, `crate::config::target`, or `TemplateOptIn`, and the lint applies the same rule to fully qualified paths. `digest_snapshot` takes an explicit list of leaves, so computing a digest needs no projection type.

### INV-3 to INV-9

- INV-3: target-side writes happen in `sync` and `cli`, with one exception. `source::worktree_deploy` writes the history overlay's `.git` gitlink, and the placeholder directories for submodule entries, under the deploy root. It runs only when sync calls it, under the per-mirror lock. Outside that, source performs source and cache I/O, and projection performs none. The lint allows filesystem, process and network APIs only in `sync`, `cli`, `main.rs` and the source I/O owners listed in `SOURCE_IO_OWNERS`.
- INV-4: serialized formats stay compatible. `phora.lock`, registry records, target metadata and the journal keep schema version 1. A field added later is optional: it reads as its default when absent and is left out on write when unset or empty. Older files still parse, and a file that doesn't use a feature serializes as it did before the feature existed. `tests/compat_serialized.rs` pins the formats against goldens in `tests/compat/serialized/`.
- INV-5: staging is deterministic. The same snapshot, selection, vars and export policy produce the same bytes, modes, mtimes, manifest and digests.
- INV-6: URL sources resolve to the same snapshot representation as git sources, `SnapshotId::Git` over a synthetic commit. Working-tree snapshots import the captured tree the same way, so inventory and reads see one frozen tree.
- INV-7: `preview`, `sync` and prune compute artifact identities from the same projection code (`sync/plan.rs`).
- INV-8: `sync::reconcile` is a pure function of `(Projection, ObservedProjectState, policy)`. The lint forbids filesystem APIs and `Path` filesystem methods in `sync/reconcile.rs`.
- INV-9: human-readable CLI output and exit codes are a compatibility surface. `cli/render.rs` produces the output, `cli::exit_code` maps errors to codes, and only `main.rs` calls `std::process::exit`.
