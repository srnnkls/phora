---
created: 2026-06-07
status: active
issue_type: Feature
depends_on_scope: deploy-link-dev-mode
---

# worktree-include

## Goal

Let phora natively populate a new git worktree with a configured set of **back-reference symlinks**
(and optional copies) into the repo's primary checkout, replacing the external
`git-worktreeinclude` tool. Driven by native phora config, applied by a `phora worktree apply` command
that the repo's `post-checkout` hook invokes on checkout / worktree creation.

## Context

Today (`~/getml/projects/arvato`) this is done by a forked `git-worktreeinclude` (`mise.toml:12`,
`github:srnnkls/git-worktreeinclude@0.9.3`) reading a `.worktreeinclude` manifest, triggered from the
`post-checkout` hook in `hk.pkl` (`README.md:45`). Each worktree gets absolute symlinks back to the
primary checkout — e.g. `…/.worktrees/<branch>/.codex -> /Users/srnnkls/getml/projects/arvato/.codex`.

Why phora's existing pipeline can't serve this: roughly half the manifest entries are **gitignored /
local-only** (`mise.local.toml`, `fnox.local.toml`, `.codex`, `.gitignore.local`, `gestalt.local.toml`,
`.cmw.local/config.yaml`) — they exist in no commit, so phora's git-ODB `source → discover → export`
path is blind to them. Worktree-include needs a **local-directory source + explicit path manifest +
link projection**, distinct from artifact deployment.

This reuses the link primitive from the `deploy-link-dev-mode` scope (atomic journalled symlink,
warn-and-continue, integrity-quarantine) but adds the primary-worktree source model, the explicit
manifest, and the worktree trigger.

## Relationship to submodules (explicit non-goal)

The arvato manifest also links `resources/effect/*` via `symlink submodule-walk`. Those are git
submodules, and **phora exists to replace submodules**, so they are *not* modeled here. The intended
migration is to declare each as an ordinary phora **source** (remote git URL, pinned commit) deployed
through the normal pipeline — not as a worktree-include entry. No `submodule-walk` mode is added. A
shared-materialization optimization (deploy a large source once, link it into many worktrees) is noted
as future work, not part of this scope.

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Config surface | Native `[worktree].includes = [{ path, mode }]`, **committed `phora.toml` base + `phora.local.toml` overlay**; drop `.worktreeinclude` | Include *paths* aren't secret; living in committed config means a **fresh worktree has the manifest at apply time** (a gitignored local file isn't checked out). Local overlay holds machine-specific additions. (review H2) |
| Modes | Own `IncludeMode { Symlink, Copy }` (default Symlink) | **Decoupled** from deploy-link's `DeployMode{Copy,Link}` — different axis/semantics; submodule-walk dropped. (review H1) |
| Dependency | **None on deploy-link-dev-mode.** Own ~5-line atomic placement helper; reuse only `projection::copy_file` (reflink) | The two enums differ and the shared primitive is tiny; coupling to a draft scope added risk without payoff. (review H1) |
| Link source | The repo's **primary (non-linked) worktree**, detected via **gix** (`discover` + `main_repo().work_dir()`), not a `git` shell-out | Codebase is pure-gix. Absolute targets into it (matches current behavior); bare main repo => error. (review feasibility) |
| Tracked-path guard | Real **gix index** check (own task), refusing include paths present in the worktree index | New but feasible plumbing (gix default features compile `gix-index`); untracked-but-present `.codex`/`*.local.toml` correctly read as absent. (review H3) |
| Trigger | `phora worktree apply`, invoked by the existing `post-checkout` hook (swap the binary in `hk.pkl`) | Preserves automatic-on-checkout UX with minimal workflow change. |
| State | Stateless / idempotent; **not** recorded in the artifact registry; opens **no** registry and takes **no** `state.lock` | Includes are worktree scaffolding, cleaned up when `git worktree remove` deletes the dir. Re-apply re-points stale links (no journal needed — a partial apply self-heals on next checkout). (review medium) |
| Nested entries | A child path inside a real dir (`.cmw.local/config.yaml`, `.claude/skills/issue`) creates parents and links **only the leaf**, never clobbering the parent | The arvato manifest's common shape; must not disturb surrounding tracked content. (review H4) |
| Symlink path style | Absolute | Matches today's links; relative-target portability is out of scope for v1. |
| Integrity | Out of the drift/verify model (same carve-out as linked artifacts) | A symlink to a live primary has no stable content hash. |
| Apply scope | Linked worktrees only; no-op in the primary | Prevents a checkout from symlinking onto itself. |

## Requirements

### Behavior
- `[worktree].includes` lists `{ path, mode }` entries; `phora worktree apply` (run inside a linked
  worktree) materializes each at `<worktree>/<path>` pointing at / copied from `<primary>/<path>`.
- `symlink` mode creates an absolute symlink to the primary; `copy` mode reflink-copies (reusing the
  existing reflink-or-copy path).
- Application is idempotent: an already-correct entry is left untouched; a stale/wrong symlink is
  re-pointed; a missing one is created.
- Base + local `[worktree]` sections merge via the existing overlay (arrays replace, not concatenate),
  so gitignored entries declared in phora.local.toml compose with shared ones in phora.toml.

### Guardrails (Given/When/Then)
- **Given** `phora worktree apply` runs in the **primary** worktree, **then** it is a no-op with a
  notice (never symlinks the primary onto itself).
- **Given** an include `path` is a **git-tracked** file in the worktree, **then** that entry is
  refused with a warning (includes target ignored/untracked paths only; never shadow committed content).
- **Given** a primary-worktree path for an entry does not exist, **then** that entry warns and is
  skipped (no dangling link created).
- **Given** symlink/copy creation fails (e.g. Windows privilege), **then** warn, skip the entry,
  continue; exit reflects that some entries failed.

### Source detection
- Resolve the primary worktree via git (e.g. `git worktree list --porcelain` / common-git-dir); error
  clearly if CWD is not within a git repo.

### Crash safety
- Each entry is placed atomically (temp symlink/copy + rename), reusing the `deploy-link-dev-mode`
  journalled-swap primitive, so an interrupted apply leaves each entry old-or-new, never half.

## Key Files

| File | Change |
|---|---|
| `src/config.rs` | `WorktreeConfig { includes: Vec<Include> }`, `Include { path, mode: IncludeMode }`, `IncludeMode { Symlink, Copy }`; `Config.worktree`; `WorktreeConfig::merged_with` + extend `merge_configs`; mode/path validation. |
| `src/worktree.rs` (new) | gix primary-worktree detection (`discover` + `main_repo().work_dir()`); stateless include-application engine (lean temp+rename, no journal/registry); `is_path_tracked` via gix index; primary no-op + missing-primary guards. |
| `src/projection.rs` | Reuse `copy_file` (reflink); own small symlink helper (file vs dir, Windows `symlink_dir`). |
| `src/cli.rs` | `Worktree` subcommand enum `{ Apply, ImportLegacy }` (first nested subcommand group). |
| `hk.pkl` (arvato, downstream) | Swap `git-worktreeinclude apply` → `phora worktree apply` in `post-checkout`. |
| `README.md` / example | Document `[worktree]` config + hook wiring + migration off `.worktreeinclude`. |

## Out of Scope
- `submodule-walk` / any submodule handling — replaced by declaring submodules as phora sources.
- Relative symlink targets (absolute only in v1).
- Shared-materialization dedup for large sources across many worktrees (future).
- Registry tracking / drift-verify of include entries (deliberately stateless).

## Verification
- `cargo test` green; new tests per task.
- **Acceptance oracle (CI):** an integration test creates two tempdir "worktrees" (primary + linked),
  writes a `phora.toml` with `[worktree].includes`, runs `phora worktree apply` in the linked dir, and
  asserts the exact symlink targets + idempotent re-apply + a refused tracked path + a warned missing
  primary path. (review L-001 — replaces the manual arvato check as the gate.)
- **Smoke (post-merge, not a gate):** in arvato, define `[worktree]` includes, swap the hook to
  `phora worktree apply`, create a fresh worktree, confirm parity with `git-worktreeinclude` output.
- Migration aid: `phora worktree import-legacy` converts an existing `.worktreeinclude` to `[worktree]`
  config (one-shot), letting arvato drop the file and the tool.
