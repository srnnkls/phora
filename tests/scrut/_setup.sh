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

# macOS canonicalizes /var -> /private/var, so `phora add` emits the /private
# form while `target add` echoes the raw $PWD; collapse both to <ROOT>.
normalize() {
	sed -e "s#/private${PWD}#<ROOT>#g" -e "s#${PWD}#<ROOT>#g"
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

# The report prints the hook command's literal `$HOME` text (it is a format string, not a shell), so it stays byte-stable; the path expands only when the hook runs.
seed_config_with_hooks() {
	url="$1"
	if [ -z "$url" ]; then
		url="$(make_git_source seed)"
	fi
	target="$PWD/target-home"
	mkdir -p "$target"
	cat >"$PWD/phora.toml" <<'EOF'
version = 1

[sources.dotfiles]
path = "__URL__"
branch = "main"
include = ["editor", "lint"]

[targets.home]
path = "__TARGET__"
sources = ["dotfiles"]
layout = "flat"

[targets.home.hooks]
on_change = "cat \"$HOME/target-home/editor/init.lua\" >> \"$HOME/hook.log\""
EOF
	sed -i.bak -e "s#__URL__#$url#" -e "s#__TARGET__#$target#" "$PWD/phora.toml"
	rm -f "$PWD/phora.toml.bak"
}

# on_change hook that succeeds only once $HOME/allow exists: lets a scenario fail
# a hook, then fix the cause and prove it re-fires.
seed_config_failing_hook() {
	url="$1"
	target="$PWD/target-home"
	mkdir -p "$target"
	cat >"$PWD/phora.toml" <<'EOF'
version = 1

[sources.dotfiles]
path = "__URL__"
branch = "main"
include = ["editor"]

[targets.home]
path = "__TARGET__"
sources = ["dotfiles"]
layout = "flat"

[targets.home.hooks]
on_change = "test -f \"$HOME/allow\" && echo ran >> \"$HOME/hook.log\""
EOF
	sed -i.bak -e "s#__URL__#$url#" -e "s#__TARGET__#$target#" "$PWD/phora.toml"
	rm -f "$PWD/phora.toml.bak"
}

# A source repo whose own tree carries a hook-shaped phora.toml under payload/ —
# INV-1 fixture: that hook must stay inert when the tree is synced as content.
make_evil_source() {
	repo="$PWD/src-evil"
	mkdir -p "$repo/payload"
	_phora_git init -q -b main "$repo"
	_phora_write "$repo/payload/phora.toml" 'version = 1
[targets.x.hooks]
on_change = "touch \"$HOME/PWNED\""
'
	_phora_write "$repo/payload/data.txt" "hi
"
	_phora_git -C "$repo" add -A
	_phora_commit "$_PHORA_GIT_AUTHOR_DATE" "$_PHORA_GIT_COMMITTER_DATE" "$repo" "fixture"
	printf '%s\n' "$repo"
}

# Consumer config that includes the evil source's payload subtree and declares
# only a global post_sync hook (no target hooks).
seed_config_post_sync() {
	url="$1"
	target="$PWD/target-home"
	mkdir -p "$target"
	cat >"$PWD/phora.toml" <<'EOF'
version = 1

[sources.dotfiles]
path = "__URL__"
branch = "main"
include = ["payload"]

[targets.home]
path = "__TARGET__"
sources = ["dotfiles"]
layout = "flat"

[hooks]
post_sync = "echo post >> \"$HOME/post.log\""
EOF
	sed -i.bak -e "s#__URL__#$url#" -e "s#__TARGET__#$target#" "$PWD/phora.toml"
	rm -f "$PWD/phora.toml.bak"
}
