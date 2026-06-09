---
created: 2026-06-09
status: draft
issue_type: Task
---

# cli-verbosity

## Goal

Add two global flags — `--verbose`/`-v` (more output) and `--debug` (structured
log output) — backed by the `tracing` crate, so a user can see *what phora read,
resolved, and decided* without changing the default, quiet output. `--debug`
instruments four areas: config resolution, source resolution, projection/deploy,
and lock/registry decisions.

This is the "answer the confusion proactively" affordance the README/GUIDE point
at: today phora offers `check-match`/`verify`/`where` for after-the-fact
inspection, but no way to watch a run explain itself (the ripgrep `--debug`
model — "which config did you read, what did you resolve to?").

## Context

- The CLI is clap-derive: `Cli` (`src/cli.rs:21`) holds only `command`; subcommands
  are `Command` (`src/cli.rs:27`). Global flags belong on `Cli` with
  `#[arg(global = true)]`.
- `sync()` (`src/sync.rs:112`) is the orchestration spine: it merges config,
  resolves remotes, resolves sources against the lock, projects to targets, and
  records to the registry. It is single-threaded and sequential — a `tracing`
  subscriber with no spans-across-threads concerns is sufficient.
- Decision sites that `--debug` should narrate already exist; this task adds events
  at them, it does not restructure them:
  - config merge — `merge_configs` / `Config::validate` (`src/config.rs`)
  - source resolution — `resolved_remotes` + `resolve_sources` (`src/sync.rs:87,446`),
    `GitBackend`/`HttpBackend` `fetch`/`resolve` (`src/source.rs`), `mirror_path`
  - projection/deploy — `deploy_target` and the export walk (`src/sync.rs`, `src/source.rs`)
  - lock/registry — `source_matches` (`src/lock.rs:52`), registry record writes +
    journal/recovery (`src/registry.rs`, `src/sync.rs`)
- No verbosity/logging exists today; output is hand-rolled `println!`/`eprintln!`.

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Mechanism | `tracing` + `tracing-subscriber` (EnvFilter) | Structured events + levels; room to grow to per-module filters / JSON without reshaping call sites. (user-chosen) |
| Flag placement | `--verbose`/`-v` and `--debug` as `global = true` args on `Cli` | Work before or after the subcommand; one init path. |
| Level mapping | default = `WARN` (quiet); `-v` = `INFO`; `--debug` = `DEBUG`. `--debug` wins if both given | Predictable; `--verbose` is "more of the normal story", `--debug` is "explain every decision". |
| Sink | events to stderr; existing user-facing output on stdout is unchanged | Logs never pollute pipeable stdout; default runs look identical to today. |
| `RUST_LOG` | honored as an override when set (EnvFilter) | Power users can target a module; flags are the friendly front door. |
| Surfaces (`--debug`) | config resolution, source resolution, projection/deploy, lock/registry | All four chosen; each is a decision a confused user needs to see. (user-chosen) |

## Requirements

### Behavior
- `phora <cmd>` with no flag behaves exactly as today (no new output, stdout
  unchanged).
- `phora -v <cmd>` adds info-level narration to stderr (per-source resolved commit,
  per-artifact deploy summary).
- `phora --debug <cmd>` emits debug-level events at the four surfaces to stderr.
- `RUST_LOG`, when set, overrides the flag-derived filter.

### Given/When/Then
- Given `--debug sync`, then stderr shows which `phora.toml`/`phora.local.toml`
  were read and the merged result, each source's resolved remote/URL + mirror key +
  chosen commit, per-artifact projection decisions, and each lock match-vs-refetch
  decision.
- Given a lock that matches, when `--debug sync`, then a debug event states the
  source was reused (no fetch) and why (url/identity + config_digest match).
- Given no flag, then no `tracing` output appears and stdout is byte-identical to
  the pre-change behavior.
- Given both `-v` and `--debug`, then the effective level is `DEBUG`.

## Key Files

| File | Change |
|---|---|
| `Cargo.toml` | add `tracing`, `tracing-subscriber` (EnvFilter feature) |
| `src/main.rs` / `src/cli.rs` | global `--verbose`/`--debug` on `Cli`; init the subscriber once before dispatch from the resolved level (and `RUST_LOG` override) |
| `src/sync.rs` | `tracing` events/spans at config merge, source resolution, deploy; lock match decisions |
| `src/source.rs` | events at fetch/resolve (git + http), mirror path, synthetic-commit import |
| `src/lock.rs` | event at `source_matches` outcomes (reuse vs refetch + reason) |
| `README.md` / `GUIDE.md` | document the two flags; GUIDE gets the `--debug` "watch a run explain itself" note |

## Out of Scope
- JSON / machine-readable log output.
- A per-module verbosity UI beyond `RUST_LOG`.
- Logging to a file, log rotation.
- Async / parallel execution (sync stays sequential).
- Progress bars / TTY spinners.

## Verification
- `cargo test` green; default-run output unchanged (snapshot/assert a no-flag run
  emits no stderr tracing).
- `--debug sync` over a fixture shows config + source + deploy + lock events;
  `-v` shows the info subset; both-flags resolves to DEBUG.
- `RUST_LOG=phora::lock=debug` overrides the flag filter.
- Clippy clean; no new output on the default path.
