# Release assets, without curl | tar

bat ships its shell completions inside the release tarball. Piping `curl | tar`
into your fpath works but records nothing about which version landed. This suite
deploys the completions from the real v0.24.0 release asset — digest-checked
before extraction, recorded after — and then shows what a wrong digest looks like.

State is hermetic — the first command points `HOME` and the XDG cache/state
roots at scrut's per-document tempdir; the download is real. Release assets are
uploaded bytes, not generated-on-demand tarballs, so the digest and the imported
commit are stable for as long as the asset exists.

## Start

```scrut
$ export HOME="$PWD" XDG_CACHE_HOME="$PWD/cache" XDG_STATE_HOME="$PWD/state" && mkdir -p cache state && echo ready
ready
```

A URL source is declared, not `add`ed — the digest belongs in the committed
config, and writing the file is the clearest way to put it there:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.bat]
> url = "https://github.com/sharkdp/bat/releases/download/v0.24.0/bat-v0.24.0-x86_64-unknown-linux-gnu.tar.gz"
> digest = "sha256:0faf5d51b85bf81b92495dc93bf687d5c904adc9818b16f61ec2e7a4f925c77a"
> include = ["autocomplete"]
>
> [targets.completions]
> path = "completions"
> sources = ["bat"]
> EOF
```

## Sync

Download, check the digest against the raw bytes, extract (validating every
entry path), strip the `bat-v0.24.0-…/` wrapper directory that release tarballs
commonly include, import the tree, project the selection. The order matters: the
digest is checked before extraction, so a mismatch stops before any file is written.

```scrut
$ phora sync
sync complete
```

```scrut
$ phora list
completions:
  bat/autocomplete  ✓ clean
```

```scrut
$ find completions -type f | LC_ALL=C sort
completions/autocomplete/_bat.ps1
completions/autocomplete/bat.bash
completions/autocomplete/bat.fish
completions/autocomplete/bat.zsh
```

A URL source has no git history, so phora gives it a synthetic commit —
content-addressed, so identical bytes import to the identical commit on any
machine, and this assertion holds verbatim:

```scrut
$ phora where
Artifact: bat/autocomplete (commit 48be2334, digest blake3:15eb1aaba8952b1214d23f7ad437c068163707a09d615b6fca92693e98e360fd)
  - completions
```

```scrut
$ phora verify
all verified
```

phora's artifact unit is the offered *leaf*, not a top-level directory, so a
single loose file deploys just as readily as a tree. The `bat` binary sits at the
tarball root; widen the offer to `include = ["bat", "autocomplete"]` and it lands
as its own `bat/bat` artifact, executable bit and all — the binary itself is now
in scope, no longer just its completions.

## What a wrong digest looks like

Suppose the config carried the wrong digest — a typo, or bytes that genuinely
are not what you were promised:

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
>
> [sources.bat]
> url = "https://github.com/sharkdp/bat/releases/download/v0.24.0/bat-v0.24.0-x86_64-unknown-linux-gnu.tar.gz"
> digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
> include = ["autocomplete"]
>
> [targets.completions]
> path = "completions"
> sources = ["bat"]
> EOF
```

A plain `sync` does not notice — it honors the lock, the lock still matches, and
nothing is re-downloaded:

```scrut
$ phora sync
sync complete
```

`update` is the command that reaches for the network, so it is the one that
re-downloads — and the check fires against the fresh bytes, before extraction:

```scrut
$ phora update 2>&1
error: source error: source bat: source error: sha256 digest mismatch: expected 0000000000000000000000000000000000000000000000000000000000000000, got 0faf5d51b85bf81b92495dc93bf687d5c904adc9818b16f61ec2e7a4f925c77a
[1]
```

The mismatch stopped before extraction. The previously deployed files are
untouched and still verify against the old, good sync:

```scrut
$ phora verify
all verified
```
