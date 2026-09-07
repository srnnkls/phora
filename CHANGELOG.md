# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0](https://github.com/srnnkls/phora/compare/v0.1.2...v0.2.0) - 2026-09-07

### Features

- *(sync)* Return structured workspace outcomes
- *(sync)* Add request and report contract
- *(sync)* Observation pipeline drives reconcile-based apply; retire StageBridge
- *(sync)* Complete facade retirement; bindings-only R1 keying
- *(sync)* R1 reconcile keying by (target, binding identity, published_key)
- *(sync)* Retire projection compat facade; cross-target overlap guard
- *(projection)* Pure build_workspace split; sync builds value inputs
- *(sync)* Keyed reconcile correlation; full desired×observed matrix
- *(sync)* Whole-run preflight conflict resolution; kind-carrying Conflict
- *(sync)* Pure reconcile floor; reconcile_use_ok FQ-path lint goes live
- *(sync)* Observed-state contract — inspect onto StateStore, reconcile value types
- *(sync)* Introduce StateStore with an in-place FileRegistry impl
- *(source)* [**breaking**] Delete the export path; source no longer stages
- *(sync)* Add stage_artifact executing the T032 staging contract
- *(sync)* Define the StageRequest/StagedArtifact staging contract
- *(sync)* Add the StageBridge orchestrator bridge for PR6 staging
- *(source)* Add digest_snapshot and the SourceBackend caller-migration table
- *(source)* Add SourceStore snapshot API with eager worktree capture
- *(ci)* Close INV-1 fully-qualified crate-path bypass; verify PR2 purity
- *(cliff)* Advise on network-filesystem state roots
- *(cliff)* Sync --frozen falls back lockless on a read-only state root
- *(cliff)* Orphan visibility with physical prune, per-clone project identity
- *(cliff)* Refuse target rm with live artifacts, persist deploy roots, pin unicode folds
- *(sync)* Add --fast-forward to follow moved pins on drop
- *(preview)* Show each target's deploy path ([#60](https://github.com/srnnkls/phora/pull/60))

### Bug Fixes

- *(bench)* Make the fetch_sweep awk reporter BSD-awk compatible
- *(sync)* Close final review mutation races
- *(sync)* Batch-20 review round — R4 pre-mutation + path normalization; API/comment cleanups
- *(arch)* Close three source_use_ok bypass routes
- *(sync)* Refuse ambiguous root-relative source keys in stage_artifact
- *(http)* Reject non-success final status before writing download ([#64](https://github.com/srnnkls/phora/pull/64))
- *(cliff)* Gate legacy-registry adoption on the frozen read-only fallback
- *(source)* Saturate symlink escape depth increment ([#63](https://github.com/srnnkls/phora/pull/63))
- *(source)* Treat drive-letter prefixes as filesystem paths
- *(source)* Validate symlink target bytes at stage time
- *(sync,cli,http)* Batch 1 correctness fixes from three-axis audit
- *(sync)* Guard fast-forward drops against live artifacts and out-of-anchor paths ([#62](https://github.com/srnnkls/phora/pull/62))
- *(sync)* Only report a binding's pin when it actually moves
- *(sync)* Honor the ejected list in the sealed-offer guard ([#59](https://github.com/srnnkls/phora/pull/59))
- *(sync)* Treat a collapse-key flip as redeploy, not a Foreign conflict ([#58](https://github.com/srnnkls/phora/pull/58))

### Performance

- *(sync)* Raise the default fetch ceiling floor to 50 threads

### Refactor

- *(source-projection-sync)* Remove compatibility facades
- *(architecture)* Move kernel identities to owners
- *(cli)* Centralize process exit mapping
- *(sync)* Carry resolver through SyncRequest
- *(state)* Relocate file registry
- *(sync)* Adopt apply vocabulary
- *(sync)* Absorb deployment implementation
- *(sync)* Share projection across sync lifecycle
- *(sync)* Carve inspect and scan out of deploy — move-only
- *(sync)* Rewire rebuild_one onto stage_artifact
- *(sync)* Rewire deploy_one onto stage_artifact via a scratch-read bridge
- *(sync)* Move physical symlink validation into sync/stage.rs
- *(sync)* Carve the staging walk and renderer into sync/stage.rs
- *(source)* Isolate the gix read behind an export-walk resolve seam
- *(source)* Split source.rs into the source/ module tree
- *(projection)* Move config-free projection core into projection/{model,build,diagnostic}
- *(projection)* Decouple plan.rs in place — pure types, specs, config-free projection
- *(projection)* Git mv kernel selection/take/collapse into projection
- *(cliff)* Drop .phora-id for path-hash identity, no tree or git writes

### Documentation

- *(sync)* Clarify that 50 floors the pool cap, not the thread count ([#65](https://github.com/srnnkls/phora/pull/65))
- *(source-projection-sync)* Finalize architecture boundaries

### Testing

- *(hooks)* Pin whole-run abort atomicity in the scrut contract
- *(cli)* Bound PTY phases by absolute deadlines
- *(cli)* Make PTY status portable on macOS
- *(cli)* Cover PTY conflicts and warning order
- *(state)* Close relocation parity
- *(sync)* Close deploy recovery parity
- *(sync)* R7 ejected-transition + frozen write-free pins; R4 aliasing regression
- *(sync)* Behavioral R6 + S6 pins on the landed observation seams
- *(compat)* Drive the staging and source-digest goldens through stage_artifact
- *(compat)* Pin moved projection output to the T002 goldens
- *(compat)* Pin recovery/CLI baselines; feat(ci): arch-check lint
- *(compat)* Pin golden staging matrix at the PR1 baseline
- *(compat)* Pin PR1 baselines — architecture record + serialized goldens

### Styling

- *(projection)* Drop the inert Sized bound; signature pin tolerates rustfmt trailing comma
- *(tests)* Drop spent coupling-point narration from the T018 suites

[0.2.0]: https://github.com/srnnkls/phora/compare/0.1.2..0.2.0

## [0.1.2](https://github.com/srnnkls/phora/compare/v0.1.1...v0.1.2) - 2026-07-01

### Bug Fixes

- *(source)* Keep a staging dir when its mtime is unreadable ([#55](https://github.com/srnnkls/phora/pull/55))
- *(source)* Re-fetch a locked source when its mirror cache is gone
- *(source)* Self-heal a corrupt mirror and sweep orphaned staging dirs

[0.1.2]: https://github.com/srnnkls/phora/compare/0.1.1..0.1.2

## [0.1.1](https://github.com/srnnkls/phora/compare/v0.1.0...v0.1.1) - 2026-06-30

### Bug Fixes

- *(release)* Define [profile.dist] for cargo-dist builds

[0.1.1]: https://github.com/srnnkls/phora/compare/0.1.0..0.1.1

## [Unreleased]
