#!/usr/bin/env bash
# Clones real remotes over the network: run by hand, never in CI.
# Usage: benches/fetch_sweep.sh [path-to-phora-release-binary]
set -euo pipefail

BIN="${1:-target/release/phora}"
[ -x "$BIN" ] || { echo "build first: cargo build --release  (got: $BIN)"; exit 1; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

GIT_REPOS=(
  https://github.com/octocat/Hello-World.git https://github.com/octocat/Spoon-Knife.git
  https://github.com/octocat/git-consortium.git https://github.com/octocat/octocat.github.io.git
  https://github.com/octocat/test-repo1.git https://github.com/sindresorhus/is-npm.git
  https://github.com/sindresorhus/is-online.git https://github.com/sindresorhus/is-root.git
  https://github.com/sindresorhus/is-png.git https://github.com/sindresorhus/is-gif.git
  https://github.com/sindresorhus/is-jpg.git https://github.com/sindresorhus/is-svg.git
  https://github.com/sindresorhus/is-wsl.git https://github.com/sindresorhus/is-docker.git
  https://github.com/sindresorhus/is-path-cwd.git https://github.com/sindresorhus/is-stream.git
  https://github.com/sindresorhus/is-plain-obj.git https://github.com/sindresorhus/leven.git
  https://github.com/sindresorhus/escape-string-regexp.git https://github.com/sindresorhus/slash.git
)
GIT_REPOS_HEAVY=(
  https://github.com/BurntSushi/ripgrep.git https://github.com/sharkdp/fd.git
  https://github.com/sharkdp/bat.git https://github.com/junegunn/fzf.git
  https://github.com/jqlang/jq.git https://github.com/sharkdp/hyperfine.git
  https://github.com/dandavison/delta.git https://github.com/ajeetdsouza/zoxide.git
  https://github.com/starship/starship.git https://github.com/eza-community/eza.git
  https://github.com/cli/cli.git
)
# Real release-asset tarballs (plain tar; GitHub's auto-generated source tarballs
# carry a pax_global_header that phora's extractor rejects).
URL_TARBALLS=(
  https://nodejs.org/dist/v20.11.0/node-v20.11.0-darwin-arm64.tar.gz
  https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-aarch64-apple-darwin.tar.gz
)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/async-fetch-bench.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
export XDG_STATE_HOME="$WORK/state"

emit() { # label -> reads $WORK/t.txt
  awk -v l="$1" '/^real/{r=$2}/^user/{u=$2}/^sys/{s=$2}
    END{c=u+s; printf "%-18s real=%7.2fs cpu=%6.2fs io_wait=%7.2fs (%2.0f%% wait)\n", l, r, c, r-c, r>0?100*(r-c)/r:0}' "$WORK/t.txt"
}
sync_timed() { # jobs cold|warm -> exit nonzero on phora failure
  local jobs="$1" mode="$2"
  export XDG_CACHE_HOME="$WORK/cache"
  [ "$mode" = cold ] && rm -rf "$WORK/cache" "$WORK/state" "$WORK/proj/phora.lock" "$WORK/proj/phora.local.lock"
  ( cd "$WORK/proj" && /usr/bin/time -p "$BIN" sync --jobs "$jobs" >/dev/null 2>"$WORK/t.txt" )
}

mkdir -p "$WORK/proj"

write_git_config() { # repos... -> $WORK/proj/phora.toml
  { echo "version = 1"
    local i=0 u br
    for u in "$@"; do
      br="$(git ls-remote --symref "$u" HEAD 2>/dev/null | awk '/^ref:/{sub("refs/heads/","",$2);print $2;exit}')"; : "${br:=main}"
      printf '\n[sources.src%02d]\ngit = "%s"\nbranch = "%s"\n' "$i" "$u" "$br"; i=$((i+1))
    done; } > "$WORK/proj/phora.toml"
}

echo "== GIT tiny: ${#GIT_REPOS[@]}-distinct-source concurrency sweep =="
write_git_config "${GIT_REPOS[@]}"
for j in 1 8 16 50; do sync_timed "$j" cold && emit "cold jobs=$j"; done
sync_timed 8 warm && emit "warm jobs=8"

echo
echo "== GIT heavy: ${#GIT_REPOS_HEAVY[@]} medium/large repos (real index CPU) =="
write_git_config "${GIT_REPOS_HEAVY[@]}"
for j in 8 16 32; do sync_timed "$j" cold && emit "cold jobs=$j"; done

echo
echo "== URL: large-tarball download vs extract/import (jobs=1) =="
for t in "${URL_TARBALLS[@]}"; do
  printf 'version = 1\n[sources.tarball]\nurl = "%s"\n' "$t" > "$WORK/proj/phora.toml"
  sz="$(curl -fsIL "$t" | awk -F': ' 'tolower($1)=="content-length"{v=$2}END{print v+0}')"
  sync_timed 1 cold && emit "url $(printf %d $((sz/1024)))KB"
done
