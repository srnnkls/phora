# Architecture

Phora is organized around three capability modules — *source*, *projection*, and
*sync*. `cli` sits at the top of the stack and drives them; `lock`, `paths`,
`digest`, `diagnostic`, and `error` are shared support modules. `config` is
parsed at the edge and consumed at the sync boundary — after PR3 it reaches
projection only as converted spec values, never as a raw DTO. The physical
module layout matches the real data flow: content is obtained, a desired target
structure is calculated, and machine state is reconciled to match it.

## The source → projection → sync decision

The crate's capability chain is **source → projection → sync**, and this is the
architectural decision that governs where every responsibility lives.

- *source* obtains immutable content. Given a Git, HTTP, or worktree origin it
  resolves a snapshot and exposes an inventory of entries and their bytes. It
  performs source and cache I/O only.
- *projection* is a pure, I/O-free calculation. It maps source values and
  inventories to the desired target structure — a `Projection` of projected
  artifacts and leaves — and performs no I/O of any kind.
- *sync* is the sole owner of target-side machine state. It observes what is on
  disk, reconciles it against the projection, stages and applies changes,
  journals for recovery, and owns state records, ejections, hooks, and locking.

Data flows one way: source feeds projection, projection feeds sync. Imports run
the opposite way: a downstream capability may import an upstream one — sync
imports projection and source, projection imports source's pure value types —
but an upstream capability never imports a downstream one.

## Module placement rule

Each responsibility is placed by which capability owns it. A change belongs to
the module whose sentence below it satisfies:

- source obtains immutable content — it reads and fetches origin bytes into a
  snapshot, and never mutates them.
- projection is a pure, I/O-free calculation of the desired target structure — it
  computes a deterministic `Projection` from values and inventories alone.
- sync is the sole owner of the target machine state — it owns observation,
  reconciliation, staging, application, recovery, state records, and locking.

If a piece of work needs the filesystem or the network to decide the target
shape, it does not belong in projection. If it inspects or mutates the machine,
it belongs in sync. If it acquires origin content, it belongs in source.
Projection's allowance to import the pure kernel identities it consumes
(`TargetName`/`ArtifactName`/`SourceName`/`Commit`) is phase-scoped: it expires
at T029, when the kernel module dissolves — `TargetName`/`ArtifactName` become
projection-owned and `SourceName`/`Commit` move to `source::model`.

## Invariants

The placement rule is enforced by a small `rg` arch-lint (`scripts/arch-check.sh`,
in CI from PR1) plus module privacy. The two structural invariants below are the
load-bearing boundaries; the remaining invariants are summarized after them, with
design.md holding the authoritative specification.

### INV-1 — projection purity

Projection performs no I/O; it is a pure, I/O-free calculation of the desired
target structure. The projection module imports no `config` DTO, no source I/O
(`SourceStore`, `SnapshotId`, resolve, or fetch), and none of `sync`, `cli`,
`std::fs`, `std::process`, network libraries, `gix`, `serde`, or `chrono`. It may
import only the pure source value types (`SourcePath`, `SourceInventory`,
`SourceEntryMeta`, `SourceEntryKind`), the pure kernel identities it consumes,
the pure external crates it genuinely needs, and its own modules. A positive
allowlist lint rejects any other import. The kernel-identity allowance is
phase-scoped and expires at T029, when the kernel module dissolves.

### INV-2 — source boundary

Source never imports projection, target, manifest, or registry types. It also
does not reference `sync`, `sync::state`, target paths, or template deployment
policy. The boundary is enforced as three independently dated clauses tied to
expiry tasks: target-type removal (PR5), staging and manifest removal (PR6), and
source-transitive extraction (PR12). `digest_snapshot` takes explicit leaves, not
an `OfferSelection`, so source never depends on a projection type to compute a
digest.

### INV-3 through INV-10

design.md is the authoritative specification for these invariants; each line
below is the durable one-sentence record.

- INV-3 — only `sync` and `cli` perform target-side I/O; source performs only
  source/cache I/O. The legacy `deploy.rs`/`store.rs` allowlist only shrinks and
  reaches strict at PR10.
- INV-4 — serialized formats (lock, registry, journal, config, cache/mirror
  layout) stay byte-identical throughout; no schema or version bump.
- INV-5 — artifact, source, variable, and file digests, modes, mtimes,
  manifests, symlink rejection, and template errors are unchanged when export
  moves out of source.
- INV-6 — URL sources resolve to the same Git snapshot representation as Git
  sources with unchanged synthetic commit IDs; worktree snapshots freeze source
  content so inventory→read is race-free.
- INV-7 — previewed projected artifacts equal the artifacts sync manages equal
  the artifact keys prune protects.
- INV-8 — reconciliation is a pure function of `(Projection,
  ObservedProjectState, policy)`; no filesystem access after
  `ObservedProjectState` is constructed.
- INV-9 — CLI human-readable output and exit codes are unchanged; only `main`
  calls `std::process::exit`.
- INV-10 — git blame is preserved across file relocations, via `git mv` before
  edits and whole-file moves with `git log --follow` spot-checks.

## DoD → task → test traceability

Each Definition-of-Done item from refactor-plan §16 maps to the task that
delivers it and a test that pins the surface. Where an item's dedicated pinning
test lands in a later PR, the cited file is the existing on-disk test that
currently covers that surface; task T031 re-verifies and finalizes this table at
the end of the migration.

| DoD | Requirement (plan §16) | Task | Pinning test |
| --- | --- | --- | --- |
| 1 | source, projection, and sync are the three capability modules | T006 | tests/golden.rs |
| 2 | no top-level backend, deploy, store, or kernel | T030 | tests/golden.rs |
| 3 | projection performs no I/O | T007 | tests/compat_serialized.rs |
| 4 | projection imports no config DTOs or source traits | T009 | tests/compat_serialized.rs |
| 5 | source imports no projection, sync-state, target-path, template-policy, or manifest types | T013 / T016 / T029 | tests/transitive_resolve.rs |
| 6 | every Git, HTTP, and worktree source resolves to a snapshot abstraction | T012 | tests/digest_pin.rs |
| 7 | SourceBackend and unsupported default methods are gone | T013 / T030 | tests/http_redirect_scheme.rs |
| 8 | target-specific rendering and manifest generation live under sync | T015 | tests/golden.rs |
| 9 | desired state is represented by a Projection | T020 | tests/compat_serialized.rs |
| 10 | current machine state is represented by ObservedProjectState | T017 | tests/project_identity.rs |
| 11 | pending work is represented by a ChangeSet | T018 | tests/golden.rs |
| 12 | preview, sync, and prune share the same projected artifact identities | T021 | tests/compat_serialized.rs |
| 13 | drift classification after inspection is pure | T019 | tests/golden.rs |
| 14 | state records, ejections, hooks, and locking live under sync::state | T025 | tests/lock_contention.rs |
| 15 | journaling and recovery live under sync | T022 | tests/frozen_readonly.rs |
| 16 | the CLI constructs dependencies, renders reports, and maps exit codes, but does not implement sync behavior | T027 | tests/exit_code.rs |
| 17 | existing config, lock, registry, journal, digest, and CLI behavior remains compatible | T002 / T004 | tests/migration_warnings.rs |
| 18 | architecture checks prevent forbidden dependencies | T005 | tests/doc_invariants.rs |
| 19 | crate-level documentation describes the feature-oriented architecture | T031 | tests/doc_invariants.rs |
| 20 | all compatibility fixtures, unit tests, integration suites, Clippy, and formatting checks pass | T031 | tests/compat_serialized.rs |

## SourceBackend caller-migration table

`SourceBackend` is the legacy compat port and only shrinks: a method leaves the
trait once repo-wide caller checks show zero consumers outside `src/source/`.
As of T013 every consumer below is still live, so every method keeps its
surface; T020/T030 migrate the callers onto the `SourceStore` path.

| Method | Disposition | Successor | Consumers to migrate |
| --- | --- | --- | --- |
| `fetch` | retained | none until the T020/T030 caller migration | src/sync/resolve.rs, src/sync/transitive.rs |
| `mirror_ready` | retained | none until the T020/T030 caller migration | src/sync/resolve.rs |
| `read_file_at` | retained | `SourceStore::read` | src/sync/transitive.rs, src/cli/trust.rs |
| `list_source_leaves` | retained | `SourceStore::inventory` | src/sync/{mod,plan,preview,target,transitive}.rs, src/cli/query.rs |
| `list_tree_at` | retained | none until the T020/T030 caller migration | src/cli/trust.rs |
| `resolve` | retained | snapshot resolution beside `resolve_worktree` | src/sync/resolve.rs, src/sync/transitive.rs |
| `commit_time` | retained | none until the T020/T030 caller migration | src/sync/target.rs, src/sync/rebuild.rs |
| `export_artifact` | removed | `stage_artifact` over `SourceStore::read` (T016) | none |
| `compute_digest` | delegates | `SourceStore::digest_snapshot` | src/sync/resolve.rs |
