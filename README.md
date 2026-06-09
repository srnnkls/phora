# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

Phora keeps files in sync from git repositories into local directories. It mirrors
git **sources**, exports selected top-level entries (**artifacts**) from them, and
deploys those artifacts into **targets** (directories). Deployments are
content-addressed (BLAKE3) and recorded in a lock file and a global registry, so
re-syncs are deterministic and drift is detectable.

## Installation

Phora is written in Rust (edition 2024). Build from source:

```bash
cargo install --path .          # installs the `phora` binary
# or
cargo build --release           # binary at target/release/phora
```

## Usage

```bash
# Add a source to phora.toml
phora add https://github.com/owner/repo
phora add owner/repo --branch main --root config

# Fetch sources and deploy artifacts to targets
phora sync
phora sync --prune              # also remove artifacts no longer configured
phora sync --force              # re-deploy even when unchanged

# Bump the lock to the latest commit, then sync
phora update
phora update <source>

# Inspect
phora list                      # sources and deployment state
phora list --plan               # what a sync would change
phora verify                    # re-hash deployed files, detect drift
phora where --source <source>   # query the global registry
phora check-match --source <source> <path>   # debug include/exclude

# Stop / resume managing an artifact (files stay in place)
phora eject <artifact> --source <source> --target <target>
phora uneject <artifact> --source <source> --target <target>

# Rebuild the registry from the lock and on-disk targets
phora rebuild-registry
```

## Configuration

Phora reads `phora.toml` from the current directory:

```toml
version = 1

# Hosts (optional): a git URL template + auth, referenced by sources.
[hosts.github]
git_url = "https://github.com/{owner}/{repo}.git"   # {owner} {repo} {ref} {path}
auth = { type = "token", env = "GITHUB_TOKEN" }      # or { type = "ssh", key = "~/.ssh/id_ed25519" }

# Sources: git repositories to mirror, and which artifacts to export.
[sources.dotfiles]
git = "https://github.com/me/dotfiles.git"
branch = "main"             # or `tag = "..."`, or `rev = "..."`
root = "config"             # optional subdirectory to export from
include = ["nvim", "git"]   # artifact globs (top-level entries); empty = all
exclude = ["secrets"]
allow_symlinks = false
allow_submodules = false
preserve_executable = true

# Targets: where exported artifacts are deployed.
[targets.home]
path = "~/.config"
sources = ["dotfiles"]      # optional; omit to deploy every source
layout = "flat"             # "flat" | "by-source" | "prefixed"
# layout = { type = "prefixed", separator = "-" }
```

### Layouts

How a target lays out the artifacts it receives:

- **flat** — artifacts deploy directly under the target `path`.
- **by-source** — nested under `<source>/<artifact>`.
- **prefixed** — `<source><separator><artifact>` (default separator `-`).
