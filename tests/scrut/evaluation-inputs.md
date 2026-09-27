# Hold evaluation inputs steady while changing instructions

An evaluation compares two tagged instruction versions against the same fixture
release. Binding aliases let both versions come from one source. Phora prepares
and verifies the files; the evaluation runner consumes them separately.

```scrut
$ source "$TESTDIR"/_setup.sh && isolate_state && INSTRUCTIONS="$(make_git_source instructions)" && CASES="$(make_git_source cases)" && map_insteadof https://github.com/mock/instructions.git "$INSTRUCTIONS" && map_insteadof https://github.com/mock/cases.git "$CASES"
```

```scrut
$ _phora_write "$INSTRUCTIONS/AGENTS.md" $'Report each defect with its file and line.\n' && _phora_git -C "$INSTRUCTIONS" add -A && _phora_commit '@1700000002 +0000' '@1800000002 +0000' "$INSTRUCTIONS" 'baseline' && _phora_git -C "$INSTRUCTIONS" tag baseline
```

```scrut
$ _phora_write "$INSTRUCTIONS/AGENTS.md" $'Report each defect with its file, line, and a reproducer.\n' && _phora_git -C "$INSTRUCTIONS" add -A && _phora_commit '@1700000003 +0000' '@1800000003 +0000' "$INSTRUCTIONS" 'candidate' && _phora_git -C "$INSTRUCTIONS" tag candidate
```

```scrut
$ _phora_write "$CASES/cases/auth.json" $'{"request":"GET /admin","role":"guest","expected_status":403}\n' && _phora_git -C "$CASES" add -A && _phora_commit '@1700000002 +0000' '@1800000002 +0000' "$CASES" 'evaluation cases' && _phora_git -C "$CASES" tag v1
```

```scrut
$ cat > phora.toml <<'EOF'
> version = 1
> [sources.instructions]
> git = "https://github.com/mock/instructions.git"
> tag = "baseline"
> include = ["AGENTS.md"]
> [sources.fixtures]
> git = "https://github.com/mock/cases.git"
> tag = "v1"
> include = ["cases"]
> [targets.baseline]
> path = "runs/baseline"
> layout = "flat"
> [targets.baseline.sources]
> baseline = { source = "instructions" }
> fixtures = {}
> [targets.candidate]
> path = "runs/candidate"
> layout = "flat"
> [targets.candidate.sources]
> candidate = { source = "instructions", tag = "candidate" }
> fixtures = {}
> EOF
```

```scrut
$ phora sync
sync complete
```

```scrut
$ cat runs/baseline/AGENTS.md runs/candidate/AGENTS.md
Report each defect with its file and line.
Report each defect with its file, line, and a reproducer.
```

```scrut
$ cat runs/baseline/cases/auth.json && cmp runs/baseline/cases/auth.json runs/candidate/cases/auth.json
{"request":"GET /admin","role":"guest","expected_status":403}
```

Advancing both upstream branches does not change the locked evaluation inputs.
Frozen sync preserves both the lock and every deployed byte.

```scrut
$ cp phora.lock locked && cp -R runs before && _phora_write "$INSTRUCTIONS/AGENTS.md" $'Unreleased instructions.\n' && _phora_git -C "$INSTRUCTIONS" add -A && _phora_commit '@1700000004 +0000' '@1800000004 +0000' "$INSTRUCTIONS" 'next instructions' && _phora_write "$CASES/cases/auth.json" $'{"expected_status":200}\n' && _phora_git -C "$CASES" add -A && _phora_commit '@1700000004 +0000' '@1800000004 +0000' "$CASES" 'next cases'
```

```scrut
$ phora sync --frozen && cmp locked phora.lock && diff -r before runs && phora verify
sync complete
all verified
```

A runner that edits its fixture makes the candidate's inputs unverifiable,
while the baseline fixture remains intact.

```scrut
$ printf '{"expected_status":200}' > runs/candidate/cases/auth.json
```

```scrut
$ phora verify 2>&1
fixtures/cases: auth.json (content mismatch)
[1]
```

```scrut
$ cmp before/baseline/cases/auth.json runs/baseline/cases/auth.json && echo baseline-unchanged
baseline-unchanged
```
