# shellcheck shell=sh

# Pinned so fixture commit hashes are byte-for-byte stable across machines.
_PHORA_GIT_AUTHOR_DATE="@1700000000 +0000"
_PHORA_GIT_COMMITTER_DATE="@1800000000 +0000"

_phora_git() {
	git -c user.email=test@example.com -c user.name=Test \
		-c core.autocrlf=false -c commit.gpgsign=false "$@"
}

# GIT_*_DATE is scoped to the commit only; phora runs must never inherit them.
_phora_commit() {
	GIT_AUTHOR_DATE="$1" GIT_COMMITTER_DATE="$2" \
		_phora_git -C "$3" commit -q -m "$4"
}

_phora_write() {
	mkdir -p "$(dirname "$1")"
	printf '%s' "$2" >"$1"
}

isolate_state() {
	export HOME="$PWD"
	export XDG_CACHE_HOME="$PWD/cache"
	export XDG_STATE_HOME="$PWD/state"
	# XDG_DATA_HOME deliberately unset — matches the xdg-base-dirs scope.
	unset GIT_AUTHOR_DATE GIT_COMMITTER_DATE
	mkdir -p "$XDG_CACHE_HOME" "$XDG_STATE_HOME"
}

make_git_source() {
	repo="$PWD/src-$1"
	mkdir -p "$repo"
	_phora_git init -q -b main "$repo"

	_phora_write "$repo/editor/init.lua" "-- init
"
	_phora_write "$repo/editor/lua/opts.lua" "return {}
"
	_phora_write "$repo/lint/rules.toml" "[rules]
"
	_phora_write "$repo/README.md" "loose root file
"
	_phora_write "$repo/.config/settings.json" "{\"k\":1}
"

	_phora_git -C "$repo" add -A
	_phora_commit "$_PHORA_GIT_AUTHOR_DATE" "$_PHORA_GIT_COMMITTER_DATE" \
		"$repo" "fixture"

	# Bare path, not file:// — phora's is_local_path rejects any `://`.
	printf '%s\n' "$repo"
}

add_commit() {
	repo="$PWD/src-$1"
	_phora_write "$repo/editor/init.lua" "-- init v2
"
	_phora_git -C "$repo" add -A
	_phora_commit "@1700000001 +0000" "@1800000001 +0000" "$repo" "second"
}

make_overlay() {
	overlay="$PWD/overlay-$1"
	_phora_write "$overlay/config/app.toml" "[app]
"
	_phora_write "$overlay/notes.txt" "note
"
	printf '%s\n' "$overlay"
}

seed_config() {
	url="$1"
	if [ -z "$url" ]; then
		url="$(make_git_source seed)"
	fi
	target="$PWD/target-home"
	mkdir -p "$target"
	cat >"$PWD/phora.toml" <<EOF
version = 1

[sources.dotfiles]
path = "$url"
branch = "main"
include = ["editor", "lint"]

[targets.home]
path = "$target"
sources = ["dotfiles"]
layout = "flat"
EOF
}
