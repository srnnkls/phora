# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.3](https://github.com/srnnkls/phora/compare/v0.4.2...v0.4.3) - 2026-10-15

### Bug Fixes

- *(state)* Wait for a held state.lock up to a deadline ([#136](https://github.com/srnnkls/phora/pull/136))

[0.4.3]: https://github.com/srnnkls/phora/compare/v0.4.2...v0.4.3

## [0.4.2](https://github.com/srnnkls/phora/compare/v0.4.1...v0.4.2) - 2026-10-05

### Bug Fixes

- *(sync)* Remove directories emptied by pruned artifacts ([#133](https://github.com/srnnkls/phora/pull/133))

[0.4.2]: https://github.com/srnnkls/phora/compare/v0.4.1...v0.4.2

## [0.4.1](https://github.com/srnnkls/phora/compare/v0.4.0...v0.4.1) - 2026-10-01

### Bug Fixes

- *(cli)* Eject and uneject a target declared in phora.local.toml ([#131](https://github.com/srnnkls/phora/pull/131))

[0.4.1]: https://github.com/srnnkls/phora/compare/v0.4.0...v0.4.1

## [0.4.0](https://github.com/srnnkls/phora/compare/v0.3.3...v0.4.0) - 2026-10-01

### Breaking Changes

- *(transitive)* A target binds a transitive source through `sources` and picks one of its offers with `offer`; the `imports` key and the target-level `[take]`/`[collapse]` tables are gone (a binding's `take`/`collapse` apply instead).
- *(transitive)* A dependency publishes through `[offers]`: its top-level `[targets]` are offered by the implicit `default` offer when their paths are relative and their sources can be fetched; a `path = "."` source no longer publishes the repo's own files.
- *(build)* A dependency build names a tool (`tool = "<identity>@<version>"`) that the consumer grants under `[tools]`; `phora.local.toml` cannot declare `[offers]`.

### Features

- *(build)* Offer builds that run a consumer-granted tool ([#130](https://github.com/srnnkls/phora/pull/130))
- *(transitive)* Bind transitive sources and select offered targets by key ([#129](https://github.com/srnnkls/phora/pull/129))

### Bug Fixes

- *(sync)* Adopt the clean orphan when a stale one shares its destination ([#128](https://github.com/srnnkls/phora/pull/128))

[0.4.0]: https://github.com/srnnkls/phora/compare/v0.3.3...v0.4.0

## [0.3.3](https://github.com/srnnkls/phora/compare/v0.3.2...v0.3.3) - 2026-09-30

### Bug Fixes

- *(sync)* Adopt bindings moved between live targets at their destinations ([#123](https://github.com/srnnkls/phora/pull/123))
- *(cli)* List composed import records under their importing target ([#122](https://github.com/srnnkls/phora/pull/122))

[0.3.3]: https://github.com/srnnkls/phora/compare/v0.3.2...v0.3.3

## [0.3.2](https://github.com/srnnkls/phora/compare/v0.3.1...v0.3.2) - 2026-09-30

### Features

- *(config)* Warn when a source is bound by no target, import, or build ([#117](https://github.com/srnnkls/phora/pull/117))

### Bug Fixes

- *(transitive)* Apply the importing source's offer to its composed package ([#121](https://github.com/srnnkls/phora/pull/121))
- *(sync)* Adopt orphaned records at restructured targets' destinations ([#120](https://github.com/srnnkls/phora/pull/120))
- *(offer)* Match a slash pattern without trailing slash as a directory too ([#116](https://github.com/srnnkls/phora/pull/116))

[0.3.2]: https://github.com/srnnkls/phora/compare/v0.3.1...v0.3.2

## [0.3.1](https://github.com/srnnkls/phora/compare/v0.3.0...v0.3.1) - 2026-09-30

### Features

- *(cache)* Prune content-store objects no deployment records ([#114](https://github.com/srnnkls/phora/pull/114))
- *(sync)* Verify every content-store clone against its bytes ([#113](https://github.com/srnnkls/phora/pull/113))
- *(sync)* Reflink copy deployments from a content store ([#112](https://github.com/srnnkls/phora/pull/112))
- *(source)* Build sources replace prepare targets ([#104](https://github.com/srnnkls/phora/pull/104))
- *(history)* Eject a history overlay into a standalone clone ([#101](https://github.com/srnnkls/phora/pull/101))

### Bug Fixes

- *(sync)* Keep a shadowed pin at its prior lock position ([#111](https://github.com/srnnkls/phora/pull/111))
- *(sync)* Keep shared pins for locally overridden sources ([#110](https://github.com/srnnkls/phora/pull/110))
- *(sync)* Deploy history checkouts verbatim ([#109](https://github.com/srnnkls/phora/pull/109))
- *(source)* Keep credential helpers from prompting without a terminal ([#107](https://github.com/srnnkls/phora/pull/107))
- *(source)* Pass configured credential username to helpers ([#106](https://github.com/srnnkls/phora/pull/106))
- *(sync)* --force overwrites without re-resolving sources ([#102](https://github.com/srnnkls/phora/pull/102))
- *(sync)* Deploy files with their write time ([#103](https://github.com/srnnkls/phora/pull/103))
- *(history)* Make overlays read-only and close git race windows ([#100](https://github.com/srnnkls/phora/pull/100))

### Performance

- *(source)* Fetch sliced mirrors without blobs and backfill selected files ([#99](https://github.com/srnnkls/phora/pull/99))
- *(source)* Clone mirrors as depth-1 slices unless a binding needs history ([#98](https://github.com/srnnkls/phora/pull/98))

[0.3.1]: https://github.com/srnnkls/phora/compare/v0.3.0...v0.3.1

## [0.3.0](https://github.com/srnnkls/phora/compare/v0.2.0...v0.3.0) - 2026-09-24

### Features

- *(transitive)* Resolve linked packages from the working tree ([#91](https://github.com/srnnkls/phora/pull/91))
- *(take)* Re-root subtrees with directory renames ([#90](https://github.com/srnnkls/phora/pull/90))
- *(sync)* Prepare inputs before resolving generated sources ([#89](https://github.com/srnnkls/phora/pull/89))
- *(imports)* Compose local packages with transitive sources ([#88](https://github.com/srnnkls/phora/pull/88))
- *(cli)* Emit NDJSON from `phora sync --json` ([#83](https://github.com/srnnkls/phora/pull/83))
- *(cli)* Draw live progress and a rich summary on a terminal ([#85](https://github.com/srnnkls/phora/pull/85))

### Bug Fixes

- *(sync)* Check prepare overlap against deployed artifacts ([#92](https://github.com/srnnkls/phora/pull/92))
- *(config)* Expand home paths for local sources ([#87](https://github.com/srnnkls/phora/pull/87))
- *(add)* Confirm a missing `--to` target instead of asking for its path ([#86](https://github.com/srnnkls/phora/pull/86))

### Performance

- *(sync)* Skip unchanged directory walks; journal-covered registry writes ([#79](https://github.com/srnnkls/phora/pull/79))

[0.3.0]: https://github.com/srnnkls/phora/compare/v0.2.0...v0.3.0

## [0.2.0](https://github.com/srnnkls/phora/compare/v0.1.2...v0.2.0) - 2026-09-14

### Features

- *(config)* Default the schema version when omitted ([#69](https://github.com/srnnkls/phora/pull/69))
- *(history)* Git-history overlay on copy-mode bindings ([#68](https://github.com/srnnkls/phora/pull/68))
- *(sync)* Refuse a sync where two targets deploy to the same destination or one inside another
- *(sync)* Whole-run preflight conflict resolution; kind-carrying Conflict
- Advise on network-filesystem state roots
- Sync --frozen falls back lockless on a read-only state root
- Orphan visibility with physical prune, per-clone project identity
- Refuse target rm with live artifacts, persist deploy roots, pin unicode folds
- *(sync)* Add --fast-forward to follow moved pins on drop
- *(preview)* Show each target's deploy path ([#60](https://github.com/srnnkls/phora/pull/60))

### Bug Fixes

- *(source)* Accept global pax headers in url archives ([#77](https://github.com/srnnkls/phora/pull/77))
- *(sync)* Handle shared and dangling deployment links ([#76](https://github.com/srnnkls/phora/pull/76))
- *(sync)* Compare link destination entries for overlap ([#73](https://github.com/srnnkls/phora/pull/73))
- *(source)* Fix aarch64 git pack decoding ([#75](https://github.com/srnnkls/phora/pull/75))
- *(sync)* Run pre_sync before source discovery ([#72](https://github.com/srnnkls/phora/pull/72))
- *(sync)* Run every `pre_deploy` gate before any drop or target change is applied
- *(sync)* Reuse conflict answers when state is re-read after `pre_deploy` hooks
- *(sync)* Resolve symlinks and letter case when checking targets for overlapping destinations
- *(sync)* Treat relative and absolute spellings of a target root as the same path in overlap checks
- *(sync)* Reject overlapping targets before `--fast-forward` drops delete any file or record
- *(http)* Reject non-success final status before writing download ([#64](https://github.com/srnnkls/phora/pull/64))
- Gate legacy-registry adoption on the frozen read-only fallback
- *(source)* Saturate symlink escape depth increment ([#63](https://github.com/srnnkls/phora/pull/63))
- *(source)* Treat drive-letter prefixes as filesystem paths
- *(source)* Validate symlink target bytes at stage time
- *(sync)* Detect deploy-name collisions that differ only in letter case
- *(sync)* Report a switch between link and copy mode as a conflict
- *(config)* Drop a stale url digest when a local override changes the source url
- *(sync)* Keep prune records for destinations that another binding also deploys
- *(add)* Parse urls with nested groups or a trailing slash
- *(bind)* Check sources and targets against the merged config for `bind --local`
- *(http)* Follow download redirects only to allowed schemes
- *(sync)* Guard fast-forward drops against live artifacts and out-of-anchor paths ([#62](https://github.com/srnnkls/phora/pull/62))
- *(sync)* Only report a binding's pin when it actually moves
- *(sync)* Honor the ejected list in the sealed-offer guard ([#59](https://github.com/srnnkls/phora/pull/59))
- *(sync)* Treat a collapse-key flip as redeploy, not a Foreign conflict ([#58](https://github.com/srnnkls/phora/pull/58))

### Performance

- *(sync)* Skip unchanged directory walks and redundant flushes ([#74](https://github.com/srnnkls/phora/pull/74))
- *(sync)* Reuse opened mirrors, barrier fsync, single-open staging ([#71](https://github.com/srnnkls/phora/pull/71))
- *(sync)* Reuse locked source digest on no-op ([#70](https://github.com/srnnkls/phora/pull/70))
- *(sync)* Raise the default fetch ceiling floor to 50 threads

### Documentation

- *(sync)* Clarify that 50 floors the pool cap, not the thread count ([#65](https://github.com/srnnkls/phora/pull/65))

[0.2.0]: https://github.com/srnnkls/phora/compare/v0.1.2...v0.2.0

## [0.1.2](https://github.com/srnnkls/phora/compare/v0.1.1...v0.1.2) - 2026-07-01

### Bug Fixes

- *(source)* Keep a staging dir when its mtime is unreadable ([#55](https://github.com/srnnkls/phora/pull/55))
- *(source)* Re-fetch a locked source when its mirror cache is gone
- *(source)* Self-heal a corrupt mirror and sweep orphaned staging dirs

[0.1.2]: https://github.com/srnnkls/phora/compare/v0.1.1...v0.1.2

## [0.1.1](https://github.com/srnnkls/phora/compare/v0.1.0...v0.1.1) - 2026-06-30

### Bug Fixes

- *(release)* Define [profile.dist] for cargo-dist builds

[0.1.1]: https://github.com/srnnkls/phora/compare/v0.1.0...v0.1.1
