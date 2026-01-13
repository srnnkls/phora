# Phora Namespace and Reference Tests

## Deploy Non-Global Source (Namespaced)

```scrut
$ rm -rf /tmp/phora-test-namespace && cd "$TESTDIR/fixtures-namespace" && phora deploy --source . --target claude 2>&1
Deployed 1 artifact(s)
```

## Verify Namespaced Artifact Path

The artifact from non-global source "company" should be at company.code-test.

```scrut
$ ls /tmp/phora-test-namespace/skills/
company.code-test
```

## Verify Namespaced Directory Structure

```scrut
$ ls /tmp/phora-test-namespace/skills/company.code-test/
SKILL.md
```

## Verify Reference Transformation

Skill references (`$skill`) should be transformed to `/skill` format.
Tool references (`!tool`) should be mapped to tool names.

```scrut
$ cat /tmp/phora-test-namespace/skills/company.code-test/SKILL.md
---
allowed_tools:
  - bash
  - read
description: TDD workflow
name: code-test
---

# Code Test

Use `/code-review` skill after implementation.
Run `Bash` to execute tests.
```

## Verify Lock File Uses FullName

```scrut
$ grep "company.code-test" /tmp/phora-test-namespace/.phora.lock
path = 'skills/company.code-test/SKILL.md'
artifact = 'company.code-test'
```
