# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3](https://github.com/srnnkls/phora/compare/v0.1.2...v0.1.3) - 2026-07-02

### Features

- *(sync)* Add --fast-forward to follow moved pins on drop
- *(preview)* Show each target's deploy path ([#60](https://github.com/srnnkls/phora/pull/60))

### Bug Fixes

- *(sync)* Guard fast-forward drops against live artifacts and out-of-anchor paths ([#62](https://github.com/srnnkls/phora/pull/62))
- *(sync)* Only report a binding's pin when it actually moves
- *(sync)* Honor the ejected list in the sealed-offer guard ([#59](https://github.com/srnnkls/phora/pull/59))
- *(sync)* Treat a collapse-key flip as redeploy, not a Foreign conflict ([#58](https://github.com/srnnkls/phora/pull/58))

[0.1.3]: https://github.com/srnnkls/phora/compare/0.1.2..0.1.3

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
