# Release assets, without curl | tar

Plenty of tools ship their shell completions inside the release tarball rather
than in the repo. Piping `curl | tar` into your fpath works but records nothing
about which version landed. This suite deploys the completions from a release
asset — digest-checked before extraction, recorded after — and then shows what a
wrong digest looks like.

State is hermetic — `isolate_state` points `HOME` and the XDG cache/state roots
at scrut's per-document tempdir, and the asset is built here and served by a
loopback HTTP server on `127.0.0.1`, so the download is a real HTTP download that
never leaves the machine. The tarball's bytes are hashed as they are found, so
the digest the config carries and the digest the error reports are the real ones.

## Start

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && echo ready
ready
```

The asset is packaged the way release tarballs commonly are: everything inside a
single top-level `<name>-<version>-<triple>/` directory, with the completions in
`autocomplete/` and the binary at the package root.

```scrut
$ PKG=bat-v0.24.0-x86_64-unknown-linux-gnu && mkdir -p srv "build/$PKG/autocomplete" && printf '_bat() { :; }\n' > "build/$PKG/autocomplete/bat.bash" && printf 'complete -c bat\n' > "build/$PKG/autocomplete/bat.fish" && printf '#compdef bat\n' > "build/$PKG/autocomplete/bat.zsh" && printf 'Register-ArgumentCompleter -Native -CommandName bat\n' > "build/$PKG/autocomplete/_bat.ps1" && printf '#!/bin/sh\necho bat\n' > "build/$PKG/bat" && chmod +x "build/$PKG/bat" && COPYFILE_DISABLE=1 tar --no-xattrs -czf "srv/$PKG.tar.gz" -C build "$PKG" && echo packed
packed
```

```scrut
$ BASE="$(serve_http_dir "$PWD/srv")" && DIGEST="$(sha256_of "srv/$PKG.tar.gz")" && echo serving
serving
```

A URL source is declared, not `add`ed — the digest belongs in the committed
config, and writing the file is the clearest way to put it there:

```scrut
$ printf 'version = 1\n\n[sources.bat]\nurl = "%s/%s.tar.gz"\ndigest = "sha256:%s"\ninclude = ["autocomplete"]\n\n[targets.completions]\npath = "completions"\nsources = ["bat"]\n' "$BASE" "$PKG" "$DIGEST" > phora.toml && echo declared
declared
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
Artifact: bat/autocomplete (commit cbfac8a6, digest blake3:54ae469bc5937fe62b2e37013bb7390675b3b877ddf3173cab6e0f79003177ae)
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
$ printf 'version = 1\n\n[sources.bat]\nurl = "%s/%s.tar.gz"\ndigest = "sha256:%s"\ninclude = ["autocomplete"]\n\n[targets.completions]\npath = "completions"\nsources = ["bat"]\n' "$BASE" "$PKG" "0000000000000000000000000000000000000000000000000000000000000000" > phora.toml && echo declared
declared
```

A plain `sync` does not notice — it honors the lock, the lock still matches, and
nothing is re-downloaded:

```scrut
$ phora sync
sync complete
```

`update` is the command that reaches for the network, so it is the one that
re-downloads — and the check fires against the fresh bytes, before extraction.
The reported digest is folded to `<ACTUAL>` only by substituting the tarball's
own sha256, which is what makes this an assertion that the two agree:

```scrut
$ phora update > update.log 2>&1
[1]
```

```scrut
$ sed "s/$DIGEST/<ACTUAL>/" update.log
error: source error: source bat: source error: sha256 digest mismatch: expected 0000000000000000000000000000000000000000000000000000000000000000, got <ACTUAL>
```

The mismatch stopped before extraction. The previously deployed files are
untouched and still verify against the old, good sync:

```scrut
$ phora verify
all verified
```

```scrut
$ stop_http_dir && echo stopped
stopped
```
