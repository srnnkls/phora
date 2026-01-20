# Implementation Context

## Native Plan

**Source:** `.claude/plans/silly-baking-rossum.md`

Design discussion that established:
- Cargo/Poetry inline table syntax for sources
- Single canonical `git` field format (no github/gitlab shorthand)
- Manifest as filter-only (no path rewriting, no action at distance)
- Target defaults to source key name
- Lock file with digest for lazy sync + per-file drift detection
- URL parser inspired by vendir patterns
- Root package (`phora.Config`, `phora.Lock`) as canonical types
- Single unified `phora.lock` at project root

## Key Files

### Config
- `config.go` - Source struct redefinition
- `config_test.go` - Config parsing tests
- `internal/defaults/phora.toml` - Default config example

### Lock File
- `lockfile.go` - Lock struct with digest, file hashes
- `lockfile_test.go` - Lock file tests

### URL Parser (New)
- `parse.go` - GitHub/GitLab URL parser
- `parse_test.go` - Parser tests

### CLI
- `internal/cli/add.go` - Integrate URL parser

## Reference Implementations

### Cargo.toml (Rust)
```toml
[dependencies]
rand = { git = "https://github.com/rust-lang/rand", branch = "next" }
rand = { git = "https://github.com/rust-lang/rand", tag = "v0.8.5" }
rand = { git = "https://github.com/rust-lang/rand", rev = "9f35b8e" }
```

### Poetry (Python)
```toml
[tool.poetry.dependencies]
package = { git = "https://github.com/user/repo.git", tag = "v1.0.0" }
```

### Vendir (DevOps)
- Config digest for lazy sync
- Per-file integrity hashing
- Include/exclude glob patterns

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Source format | Single `git` field with full URL | No shorthand complexity, explicit and unambiguous |
| Ref syntax | Explicit `branch`/`tag`/`rev` | Cargo convention, clear intent |
| Manifest artifacts | Export list (directory-organized) | What downstream consumers can import |
| Config sections | `[sources]` + `[manifest]` | Clear separation of concerns |
| Target default | Source key name | Predictable, explicit |
| Config digest | SHA256 of JSON with sorted keys | Standard Go pattern (Terraform/K8s) |
| File hashes | Per-file SHA256 | Integrity verification, drift detection |
| Drift handling | Error on mismatch, `--force` to overwrite | Explicit user control |
| Branch updates | Lock SHA authoritative, `phora update` to refresh | Cargo/Poetry pattern |
| Migration | Breaking change, no migration | Clean break, simplifies implementation |
| Filter algorithm | Include-then-exclude (no negation) | Simpler than gitignore, explicit |
| Slash refs in URLs | Require `--ref` flag | Avoids ambiguity |
| URL disambiguation | GitHub default | `owner/repo/path` assumes GitHub unless host detected |
| Canonical package | Root `phora` package | `phora.Config`, `phora.Lock` become single source of truth |
| Lock file location | Single `phora.lock` at project root | Replaces both repo-level and file-level lock files |
| Lock schema migration | Complete replacement, no migration | Old lock files ignored, new schema is sole source of truth |

## Gotchas

- GitHub `tree/ref/path` URLs with slash-containing refs require `--ref` flag
- Mutually exclusive validation: only one of branch/tag/rev allowed
- Empty `include = []` means no filtering (not "include nothing")
- Filter algorithm is include-then-exclude, NOT full gitignore (no negation)
- Locked SHA is authoritative for branches - use `phora update` to refresh
