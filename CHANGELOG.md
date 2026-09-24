# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
