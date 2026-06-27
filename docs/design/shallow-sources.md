# Design: `shallow` option on sources

## Status

Draft

## Context

Today every git source gets a **full bare mirror** — all branches and tags
fetched via `+refs/heads/*:refs/heads/*` and `+refs/tags/*:refs/tags/*`.
This is correct but expensive for large repos where the consumer only
needs a single ref (often a tag). The existing `shallow_read_root_manifest`
already proves the approach for transitive manifest reads: depth-1 clone
of a single ref into an ephemeral staging dir. This design promotes that
pattern to a user-facing source option.

## Lock keying: one entry per target ref

The lock already keys entries by `(name, ref, instance)`, not by source
name alone. When two targets bind the same source at different refs, each
gets its own `LockedSource` with a `ref` discriminator
(`src/lock.rs:99–105`). This is the correct granularity because:

- A target can override the source's default ref via its binding
  (`branch`, `tag`, `rev` on the binding in `[targets.*.sources]`).
- Two bindings to different refs of the same source resolve to different
  commits and different digests — they must be separate lock entries.
- The merge logic in `merge_locks` deduplicates by the full
  `(name, ref, instance)` triple, so a local override on one ref
  preserves the other.

No changes needed here. The shallow option is orthogonal to lock keying.

## Proposal

### Config surface

Add an optional `shallow` boolean to the `Source` struct:

```toml
[sources.huge-repo]
repo = "org/huge-repo"
tag = "v2.0.0"
shallow = true        # new
```

Default: `false` (today's full-mirror behaviour).

Shallow is **only valid on git-mode sources** (forge, literal `git`, local
`path`). Rejected on `url` sources (they already fetch exactly one
archive). Rejected when no ref is pinned (i.e. when `Refspec::Default` —
the remote's advertised HEAD can change, and a shallow mirror has no way
to discover the new default branch without a full ls-remote).

### Semantics

| shallow | behaviour |
|---------|-----------|
| `false` | Full mirror: `MIRROR_REFSPECS` fetches all heads + tags. Any ref resolvable offline. |
| `true`  | Narrow mirror: fetches **only the requested ref** at depth 1. Resolution of other refs requires a new fetch. |

### Mirror strategy

A shallow source uses a **separate mirror namespace** to avoid corrupting
the full mirror (mixing shallow and non-shallow history in one repo is
fragile in git). The mirror key gains a `+shallow` suffix:

```
<MirrorKey>.git          # full mirror (existing)
<MirrorKey>+shallow.git  # shallow mirror (new)
```

Within the shallow mirror, only the currently-needed ref is present. On
ref change (e.g. the user bumps the tag from `v2.0.0` to `v2.1.0`):

1. The lock miss triggers a fetch (existing `lock_hit` returns `None`
   because `effective_ref.to_string() != locked.resolved`).
2. **Lazy fetch**: instead of re-fetching all refs, fetch only the new
   ref into the existing shallow mirror via a targeted
   `git fetch --depth=1 origin <refspec>`.
3. Old refs remain in the mirror (they're cheap — one commit each) but
   are never proactively fetched. The mirror grows only as the user
   changes refs.

### Implementation sketch

#### 1. Config (`src/config/source.rs`)

```rust
// Add to Source struct:
pub shallow: Option<bool>,

// Add to ParsedSource struct:
shallow: bool,
```

Validation in `classify()`: reject `shallow = true` when kind is `Url`,
or when no branch/tag/rev is set (Refspec::Default).

Include `shallow` in `config_digest()` so switching between shallow and
full forces a re-export (the mirror backend changes, which could affect
digest if the mirror is corrupt — belt and suspenders).

#### 2. Mirror path (`src/source.rs`)

```rust
impl GitBackend {
    fn mirror_path_for(&self, url: &str, shallow: bool) -> PathBuf {
        let key = MirrorKey::from_url(&NormalizedUrl::parse(url));
        if shallow {
            self.git_dir.join(format!("{}+shallow.git", key.as_str()))
        } else {
            self.git_dir.join(format!("{}.git", key.as_str()))
        }
    }
}
```

#### 3. Fetch (`src/source.rs` — `GitBackend::fetch`)

Today: always fetches `MIRROR_REFSPECS` (all heads + tags).

With shallow:

```rust
fn fetch_shallow(
    &self, source: &SourceName, url: &str, refspec: &Refspec,
) -> Result<()> {
    let mirror = self.mirror_path_for(url, true);
    let _lock = lock_mirror(&self.git_dir, source, url)?;

    if mirror.exists() {
        // Incremental: fetch only the requested ref, depth 1
        let repo = gix::open(&mirror)?;
        let mut remote = repo.find_remote("origin")?;
        let ref_name = shallow_ref_name(refspec)
            .expect("validated: shallow requires a pinned ref");
        remote.replace_refspecs(
            [format!("+{ref_name}:{ref_name}")],
            gix::remote::Direction::Fetch,
        )?;
        remote
            .connect(gix::remote::Direction::Fetch)?
            .prepare_fetch(gix::progress::Discard, Default::default())?
            .with_shallow(Shallow::DepthAtRemote(depth_1))
            .receive(gix::progress::Discard, &IS_INTERRUPTED)?;
    } else {
        // Initial: shallow clone of exactly one ref
        let staging = MirrorStaging::create(&self.git_dir, url);
        gix::prepare_clone_bare(url, &staging.path)?
            .with_shallow(Shallow::DepthAtRemote(depth_1))
            .with_ref_name(Some(shallow_ref_name(refspec).unwrap()))?
            .fetch_only(gix::progress::Discard, &IS_INTERRUPTED)?;
        staging.commit_to(&mirror, source.as_str())?;
    }
    Ok(())
}
```

#### 4. Resolve flow (`src/sync/resolve.rs`)

The `fetch_distinct_mirrors` function needs the effective refspec to know
_what_ to shallow-fetch. Today it only decides _whether_ to fetch (via
`lock_hit`); it doesn't pass the refspec to `backend.fetch()`.

For shallow sources, the fetch call must include the target ref:

```rust
// In fetch_distinct_mirrors, when the source is shallow:
backend.fetch_shallow(&name, &git, &unit.effective_ref)?;
```

This means the fetch grouping must carry the refspec for shallow units.
Full-mirror units continue to use the current `backend.fetch(name, git)`
(refspec-agnostic, fetches everything).

#### 5. SourceBackend trait

Extend the trait with an optional shallow fetch method:

```rust
trait SourceBackend {
    fn fetch(&self, source: &SourceName, url: &str) -> Result<()>;
    fn fetch_shallow(
        &self, source: &SourceName, url: &str, refspec: &Refspec,
    ) -> Result<()>;
    // ... rest unchanged
}
```

Or alternatively, merge into `fetch` with an `Option<&Refspec>` parameter
that signals shallow mode:

```rust
fn fetch(
    &self, source: &SourceName, url: &str, shallow_ref: Option<&Refspec>,
) -> Result<()>;
```

The second form is simpler. `None` = full mirror (today). `Some(ref)` =
shallow fetch of that single ref.

#### 6. Fetch grouping changes

Today, git-mode sources sharing a `MirrorKey` are deduplicated — one
fetch per mirror. Shallow sources break this assumption because different
units may need different refs fetched into the same shallow mirror.

Options:

**(a) Serial per-ref within a mirror key.** Keep the existing grouping
but let multiple refs queue. Each shallow fetch is cheap (one commit), so
the serial overhead is small.

**(b) Separate mirror per (url, ref).** Mirror key becomes
`<MirrorKey>+shallow+<encoded_ref>.git`. Maximally parallel but creates
many tiny repos. On ref change, the old mirror is orphaned until GC.

**(c) One shallow mirror, fetch all needed refs in one pass.** Collect all
distinct refs for a shallow source, build a multi-refspec fetch:
`["+refs/tags/v1:refs/tags/v1", "+refs/tags/v2:refs/tags/v2"]`, run once
with `Shallow::DepthAtRemote(1)`. Single round-trip, single mirror.

**Recommendation: (c).** It matches the existing "one mirror per URL"
invariant, minimises round-trips, and the mirror stays small (one commit
per ref, no history). The grouping in `fetch_distinct_mirrors` already
collects by `MirrorKey`; it just needs to accumulate `(refspec, source_name)`
pairs per key instead of stopping at the first.

### Source merging (local overlay)

`shallow` follows the same override pattern as other `Option` fields in
`Source.merged_with(local)`: if the local source sets `shallow`, it wins.
This lets a local overlay switch a source between shallow and full:

```toml
# phora.local.toml — fetch less during dev
[sources.huge-repo]
shallow = true
```

Switching from full to shallow (or vice versa) uses a different mirror
path, so there is no corruption risk. The next sync creates the missing
mirror from scratch.

### Interaction with `deploy = "link"`

Link-mode sources skip fetching entirely (they read `HEAD` from the local
checkout). `shallow` is meaningless on a linked source but harmless — the
fetch path is never entered. No validation error; just ignored.

### Interaction with transitive sources

A transitive shallow source would need its `phora.toml` readable at the
shallow commit. This works — `shallow_read_root_manifest` already handles
it. But the transitive _children_ inherit the parent's mirror. If the
parent is shallow, children's refs must also be fetchable.

For the initial implementation: **reject `shallow = true` on
`transitive = true` sources.** The interaction is tractable but adds
complexity; punt until there's a real use case.

### Interaction with `--force`

`--force` bypasses `lock_hit` and always fetches. For a shallow source
this means re-fetching the target ref at depth 1 — quick and correct.

### Interaction with `--frozen`

`--frozen` refuses to fetch. Shallow and full sources behave identically
here: if the lock has an entry, use it; if not, error. No special handling.

### Diagnostics

`phora status` (or whatever query surface) should indicate when a source
uses a shallow mirror, so users understand why resolving an arbitrary ref
might fail without a fetch.

## Open questions

1. **Should `shallow` affect `compute_digest`?** The digest reads blobs
   from the mirror's commit tree. A shallow clone has the full tree at the
   fetched commit (only _history_ is truncated). So `compute_digest` works
   unchanged. No impact.

2. **GC of stale shallow refs.** After a ref bump (v1→v2), the old commit
   remains in the shallow mirror. Over many bumps, refs accumulate.
   Consider a `phora gc` that prunes refs not referenced by any lock entry.
   Not blocking for v1.

3. **`Refspec::Rev` + shallow.** A bare commit hash can be fetched shallow
   if the server supports `uploadpack.allowReachableSHA1InWant`. Most
   major forges do. If the fetch fails, surface a clear error suggesting
   the user pin a branch or tag instead. Alternatively, reject
   `shallow + rev` at config validation time for safety.

4. **Should `shallow` be in `config_digest`?** It doesn't affect the
   _content_ of the export (same commit, same tree, same blobs). But
   including it forces a re-resolve on toggle, which is a useful safety
   net to ensure the new mirror type is populated. Lean yes.
