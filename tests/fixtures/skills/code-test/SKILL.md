---
name: code-test
description: "TDD workflow using {{.model_strong}}"
model: "{{.model_strong}}"
user-invocable: true
allowed_tools:
  - read
  - write
  - bash
---

# Test-Driven Development

Use {{.model_strong}} for complex reasoning tasks.
Use {{.model_weak}} for simple validations.

## Workflow

1. Write failing test (RED)
2. Write minimal code (GREEN)
3. Refactor
