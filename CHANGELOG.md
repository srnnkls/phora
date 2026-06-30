# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0](https://github.com/srnnkls/phora/releases/tag/v0.1.0) - 2026-06-30

### Features

- *(release)* Ship Linux + macOS only; defer Windows to a follow-up
- *(release)* Generate cargo-dist release.yml (workflow_dispatch handoff)
- *(release)* Cargo-dist matrix, 3-OS CI, release-plz workflow + dispatch handoff
- *(release)* Crates.io metadata, LICENSE, git-cliff config, pinned toolchain
- *(hooks)* Add per-target pre_deploy hook with configurable on-fail
- *(hooks)* Add global pre_sync lifecycle hook (abort-on-failure gate)
- *(hooks)* Add shell-free argv hook form (HookCommand::Exec)
- *(sync)* Persist the revalidated stat refresh on the write path
- *(deploy)* Escalate stat-miss to recorded blake3 in the drift gate
- *(deploy)* Add ArtifactState::Revalidated carrier + match-arm ripple
- *(trust)* Add `trust <source> --show <path>` offline file inspection
- *(trust)* List composed file surface on first trust, offline
- *(transitive)* Confine observers + verify hook-gate (TDEP-CONFINE-OBSERVERS-001)
- *(source)* Follow the remote default branch when no ref is pinned
- *(selection)* SMR-063 error→remedy + debug-command linkage + named-phrase catalog (batch12)
- *(selection)* SMR-062 rich phora preview — surface plan warnings + no-match-glob suggestions (batch11)
- *(selection)* SMR-061 phora explain + SMR-041 docs to new model (batch10)
- *(selection)* SMR-060 did-you-mean engine over the offer set (batch9)
- *(selection)* SMR-032 preview take-pin + SMR-033 Instance-granular prune (batch9)
- *(selection)* SMR-031 8c — merge-CLEAR + VCS-prune-at-write + manifest re-pin (FINAL)
- *(selection)* Full Selection elimination — explicit leaf→dest export + leaf-granular rebuild/preview/query (SMR-031 8b)
- *(selection)* Flip live deploy onto leaf-granular TargetPlan (SMR-031 8a)
- *(selection)* Leaf-granular discovery/plan — first kernel deploy caller (SMR-030)
- *(selection)* Hybrid dir-collapse planner + `collapse` override (SMR-022)
- *(selection)* Take resolution spine — Scala-3 precedence + glob front-door consolidation (SMR-021)
- *(selection)* Parse-time binding-scope rejection + offer→leaf compiler (SMR-012/020)
- *(selection)* Source offer + binding take DTO; remove binding scope & map (SMR-010/011)
- *(diagnostic)* Structured SelectionDiagnostic foundation (SMR-013)
- *(selection)* Salvage RecordKind + safe_relpath + kind-aware deploy base (SMR-001)
- *(paths)* Phora.toml [paths] override for cache and state roots ([#41](https://github.com/srnnkls/phora/pull/41))
- *(transitive)* Phora trust CLI + A' stripped-hook exit + commit-pinned file-diff (TDEP-TRUST-CLI-001)
- *(transitive)* Read_file_at port + single-ref shallow fetch for phora add (TDEP-ADD-INFORM-001)
- *(transitive)* Resolve inner remotes against consumer host registry (TDEP-DEPCONFIG-001)
- *(transitive)* Behavior-pinned, consumer-owned hook trust (TDEP-HOOK-TRUST-001)
- *(transitive)* Hook-admission gate — interpret opaque hooks to inert candidates
- *(transitive)* Namespaced graph lock + trusted_hooks + --frozen
- *(transitive)* Nested-depth (closure) composition of transitive deps
- *(transitive)* Destination confinement (security-critical)
- *(transitive)* Imports mount field + dependency-instance namespaced composition
- *(transitive)* Activation, single-parse manifest DTO, cycle-guarded recursion + lock v1→v2
- *(config)* Keyed-table per-target bindings + keyed CLI writer
- *(source)* Atomic temp+rename mirror creation (ATOMIC-MIRROR-001)
- *(source)* Per-mirror flock around HttpBackend::fetch import (MIRROR-LOCK-002)
- *(source)* Per-mirror blocking flock around GitBackend::fetch (MIRROR-LOCK-001)
- *(sync)* Parallelize fetch/resolve/digest with rayon (PAR-001)
- *(cli)* Acquire exclusive process lock in state-mutating commands (LOCK-001)
- *(sync)* Allow link mode for local sources in committed config; warn on absolute path
- *(cli)* Mapped-leaf observability via shared record path helper (leaf-aliasing T8)
- *(sync)* Mapped-leaf link-deploy + prune/verify/rebuild map-layout (leaf-aliasing T6, T7, T7b)
- *(sync)* Mapped-leaf copy-deploy, preview parity, dest collisions (leaf-aliasing T5, T3, T4)
- *(source,plan)* Mapped-leaf export + plan projection (leaf-aliasing T2, T2b)
- *(config)* Binding `map` schema + load-time validation (leaf-aliasing T1)
- *(preview)* Annotate templated files; README templating section (TPH-012)
- *(deploy)* Per-artifact vars digest; var change redeploys, lock untouched (TPH-010)
- *(deploy)* Minijinja render at stage time; manifest hashes rendered bytes, lock unchanged (TPH-009)
- *(config)* [vars] table + template opt-in (globs + .tmpl suffix) (TPH-008)
- *(cli)* --no-hooks flag + hook execution report in sync output (TPH-004)
- *(sync)* Dispatch on_change + post_sync hooks after commit (TPH-003)
- *(store)* Per-hook last-success digest-set records (TPH-002)
- *(config)* Hooks schema with mise value grammar (TPH-001)
- *(paths)* Resolve shared state via XDG base directories
- *(cli)* Add `phora preview` — offline dry-run projection tree
- *(cli)* Bind --branch/--tag/--rev; reject ref on link source; show effective ref
- *(sync,lock)* Lock keyed by (source, resolved commit); project per-binding ref
- *(source)* Mirror tags so one fetch covers tag/rev refs (fetch-once)
- *(config)* Validate per-target binding refs (url-ref, one-ref-max, distinct-as)
- *(config)* Per-target binding ref override (effective_ref) + lock back-compat golden
- *(cli)* Refinement flags on bind/add, identity-aware unbind and scrub
- *(config,sync)* Per-binding refinement engine — bindings, identity keying, lock split
- *(cli)* Symmetric source/target CLI with explicit sources and default-target DX
- *(cli)* Add `phora add --local/--symlink` for local overlay sources ([#14](https://github.com/srnnkls/phora/pull/14))
- *(config)* Source-key migration warnings (ARCH-015)
- *(config)* Typed source-kind keys (path=local, forge path->repo) + back-compat aliases
- [**breaking**] Drop the worktree-includes subsystem ([#12](https://github.com/srnnkls/phora/pull/12))
- *(lock)* Content identity for url sources + docs
- *(backend)* HttpBackend + mode-aware router; url-aware resolve
- *(source)* Deterministic synthetic-commit import via gix
- *(archive)* Extract tar/gz/zip/raw with traversal guard + auto-strip
- *(http)* Download + digest verification for url sources
- *(config)* Url source mode + DownloadDigest, three-way exclusivity
- *(config)* Default host to github; docs for host-aliased sources
- *(lock)* Protocol-independent source_matches (no spurious refetch)
- *(sync)* Thread resolved_remote through fetch/sync/discovery
- *(cli)* Add persists symbolic host/path instead of expanded URL
- *(source)* Resolved_remote + single built-in forge registry
- *(config)* Host/path/protocol + Host.remote string-or-table
- *(worktree)* Import-legacy + worktree subcommand group + docs
- *(worktree)* Apply guardrails — primary no-op, warn-and-continue
- *(worktree)* Stateless atomic apply engine + worktree apply CLI
- *(worktree)* Submodule include modes + is_submodule
- *(worktree)* Tracked-path guard via gix index
- *(worktree)* Primary-worktree detection via gix
- *(config)* Add [worktree].includes with IncludeMode
- *(sync)* Deploy-mode transitions link<->copy
- *(sync)* Quarantine linked artifacts from verify
- *(projection)* Atomic symlink link deployment
- *(sync)* Link-mode guardrails
- *(sync)* Link-source resolution sidesteps the mirror
- *(sync)* Mode-aware working-tree discovery
- *(registry)* Linked marker + ArtifactState::Linked
- *(config)* DeployMode enum + Source.deploy field
- *(cli)* Phora rebuild-registry
- *(cli)* Phora list + sync + update
- *(cli)* Phora add (URL parsing + host templates + toml_edit)
- *(cli)* Phora eject/uneject + verify
- *(cli)* Phora where + check-match
- *(sync)* Interactive conflict resolution + start-of-run recovery
- *(sync)* Phase 2 deploy/collision + Phase 3 prune
- *(sync)* Phase 1 orchestration (fetch/resolve/digest/lock routing)
- *(projection)* Crash-safe atomic swap + journal + recovery
- *(projection)* Copy_file, scan_dir, check_artifact_state
- *(registry)* FileRegistry CRUD, state.lock, ejected, reverse lookup
- *(source)* GitBackend export pipeline (discover/export/digest)
- *(source)* GitBackend fetch/resolve/commit_time via gix
- *(lock)* Source_matches + split_locks; extract lock module
- *(config)* Parsing, validation, and effective-config overlay
- Phora — git-sourced artifact sync (Rust)

### Bug Fixes

- *(release)* Pin tag format to v* and dispatch on the tagged ref
- *(release)* Guard cliff.toml unreleased footer on missing previous tag
- *(hooks)* Reject `cmd` + `shell` instead of silently dropping shell
- *(selection)* Resolve recorded rename dest to its source leaf in sealed-offer + explain
- *(query)* Surface offer-compile errors instead of empty fallback (review M4)
- *(deploy)* Preserve symlinks on cross-device copy fallback (review H2)
- *(selection)* Reject cross-binding ancestor/descendant overlap (review H1)
- *(cli)* Set_source_roots errors on absent source instead of silently dropping --root
- *(transitive)* Frozen offline-read, prune record-leak, hook prompt dedup
- *(transitive)* Unify frozen-miss diagnostic for dropped composed pins
- *(config)* Harden keyed bindings from multi-agent review
- *(config)* Reject legacy array-of-tables on source removal
- *(cli)* Explain empty list/where output instead of rendering blank
- *(config,sync)* Allow same-source duplicate identity; key collision on dest
- *(test,hk)* Per-doc TMPDIR + disable hk stash; gitignore /target
- *(ci)* Cargo fmt + drop stale Error-debug line from collision golden
- *(store)* Order list_target/list_all by (target, source, artifact)
- *(source)* Catch symlink deployed-name collisions, bound minijinja fuel, style nits (TPH-009 review)
- *(config)* Reject empty template list, guard bare .tmpl, align glob semantics (TPH-008 review)
- *(cli)* Accurate hook-failure diagnostic, lowercase status, style nits (TPH-004 review)
- *(sync)* Include shell in hook id to match dedupe key; uniform newline env delimiters (TPH-003 review r2)
- *(list)* Show machine-local overlays in `phora list`
- *(eject)* Keep the registry record so eject state is observable
- *(sync)* Redeploy outdated artifacts instead of treating them as foreign
- *(cli)* `add` recognizes local path sources
- *(config)* Warn on phora.local.toml alias keys too (review)
- *(sync)* Foreign scan consults Selection (stranded dotfile orphans)

### Performance

- *(rebuild)* Reuse parsed sources instead of re-parsing per binding (review M5)
- *(sync)* Default fetch pool to 2x cores via available_parallelism
- *(ci)* Run scrut docs in parallel (xargs -P0), ~7.7s -> ~2.8s

### Refactor

- *(kernel)* Drop as_dir bool-flag helper in translate (review M7)
- *(kernel)* Hoist published_key onto Materialization (review M3)
- *(source)* Remove dead allow_submodules export policy
- *(transitive)* Drop lock schema v2 bump — pre-alpha single schema
- *(review)* Address branch-diff review findings (M1–M8)
- *(sync)* Directional on_change trigger, HookOutcome reporting seam, stable hook ids (TPH-003 review)
- *(store)* Hook API on Registry trait + version canonicalization (TPH-002 review)
- *(config)* Review fixes — eq derives, merge characterization test (TPH-001)
- *(error)* Preserve kernel error chain; document edge construction rule (review)
- *(error)* Per-context SourceError/StoreError; ports return typed errors (ARCH-012)
- *(kernel)* Key HttpBackend digests by SourceName; rename bypass ctor to trusted() (review)
- *(kernel)* SourceName/ArtifactName newtypes at the boundary (ARCH-014)
- *(layout)* Rename projection->deploy, registry->store; split config.rs into config/; ProjectId->kernel (ARCH-011)
- *(cli)* Split god-module into cli/{add,render,query,sync} per command family (ARCH-010)
- *(sync)* Split god-module into sync/{resolve,discover,target,prune,verify,rebuild} (ARCH-009)
- *(config)* Parse Source into a Remote ADT once after merge (ARCH-006)
- *(backend,archive)* Drop adapter getters + confine gix (ARCH-007, ARCH-008)
- *(kernel)* Selection seam + dotfile opt-in (canary fix)
- *(kernel)* Unify Digest + add RelPath/Commit value objects

### Documentation

- *(release)* README install matrix + release runbook
- *(readme)* Transitive residual-risk statement (TDEP-DOCS-001)
- *(kernel)* Note plan_collapse per-binding quadratic tradeoff (review M1)
- *(readme)* Compose loqui into the loqui skill, not top level ([#43](https://github.com/srnnkls/phora/pull/43))
- *(readme)* Describe tropos accurately (agent-harness toolkit) ([#42](https://github.com/srnnkls/phora/pull/42))
- *(readme)* Document transitive dependencies; tighten emphasis ([#40](https://github.com/srnnkls/phora/pull/40))
- *(bindings)* Migrate README + scrut fixtures to keyed-table model
- *(readme)* Align identity rule with same-source duplicate relaxation
- Document mapped-leaf `map` surface + example (leaf-aliasing T10)
- *(readme)* Document hooks — config grammar, semantics, env, trust boundary (TPH-006)
- *(paths)* Document XDG resolution order and overrides
- Document per-target version (binding ref override) + stable/canary example
- Document per-binding refinement (Bindings section + example)
- Document source/target CLI, explicit sources, and default-target DX
- *(source)* Document content_digest contract (zero-churn pin)
- Note url/http sources in README About; mark http-sources scope done
- *(scope)* Final review (ready_to_merge); correct root-on-url note
- *(scope)* Record final review (ready_to_merge)
- *(scope)* Add submodule support (user reversal)
- *(scope)* Final review — ready to merge
- *(deploy-link)* Document deploy = link mode
- *(scopes)* Add host-alias-sources and http-sources scopes
- *(readme)* Rewrite README and example for the Rust implementation
- *(scopes)* Add deploy-link-dev-mode and worktree-include scopes

### Testing

- *(sync)* Update verify call sites for lock arg + is_clean
- *(trust)* Integration tests + scrut doc for trust inspection; fix --show commit coherence
- *(selection)* SMR-042 acceptance scrut suite for the new selection model (batch13)
- *(scrut)* Update where/list goldens for empty-result explanations
- *(fixtures)* Sandbox-guard git helpers and isolate global config
- *(scrut)* Expect portability warning on committed absolute-path link
- *(scrut)* E2e mapped-leaf fan-out, rename/prune, link, collision, eject (leaf-aliasing T9)
- *(scrut)* Templates.md e2e suite — render, two-machine locks, var redeploy (TPH-011)
- *(sync)* Pin INV-8 lock digest to golden constant, not compute_digest oracle (TPH-009 review)
- *(sync)* RED render-pipeline suite + minijinja dep (TPH-009)
- *(config)* INV-8 byte-stability over field-absence proxy (TPH-008 review)
- *(config)* RED [vars] + template opt-in suite (TPH-008)
- *(scrut)* End-to-end hooks suite — acceptance criteria + INV-1 inertness (TPH-005)
- *(cli)* RED --no-hooks flag + hook report suite (TPH-004)
- *(sync)* Make INV-4 non-recording assertion id-scheme independent (TPH-003 review)
- *(sync)* RED hook-dispatch suite (TPH-003)
- *(store)* RED hook-state registry suite (TPH-002)
- *(config)* Pin stored when value (TPH-001 review round 2)
- *(config)* Address test-review findings (TPH-001)
- *(config)* RED hooks schema suite (TPH-001)
- *(scrut)* Query, manage, and showcase suites (T4, T5, T6)
- *(scrut)* Lifecycle suite + normalize substitution (T3)
- *(scrut)* Mise scrut harness + fixture helpers
- *(arch-000)* Golden snapshot + digest-pin harness
- *(config)* Generic protocol-template validation + example guard
- *(worktree)* Binary-level acceptance oracle + fix --path config load
- *(source)* Pin URL normalization + mirror keying
- *(matcher)* Pin classification, anchoring, and eval-order

### Styling

- Cargo fmt src/source.rs
- *(paths)* Rustfmt src/paths.rs
- Rustfmt normalize split test modules
- Normalize rustfmt across the tree
- *(deploy-link)* Rustfmt + clippy semicolon fixup

### Miscellaneous Tasks

- Cache cargo artifacts, split lint/test from integration
- Ensure clippy+rustfmt components after mise cache restore
- Install rust default profile so clippy+rustfmt exist on CI
- Add lint + test + scrut integration workflow (T7)
- Stop tracking scopes/ (local-only working notes) ([#15](https://github.com/srnnkls/phora/pull/15))
- *(scope)* Checkpoint architecture-cleanup after ARCH-012 (batch 9)
- *(scope)* Checkpoint architecture-cleanup after ARCH-014 (batch 8)
- *(scope)* Checkpoint architecture-cleanup after ARCH-015 (batch 7)
- *(scope)* Port round-2/3 doc deltas — ARCH-014 newtypes, ARCH-012 mandatory, ARCH-015 migration warnings
- *(scope)* Check off acceptance criteria; record ARCH-012/013 deferral
- *(scope)* Reopen architecture-cleanup; defer ARCH-012/013 to a future session (not descoped)
- *(scope)* Mark architecture-cleanup done (ARCH-000..011; 012/013 descoped)
- *(scope)* Checkpoint architecture-cleanup after ARCH-011 (batch 6); descope ARCH-012/013
- *(scope)* Checkpoint architecture-cleanup after ARCH-010 (batch 5)
- *(scope)* Checkpoint architecture-cleanup after ARCH-009 (batch 4)
- *(scope)* Checkpoint architecture-cleanup after Phase 2 (ARCH-006..008, batch 3)
- *(scope)* Checkpoint architecture-cleanup after ARCH-005 (batch 2)
- *(scope)* Checkpoint architecture-cleanup after Phase 1 (ARCH-000..004)
- *(scope)* Mark host-alias-sources done
- *(scope)* Mark worktree-include done
- *(scope)* Mark deploy-link-dev-mode done
- *(cli)* Remove stale needless_pass_by_value suppression
- Align with modern-Rust/loqui conventions

### Bench

- *(fetch)* Manual network sweep harness for fetch I/O-vs-CPU gate
- *(sync)* Criterion bench for PAR-001 parallel resolve

### Build

- *(deps)* Enable gix HTTPS transport (reqwest + rustls); ignore /resources/
- *(hk)* Drop pre-push hook
- *(hk)* Pre-commit fmt+clippy, pre-push full CI mirror
- *(mise)* Pin rust to 1.96.0


## [Unreleased]
