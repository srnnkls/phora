---
created: 2026-06-08
status: active
issue_type: Feature
---

# http-sources

## Goal

Add a third source kind — a plain-https resource (tarball, zip, or single file) declared with
`url = "https://…"` — that flows through phora's **existing** git-ODB store and projection pipeline by
being imported as a synthetic, content-addressed git commit. One store, one digest model, one
verify/projection path; the new code is confined to download + extract + import behind the existing
`SourceBackend` port.

## Context

Today `GitBackend` is the only `SourceBackend` (`source.rs:54`); the store is exclusively
`~/.phora/git/<MirrorKey>.git` bare mirrors, and `fetch` git-clones (`source.rs:171`). A non-git https
resource is unrepresentable — pointing the clone at a tarball just errors. There is no http/archive
source kind and no http/archive deps (`flate2`/`tar`/`zip`/an http client are all new).

**The store is a general CAS.** gix 0.84 exposes object writing (`write_blob`/`write_object`), so an
arbitrary directory tree can be imported as git objects and resolved to a commit. Once content is a
git tree, `discover_artifacts`, `export_artifact`, and `compute_digest` (`source.rs:60-78`) work
**unchanged**. So an http source needs only: download → (verify) → extract → strip → import to a
synthetic commit; everything downstream is reused.

This is the unification approach (Backend A) chosen over a separate http cache — it keeps phora's
single-substrate elegance and gives http sources the same content integrity git sources have (which
vendir's http sources lack).

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Declaration | `url = "https://…"` (third mode); source is **git** XOR **host+path** XOR **url** | `url` is free (git uses `git`/`host`+`path`); unambiguous. (user-chosen) |
| Store | Synthetic bare mirror at `~/.phora/git/<MirrorKey>.git`, `MirrorKey = blake3(NormalizedUrl)` | One unified store; reuses mirror keying + projection/verify/journal. |
| Import | Download → extract → build blobs/trees → `commit` via gix, with **fixed identity + fixed time (epoch 0) + constant message** ⇒ commit id = f(content only) | Deterministic & content-addressed: identical bytes ⇒ identical commit ⇒ no lock churn; `update` makes a new commit iff content changed. |
| Resolve | `fetch` writes the synthetic commit + `refs/heads/phora`; `resolve` reads that ref and **ignores the refspec**. A url source carries a new `Refspec::None` so `resolve_sources` never passes the default `Branch("main")` | review-CRITICAL: `source.refspec()` defaults to `Branch("main")` (`config.rs:181`); without `None`, `HttpBackend::resolve` would look up `refs/heads/main`. |
| `commit_time` | Sentinel **epoch+1** ⇒ deterministic exported mtimes | review: epoch 0 is clamped on some filesystems (HFS+/FAT32) → perpetual `Modified`; +1 avoids it. |
| Formats | `tar`, `tar.gz`/`tgz`, `zip`, and a raw single file (tree with one entry named from the URL basename) | Covers release assets; detected by extension/magic. |
| Strip | Auto-strip a single top-level dir when the archive has exactly one; `root` is **rejected** on a url source (archive is pre-stripped — review-elevated, HTP-001) | Handles version-stamped GitHub tarballs without a brittle per-version `root`. (user-chosen; root-on-url rejection added in review) |
| Integrity | Optional `digest = "<algo>:<hex>"` (`sha256:`/`blake3:`), verified **before** import; mismatch errors | Supply-chain safety. review-MEDIUM: use a **new `DownloadDigest` type**, NOT `registry::Digest` (which is blake3-only and feeds projection verify — must not be widened). (user-chosen) |
| Dispatch | `RouterBackend` (a `SourceBackend`) holds Git+Http backends + a **source-name → mode** map and dispatches each call on the `source` **name** param — **not** the url scheme | review-CRITICAL: a git source legitimately uses `https://…​.git`, so scheme-based routing mis-routes it. The trait already passes the source name, so `sync(&dyn SourceBackend)` stays unchanged. |
| Refspec validation | `branch`/`tag`/`rev` on a `url` source ⇒ config error | They have no meaning for a static resource. |

## Requirements

### Behavior
- `[sources.X] url = "https://…/foo.tar.gz"` (+ optional `digest`, `root`, `include`/`exclude`)
  downloads, optionally verifies, extracts, auto-strips a single top dir, imports as a synthetic
  commit, then discovers/exports/deploys exactly like a git source.
- A raw (non-archive) `url` becomes a one-file tree.
- `phora sync` over an unchanged `url` (same bytes) is a no-op (same synthetic commit, lock matches).
- `phora update` re-downloads; the lock advances only if the content changed.
- `phora verify` re-hashes deployed files against the record — identical guarantees to git sources.

### Given/When/Then
- **Given** a `url` source with a `digest` that doesn't match the download, **then** sync errors before
  extraction (naming the source, expected vs actual).
- **Given** a `url` source that also sets `git`/`host`/`branch`/`tag`/`rev`, **then** a config error.
- **Given** an archive containing a path-traversal entry (`../`), **then** extraction rejects it
  (reuse the `safe_component` guard, `source.rs:542`).
- **Given** two `url` sources with byte-identical content at different URLs, **then** each keys its own
  mirror (no cross-URL dedup) but both import deterministically.

## Key Files

| File | Change |
|---|---|
| `src/config.rs` | `Source.url`, `Source.digest`; three-way mode exclusivity + refspec-on-url rejection; `merged_with`. |
| `src/source.rs` | `HttpBackend` implementing `SourceBackend`; share the git-tree `discover`/`export`/`compute_digest` with `GitBackend`; deterministic synthetic-commit import via gix object write. |
| `src/backend.rs` (new) | `SourceBackend` router dispatching per source mode. |
| `src/sync.rs` | Use the router; thread source mode to backend selection. |
| `src/lock.rs` | Lock pins the synthetic commit; `source_matches` stable when content unchanged. |
| `src/cli.rs` | `add` may accept a direct `url` (optional); `list`/`where` show url sources. |
| `Cargo.toml` | New deps: http client (e.g. `ureq` + rustls), `flate2`, `tar`, `zip`, `sha2`. |
| `README.md` / `phora.example.toml` | Document `url` sources, `digest`, formats, auto-strip. |

## Out of Scope
- Auth for private assets (bearer-token env) — a small follow-up; v1 targets public URLs.
- GitHub/GitLab **release** resolution (latest tag → asset URL) — that's a host/forge feature, not raw http.
- Last-Modified-derived mtimes (epoch 0 sentinel for v1).
- Incremental/range downloads or caching beyond the synthetic mirror.

## Coordination (HARD sequence — review-elevated)
- **Land `host-alias-sources` first.** Both scopes mutate the SAME `Source` struct, the SAME
  mode-exclusivity validator, and `merged_with`; landing independently guarantees a merge conflict in
  `config.rs:30-43` and `:132-160`. host-alias-sources should factor `Source::mode() -> SourceMode` +
  the mode-aware `merged_with` and the `git → Option` change; this scope then only **adds the `url`
  arm** (two-way → three-way) and the `Source::source_url()` accessor. Sequence, don't parallelize.

## Verification
- `cargo test` green; new tests per task.
- A `url` tarball source resolves/fetches/deploys/verifies identically to an equivalent git source;
  re-sync of identical content is a no-op; changed content advances the lock.
- Each format (tar/tgz/zip/raw) extracts; single-top-dir auto-strip works; traversal is rejected.
- A `digest` mismatch errors before import; a matching `digest` passes.
- **Spike first:** deterministic synthetic-commit import via gix (`write_blob`/`write_object` →
  `commit`), asserting identical bytes ⇒ identical commit id across runs.
