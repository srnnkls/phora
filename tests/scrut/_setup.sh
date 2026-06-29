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
	# TMPDIR per-doc so concurrent suites don't share rebuild staging in the system temp.
	export TMPDIR="$PWD/tmp"
	unset GIT_AUTHOR_DATE GIT_COMMITTER_DATE
	mkdir -p "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$TMPDIR"
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

# Exec (shell-free) on_change hook; the argv `$HOME` token must reach the file verbatim — a shell would expand it. Two identical entries also exercise enum-aware dedupe.
seed_config_exec_hook() {
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
on_change = [{ cmd = ["touch", "exec_ran_$HOME"] }, { cmd = ["touch", "exec_ran_$HOME"] }]
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

reset_deploy() {
	rm -rf "$PWD/target-home" "$XDG_STATE_HOME/phora/projects" "$PWD/phora.lock"
}

# Heredoc-free: scrut folds a `>` heredoc continuation into the command line.
seed_selection() {
	url="$1"
	offer="$2"
	binding_body="$3"
	target="$PWD/target-home"
	mkdir -p "$target"
	{
		printf 'version = 1\n\n[sources.dotfiles]\npath = "%s"\nbranch = "main"\n' "$url"
		if [ -n "$offer" ]; then
			printf '%s\n' "$offer"
		fi
		printf '\n[targets.home]\npath = "%s"\nlayout = "flat"\n' "$target"
		printf '\n[targets.home.sources.dotfiles]\n%s\n' "$binding_body"
	} >"$PWD/phora.toml"
}

# The plain `static.txt` sibling exists to prove non-`.tmpl` files copy untouched.
make_templated_source() {
	repo="$PWD/src-$1"
	mkdir -p "$repo"
	_phora_git init -q -b main "$repo"

	_phora_write "$repo/editor/motd.tmpl" "hello {{ greeting }}!
"
	_phora_write "$repo/editor/static.txt" "plain content
"

	_phora_git -C "$repo" add -A
	_phora_commit "$_PHORA_GIT_AUTHOR_DATE" "$_PHORA_GIT_COMMITTER_DATE" \
		"$repo" "fixture"

	printf '%s\n' "$repo"
}

# Quoted heredoc keeps `{{ greeting }}` literal; __URL__/__TARGET__ are sed-filled.
seed_config_with_vars() {
	url="$1"
	target="$PWD/target-home"
	mkdir -p "$target"
	cat >"$PWD/phora.toml" <<'EOF'
version = 1

[vars]
greeting = "base"

[sources.dotfiles]
path = "__URL__"
branch = "main"
include = ["editor"]

[targets.home]
path = "__TARGET__"
sources = ["dotfiles"]
layout = "flat"
EOF
	sed -i.bak -e "s#__URL__#$url#" -e "s#__TARGET__#$target#" "$PWD/phora.toml"
	rm -f "$PWD/phora.toml.bak"
}

seed_local_vars() {
	cat >"$PWD/phora.local.toml" <<EOF
version = 1

[vars]
greeting = "$1"
EOF
}

# A leaf repo carrying an `nvim/` subtree (and a root phora.toml outside it), with
# no hooks of its own — the composed surface a transitive dep binds.
make_composed_leaf() {
	repo="$PWD/src-$1"
	mkdir -p "$repo"
	_phora_git init -q -b main "$repo"

	_phora_write "$repo/nvim/init.lua" "-- init
"
	_phora_write "$repo/nvim/lua/opts.lua" "-- opts
"
	_phora_write "$repo/phora.toml" "version = 1
"

	_phora_git -C "$repo" add -A
	_phora_commit "$_PHORA_GIT_AUTHOR_DATE" "$_PHORA_GIT_COMMITTER_DATE" \
		"$repo" "leaf"

	printf '%s\n' "$repo"
}

# A transitive source must resolve to a real remote, never a bare local path; this
# rewrites a stable mock URL to the local fixture repo so a committed manifest can
# pin the URL while git fetches resolve offline.
map_insteadof() {
	cat >>"$HOME/.gitconfig" <<EOF
[url "$2"]
	insteadOf = $1
EOF
}

# A transitive dep repo composing a leaf's `nvim` subtree into a target that
# carries an on_change hook; `$2` is the mock URL the dep manifest pins for the leaf.
make_composing_dep() {
	repo="$PWD/src-$1"
	leaf_url="$2"
	mkdir -p "$repo"
	_phora_git init -q -b main "$repo"
	cat >"$repo/phora.toml" <<EOF
version = 1

[sources.editor]
git = "$leaf_url"
include = ["nvim"]

[targets.nvim]
path = "nvim"
sources = ["editor"]

[targets.nvim.hooks]
on_change = "touch \"\$HOME/dep-hook.sentinel\""
EOF
	_phora_git -C "$repo" add -A
	_phora_commit "$_PHORA_GIT_AUTHOR_DATE" "$_PHORA_GIT_COMMITTER_DATE" \
		"$repo" "dep"

	printf '%s\n' "$repo"
}

# Consumer config importing the transitive dep (pinned by mock URL `$1`) into a target.
seed_config_transitive() {
	dep_url="$1"
	target="$PWD/target-cfg"
	mkdir -p "$target"
	cat >"$PWD/phora.toml" <<EOF
version = 1

[sources.mydeps]
git = "$dep_url"
transitive = true

[targets.dotcfg]
path = "$target"
imports = ["mydeps"]
EOF
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
