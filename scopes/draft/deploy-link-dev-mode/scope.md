---
created: 2026-06-07
status: draft
issue_type: Feature
---

# deploy-link-dev-mode

## Goal

Implement the deferred `deploy = "link"` deployment mode for local development sources: a
`phora.local.toml`-only, local-path-only opt-in that **symlinks** an artifact destination at the
source's live working tree, so uncommitted edits are instantly visible in the target harness. The
global default deployment stays reflink-copy (`design.md` "Reflinks over Symlinks"); link-mode is a
quarantined escape hatch that sits **outside** the content-integrity model.

## Context

Link-mode was designed-but-deferred in the original build:

- Intended use is recorded at `scopes/done/phora-artifact-manager/scope.md:138-145` (one of the four
  sanctioned `phora.local.toml` overlay uses: "Development-mode deployment choices (e.g., link-mode
  for local sources)") and illustrated at `scope.md:286` (`deploy = "link"  # optional`).
- The override *mechanism* shipped: `merge_configs` overlay (`src/config.rs:48`), base/local lock
  split (`src/sync.rs`), local-path source normalization (`NormalizedUrl`, tested at
  `src/source.rs:1164`).
- The *field* never landed: `Source` (`src/config.rs:117`) has no `deploy`/link-mode field, no
  `DeployMode` enum, no deploy-path handling. `#[serde(deny_unknown_fields)]` means `deploy = "link"`
  currently fails to parse.

Why a symlink and not the existing reflink: phora exports from the **committed git ODB**
(`src/source.rs:266`), so a reflink copy is always point-in-time — there is no path to working-tree-live
deployment without a symlink. This is the one capability reflinks structurally cannot provide.

The integrity carve-out already exists as precedent:
`scopes/done/phora-artifact-manager/validation.yaml:163` — "allowed symlinks have no integrity
coverage" — established that symlinks live outside drift/verify. Linked artifacts extend that carve-out.

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Link target | Live working tree: `<local source path>/<root>/<artifact>`, absolute | Uncommitted edits go live; the dev-loop purpose. Bypasses git mirror/fetch/export for that deployment. |
| Source eligibility | Local filesystem-path sources only | A remote URL has no working tree. `deploy = "link"` on a remote `git` is a config error. |
| Provenance | Honored only via `phora.local.toml` overlay | Quarantines the symlink hazard out of committed/shared config. Set in base `phora.toml` ⇒ config error. |
| Integrity | Tracked-quarantine | Registry record carries `linked: true`, no per-file hashes; a `Linked` artifact state is never `Modified`/`Foreign`; verify and rebuild skip it; prune may still remove the symlink. |
| Granularity | One symlink per artifact, at the artifact destination dir | Matches "live the whole skill/command dir"; simplest atomic unit. |
| Discovery | Filesystem scan of the working-tree path (honoring `root` + matcher), via a **mode-aware helper used by deploy, prune, AND rebuild** | Linked sources are not exported from git; all three discovery sites must scan disk, not the ODB (review C2). |
| Resolution (review C1) | Link sources **sidestep the git mirror** (no `fetch`/mirror digest) but still **synthesize a lock entry for auditing** (local path + HEAD read directly via `gix::open` + working-tree/sentinel digest + link marker) | A local path used only as a link may have no mirror; the audit trail is preserved without cloning. |
| Failure | Warn-and-continue | Reuses existing Windows/privilege policy (`done/.../validation.yaml:206`); a symlink failure skips the artifact, never aborts sync. |
| Global default | Unchanged (reflink copy) | Link-mode is additive and opt-in; `deploy` absent ⇒ `Copy`. |
| `config_digest` | `deploy` excluded | Deploy mode does not alter exported ODB content, so omitting it avoids spurious lock invalidation (confirmed safe via `source_matches`, `lock.rs:50`). Note: this is independent of base/local lock routing. |
| Idempotence (review H1) | `ArtifactState::Linked` is added to the `matches!` no-deploy guard at `sync.rs:299` | The guard matches on `conflict_kind` (an `Option`), so the new variant is **not** caught by exhaustiveness; without this, `Linked` falls through to `Overwrite` and re-deploys every sync. |

## Requirements

### Behavior
- A source with `deploy = "link"` and a local-path `git`, present in `phora.local.toml`, deploys each
  discovered artifact as a symlink `target_path/<layout artifact_path>` → absolute
  `<source path>/<root>/<artifact>`.
- Editing a file under the source working tree is immediately reflected through the symlink with no
  re-sync.
- `deploy` defaults to `copy`; absent or `copy` preserves today's reflink-swap behavior byte-for-byte.
- Re-sync of an already-correct link is idempotent (no churn, no spurious drift).
- Switching a source `link → copy` (or `copy → link`) on the next sync replaces the destination:
  symlink → materialized reflink copy with full integrity restored; real dir → symlink.

### Guardrails (Given/When/Then)
- **Given** `deploy = "link"` on a source whose `git` is a remote URL, **when** sync runs, **then** a
  config error names the source and rejects link-mode (no symlink, no partial deploy).
- **Given** `deploy = "link"` set in the committed `phora.toml` (not the local overlay), **when** sync
  runs, **then** a config error rejects it as a non-local-only setting.
- **Given** symlink creation fails (e.g. Windows without privilege), **when** deploying a linked
  artifact, **then** phora warns, skips that artifact, and continues the rest of sync (exit reflects a
  deploy failure, consistent with existing `had_failures`).

### Integrity quarantine
- `check_artifact_state` returns a new `Linked` state for keys recorded `linked: true`; never
  `Modified` or `Foreign` for them.
- `verify` skips linked records (no content hash to check).
- `rebuild_registry` skips linked artifacts (or reconstructs the `linked` marker without hashing) and
  does not classify a present link as `foreign`/`modified`.
- `prune` removes an orphaned linked artifact by deleting the symlink only (never `remove_dir_all`
  through it); existing `remove_orphan_path` symlink handling (`src/sync.rs:501`) is the basis.

### Crash safety
- Symlink placement is atomic (create temp symlink, `rename` into place) and journalled like the copy
  swap, so an interrupted link deploy leaves either the old or new destination, never a half state.

## Key Files

| File | Change |
|---|---|
| `src/config.rs` | `DeployMode { Copy, Link }`; `Source.deploy: Option<DeployMode>`; `merged_with`; keep out of `config_digest`; local-only + local-path validation hooks. |
| `src/source.rs` | Filesystem artifact discovery for local-path linked sources (honor `root` + matcher); resolve absolute link target; local-repo HEAD read for the audit lock entry. |
| `src/projection.rs` | `link_artifact` (own lean temp-symlink + rename — **not** via `swap_into`'s copy_tree fallback); `ArtifactState::Linked`; `check_artifact_state` link short-circuit before commit check; Windows `symlink_dir`; prune symlink-safe (already partially handled). |
| `src/registry.rs` | `RegistryRecord.linked: bool` (default false); linked records carry empty `files` + sentinel `commit`/`digest`. |
| `src/sync.rs` | Deploy dispatch on `DeployMode` at the `deploy_artifact_entry` closure (`sync.rs:279`, keeps Copy path byte-identical); `resolve_sources` link carve-out (DLD-009); `Linked` added to the `matches!` no-deploy guard (`sync.rs:299`); guardrail validation against `base_config`/`local_names`; mode-aware discovery in `deploy`/`prune`/`rebuild`; `verify` skips linked. |
| `src/cli.rs` | `state_label` (`cli.rs:882`, exhaustive match) gains a `Linked` arm so `phora list` shows linked. |
| `README.md` / example | Document `deploy = "link"`: local-only, live-edit, out-of-integrity semantics. |

## Out of Scope
- Per-file (sub-artifact) linking — granularity is whole-artifact-dir.
- Live-link for remote sources or committed config.
- Bringing linked artifacts into drift/verify (they are deliberately outside it).
- Changing the global reflink default or the copy deploy path semantics.

## Verification
- `cargo test` green; new tests per task (config parse/merge/default, guardrail rejections,
  filesystem discovery, atomic symlink + crash window, Linked state, verify/rebuild/prune skip,
  link↔copy transitions, Windows warn-and-continue).
- Manual: local-path source in `phora.local.toml` with `deploy = "link"`; edit a file in the checkout;
  confirm the change is live in the target without re-sync; `phora verify` reports no drift; `phora
  list` shows the artifact as linked.
