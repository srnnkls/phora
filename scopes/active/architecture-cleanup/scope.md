---
created: 2026-06-09
status: active
issue_type: Feature
target_tree: "origin/main @ 9310c5a (Rust); this branch chore/architecture-cleanup builds on it"
revised: 2026-06-10   # ARCH-000..011 landed; round 3 (loqui-alignment): ARCH-014 name newtypes added, ARCH-012 mandatory + decoupled from ARCH-013
---

# architecture-cleanup

## Goal

Give **artifact membership** ("what counts as an artifact of this source") a single home, and
re-seat phora onto a **hexagonal, feature-organized, parse-don't-validate** architecture so the
rule — and the others like it — cannot drift again. Membership is currently re-decided at five
sites that disagree (`source.rs:274`, `sync.rs:699`, `source.rs:397`, `sync.rs:1151`,
`sync.rs:1204`, plus `cli.rs:605`); centralizing it lands the dotfile opt-in and fixes the one
real defect (stranded dotfile orphans). The re-seat also absorbs a second parse-don't-validate
finding: the `Source.git` field is **misnamed** — it holds a literal *location* (remote URL,
scp/ssh, or a local path; in link mode, any directory), not specifically a git remote.

Full design (authoritative): [resources/CLEANUP.md](resources/CLEANUP.md).

## Context

Triggered by a docs question — *can a top-level dotfile directory be opted into as an artifact?*
It can't, and tracing why exposed the dotfile-skip as one of five open-coded copies of the
membership predicate. After multi-agent review (`review.yaml`), the corrected picture:

- **Discovery** (`source.rs:274` git, `sync.rs:699` link) skips dotfiles and applies the matcher.
- **Digest** (`hash_tree`, `source.rs:397`) skips no top-level dotfiles. But `LockedSource.digest`
  has **no production consumer** — `source_matches` (`lock.rs:52`) gates lock-reuse on
  `commit + config_digest`, never on `digest` (only test assertions read it, `sync.rs:1529`). So
  the divergence is a *latent inconsistency*, not a runtime defect; the digest is a recorded
  resolved-subtree fingerprint. **It is left untouched** (changing its domain would re-baseline
  locks broadly for zero behavioral gain) — the dotfile opt-in needs no digest change.
- **Prune** (`sync.rs:1151`, `1204`) skips dotfiles but ignores the matcher — so an orphaned
  dotfile artifact is removed from the registry yet stranded on disk. **This is the one real
  defect**, fixed by routing prune through the shared `Selection`.
- **`check-match`** (`cli.rs:605`) applies the matcher but not the dotfile rule; routing it
  through `Selection` corrects (and slightly changes) its debug output.

Separately, the `git` field's name describes one protocol, not its role. `git = "/home/me/dev/loqui"`
is already valid today (link-mode local path), and link mode accepts a non-repo directory — so a
field named `git` already holds non-git values. Its real meaning is "the literal source location,"
as opposed to symbolic `host`+`path` or `url`. This is the same parse-don't-validate gap as the
mode `Option`-bag, and the `Remote` ADT's first arm is therefore `Literal`, not `Git`.

This mirrors gestalt's `architecture-cleanup`, where adding a language was 14-site shotgun surgery
across a closed enum; the fix was a `Language` extension port. phora's analogue is a `Selection`
value object the discovery/prune/debug paths all consume.

### Reference shape

`~/projects/gestalt/.worktrees/architecture-cleanup` (done, 17 ARCH tasks): kernel value objects,
parse-don't-validate boundaries, the extension seam, a behavior-identical golden-test acceptance
bar, phased low→high-risk migration.

### Current state (verified)

| Concern | Today | `file:line` |
|---|---|---|
| Membership rule | 5 open-coded copies, disagreeing | table above |
| Digest vs deploy | `hash_tree` walks more than deploy; but digest has no production reader | `source.rs:397` vs `:274`; `lock.rs:52` |
| `Source.git` misnomer | holds remote URL, scp/ssh, **or local path** (incl. non-repo dir in link mode) | `config.rs` Source; README link-mode example |
| `Source` config | `Option`-bag; `git XOR host+path XOR url` + `branch XOR tag XOR rev` validated by `is_some()` counting in `parse` **and** `validate` | `config.rs:33`, `:100`; `mode()` `:622` |
| Value objects | none for rel-path/commit; two `Digest` types | `registry.rs:57` vs `config.rs:446` |
| Orchestration | `sync.rs` god-module | 5 655 lines |
| CLI | argv mixed with business logic; `ParsedSource` name already taken | `cli.rs` 2 598 lines; `cli.rs:613` |
| Ports | `SourceBackend`/`Registry` unsealed; git adapter leaks via getter | `source.rs:64`, `registry.rs:74`, `backend.rs:25` |
| `gix` confinement | `source.rs` (git adapter) + `archive.rs` (`EntryKind`) only; **worktree subsystem dropped in #12** | — |
| Golden tests | none exist (no snapshot harness) | — |

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Membership home | one `Selection` value object (`kernel/selection.rs`), absorbing `PathMatcher` | one rule; four consumers call it |
| Dotfile opt-in | hidden top-level dir selected iff matched by an include pattern beginning with `.` (`.config`, `.*` opt in; `*` does not) | dotglob convention (user-chosen); a bespoke gate, globset `*` matches dotfiles |
| Source-level digest | **leave unchanged**; document its contract + pin a zero-churn test | no production consumer; "fixing" it re-baselines locks for no behavioral gain (review B1/B2) |
| location fields | **typed source-kind keys** (uv model) as a proper `Remote` sum type with per-kind payloads: `git` (git-only) · `path` (local dir/file) · `url` (download) · `host`+`repo` (forge). Ships as a **standalone leading PR** before add-local-symlink and before this scope's full `Remote` ADT | each key honest and distinct; `git` no longer doubles as a local path |
| forge key `path`→`repo` | rename host-mode `path`→`repo` (it's `owner/repo`, not a path); bare `repo` keeps the github default; `host`+`path` accepted as a deprecated alias | frees `path` for local; `repo` is the truthful name |
| local splits off `git` | link-mode/local sources move `git=<path>`→`path=<path>`; `git=<localpath>` kept as back-compat alias | `git` becomes honestly git-only |
| one intentional break | bare `path = "owner/repo"` (old github shorthand) now means a local path; the shorthand moves to bare `repo` | git sources + `host`+`path` configs unaffected |
| alias deprecation | alias forms (`git`=<localpath>, `host`+`path`) emit a one-line deprecation warning naming the new key; bare `path` that looks like `owner/repo` and does not exist locally emits a meaning-changed hint | round 2; ARCH-005 shipped WITHOUT them — retrofit is ARCH-015. Horizon: drop decided before next major, not this cycle |
| Source config | `ParsedSource { remote: Remote = Literal \| Host \| Url, refspec, … }`, parsed once after merge | illegal states unrepresentable; deletes the `is_some()`-counting validators |
| Ports | drop the leaky adapter getters; rewrite getter-based dispatch tests; **no formal sealing** | single binary, no downstream impls; sealing breaks in-crate test doubles (review B3) |
| Clock | **not introduced** | 6 `now()` sites are informational timestamps nothing asserts (review OE1) |
| Newtypes | keep `RelPath`, unified `Digest`, `Commit`; **`ArtifactName`/`SourceName` reinstated** (ARCH-014) | round 3 (loqui-alignment) reverses review OE4: types.md — domain identifiers are newtyped; boundary checks move into constructors |
| Split | `sync.rs`→`sync/`, `cli.rs`→`cli/`, `projection`→`deploy/`, `registry`→`store/`, `matcher`→`kernel/selection`, `config.rs`→`config/` | feature-organized bounded contexts |
| Per-context errors / crate split | **decoupled** (round 3, loqui-alignment): ARCH-012 (per-context errors, thiserror, typed errors per port) ships this cycle; ARCH-013 (crate split) stays optional/deferred | traits.md: typed errors per port; the empirical zero-variant-consumer finding stands but the port contract is the value (tasks.yaml ARCH-012) |

## Requirements

### Functional requirements

- Pure refactor: `sync`, `update`, `verify`, `list`, `where`, `add`, `eject`, `uneject`,
  `rebuild-registry`, `check-match` behave identically — **except** the dotfile opt-in, the
  prune-orphan fix, and `check-match`'s output for dotfile names.
- Dotfile opt-in: `include = [".config"]` / `[".*"]` selects a hidden top-level dir; `["*"]` /
  `["code-*"]` does not.
- Orphan fix: a deployed dotfile artifact removed from config is pruned from disk by `--prune`.
- Typed source-kind keys: `path = "<local>"` declares a local source; `host`+`repo` declares a
  forge source; `git`/`url` unchanged. Back-compat: `git = "<localpath>"` still works (alias for
  `path`), `host`+`path` still works (alias for `host`+`repo`). `add` and docs emit the new keys.

### Technical requirements

- `Selection` is the sole definition of artifact membership and the dotfile policy.
- Source-level `content_digest` is byte-identical before/after (untouched).
- Parse-don't-validate: `ParsedSource`/`Remote` ADT (`Literal | Host | Url`); kernel `RelPath`,
  unified `Digest`, `Commit`.
- Drop adapter getters; `gix`/`ureq`/archive crates confined in production code.
- Feature-organized module layout per [resources/CLEANUP.md](resources/CLEANUP.md).

## Acceptance Criteria

- [x] **Golden harness exists (ARCH-000):** snapshot tests capture `sync`/`list`/`verify`/`where`/
      `check-match` output before any structural change; refactor diffs are reviewed against them.
- [x] Pure refactor: command output identical save the documented exceptions (dotfile opt-in,
      orphan-prune, check-match dotfile output).
- [x] **Zero lock churn:** a property test asserts `content_digest` is byte-identical pre/post.
- [x] Membership `starts_with('.')` for artifacts decided in exactly one place (`kernel/selection.rs`;
      `sync/rebuild.rs`'s foreign scan open-codes only the non-hidden fast path and delegates the
      membership decision to `Selection::selects_artifact`).
- [x] `include=[".config"]`/`[".*"]` selects `.config`; `include=["*"]` does not.
- [x] A deployed dotfile artifact later removed from config is removed from disk by `--prune`.
- [x] `path="<local>"` and `host`+`repo` configs work; `git="<localpath>"` and `host`+`path`
      configs still work (aliases); `add` emits the new keys; bare `repo="owner/repo"` resolves to github.
- [ ] Alias forms warn (deprecation, naming the new key); bare `path` resembling `owner/repo`
      with no such local path warns about the moved shorthand (ARCH-015).
- [x] Confinement (grep, production code): `gix` only in `src/source.rs`, `ureq` only in
      `src/http.rs`, archive crates only in `src/archive.rs` (ARCH-011 left `source.rs` flat —
      the planned `source/` dir never materialized; greps re-verified 2026-06-10).
- [x] `cargo clippy --all-targets` warning-free at `pedantic`.

## Implementation Strategy

Three phases, low → structural risk. Detail in [resources/CLEANUP.md](resources/CLEANUP.md).

- **Phase 0/1 — harness + selection seam:** ARCH-000 golden harness (prereq); then `Selection`
  + kernel value objects; route discovery/prune/check-match through it; dotfile opt-in; document
  the digest contract. Fixes the orphan bug; zero lock churn.
- **Phase 2 — parse + typed keys:** the typed source-kind keys (ARCH-005, leading PR); `ParsedSource`/`Remote`
  ADT (after renaming `cli.rs::ParsedSource`); drop adapter getters + rewrite dispatch tests;
  confine `archive.rs`'s gix use.
- **Phase 3 — context internals:** split `sync/` and `cli/`; relocate `deploy/`/`store/`/
  `kernel`/`config/`; per-context errors + crate split deferred (optional).

## Dependency Graph

> Machine-readable: [dependencies.yaml](dependencies.yaml)

```
Phase 1   ARCH-000 golden harness ─┬→ ARCH-001 kernel (RelPath, Digest, Commit)
                                   │   → ARCH-002 Selection (+ discovery, check-match, dotfile opt-in)
                                   │   → ARCH-003 prune via Selection (orphan fix)
                                   └→ ARCH-004 document digest contract (zero-churn test)
Phase 2   ARCH-005 typed keys: +path, path→repo (aliases)  → ARCH-006 ParsedSource/Remote ADT (+ rename cli ParsedSource)
          ARCH-007 drop getters + fix dispatch tests   ARCH-008 confine archive.rs gix
Phase 3   ARCH-009 split sync/ → ARCH-010 split cli/
          ARCH-011 relocate deploy/+store/+kernel+config/
          ARCH-012 per-context errors + ARCH-013 crate split (optional, together)
```

## Non-Goals

- No behavior changes beyond the dotfile opt-in, the orphan-prune fix, and check-match's dotfile output.
- No change to the source-level digest, the on-disk store/registry/journal formats, or lock identity.
- The typed-keys change keeps back-compat aliases (`git`=<localpath>, `host`+`path`); it does
  **not** drop them this cycle. `git`/`url` for their proper kinds are unchanged.
- No async; phora stays synchronous.
- No `Clock` port, no formal trait sealing (see Decisions). `ArtifactName`/`SourceName`
  newtypes are IN scope as of round 3 (ARCH-014, loqui-alignment).
- Per-context errors (ARCH-012) ship this cycle (round 3, decoupled); only the
  `phora-core`/`phora-cli` crate split (ARCH-013) stays deferred (reopen triggers in tasks.yaml).

## Verification

- `mise run check` green (clippy pedantic `-D warnings` + rustfmt + tests).
- Golden snapshots unchanged across the refactor (save documented exceptions); digest property test green.
- Boundary greps: membership single-site; `gix`/`ureq`/archive confined (production);
  `crate::cli` absent outside `src/cli/` and `src/main.rs` (core never imports the CLI layer).

## Open Questions

- [x] Field model: **typed source-kind keys** (`git`/`path`/`url`/`host`) as a `Remote` sum
      type — not a generic `from`. (decided)
- [x] ARCH-005 sequence: **rename/typed-keys land first as their own PR**, before add-local-symlink
      and before this scope's full `Remote` ADT. (decided)
- [x] **Bare-`path` conflict:** resolved by renaming the forge key `path`→`repo` (bare `repo`
      keeps the github default), which frees `path` for local. `host`+`path` and `git=<localpath>`
      stay as back-compat aliases; bare `path="owner/repo"` is the one intentional break. (decided)
- [ ] Deprecation horizon for the `path`(forge)/`git`(local) aliases — how many releases before drop?
- [ ] ARCH-004: ever align the digest domain with deploy? (Default: no — no consumer, broad churn.)
- [x] ARCH-013: crate split this cycle or defer? **Deferred to a future session** (decided
      2026-06-10) together with ARCH-012 — core→cli boundary already clean and grep-pinned;
      reopen on an external lib consumer, a second binary, or compile-time pain.
