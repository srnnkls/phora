# phora

*φορά • a carrying, motion*

> From the zero-grade φρ- of φέρω (phérō, "to carry, bear")
>
> Pronunciation: /ˈfo.ra/

## About

Phora is an artifact manager and multiplexer. It treats ordinary files in git repositories, local
checkouts and HTTPS downloads the way a package manager treats packages, and fans one source out
to any number of directories that consume it. Each source publishes an offer of paths, and each
target takes the slice it wants. `phora.lock` pins every source to one commit, the registry
records a blake3 digest per deployed file, and an interrupted run resumes where it stopped.

Reach for it when shared configuration, editor setups, prompt or skill bundles, or release assets
live in one or more repositories but have to show up wherever other tools look for them.

## Installation

### Shell (Linux, macOS)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/srnnkls/phora/releases/latest/download/phora-installer.sh | sh
```

### Homebrew

```sh
brew install srnnkls/phora/phora
```

### mise

```sh
mise use -g github:srnnkls/phora
```

Drop `-g` to pin phora in a project's `mise.toml` instead.

### Cargo

```sh
cargo install phora
```

### Prebuilt binaries

Download an archive for your platform from the [releases page](https://github.com/srnnkls/phora/releases). Prebuilt targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Every release artifact ships with a SHA-256 checksum and an SLSA build-provenance attestation,
which you can check with `gh attestation verify <file> --repo srnnkls/phora`.

### From source

```sh
cargo install --path .
```

Run this from a checkout. It needs Rust 1.96 or newer.

## Getting started

Make a throwaway project:

```sh
mkdir phora-quickstart
cd phora-quickstart
cat > phora.toml <<'TOML'
[sources.phora]
repo = "srnnkls/phora"
branch = "main"
include = ["README.md"]

[targets.demo]
path = "./out"
sources = ["phora"]
TOML
```

The source is this public repository and the target is `./out`, so it runs as written:

```console
$ phora sync
sync complete

$ phora list
demo:
  phora/README.md  ✓ clean

$ phora verify
all verified
```

`phora sync` resolves `main` to a commit, records it in `phora.lock`, and copies `README.md` into
`out`. On a terminal it shows progress and a summary instead of `sync complete`. `phora list`
reports what landed, and `phora verify` re-hashes it and exits 1 if anything changed.

Next, try `phora preview` to see what a sync would do, and `phora add` and `phora bind` to edit the
configuration from the command line.

## Commands

| Command | Does |
| --- | --- |
| [`sync`](REFERENCE.md#phora-sync) | deploy every target from the locked sources |
| [`update`](REFERENCE.md#phora-update) | move pins to the newest commits, then sync |
| [`list`](REFERENCE.md#phora-list) | show each artifact and its state |
| [`verify`](REFERENCE.md#phora-verify) | re-hash deployed files against the registry |
| [`where`](REFERENCE.md#phora-where) | find where an artifact came from and where it went |
| [`preview`](REFERENCE.md#phora-preview) | show what a sync would deploy, offline |
| [`explain`](REFERENCE.md#phora-explain) | show why a path is or isn't deployed |
| [`check-match`](REFERENCE.md#phora-check-match) | test a path against a source's include and exclude rules |
| [`add`](REFERENCE.md#phora-add) | add a source from a URL or path and bind it |
| [`rm`](REFERENCE.md#phora-rm) | remove a source and its bindings |
| [`source`](REFERENCE.md#phora-source) | add, remove, list or show sources |
| [`target`](REFERENCE.md#phora-target) | add, remove, list or show targets |
| [`bind`](REFERENCE.md#phora-bind) | bind sources to targets |
| [`unbind`](REFERENCE.md#phora-unbind) | remove bindings from a target |
| [`eject`](REFERENCE.md#phora-eject) | stop managing an artifact and keep its files |
| [`uneject`](REFERENCE.md#phora-uneject) | manage an ejected artifact again |
| [`trust`](REFERENCE.md#phora-trust) | review and approve hooks from imported dependencies |
| [`rebuild-registry`](REFERENCE.md#phora-rebuild-registry) | rebuild the registry from the lock and the disk |

Every command accepts `-C <directory>` to run in another project.

## Concepts

| Term | Meaning |
| --- | --- |
| *source* | a git repository, local directory or HTTPS download, pinned by `branch`, `tag` or `rev` |
| *offer* | the files a source makes available after `root`, `include` and `exclude` |
| *target* | a directory that artifacts deploy into, listing the sources it uses |
| *binding* | one source used by one target, with an optional `take` |
| *take* | which offered files a binding deploys, and under which names |
| *artifact* | one offered file, deployed and tracked as a unit; a directory taken whole may collapse into one directory artifact |
| *layout* | where an artifact's path lands inside its target |
| *lock* | `phora.lock`, the commit pinned for each source |
| *registry* | the machine-local record of what was deployed where, at which commit and digest |

A source with its own `phora.toml` can be imported as a package, and its targets then deploy
beneath yours. [GUIDE.md](GUIDE.md#transitive-dependencies) covers how that composes.

## Documentation

- [GUIDE.md](GUIDE.md) explains how phora works, starting at [How phora works](GUIDE.md#how-phora-works).
- [USE-CASES.md](USE-CASES.md) has complete configs for dotfiles, shared lint config, release
  binaries, vendored protos and more.
- [REFERENCE.md](REFERENCE.md) lists every command, flag, key, environment variable and exit code.
- [phora.example.toml](phora.example.toml) and [phora.local.example.toml](phora.local.example.toml)
  show every configuration section.

## Development

```sh
mise run check              # clippy (pedantic, -D warnings), rustfmt --check, tests
mise run test               # cargo test
mise run test-integration   # scrut suites in tests/scrut/ against a release build
mise run fmt                # cargo fmt
mise run build              # cargo build
```

The scrut suites run the shipped binary end to end, and
[tests/scrut/showcase.md](tests/scrut/showcase.md) walks through a full session.
[docs/architecture.md](docs/architecture.md) describes the internals, and
[docs/RELEASING.md](docs/RELEASING.md) the release process.
