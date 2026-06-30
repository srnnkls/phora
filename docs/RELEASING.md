# Releasing phora

Operator runbook for the `phora` release pipeline. The pipeline is a two-workflow handoff:

- `.github/workflows/release-plz.yml` — opens/maintains the release PR, then on merge bumps the version, regenerates the changelog, publishes to crates.io, pushes the `v*` tag, and dispatches the build workflow.
- `.github/workflows/release.yml` — cargo-dist; on dispatch builds the platform matrix, attaches installers and attestations, creates the GitHub Release, and pushes the Homebrew formula.

Every secret, job, and command below is taken verbatim from those two workflows. Keep this file in sync with them.

## Secrets

Two repository secrets must be set. `GITHUB_TOKEN` is provided automatically by Actions and is not configured by an operator.

| Secret | Scope | Consuming job |
| --- | --- | --- |
| `CARGO_REGISTRY_TOKEN` | crates.io publish token | `release-pr` and `release` jobs in `release-plz.yml` |
| `HOMEBREW_TAP_TOKEN` | `contents:write` on `srnnkls/homebrew-phora` | `publish-homebrew-formula` job in `release.yml` |
| `GITHUB_TOKEN` (automatic) | repo-scoped; `contents:write` + `actions:write` on the `release` job to dispatch `release.yml` | every job in both workflows |

`CARGO_REGISTRY_TOKEN` is mandatory for the first publish: crates.io Trusted Publishing (OIDC) can only be configured after the crate already exists. The first release is therefore token-first; migrating to OIDC is an optional follow-up once `phora` is on crates.io.

The crates.io path (`CARGO_REGISTRY_TOKEN`) and the Homebrew path (`HOMEBREW_TAP_TOKEN`) are distinct and independent — one can be configured and tested without the other.

The tag-to-build handoff needs no PAT: the `release` job dispatches `release.yml` with `gh workflow run` over `workflow_dispatch` under the automatic `GITHUB_TOKEN`, which the job grants `actions: write` for that purpose.

## Prerequisites and first-publish bootstrap

Do these once, in order, before the first release:

1. Claim the `phora` name on crates.io (publish requires the name to be available or already owned).
2. Create the empty repository `srnnkls/homebrew-phora`. cargo-dist pushes the generated formula into it; the repo must exist first.
3. Set both repository secrets:
   - `CARGO_REGISTRY_TOKEN` — a crates.io API token with publish scope.
   - `HOMEBREW_TAP_TOKEN` — a token with `contents:write` on `srnnkls/homebrew-phora`.

### First 0.1.x release

Treat the very first `0.1.x` release as a throwaway end-to-end proof of the release-plz to cargo-dist dispatch handoff — its purpose is to exercise the path, not to ship a meaningful artifact.

1. Push to `main`. `release-plz.yml` opens a release PR.
2. Merge the release PR. The `release` job publishes to crates.io, pushes the `v0.1.x` tag, and (because `releases_created` is true) runs `gh workflow run release.yml --ref v0.1.x -f tag=v0.1.x`.
3. Watch `release.yml` complete the matrix build, the GitHub Release, and the Homebrew formula push.
4. If any stage fails, use the recovery steps below; the dispatch handoff is the stage most likely to need a manual retry on the first run.

## Dry-run procedure

All commands use the pinned mise tools. Run from the repository root.

```bash
mise exec -- release-plz release --dry-run
mise exec -- dist plan
mise exec -- dist generate --check
mise exec -- actionlint .github/workflows/*.yml
```

- `release-plz release --dry-run` — shows what would be published without publishing.
- `dist plan` — prints the build matrix and artifacts cargo-dist would produce.
- `dist generate --check` — fails if the committed `release.yml` has drifted from what cargo-dist would generate from `dist-workspace.toml`.
- `actionlint .github/workflows/*.yml` — lints both workflows.

## Normal release flow

1. Push to `main`. The `release-pr` job in `release-plz.yml` opens or updates a release PR. (A weekly `schedule` and `workflow_dispatch` also trigger it.)
2. Review and merge the release PR.
3. On merge, the `release` job:
   - bumps the version and regenerates `CHANGELOG` via git-cliff (`cliff.toml`),
   - publishes the crate to crates.io,
   - pushes the `v*` tag. Because `git_release_enable = false` in `release-plz.toml`, release-plz does not create a GitHub Release — cargo-dist owns that.
   - when `steps.release.outputs.releases_created == 'true'`, dispatches the build: `gh workflow run release.yml --ref "$TAG" -f tag="$TAG"`, where `$TAG` is `fromJSON(steps.release.outputs.releases)[0].tag`.
4. `release.yml` then:
   - `plan` computes the matrix,
   - `build-local-artifacts` builds the 6-target matrix (`x86_64`/`aarch64` linux-gnu, `x86_64`/`aarch64` linux-musl, `aarch64`/`x86_64` apple-darwin) and produces SLSA attestations,
   - `build-global-artifacts` and `host` upload artifacts and create the GitHub Release,
   - `publish-homebrew-formula` pushes the formula to `srnnkls/homebrew-phora` using `HOMEBREW_TAP_TOKEN`,
   - `announce` finalizes.

## Recovery from a partial or failed publish

The dispatch handoff between the two workflows is the key failure mode: the crate and tag can land while the build never starts.

### Crate published and tag pushed, but the build did not run

The `release` job succeeded through publish and tag but the dispatch step was missed or failed. Manually dispatch the build with the exact emitted tag:

```bash
gh workflow run release.yml --ref v<x.y.z> -f tag=v<x.y.z>
```

This is the same `release.yml` + `--ref`/`-f tag=` contract the `release` job uses, so it resumes the build identically. Do not re-run the `release` job — the crate is already published and the tag already exists; re-publishing the same version fails.

### Build started but a later job failed

- `publish-homebrew-formula` failed (binaries built, GitHub Release created) — re-run the failed job from the Actions run. Confirm `HOMEBREW_TAP_TOKEN` is valid and `srnnkls/homebrew-phora` exists.
- `host` or a build job failed mid-matrix — re-run the failed jobs; no new dispatch is needed while the workflow run still exists.

### Crate publish itself failed

If crates.io publish failed, no tag was pushed. Fix the cause (token, name ownership, version conflict) and re-run the `release` job.

## Residual risk

The automatic dispatch from `release-plz.yml` to `release.yml` has no role-model precedent — uv and similar projects dispatch `release.yml` manually rather than chaining it from the version-bump job. The first real `0.1.x` release is the only true end-to-end test of this handoff. Until it has run green once, treat the dispatch step as unproven and keep the manual `gh workflow run release.yml --ref v<x.y.z> -f tag=v<x.y.z>` recovery command ready.
