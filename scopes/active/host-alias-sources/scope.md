---
created: 2026-06-07
status: active
issue_type: Feature
---

# host-alias-sources

## Goal

Let a source be declared symbolically against a host alias — `host = "github"`, `path = "owner/name"`,
optional `protocol = "ssh"` — instead of a fully-resolved low-level git remote. The alias and the host
definition stay in config as the single source of truth; the remote is resolved at runtime from the
host's `remote` template. `phora add github:owner/repo` persists the symbolic form, not the expanded
URL.

The model is **forge-agnostic**: a source carries one opaque `path` (the project's path on the forge,
possibly nested), the host owns the remote shape per protocol, and `protocol` selects https vs ssh at
the call side. Because phora already collapses ssh and https forms of one repo to a single
`MirrorKey` (`source.rs:1172`), `protocol` is a pure transport choice — flipping it never re-clones.

## Context

Today the alias is resolved eagerly and discarded:

- `parse_add_url` (`cli.rs:571`) expands a shorthand via `expand_template` (`cli.rs:711`) into a full
  URL; `insert_source_with_ref` writes `table["git"] = <resolved https URL>` (`cli.rs:221`).
- At runtime `source.git` is consumed **literally**: `NormalizedUrl::parse(&source.git)` →
  `MirrorKey` (`source.rs:85,118`) → `mirror_path` (`source.rs:141`) → fetch.

So `phora add github:srnnkls/tropos` bakes `git = "https://github.com/srnnkls/tropos.git"` into config,
losing the alias and the DRY benefit. The `Host`/`git_url` plumbing already exists (`host_templates`,
`cli.rs:584`) but is used only for `add` expansion, https-only, and the `git_` prefix is a misnomer
for scp-style ssh remotes (`git@host:path.git` is not a URL).

References: **straight.el** (`:host`/`:repo`/`:protocol` recipes with built-in forge knowledge),
**mise** github backend (built-in github/gitlab/codeberg/forgejo).

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Declaration form | Source: `host = "<alias>"` + `path = "owner/name"` + optional `protocol` | Coherent with the template placeholder; auth/host lookup is a direct field read. (user-chosen) |
| Mode exclusivity | A source is **either** `git` (literal remote) **or** `host`+`path`; exactly one | Keeps `git` for arbitrary remotes; existing configs untouched. |
| `path` → template | `path` fills `{path}` verbatim; `{owner}` = first segment, `{repo}` = **remainder**, so `{owner}/{repo}` ≡ `{path}` at any depth (GitLab subgroups) | Forge-agnostic; the host template owns forge structure (`.git`, scheme, `_git` infix, `~`). |
| Host remote field | Rename `git_url` → **`remote`**, deserialized **string-or-table** (like `layout`, `config.rs:307`): a single template string, **or** `{ https = "…", ssh = "…" }` | `remote` is honest for scp-style ssh (not a URL); the table lets one host carry both protocols. (user-raised) |
| Protocol | `protocol = "https" \| "ssh"`, default `https`; settable globally (top-level) and overridden per-source; selects the `remote` table key | straight.el-style. Ignored when `remote` is a single string. |
| Built-in forges | Ship `github`, `gitlab`, `codeberg`, `sr.ht`, `bitbucket` as default `remote` **tables** with both https + ssh shapes, overridable by a `[hosts.X]` def | Batteries-included; no template hand-writing for known forges. |
| Default host | `host` omitted while `path` is set ⇒ defaults to `github` | Mirrors `add owner/repo` → github. |
| Resolution | Lazy `Source::resolved_remote(&hosts, protocol) -> String`, fed into the existing `NormalizedUrl`/`MirrorKey` path | No literal remote stored; symbolic and literal forms of one repo **unify to one mirror**, across protocols. |
| `add` behavior | Persist `host` + `path` (+ `protocol` if non-default); accept `host:owner/repo` and `owner/repo` CLI input | The core ask: the alias lives in config. |
| Auth | Host is named in the source ⇒ auth selection is a direct lookup, not a URL reverse-match | Simpler and unambiguous. |
| Validation | Unknown host ⇒ error; `protocol` with no matching `remote` table key ⇒ error; `path` must be a non-empty forge path; built-in forges always available | Fail at the boundary (parse-don't-validate). |
| Lock | `LockedSource` keeps `resolved` = refspec (unchanged); the **remote identity** lives in `git` (or a renamed `remote` field); symbolic origin added as serde-default fields. `source_matches` compares **`NormalizedUrl`/`MirrorKey` identity** (NOT raw strings) + refspec + config_digest | review-CRITICAL: raw-string comparison + a resolved-remote string would break on protocol flip and collide with the existing `resolved` field. Normalized comparison makes ssh/https/literal one identity → no spurious refetch. |

## Requirements

### Behavior
- `[sources.X]` accepts `host` + `path` (+ optional `protocol`, and the usual
  `branch`/`tag`/`rev`/`root`/`include`/…); resolves via `hosts[host].remote` (built-in or `[hosts.X]`).
- `[hosts.X].remote` parses as either a string template or `{ https, ssh }`; `protocol` picks the key.
- A literal `git = "…"` source (https, ssh://, or scp-style) behaves exactly as today.
- `phora add github:srnnkls/tropos` (and `phora add srnnkls/tropos` → github) writes:
  ```toml
  [sources.tropos]
  host = "github"
  path = "srnnkls/tropos"
  ```
  preserving surrounding TOML formatting (toml_edit), not the expanded URL.
- A symbolic source, its literal twin, and the same repo over the other protocol all share one
  `~/.phora/git` mirror (same `MirrorKey`).

Example:
```toml
# protocol defaults to https; uncomment to flip the global default:
# protocol = "ssh"

[hosts.company]                        # custom forge: both shapes
remote = { https = "https://git.co/{path}.git", ssh = "git@git.co:{path}.git" }

[sources.tropos]
host = "github"                        # built-in; remote table shipped (https)
path = "srnnkls/tropos"
branch = "main"

[sources.internal]
host = "company"
path = "team/sub/proj"
protocol = "ssh"                       # per-source opt-in to ssh
```

### Given/When/Then
- **Given** `host` with no matching built-in or `[hosts]` def, **then** a config error names source + host.
- **Given** both `git` and `host` on one source, **then** a config error (mode exclusivity).
- **Given** `protocol = "ssh"` but the host's `remote` is a single https string (no `ssh` key), **then**
  a config error naming the source/host.
- **Given** `[hosts.company].remote` changes, **then** every `host = "company"` source resolves to the
  new remote on next sync with no per-source edits.
- **Given** a source switched from literal `git` to the equivalent `host`+`path`, or https↔ssh, **then**
  the lock still matches (no refetch).

## Key Files

| File | Change |
|---|---|
| `src/config.rs` | `Source.git` → **`Option<String>`**; `Source.host`/`path`/`protocol`; **mode-aware `merged_with`** (git XOR host+path atomic); `Host.remote` string-or-table (`RemoteConfig`, mirroring `LayoutConfigRaw`, `config.rs:307`); top-level `protocol` default overlaid by **`merge_configs`**; mode-exclusivity in `parse`, unknown-host + protocol/remote-key in a **post-merge** validate fn; migrate `git_url` fixtures to `remote`. |
| `src/source.rs` (or shared) | `Protocol` enum; `Source::resolved_remote(&hosts, protocol)` (select `remote[protocol]` or the string; fill `{path}`, derive `{owner}`/`{repo}`); built-in forge table; feed `NormalizedUrl`. |
| `src/sync.rs` | `resolve_sources` + discovery use `resolved_remote(&hosts, protocol)`; thread hosts + effective protocol. |
| `src/lock.rs` | `LockedSource` stores resolved remote (+ symbolic origin); `source_matches` compares protocol-independent identity. |
| `src/cli.rs` | `parse_add_url` accepts `host:owner/repo`; `insert_source` writes `host`/`path`(/`protocol`); `where`/`list` show the symbolic form. |
| `README.md` / `phora.example.toml` | Document host-aliased sources, `remote` table, `protocol`, built-in forges. |

## Out of Scope
- Inline refspec in the symbolic form (`github:owner/repo@ref`) — keep `branch`/`tag`/`rev` fields.
- Per-host API/release integration (mise-style) — phora only needs the git remote.
- Auto-migrating existing literal-`git` sources (they keep working; opt-in only).

## Verification
- `cargo test` green; new tests per task.
- Symbolic `host`+`path` resolves/fetches/deploys identically to its literal twin; https and ssh of the
  same repo share one mirror dir.
- `remote` parses as both string and `{ https, ssh }`; `protocol` selects; a missing key errors.
- `phora add github:srnnkls/tropos` writes `host`/`path` (asserted against toml_edit output); a later
  `sync` resolves it.
- Unknown host / dual-mode / bad path / protocol-without-key all produce boundary config errors.
