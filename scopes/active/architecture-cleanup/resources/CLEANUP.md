# Architecture cleanup

The target is a **hexagonal, feature-organized** codebase that follows **parse, don't
validate** and puts **ADTs first**. Modules are **bounded contexts** (a feature owns its
domain types, ports, logic, errors end-to-end), not a single god-module (`sync.rs`, 5 655
lines) feeding mechanical helpers. The reference shape is the gestalt `architecture-cleanup`
(Rust: *trait = port, struct = adapter*, feature modules, kernel value objects) re-seated onto
phora's domain.

phora is already on **edition 2024** with clippy `pedantic` — modern Rust is a *gate to hold*,
not a migration to run. This document is authoritative; `scope.md` is the contract.

> Revised after the opus+sonnet review (`review.yaml`). Key corrections from the first draft:
> the source-level digest is **left untouched** (it has no behavioral consumer and the dotfile
> feature needs no digest change → zero lock churn); the **worktree subsystem was dropped in
> PR #12** and is gone from the design; the over-engineered tail (Clock port, formal sealing,
> name newtypes, per-context errors) is cut or deferred; line references are re-pinned.

## The canary

The change that surfaced this: *"top-level dotfile directories can't be opted into as
artifacts."* Tracing it showed the rule **"what counts as an artifact of this source"** has no
home — it is re-decided at five sites that already disagree:

| Site | `file:line` | Skips `.`? | Applies matcher? | Governs |
|------|-------------|:---:|:---:|---------|
| Git ODB discovery | `source.rs:274` | yes | yes (`allows_artifact`) | what **deploys** (copy) |
| Link-mode discovery | `sync.rs:699` | yes | yes | what **deploys** (link) |
| Source content digest (`hash_tree`) | `source.rs:397` | **no** | path-level only (`allows_path`) | recorded in the **lock** |
| Foreign scan (root) | `sync.rs:1151` | yes | **no** | what **prunes** |
| Foreign scan (nested) | `sync.rs:1204` | yes | **no** | what **prunes** |
| `check-match` debug | `cli.rs:605` | **no** | yes | user output |

What the disagreement actually causes (corrected after review):

1. **A latent inconsistency, not a runtime defect.** `hash_tree` hashes top-level dotfile
   directories (and loose root files, and artifact-excluded root dirs) into `LockedSource.digest`;
   discovery deploys none of those. But `LockedSource.digest` has **no production consumer** —
   `source_matches` (lock.rs:52-70) gates lock-reuse on `commit + config_digest`, never on
   `digest`; the only reads are test assertions (sync.rs:1529-1542). So the digest is a *recorded
   resolved-subtree fingerprint*, not a deploy manifest, and the divergence changes no behavior.
   The lesson is "membership has no home," **not** "the digest is wrong" — so the fix is to give
   membership a home, and **leave the digest alone** (changing its domain would re-baseline locks
   broadly for zero behavioral benefit).
2. **One real defect — stranded orphans.** An orphaned dotfile artifact is removed from the
   registry by `prune_orphans` (registry-driven) but never flagged foreign (the scan skips
   dotfiles unconditionally, sync.rs:1151/1204), so `--prune` strands the on-disk directory.

The dotfile opt-in itself needs **no digest change**: `hash_tree` already hashes a top-level
`.config` (via `allows_path`, which has no dotfile rule), so opting it in deploys it with no
fingerprint churn. The point of this scope is to give membership a single home so the five sites
can't drift, fix the prune bug, and re-seat the boundaries that let the rule scatter.

## Four principles

### 1. Parse, don't validate — type at the boundary, trust the interior
Raw inputs (TOML, archive entries, git tree names, paths, digest strings) are parsed **once, at
the edge**, into domain values; the interior takes already-valid types.

| Boundary | Today | Target |
|----------|-------|--------|
| Source declaration | `Source { git/url/host/path: Option, branch/tag/rev: Option }`; `git XOR host+path XOR url` and `branch XOR tag XOR rev` checked by `u8::from(is_some())` counting in `Source::parse` (`config.rs:33`) **and again** `Source::validate` (`config.rs:100`) | `ParsedSource { remote: Remote, refspec: Refspec, … }`, `Remote = Git{..} \| Host{..} \| Url{..}` — illegal combinations unrepresentable; validated once |
| Artifact membership | five open-coded predicates (table above) | one `Selection` value object: `selects_artifact` / `selects_path`, dotfile policy included |
| Content digest | `DownloadDigest` ADT (`config.rs:446`, bytes) **and** registry `Digest` (`registry.rs:57`, a `String` wrapper) — two types, one concept | one kernel `Digest { algo, [u8;32] }`, parsed once, rendered at the edge |
| Relative paths | ad-hoc `normalize`/`strip_prefix` at call sites (`paths.rs` + scattered) | `RelPath` normalized at construction |
| Commit / rev | `String`, hex-parsed lazily at `source.rs:217` | `Commit` validated at the resolve boundary |

`SourceName`/`ArtifactName` newtypes were **considered and cut** (review OE4): names are already
validated once — at the TOML parse boundary and at the git-tree boundary via `safe_component`
(`source.rs:853`) — so wrapping them churns ~50 call sites to delete four re-checks. Keep the
high-value newtypes (`RelPath`, `Digest`, `Commit`); leave names as `&str`.

### 2. ADTs first — make illegal states unrepresentable
phora already has good ADTs to **preserve**: `Refspec`, `DeployMode`, `SourceMode`, `LayoutKind`,
`IncludeMode`, `DownloadDigest`, `ArtifactState`, `Resolution`, `ConflictKind`, `Protocol`. The
gap is at the parse boundary, where `SourceMode` is **derived at runtime** (`source.mode()`,
`config.rs:622`) from a struct that can hold zero or three modes at once.

- **`Remote`** — typed source-kind keys (the uv model), modeled as a proper sum type with a
  distinct payload per kind, not a bag of `Option`s + a validator:
  ```rust
  enum Remote {
      Git(GitUrl),                                        // git  = "<git/ssh/scp url>"
      Path(LocalPath),                                    // path = "<local dir/file>"  (incl. non-repo, for link mode)
      Url  { url: NormalizedUrl, digest: Option<Digest> },// url  = "<download>"
      Host { host: HostName, repo: ForgePath, protocol: Protocol }, // host = "<alias>" + repo = "<owner/repo>"
  }
  ```
  Each variant carries only its own fields, so "exactly one source kind" and "url forbids
  `branch`/`tag`/`rev`/`root`" stop being runtime checks — they are the type. The shared
  per-source fields (`include`, `exclude`, `deploy`, `allow_*`, and `root` for git/host) stay on
  `ParsedSource`; only the source kind + refspec become the sum type. The wire deserialization
  picks the variant by which key is present: `git`→`Git`, `path`→`Path`, `url`→`Url`,
  `host`/`repo`→`Host`.

  This is the full misnomer fix. Two key changes, each with a back-compat alias:
  - The forge path key is renamed **`path`→`repo`** (it identifies `owner/repo`, never a path).
    Bare `repo` keeps the github default (`repo = "srnnkls/tropos"` ⇒ github); `host` makes the
    forge explicit. `host`+`path` stays accepted as a deprecated alias for `host`+`repo`.
  - The local case splits OUT of `git` into **`path`** (now free). `git` becomes honestly
    git-only; `git = "<localpath>"` stays accepted as a back-compat alias for `path`.

  The one intentional break: bare `path = "owner/repo"` (the old github forge shorthand) now means
  a local path — the shorthand moves to bare `repo`. Documented; git sources and `host`+`path`
  configs are unaffected. (Today's behavior for reference: `mode()` `config.rs:622`;
  `host.unwrap_or("github")` `:130/:574`.)
- **`Selection`** — the membership authority (next section). One type, four consumers, one rule.
- **`Digest`** — unify the two digest types into one kernel ADT (algorithm + bytes); string form
  is a `Display`/`FromStr` concern at the edge.

### 3. Hexagonal — core depends on ports, adapters live at the edge
Domain logic depends on **traits (ports)**; `gix`, `ureq`, `tar`/`zip`/`flate2`, and `std::fs`
live behind adapters. The CLI is the **driving adapter**; a `wire()` composition root injects
concrete adapters.

| Port (trait) | Adapter | Status today |
|--------------|---------|--------------|
| `SourceBackend` (fetch/resolve/discover/export/digest) | `source::git`, `source::http` | exists (`source.rs:64`); git adapter **leaks via `RouterBackend::git_backend()` getter** (`backend.rs:25`) |
| `Registry` (deployment state) | `store::file` (`FileRegistry`) | exists (`registry.rs:74`), one impl |

Rules, inspection-enforceable:
- **`gix` only in `source/`.** Verified reality: `gix` is imported by `source.rs` (the git
  adapter) and `archive.rs` (one type, `gix::object::tree::EntryKind`). The worktree leak in the
  first draft was fabricated — that subsystem was removed in PR #12. The one genuine cross-module
  use is `archive.rs`'s `EntryKind`; relocate it under `source/` so the rule is grep-clean.
- **`ureq` only in `source/download.rs`; `tar`/`zip`/`flate2` only in `source/archive.rs`** — in
  *production* code (they also appear in `#[cfg(test)]` fixture helpers; the confinement grep must
  exclude tests).
- **Core must not depend on CLI.** A `cli/` split makes this a layout fact.

**Sealing is NOT pursued** (review B3/OE2): phora is a single binary with no downstream
implementors, and the in-crate test doubles (`backend.rs:298` spy + ~6 backends in `sync.rs`
tests) would have to be whitelisted. The real win — dropping the leaky `git_backend()`/
`http_backend()` getters — is kept; the dispatch tests that assert routing *through* those
getters (`backend.rs` dispatch tests) are rewritten to capture shared spy handles instead. A doc
comment communicates "not for external impl" at zero cost.

**No `Clock` port** (review OE1): the six `chrono::Utc::now()` sites produce informational
`projected_at`/`ejected_at` strings that nothing branches on or asserts. If determinism is ever
wanted, inject an `Option<OffsetDateTime>` at the `sync()` entry — a trait + adapter + threading
a 4th dependency through the pipeline is disproportionate. (The url synthetic-commit time is a
fixed *constant*, `IMPORT_TIME_SECONDS = 1` at `source.rs`, not a clock read — already
deterministic.)

### 4. Feature-organized — bounded contexts, not a god-module
`sync.rs` (5 655 lines) and `cli.rs` (2 598 lines) are the mechanical buckets. Split each slice
top to bottom. The recurring symptom — one membership rule re-typed five times — is exactly what
feature-organization plus a kernel value object removes.

## The selection seam (highest-leverage)

phora's analogue of gestalt's `Language` port: the one abstraction that deletes a category of
shotgun edits. **Artifact membership becomes one value object; every site calls it.**

```rust
// kernel/selection.rs — parsed once from include/exclude (+ the dotfile policy).
pub struct Selection { /* artifact globs, path globs, hidden-opt-in patterns */ }

impl Selection {
    /// Top-level membership: a directory is an artifact iff selected here.
    /// Hidden (`.`-prefixed) names are excluded UNLESS an include pattern itself
    /// begins with `.` (dotglob convention) — the ONLY place this rule lives.
    pub fn selects_artifact(&self, name: &str) -> bool { … }
    /// Per-file selection within an artifact (today's allows_path).
    pub fn selects_path(&self, rel: &RelPath, is_dir: bool) -> bool { … }
}
```

The dotfile gate is its **own predicate** layered over globset — globset's `*` already matches
`.config` (no dotglob), so `selects_artifact` must scan include patterns for a literal leading
`.` and gate hidden names on that. This is a small, well-tested rule, not a "one-liner."

Consumers — all call the same method, none re-implements the rule:
- `source::git::discover_artifacts` (`source.rs:274`) and `sync::discover` link mode
  (`sync.rs:699`) → `selects_artifact`.
- `sync::prune` foreign/orphan scan (`sync.rs:1151`, `1204`) → consults `Selection` instead of a
  bare `starts_with('.')`, so an orphaned dotfile artifact is pruned (the one real defect fix).
- `cli check-match` (`cli.rs:605`) → `selects_artifact` + `selects_path`. Routing it through
  `Selection` *changes its output* for dotfile names — a documented golden-test exception.

**The source-level `content_digest` is intentionally left out of this seam.** It is a
resolved-subtree fingerprint with no behavioral consumer (see canary §1); aligning its domain to
the deploy set would re-baseline locks broadly for no benefit. ARCH-003 only *documents* its
contract and pins it with a test; it changes no bytes.

**Dotfile opt-in, end state:** `include = [".config"]` or `[".*"]` selects hidden top-level
dirs; `["*"]` does not. One change, in `Selection`. No lock churn.

## Target module layout

```
src/
  lib.rs                 # surface: re-export contexts; wire() composition root
  main.rs                # CLI entry → cli::run()

  kernel/                # value objects every context speaks
    path.rs              # RelPath (normalized at construction) — absorbs paths.rs + scattered normalize
    digest.rs            # Digest (algo + [u8;32]) — unifies DownloadDigest + registry Digest
    commit.rs            # Commit (validated)
    selection.rs         # Selection: selects_artifact / selects_path + dotfile policy  (absorbs matcher.rs)

  config/                # parse-don't-validate boundary
    mod.rs               # raw serde structs → ParsedConfig (one validate step)
    source.rs            # ParsedSource + Remote ADT (Git | Host | Url); Refspec
    host.rs              # forges, remote templates, Protocol resolution
    layout.rs            # Layout ADT
    merge.rs             # local-overlay merge (operates on raw; parse runs once after)

  source/                # driven port + adapters: the content store
    mod.rs               # SourceBackend port + ExportRequest/Result (NOT sealed; getters dropped)
    git.rs               # GitBackend — ONLY importer of gix
    http.rs              # HttpBackend — download → verify → extract → import_tree
    mirror.rs            # MirrorKey, NormalizedUrl
    archive.rs           # format detect + extract — ONLY importer of tar/zip/flate2 (+ the EntryKind use)
    download.rs          # ureq — ONLY importer of the http client

  store/                 # driven port + adapter: deployment state  (was registry.rs)
    mod.rs               # Registry port
    file.rs              # FileRegistry (atomic write, state.lock, journal dir)
    record.rs            # ArtifactKey, RegistryRecord, ManifestFile, EjectedEntry

  lock/                  # Lock domain (Lock, LockedSource; merge/split)
  deploy/                # bounded context: projecting onto disk  (was projection.rs)
    mod.rs               # deploy_artifact, link_artifact, check_artifact_state
    state.rs             # ArtifactState ADT
    journal.rs           # deploy journal + recovery_sweep

  sync/                  # orchestration, split from the 5 655-line god-module
    mod.rs               # sync(): the use-case pipeline
    resolve.rs           # resolved_remotes, protocol selection, commit resolution
    discover.rs          # THE one discovery path (copy + link) over Selection
    target.rs            # per-target deploy loop + conflict resolution
    prune.rs             # registry-driven orphan/foreign detection over Selection
    verify.rs            # re-hash deployed files
    rebuild.rs           # rebuild_registry

  cli/                   # driving adapter: argv → use-case → render
    mod.rs               # Cli/Commands (clap), run(), wire()
    add.rs sync.rs list.rs where.rs eject.rs check_match.rs rebuild.rs
    render.rs            # the sole string/JSON producer
    # NOTE: rename the existing cli.rs::ParsedSource (config.rs:613, an add-URL intermediate)
    #       to e.g. AddTarget before introducing config::ParsedSource — name collision.

  error.rs               # crate-wide enum today; per-context split only if/when the crate splits
```

### Where today's modules land

| Today | Target |
|-------|--------|
| `sync.rs` (god-module) | `sync/` (`resolve`/`discover`/`target`/`prune`/`verify`/`rebuild`) |
| `cli.rs` (one file) | `cli/` per command family; clap + `wire()` in `cli/mod.rs` |
| `source.rs` | `source/` (`mod`/`git`/`http`/`mirror`) + `kernel/` value objects |
| `http.rs`, `archive.rs` | `source/download.rs`, `source/archive.rs` |
| `registry.rs` | `store/` |
| `projection.rs` | `deploy/` |
| `matcher.rs` | `kernel/selection.rs` (the membership authority) |
| `config.rs` (2 974 lines) | `config/` (`source` ADT, `host`, `layout`, `merge`) |
| `paths.rs` | `kernel/path.rs` |
| `lock.rs` | `lock/` |

## Ports — the trait contracts

Design rules (loqui `traits.md`): name for the capability; focused not god-traits; generics by
default, `&dyn` at the seam; owned returns; typed errors per port. Not sealed (see §3).

```rust
// source/mod.rs — port; impls are the in-crate git + http adapters.
pub trait SourceBackend {
    fn fetch(&self, source: &str, remote: &Remote) -> Result<()>;
    fn resolve(&self, source: &str, remote: &Remote, refspec: &Refspec) -> Result<Commit>;
    fn commit_time(&self, source: &str, remote: &Remote, commit: &Commit) -> Result<u64>;
    fn discover_artifacts(&self, ctx: &SourceCtx<'_>, sel: &Selection) -> Result<Vec<String>>;
    fn export_artifact(&self, req: &ExportRequest<'_>) -> Result<ExportResult>;
    // unchanged contract: a resolved-(sub)tree fingerprint, NOT the deploy set (see canary §1).
    fn content_digest(&self, ctx: &SourceCtx<'_>, sel: &Selection) -> Result<Digest>;
}

// store/mod.rs — Registry port; FileRegistry is the sole impl.
pub trait Registry { /* get/put/remove/list_*/load_ejected/save_ejected/locks_dir — unchanged */ }
```

`SourceCtx<'_>` bundles `(source, remote, commit, root)` to shrink the 5-positional-arg
signatures. Use-cases are generic over the port; `cli/`'s `wire()` names the concrete adapters;
tests inject the existing spy (`backend.rs:298`).

## Per-context cleanup (essentials)

- **kernel/** — `Selection` absorbs `PathMatcher` and is the sole membership + dotfile-policy
  definition. `RelPath` absorbs `paths.rs`. One `Digest` replaces `DownloadDigest` + registry
  `Digest`. `Commit` validates hex at the resolve boundary.
- **config/** — `ParsedSource`/`Remote` ADT collapses the `is_some()`-counting validators
  (`config.rs:33` + `:100`) into one parse after merge. Raw serde structs stay as the wire format.
- **source/** — drop the leaky `git_backend()`/`http_backend()` getters (`backend.rs:25-31`);
  rewrite the getter-based dispatch tests to capture shared spy handles. Relocate `archive.rs`'s
  `EntryKind` use so "gix only in `source/`" is grep-clean. `content_digest` unchanged.
- **sync/** — `discover.rs` is the single copy+link discovery path over `Selection`; `prune.rs`
  consults `Selection` (fixes the stranded-orphan bug); `sync()` becomes a thin pipeline.
- **cli/** — split per command family; argv parsing stops carrying business logic; one render path.

## Migration phases

### Phase 1 — kernel + the selection seam (low risk, highest leverage)
Prereq **ARCH-000**: stand up a golden/snapshot harness (e.g. `insta`) capturing
`sync`/`list`/`verify`/`where`/`check-match` output + a property test pinning current digests —
no such tests exist today, so the behavior-identical bar is unverifiable without this.
Then land `kernel/` (`RelPath`, `Digest`, `Commit`) + `Selection`; route discovery, prune, and
check-match through it; land the dotfile opt-in. Fixes the prune bug; **zero lock churn**;
documents the digest contract (no digest code change).

### Phase 2 — parse-don't-validate config + getter removal (medium risk)
`ParsedSource`/`Remote` ADT (after the `cli.rs::ParsedSource` rename); drop the adapter getters
and rewrite the dispatch tests; relocate `archive.rs`'s gix use.

### Phase 3 — context internals (structural)
Split `sync/` and `cli/`; relocate `projection`→`deploy/`, `registry`→`store/`,
`matcher`→`kernel/selection`, `config.rs`→`config/`. Per-context errors and the
`phora-core`/`phora-cli` crate split are deferred together (errors only earn their keep once the
crate splits) — optional stretch.

## Acceptance (behavior-identical refactor + the canary fix)

- Pure refactor: every command (`sync`, `update`, `verify`, `list`, `where`, `add`, `eject`,
  `uneject`, `rebuild-registry`, `check-match`) behaves identically — golden tests — save two
  documented exceptions: the dotfile opt-in, and `check-match`'s output for dotfile names.
- **Zero lock churn:** a property test asserts `content_digest` is byte-identical before/after
  (the digest is untouched).
- **Membership has one home:** artifact-membership `starts_with('.')` appears in exactly one
  place (`kernel/selection.rs`); discovery, prune, and check-match call it.
- **Dotfile opt-in:** `include=[".config"]`/`[".*"]` selects a hidden top-level dir; `["*"]` does not.
- **Orphan fix:** a deployed dotfile artifact removed from config is pruned from disk by `--prune`.
- **Confinement (grep, production code):** `gix` only under `source/`, `ureq` only in
  `source/download.rs`, archive crates only in `source/archive.rs`.
- `cargo clippy --all-targets` warning-free at `pedantic`.

## References

- `~/projects/gestalt/.worktrees/architecture-cleanup` — the reference cleanup.
- Loqui Rust guidelines (`~/.claude/skills/loqui/reference/loqui/languages/rust/`): `types.md`,
  `traits.md`, `errors.md`, `modules.md`.
- [Parse, don't validate](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/) ·
  [Hexagonal architecture](https://alistair.cockburn.us/hexagonal-architecture/).
